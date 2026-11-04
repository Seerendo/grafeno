// A graph you can handle: a live force simulation (d3-force) drawn on a
// canvas of its own. Drag any node and the rest reacts; dragged nodes stay
// pinned where they were left. Pan with inertia, zoom at the cursor, lasso
// and shift-click to select several, drag from a node's handle to connect
// it to another. The engine knows nothing of Grafeno: the view gives it
// nodes and edges and listens to what the user does.

import {
  type Simulation,
  type SimulationLinkDatum,
  type SimulationNodeDatum,
  forceCollide,
  forceLink,
  forceManyBody,
  forceSimulation,
  forceX,
  forceY,
} from "d3-force";

export interface NodeInput {
  id: string;
  name: string;
  color: string;
  /** Radius in world units. */
  size: number;
  /** A drawn ring, for nodes whose neighbors are shown. */
  ring?: boolean;
  /** A collapsed group of nodes, drawn with its count. */
  count?: number;
}

export interface EdgeInput {
  id: string;
  from: string;
  to: string;
  label: string;
  /** Facts that ended are dashed. */
  ended?: boolean;
}

export interface GraphNode extends SimulationNodeDatum, NodeInput {
  pinned: boolean;
}

export interface GraphEdge extends SimulationLinkDatum<GraphNode>, Omit<EdgeInput, "from" | "to"> {
  source: GraphNode;
  target: GraphNode;
  /** Offset of the curve's control point, for parallel edges. */
  bend: number;
}

export type Target = { kind: "node"; id: string } | { kind: "edge"; id: string } | { kind: "canvas" };

export interface Events {
  /** The selection changed: nodes, or one edge. */
  select: (nodes: string[], edge: string | null) => void;
  /** Double-click on a node. */
  open: (id: string) => void;
  /** Right-click, at client coordinates. */
  menu: (target: Target, x: number, y: number, nodes: string[]) => void;
  /** A drag from a node's handle ended on another node. */
  connect: (from: string, to: string) => void;
  /** The view moved or zoomed (for the minimap and overlays). */
  view?: () => void;
}

export interface Palette {
  text: string;
  muted: string;
  faint: string;
  accent: string;
  panel: string;
  font: string;
  mono: string;
}

/** Room taken by floating panels, so fitting keeps nodes visible. */
export interface Insets {
  top: number;
  right: number;
  bottom: number;
  left: number;
}

type Point = { x: number; y: number };
type View = { x: number; y: number; k: number };

type Drag =
  | { kind: "none" }
  | { kind: "pan"; start: Point; origin: Point; last: Point; time: number; velocity: Point; moved: boolean }
  | { kind: "nodes"; ids: string[]; start: Point; origins: Map<string, Point>; moved: boolean; additive: boolean; hit: string }
  | { kind: "lasso"; points: Point[] }
  | { kind: "connect"; from: string; at: Point; over: string | null };

/** The six corners of a pointy-top hexagon of radius 1 (graphene). */
const HEX = Array.from({ length: 6 }, (_, i) => {
  const angle = Math.PI / 6 + (i * Math.PI) / 3;
  return { x: Math.cos(angle), y: Math.sin(angle) };
});

const MIN_ZOOM = 0.08;
const MAX_ZOOM = 6;
const CLICK = 4;

function inside(point: Point, polygon: Point[]): boolean {
  let hit = false;
  for (let i = 0, j = polygon.length - 1; i < polygon.length; j = i++) {
    const a = polygon[i];
    const b = polygon[j];
    if (a.y > point.y !== b.y > point.y && point.x < ((b.x - a.x) * (point.y - a.y)) / (b.y - a.y) + a.x) hit = !hit;
  }
  return hit;
}

function distanceToSegment(p: Point, a: Point, b: Point): number {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const length = dx * dx + dy * dy;
  const t = length === 0 ? 0 : Math.max(0, Math.min(1, ((p.x - a.x) * dx + (p.y - a.y) * dy) / length));
  return Math.hypot(p.x - (a.x + t * dx), p.y - (a.y + t * dy));
}

export class GraphEngine {
  private readonly ctx: CanvasRenderingContext2D;
  private nodes: GraphNode[] = [];
  private edges: GraphEdge[] = [];
  private byId = new Map<string, GraphNode>();
  private neighbors = new Map<string, Set<string>>();
  private readonly sim: Simulation<GraphNode, GraphEdge>;
  private transform: View = { x: 0, y: 0, k: 1 };
  private width = 0;
  private height = 0;
  private dpr = 1;
  private selected = new Set<string>();
  private selectedEdge: string | null = null;
  private hover: Target = { kind: "canvas" };
  private drag: Drag = { kind: "none" };
  private frame = 0;
  private tween: { from: View; to: View; start: number; length: number } | null = null;
  private inertia: Point | null = null;
  private minimap: HTMLCanvasElement | null = null;
  private insets: Insets = { top: 0, right: 0, bottom: 0, left: 0 };
  private fitted = false;
  private readonly observer: ResizeObserver;
  private readonly cleanup: (() => void)[] = [];

  private readonly canvas: HTMLCanvasElement;
  private readonly events: Events;
  private palette: Palette;

  constructor(canvas: HTMLCanvasElement, events: Events, palette: Palette) {
    this.canvas = canvas;
    this.events = events;
    this.palette = palette;
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("no 2D canvas");
    this.ctx = ctx;
    this.sim = forceSimulation<GraphNode, GraphEdge>()
      .force(
        "link",
        forceLink<GraphNode, GraphEdge>()
          .id((node) => node.id)
          .distance((edge) => 46 + edge.source.size + edge.target.size)
          .strength((edge) => 0.7 / Math.min(this.degree(edge.source.id), this.degree(edge.target.id))),
      )
      .force("charge", forceManyBody<GraphNode>().strength((node) => -180 - node.size * 8).distanceMax(700))
      .force("collide", forceCollide<GraphNode>((node) => node.size + 8).iterations(2))
      .force("x", forceX<GraphNode>(0).strength(0.035))
      .force("y", forceY<GraphNode>(0).strength(0.035))
      .alphaDecay(0.028)
      .on("tick", () => this.request());
    this.observer = new ResizeObserver(() => this.resize());
    this.observer.observe(canvas);
    this.listen();
    this.resize();
  }

  // ------------------------------------------------------------ data

  /**
   * Replaces the graph. Known nodes keep their place (and pins); new ones
   * grow out of a neighbor already drawn.
   */
  setData(nodes: NodeInput[], edges: EdgeInput[]): void {
    const previous = this.byId;
    const known = new Map<string, GraphNode>();
    const wanted = new Set(nodes.map((node) => node.id));
    const fresh: GraphNode[] = [];
    for (const input of nodes) {
      const old = previous.get(input.id);
      const node: GraphNode = old ? Object.assign(old, input) : { ...input, pinned: false };
      if (!old) fresh.push(node);
      known.set(node.id, node);
    }
    this.byId = known;
    this.nodes = [...known.values()];
    this.neighbors = new Map();
    const pairs = new Map<string, GraphEdge[]>();
    this.edges = [];
    for (const input of edges) {
      const source = known.get(input.from);
      const target = known.get(input.to);
      if (!source || !target || !wanted.has(input.from) || !wanted.has(input.to)) continue;
      const edge: GraphEdge = { id: input.id, label: input.label, ended: input.ended, source, target, bend: 0 };
      this.edges.push(edge);
      const key = input.from < input.to ? `${input.from}|${input.to}` : `${input.to}|${input.from}`;
      pairs.set(key, [...(pairs.get(key) ?? []), edge]);
      for (const [a, b] of [
        [input.from, input.to],
        [input.to, input.from],
      ]) {
        if (!this.neighbors.has(a)) this.neighbors.set(a, new Set());
        this.neighbors.get(a)!.add(b);
      }
    }
    // Parallel edges fan out; the bend is relative to one direction, so
    // edges going the other way flip it.
    for (const group of pairs.values()) {
      group.forEach((edge, index) => {
        if (group.length === 1 || edge.source === edge.target) return;
        const offset = (index - (group.length - 1) / 2) * 30;
        edge.bend = edge.source.id < edge.target.id ? offset : -offset;
      });
    }
    // New nodes start beside a neighbor that has a place.
    for (const node of fresh) {
      const anchor = [...(this.neighbors.get(node.id) ?? [])].map((id) => known.get(id)).find((other) => other && other.x !== undefined && !fresh.includes(other));
      const angle = Math.random() * Math.PI * 2;
      const radius = anchor ? 30 + Math.random() * 30 : 80 + Math.random() * 120;
      node.x = (anchor?.x ?? 0) + Math.cos(angle) * radius;
      node.y = (anchor?.y ?? 0) + Math.sin(angle) * radius;
    }
    for (const id of [...this.selected]) if (!known.has(id)) this.selected.delete(id);
    if (this.selectedEdge && !this.edges.some((edge) => edge.id === this.selectedEdge)) this.selectedEdge = null;
    this.sim.nodes(this.nodes);
    (this.sim.force("link") as ReturnType<typeof forceLink<GraphNode, GraphEdge>>).links(this.edges);
    const changed = fresh.length > 0 || previous.size !== known.size;
    if (changed) this.sim.alpha(fresh.length === this.nodes.length ? 1 : 0.6).restart();
    if (!this.fitted && this.nodes.length > 0) {
      // Let the first layout spread out, then frame it.
      this.fitted = true;
      for (let i = 0; i < 120; i++) this.sim.tick();
      this.fit(false);
    }
    this.request();
  }

  /** Changes colors and sizes without moving anything. */
  restyle(update: (node: GraphNode) => void): void {
    for (const node of this.nodes) update(node);
    (this.sim.force("collide") as ReturnType<typeof forceCollide<GraphNode>>).radius((node) => node.size + 8);
    this.sim.alpha(Math.max(this.sim.alpha(), 0.15)).restart();
    this.request();
  }

  setPalette(palette: Palette): void {
    this.palette = palette;
    this.request();
  }

  setInsets(insets: Insets): void {
    this.insets = insets;
  }

  attachMinimap(canvas: HTMLCanvasElement | null): void {
    this.minimap = canvas;
    if (!canvas) return;
    const move = (event: PointerEvent) => {
      const bounds = this.bounds();
      if (!bounds) return;
      const rect = canvas.getBoundingClientRect();
      const { scale, ox, oy } = this.minimapScale(bounds, rect.width, rect.height);
      const world = { x: (event.clientX - rect.left - ox) / scale, y: (event.clientY - rect.top - oy) / scale };
      this.centerOn(world, this.transform.k, false);
    };
    const down = (event: PointerEvent) => {
      canvas.setPointerCapture(event.pointerId);
      move(event);
      canvas.addEventListener("pointermove", move);
    };
    const up = () => canvas.removeEventListener("pointermove", move);
    canvas.addEventListener("pointerdown", down);
    canvas.addEventListener("pointerup", up);
    this.cleanup.push(() => {
      canvas.removeEventListener("pointerdown", down);
      canvas.removeEventListener("pointerup", up);
    });
    this.request();
  }

  // ------------------------------------------------------------ commands

  selection(): string[] {
    return [...this.selected];
  }

  select(ids: string[], edge: string | null = null): void {
    this.selected = new Set(ids.filter((id) => this.byId.has(id)));
    this.selectedEdge = edge;
    this.request();
  }

  pin(ids: string[], pinned: boolean): void {
    for (const id of ids) {
      const node = this.byId.get(id);
      if (!node) continue;
      node.pinned = pinned;
      node.fx = pinned ? node.x : null;
      node.fy = pinned ? node.y : null;
    }
    if (!pinned) this.sim.alpha(0.3).restart();
    this.request();
  }

  isPinned(id: string): boolean {
    return this.byId.get(id)?.pinned ?? false;
  }

  unpinAll(): void {
    this.pin(
      this.nodes.map((node) => node.id),
      false,
    );
  }

  /** Lays the graph out again from the current places. */
  reheat(): void {
    this.sim.alpha(1).restart();
  }

  /** Frames every node (or the given ones) in the free area. */
  fit(animate = true, ids?: string[]): void {
    const bounds = this.bounds(ids);
    if (!bounds) return;
    const free = {
      w: Math.max(80, this.width - this.insets.left - this.insets.right - 80),
      h: Math.max(80, this.height - this.insets.top - this.insets.bottom - 80),
    };
    const k = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, Math.min(free.w / (bounds.w || 1), free.h / (bounds.h || 1), 1.6)));
    this.centerOn({ x: bounds.x + bounds.w / 2, y: bounds.y + bounds.h / 2 }, k, animate);
  }

  /** Flies to a node and selects it. */
  focus(id: string): void {
    const node = this.byId.get(id);
    if (!node || node.x === undefined || node.y === undefined) return;
    this.select([id]);
    this.centerOn({ x: node.x, y: node.y }, Math.max(this.transform.k, 1.2), true);
    this.events.select([id], null);
  }

  zoomBy(factor: number): void {
    const center = this.freeCenter();
    this.zoomAround(center, this.transform.k * factor, true);
  }

  /** Screen position of a node, relative to the canvas. */
  screenOf(id: string): Point | null {
    const node = this.byId.get(id);
    if (!node || node.x === undefined || node.y === undefined) return null;
    return this.toScreen({ x: node.x, y: node.y });
  }

  zoom(): number {
    return this.transform.k;
  }

  destroy(): void {
    this.sim.stop();
    cancelAnimationFrame(this.frame);
    this.observer.disconnect();
    for (const undo of this.cleanup) undo();
  }

  // ------------------------------------------------------------ geometry

  private degree(id: string): number {
    return Math.max(1, this.neighbors.get(id)?.size ?? 1);
  }

  private toWorld(point: Point): Point {
    return { x: (point.x - this.transform.x) / this.transform.k, y: (point.y - this.transform.y) / this.transform.k };
  }

  private toScreen(point: Point): Point {
    return { x: point.x * this.transform.k + this.transform.x, y: point.y * this.transform.k + this.transform.y };
  }

  private freeCenter(): Point {
    return {
      x: this.insets.left + (this.width - this.insets.left - this.insets.right) / 2,
      y: this.insets.top + (this.height - this.insets.top - this.insets.bottom) / 2,
    };
  }

  private bounds(ids?: string[]): { x: number; y: number; w: number; h: number } | null {
    const list = ids ? ids.map((id) => this.byId.get(id)).filter((node): node is GraphNode => !!node) : this.nodes;
    if (list.length === 0) return null;
    let [x0, y0, x1, y1] = [Infinity, Infinity, -Infinity, -Infinity];
    for (const node of list) {
      const r = node.size + 20;
      x0 = Math.min(x0, (node.x ?? 0) - r);
      y0 = Math.min(y0, (node.y ?? 0) - r);
      x1 = Math.max(x1, (node.x ?? 0) + r);
      y1 = Math.max(y1, (node.y ?? 0) + r);
    }
    return { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
  }

  private centerOn(world: Point, k: number, animate: boolean): void {
    const center = this.freeCenter();
    const to = { k, x: center.x - world.x * k, y: center.y - world.y * k };
    if (!animate) {
      this.transform = to;
      this.tween = null;
    } else {
      this.tween = { from: { ...this.transform }, to, start: performance.now(), length: 420 };
    }
    this.inertia = null;
    this.request();
    this.events.view?.();
  }

  private zoomAround(screen: Point, k: number, animate = false): void {
    const clamped = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, k));
    const world = this.toWorld(screen);
    const to = { k: clamped, x: screen.x - world.x * clamped, y: screen.y - world.y * clamped };
    if (animate) this.tween = { from: { ...this.transform }, to, start: performance.now(), length: 240 };
    else this.transform = to;
    this.request();
    this.events.view?.();
  }

  private resize(): void {
    const rect = this.canvas.getBoundingClientRect();
    this.dpr = window.devicePixelRatio || 1;
    this.width = rect.width;
    this.height = rect.height;
    this.canvas.width = Math.round(rect.width * this.dpr);
    this.canvas.height = Math.round(rect.height * this.dpr);
    if (!this.fitted || this.nodes.length === 0) this.transform = { ...this.transform, ...this.centeredOrigin() };
    this.request();
  }

  private centeredOrigin(): Point {
    const center = this.freeCenter();
    return { x: center.x, y: center.y };
  }

  /** The node at a screen point, the nearest within its radius. */
  private nodeAt(screen: Point): GraphNode | null {
    const world = this.toWorld(screen);
    let best: GraphNode | null = null;
    let bestDistance = Infinity;
    for (const node of this.nodes) {
      const d = Math.hypot((node.x ?? 0) - world.x, (node.y ?? 0) - world.y);
      const reach = node.size + 4 / this.transform.k;
      if (d < reach && d < bestDistance) {
        best = node;
        bestDistance = d;
      }
    }
    return best;
  }

  private curve(edge: GraphEdge): { a: Point; c: Point; b: Point } {
    const a = { x: edge.source.x ?? 0, y: edge.source.y ?? 0 };
    const b = { x: edge.target.x ?? 0, y: edge.target.y ?? 0 };
    const mid = { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
    const length = Math.hypot(b.x - a.x, b.y - a.y) || 1;
    const normal = { x: -(b.y - a.y) / length, y: (b.x - a.x) / length };
    return { a, b, c: { x: mid.x + normal.x * edge.bend, y: mid.y + normal.y * edge.bend } };
  }

  private edgeAt(screen: Point): GraphEdge | null {
    const world = this.toWorld(screen);
    const tolerance = 6 / this.transform.k;
    let best: GraphEdge | null = null;
    let bestDistance = tolerance;
    for (const edge of this.edges) {
      let d: number;
      if (edge.source === edge.target) {
        const r = edge.source.size * 1.2;
        d = Math.abs(Math.hypot(world.x - (edge.source.x ?? 0), world.y - ((edge.source.y ?? 0) - edge.source.size - r)) - r);
      } else {
        const { a, b, c } = this.curve(edge);
        // The quadratic curve as a few segments.
        d = Infinity;
        let previous = a;
        for (let i = 1; i <= 8; i++) {
          const t = i / 8;
          const point = {
            x: (1 - t) * (1 - t) * a.x + 2 * (1 - t) * t * c.x + t * t * b.x,
            y: (1 - t) * (1 - t) * a.y + 2 * (1 - t) * t * c.y + t * t * b.y,
          };
          d = Math.min(d, distanceToSegment(world, previous, point));
          previous = point;
        }
      }
      if (d < bestDistance) {
        best = edge;
        bestDistance = d;
      }
    }
    return best;
  }

  /** Where the connect handle of a node sits, in screen coordinates. */
  private handleOf(node: GraphNode): Point {
    const center = this.toScreen({ x: node.x ?? 0, y: node.y ?? 0 });
    const r = node.size * this.transform.k + 9;
    return { x: center.x + r * Math.cos(-Math.PI / 4), y: center.y + r * Math.sin(-Math.PI / 4) };
  }

  private handleNode(): GraphNode | null {
    if (this.selected.size !== 1) return null;
    return this.byId.get([...this.selected][0]) ?? null;
  }

  // ------------------------------------------------------------ input

  private local(event: MouseEvent): Point {
    const rect = this.canvas.getBoundingClientRect();
    return { x: event.clientX - rect.left, y: event.clientY - rect.top };
  }

  private listen(): void {
    const canvas = this.canvas;
    const on = <K extends keyof HTMLElementEventMap>(type: K, handler: (event: HTMLElementEventMap[K]) => void, options?: AddEventListenerOptions) => {
      canvas.addEventListener(type, handler as EventListener, options);
      this.cleanup.push(() => canvas.removeEventListener(type, handler as EventListener));
    };

    on("pointerdown", (event) => {
      if (event.button !== 0) return;
      canvas.setPointerCapture(event.pointerId);
      this.tween = null;
      this.inertia = null;
      const point = this.local(event);
      const handle = this.handleNode();
      if (handle && Math.hypot(point.x - this.handleOf(handle).x, point.y - this.handleOf(handle).y) < 10) {
        this.drag = { kind: "connect", from: handle.id, at: point, over: null };
        this.request();
        return;
      }
      const node = this.nodeAt(point);
      if (node) {
        const additive = event.shiftKey || event.ctrlKey || event.metaKey;
        const ids = this.selected.has(node.id) && !additive ? [...this.selected] : [node.id];
        const origins = new Map(ids.map((id) => [id, { x: this.byId.get(id)!.x ?? 0, y: this.byId.get(id)!.y ?? 0 }]));
        this.drag = { kind: "nodes", ids, start: this.toWorld(point), origins, moved: false, additive, hit: node.id };
        return;
      }
      if (event.shiftKey) {
        this.drag = { kind: "lasso", points: [point] };
        return;
      }
      this.drag = {
        kind: "pan",
        start: point,
        origin: { x: this.transform.x, y: this.transform.y },
        last: point,
        time: performance.now(),
        velocity: { x: 0, y: 0 },
        moved: false,
      };
    });

    on("pointermove", (event) => {
      const point = this.local(event);
      const drag = this.drag;
      if (drag.kind === "pan") {
        const dx = point.x - drag.start.x;
        const dy = point.y - drag.start.y;
        if (!drag.moved && Math.hypot(dx, dy) < CLICK) return;
        drag.moved = true;
        const now = performance.now();
        const dt = Math.max(1, now - drag.time);
        drag.velocity = { x: (point.x - drag.last.x) / dt, y: (point.y - drag.last.y) / dt };
        drag.last = point;
        drag.time = now;
        this.transform = { ...this.transform, x: drag.origin.x + dx, y: drag.origin.y + dy };
        canvas.style.cursor = "grabbing";
        this.request();
        this.events.view?.();
      } else if (drag.kind === "nodes") {
        const world = this.toWorld(point);
        const dx = world.x - drag.start.x;
        const dy = world.y - drag.start.y;
        if (!drag.moved && Math.hypot(dx, dy) * this.transform.k < CLICK) return;
        if (!drag.moved) {
          drag.moved = true;
          this.sim.alphaTarget(0.25).restart();
        }
        for (const id of drag.ids) {
          const node = this.byId.get(id);
          const origin = drag.origins.get(id);
          if (!node || !origin) continue;
          node.fx = origin.x + dx;
          node.fy = origin.y + dy;
          // Follow the pointer at once, not at the next simulation tick.
          node.x = node.fx;
          node.y = node.fy;
        }
        canvas.style.cursor = "grabbing";
        this.request();
      } else if (drag.kind === "lasso") {
        drag.points.push(point);
        this.request();
      } else if (drag.kind === "connect") {
        drag.at = point;
        const over = this.nodeAt(point);
        drag.over = over && over.id !== drag.from ? over.id : null;
        this.request();
      } else {
        this.updateHover(point);
      }
    });

    const finish = (event: PointerEvent) => {
      const drag = this.drag;
      this.drag = { kind: "none" };
      canvas.style.cursor = "";
      if (canvas.hasPointerCapture(event.pointerId)) canvas.releasePointerCapture(event.pointerId);
      const point = this.local(event);
      if (drag.kind === "pan") {
        if (!drag.moved) {
          this.clickAt(point, event.shiftKey || event.ctrlKey || event.metaKey);
        } else if (Math.hypot(drag.velocity.x, drag.velocity.y) > 0.25 && performance.now() - drag.time < 80) {
          this.inertia = { x: drag.velocity.x * 16, y: drag.velocity.y * 16 };
          this.request();
        }
      } else if (drag.kind === "nodes") {
        if (!drag.moved) {
          this.clickAt(point, drag.additive);
        } else {
          // Left where the user put them.
          for (const id of drag.ids) {
            const node = this.byId.get(id);
            if (node) node.pinned = true;
          }
          this.sim.alphaTarget(0);
        }
      } else if (drag.kind === "lasso") {
        const caught = this.nodes.filter((node) => inside(this.toScreen({ x: node.x ?? 0, y: node.y ?? 0 }), drag.points)).map((node) => node.id);
        const additive = event.ctrlKey || event.metaKey;
        this.selected = new Set(additive ? [...this.selected, ...caught] : caught);
        this.selectedEdge = null;
        this.events.select([...this.selected], null);
      } else if (drag.kind === "connect" && drag.over) {
        this.events.connect(drag.from, drag.over);
      }
      this.request();
    };
    on("pointerup", finish);
    on("pointercancel", finish);

    on("pointerleave", () => {
      if (this.drag.kind === "none" && this.hover.kind !== "canvas") {
        this.hover = { kind: "canvas" };
        this.request();
      }
    });

    on(
      "wheel",
      (event) => {
        event.preventDefault();
        const factor = Math.exp(-event.deltaY * (event.ctrlKey ? 0.01 : 0.0016));
        this.tween = null;
        this.zoomAround(this.local(event), this.transform.k * factor);
      },
      { passive: false },
    );

    on("dblclick", (event) => {
      const node = this.nodeAt(this.local(event));
      if (node) this.events.open(node.id);
      else this.fit(true);
    });

    on("contextmenu", (event) => {
      event.preventDefault();
      const point = this.local(event);
      const node = this.nodeAt(point);
      if (node) {
        if (!this.selected.has(node.id)) {
          this.selected = new Set([node.id]);
          this.selectedEdge = null;
          this.events.select([node.id], null);
        }
        this.events.menu({ kind: "node", id: node.id }, event.clientX, event.clientY, [...this.selected]);
      } else {
        const edge = this.edgeAt(point);
        if (edge) {
          this.selected.clear();
          this.selectedEdge = edge.id;
          this.events.select([], edge.id);
          this.events.menu({ kind: "edge", id: edge.id }, event.clientX, event.clientY, []);
        } else {
          this.events.menu({ kind: "canvas" }, event.clientX, event.clientY, [...this.selected]);
        }
      }
      this.request();
    });
  }

  private clickAt(point: Point, additive: boolean): void {
    const node = this.nodeAt(point);
    if (node) {
      if (additive) {
        if (this.selected.has(node.id)) this.selected.delete(node.id);
        else this.selected.add(node.id);
      } else {
        this.selected = new Set([node.id]);
      }
      this.selectedEdge = null;
      this.events.select([...this.selected], null);
      return;
    }
    const edge = this.edgeAt(point);
    if (edge) {
      this.selected.clear();
      this.selectedEdge = edge.id;
      this.events.select([], edge.id);
      return;
    }
    if (!additive) {
      this.selected.clear();
      this.selectedEdge = null;
      this.events.select([], null);
    }
  }

  private updateHover(point: Point): void {
    const handle = this.handleNode();
    let cursor = "";
    let hover: Target = { kind: "canvas" };
    if (handle && Math.hypot(point.x - this.handleOf(handle).x, point.y - this.handleOf(handle).y) < 10) {
      cursor = "crosshair";
    } else {
      const node = this.nodeAt(point);
      if (node) {
        hover = { kind: "node", id: node.id };
        cursor = "grab";
      } else {
        const edge = this.edgeAt(point);
        if (edge) {
          hover = { kind: "edge", id: edge.id };
          cursor = "pointer";
        }
      }
    }
    this.canvas.style.cursor = cursor;
    const same = hover.kind === this.hover.kind && (hover.kind === "canvas" || (this.hover.kind !== "canvas" && hover.id === this.hover.id));
    if (!same) {
      this.hover = hover;
      this.request();
    }
  }

  // ------------------------------------------------------------ drawing

  private request(): void {
    if (this.frame) return;
    this.frame = requestAnimationFrame(() => {
      this.frame = 0;
      this.step();
      this.draw();
      this.drawMinimap();
    });
  }

  /** Advances the view's own motion: tweens and inertia. */
  private step(): void {
    if (this.tween) {
      const t = Math.min(1, (performance.now() - this.tween.start) / this.tween.length);
      const ease = 1 - (1 - t) ** 3;
      const { from, to } = this.tween;
      this.transform = { x: from.x + (to.x - from.x) * ease, y: from.y + (to.y - from.y) * ease, k: from.k + (to.k - from.k) * ease };
      if (t >= 1) this.tween = null;
      this.events.view?.();
      this.request();
    }
    if (this.inertia) {
      this.transform = { ...this.transform, x: this.transform.x + this.inertia.x, y: this.transform.y + this.inertia.y };
      this.inertia = { x: this.inertia.x * 0.9, y: this.inertia.y * 0.9 };
      if (Math.hypot(this.inertia.x, this.inertia.y) < 0.2) this.inertia = null;
      this.events.view?.();
      this.request();
    }
  }

  /** The node whose neighborhood is lit: the hovered one, or the only selected one. */
  private focusNode(): string | null {
    if (this.drag.kind === "nodes" && this.drag.moved) return this.drag.hit;
    if (this.hover.kind === "node") return this.hover.id;
    if (this.selected.size === 1) return [...this.selected][0];
    return null;
  }

  private draw(): void {
    const { ctx, palette } = this;
    const { x, y, k } = this.transform;
    ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    ctx.clearRect(0, 0, this.width, this.height);
    const focus = this.focusNode();
    const lit = focus ? new Set([focus, ...(this.neighbors.get(focus) ?? [])]) : null;
    const hoverEdge = this.hover.kind === "edge" ? this.hover.id : null;

    ctx.save();
    ctx.translate(x, y);
    ctx.scale(k, k);

    // Edges first, under the nodes.
    const width = 1.3 / Math.sqrt(k);
    for (const edge of this.edges) {
      const touches = !!focus && (edge.source.id === focus || edge.target.id === focus);
      const highlighted = touches || edge.id === this.selectedEdge || edge.id === hoverEdge;
      const dimmed = !!lit && !touches && edge.id !== this.selectedEdge;
      ctx.globalAlpha = dimmed ? 0.12 : highlighted ? 1 : 0.55;
      ctx.strokeStyle = highlighted ? palette.accent : palette.muted;
      ctx.fillStyle = ctx.strokeStyle;
      ctx.lineWidth = highlighted ? width * 1.9 : width;
      ctx.setLineDash(edge.ended ? [5, 4] : []);
      this.drawEdge(edge);
    }
    ctx.setLineDash([]);

    // Nodes: halo, hexagon, rings.
    for (const node of this.nodes) {
      const nx = node.x ?? 0;
      const ny = node.y ?? 0;
      const r = node.size;
      const dimmed = !!lit && !lit.has(node.id);
      const selected = this.selected.has(node.id);
      ctx.globalAlpha = dimmed ? 0.06 : node.id === focus || selected ? 0.32 : 0.16;
      ctx.fillStyle = node.color;
      ctx.beginPath();
      ctx.arc(nx, ny, r * 1.9, 0, Math.PI * 2);
      ctx.fill();

      ctx.globalAlpha = dimmed ? 0.22 : 1;
      ctx.beginPath();
      for (let i = 0; i < 6; i++) {
        const px = nx + HEX[i].x * r;
        const py = ny + HEX[i].y * r;
        if (i === 0) ctx.moveTo(px, py);
        else ctx.lineTo(px, py);
      }
      ctx.closePath();
      ctx.fill();
      ctx.lineWidth = 1.6 / k;
      ctx.strokeStyle = palette.panel;
      ctx.stroke();
      if (node.count !== undefined) {
        ctx.lineWidth = 1.4 / k;
        ctx.strokeStyle = node.color;
        ctx.beginPath();
        ctx.arc(nx, ny, r + 5, 0, Math.PI * 2);
        ctx.stroke();
      } else if (node.ring) {
        ctx.lineWidth = 1 / k;
        ctx.strokeStyle = palette.muted;
        ctx.beginPath();
        ctx.arc(nx, ny, r + 3.5, 0, Math.PI * 2);
        ctx.stroke();
      }
      if (selected) {
        ctx.lineWidth = 2.4 / k;
        ctx.strokeStyle = palette.accent;
        ctx.beginPath();
        ctx.arc(nx, ny, r + 6, 0, Math.PI * 2);
        ctx.stroke();
      }
      if (node.pinned) {
        ctx.fillStyle = palette.text;
        ctx.beginPath();
        ctx.arc(nx + r * 0.78, ny - r * 0.78, Math.max(2, r * 0.18), 0, Math.PI * 2);
        ctx.fill();
      }
    }
    ctx.restore();
    ctx.globalAlpha = 1;

    this.drawLabels(lit, focus);
    this.drawOverlays();
  }

  private drawEdge(edge: GraphEdge): void {
    const { ctx } = this;
    const arrow = 7;
    if (edge.source === edge.target) {
      const r = edge.source.size * 1.2;
      const cx = edge.source.x ?? 0;
      const cy = (edge.source.y ?? 0) - edge.source.size - r * 0.6;
      ctx.beginPath();
      ctx.arc(cx, cy, r, 0, Math.PI * 2);
      ctx.stroke();
      return;
    }
    const { a, b, c } = this.curve(edge);
    // Stop at the target's edge so the arrow tip shows.
    const angle = Math.atan2(b.y - c.y, b.x - c.x);
    const end = { x: b.x - Math.cos(angle) * (edge.target.size + 3), y: b.y - Math.sin(angle) * (edge.target.size + 3) };
    ctx.beginPath();
    ctx.moveTo(a.x, a.y);
    if (edge.bend === 0) ctx.lineTo(end.x - Math.cos(angle) * arrow * 0.6, end.y - Math.sin(angle) * arrow * 0.6);
    else ctx.quadraticCurveTo(c.x, c.y, end.x - Math.cos(angle) * arrow * 0.6, end.y - Math.sin(angle) * arrow * 0.6);
    ctx.stroke();
    ctx.setLineDash([]);
    ctx.beginPath();
    ctx.moveTo(end.x, end.y);
    ctx.lineTo(end.x - Math.cos(angle - 0.42) * arrow, end.y - Math.sin(angle - 0.42) * arrow);
    ctx.lineTo(end.x - Math.cos(angle + 0.42) * arrow, end.y - Math.sin(angle + 0.42) * arrow);
    ctx.closePath();
    ctx.fill();
    if (edge.ended) ctx.setLineDash([5, 4]);
  }

  /** Labels in screen space, so they stay crisp and the same size. */
  private drawLabels(lit: Set<string> | null, focus: string | null): void {
    const { ctx, palette } = this;
    const k = this.transform.k;
    ctx.textAlign = "center";
    ctx.lineJoin = "round";
    // Edge labels: on the lit edges, the hovered or selected one, or all when close.
    const hoverEdge = this.hover.kind === "edge" ? this.hover.id : null;
    ctx.font = `500 10px ${palette.mono}`;
    ctx.textBaseline = "middle";
    const many = this.edges.length > 400;
    for (const edge of this.edges) {
      const touches = !!focus && (edge.source.id === focus || edge.target.id === focus);
      const show = edge.id === hoverEdge || edge.id === this.selectedEdge || touches || (!lit && !many && k > 1.15);
      if (!show || edge.source === edge.target) continue;
      const { a, b, c } = this.curve(edge);
      const mid = this.toScreen({ x: 0.25 * a.x + 0.5 * c.x + 0.25 * b.x, y: 0.25 * a.y + 0.5 * c.y + 0.25 * b.y });
      const text = edge.label;
      const w = ctx.measureText(text).width + 10;
      ctx.globalAlpha = 0.92;
      ctx.fillStyle = palette.panel;
      ctx.beginPath();
      ctx.roundRect(mid.x - w / 2, mid.y - 8, w, 16, 8);
      ctx.fill();
      ctx.globalAlpha = 1;
      ctx.fillStyle = touches || edge.id === hoverEdge || edge.id === this.selectedEdge ? palette.accent : palette.muted;
      ctx.fillText(text, mid.x, mid.y + 0.5);
    }
    // Node labels: when nodes are big enough on screen, or lit.
    ctx.font = `500 12px ${palette.font}`;
    ctx.textBaseline = "top";
    for (const node of this.nodes) {
      const r = node.size * k;
      const isLit = !!lit && lit.has(node.id);
      const show = isLit || this.selected.has(node.id) || (!lit && (r >= 7 || k > 0.9)) || node.count !== undefined;
      if (!show || node.x === undefined || node.y === undefined) continue;
      if (lit && !isLit && !this.selected.has(node.id)) continue;
      const p = this.toScreen({ x: node.x, y: node.y });
      if (p.x < -100 || p.y < -40 || p.x > this.width + 100 || p.y > this.height + 40) continue;
      const text = node.count !== undefined ? `${node.name} · ${node.count}` : node.name;
      ctx.lineWidth = 3.5;
      ctx.strokeStyle = palette.panel;
      ctx.strokeText(text, p.x, p.y + r + 6);
      ctx.fillStyle = node.id === focus ? palette.accent : palette.text;
      ctx.fillText(text.length > 40 ? `${text.slice(0, 39)}…` : text, p.x, p.y + r + 6);
    }
  }

  private drawOverlays(): void {
    const { ctx, palette } = this;
    const drag = this.drag;
    // The connect handle of the selected node.
    const handle = this.handleNode();
    if (handle && drag.kind !== "nodes") {
      const p = this.handleOf(handle);
      ctx.globalAlpha = 1;
      ctx.fillStyle = palette.accent;
      ctx.shadowColor = palette.accent;
      ctx.shadowBlur = 10;
      ctx.beginPath();
      ctx.arc(p.x, p.y, 7, 0, Math.PI * 2);
      ctx.fill();
      ctx.shadowBlur = 0;
      ctx.strokeStyle = palette.panel;
      ctx.lineWidth = 1.8;
      ctx.beginPath();
      ctx.moveTo(p.x - 3.2, p.y);
      ctx.lineTo(p.x + 3.2, p.y);
      ctx.moveTo(p.x, p.y - 3.2);
      ctx.lineTo(p.x, p.y + 3.2);
      ctx.stroke();
    }
    if (drag.kind === "connect") {
      const from = this.screenOf(drag.from);
      if (from) {
        ctx.strokeStyle = palette.accent;
        ctx.lineWidth = 2;
        ctx.setLineDash([6, 5]);
        ctx.beginPath();
        ctx.moveTo(from.x, from.y);
        const to = drag.over ? (this.screenOf(drag.over) ?? drag.at) : drag.at;
        ctx.lineTo(to.x, to.y);
        ctx.stroke();
        ctx.setLineDash([]);
        if (drag.over && to) {
          const node = this.byId.get(drag.over)!;
          ctx.beginPath();
          ctx.arc(to.x, to.y, node.size * this.transform.k + 8, 0, Math.PI * 2);
          ctx.stroke();
        }
      }
    }
    if (drag.kind === "lasso" && drag.points.length > 1) {
      ctx.strokeStyle = palette.accent;
      ctx.fillStyle = palette.accent;
      ctx.lineWidth = 1.5;
      ctx.setLineDash([5, 4]);
      ctx.beginPath();
      drag.points.forEach((p, i) => (i === 0 ? ctx.moveTo(p.x, p.y) : ctx.lineTo(p.x, p.y)));
      ctx.closePath();
      ctx.globalAlpha = 0.08;
      ctx.fill();
      ctx.globalAlpha = 1;
      ctx.stroke();
      ctx.setLineDash([]);
    }
  }

  private minimapScale(bounds: { x: number; y: number; w: number; h: number }, w: number, h: number) {
    const scale = Math.min((w - 12) / (bounds.w || 1), (h - 12) / (bounds.h || 1));
    return { scale, ox: (w - bounds.w * scale) / 2 - bounds.x * scale, oy: (h - bounds.h * scale) / 2 - bounds.y * scale };
  }

  private drawMinimap(): void {
    const canvas = this.minimap;
    if (!canvas) return;
    const rect = canvas.getBoundingClientRect();
    if (rect.width === 0) return;
    if (canvas.width !== Math.round(rect.width * this.dpr)) {
      canvas.width = Math.round(rect.width * this.dpr);
      canvas.height = Math.round(rect.height * this.dpr);
    }
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    ctx.clearRect(0, 0, rect.width, rect.height);
    const bounds = this.bounds();
    if (!bounds) return;
    const { scale, ox, oy } = this.minimapScale(bounds, rect.width, rect.height);
    ctx.globalAlpha = 0.5;
    ctx.strokeStyle = this.palette.muted;
    ctx.lineWidth = 0.6;
    ctx.beginPath();
    for (const edge of this.edges) {
      ctx.moveTo((edge.source.x ?? 0) * scale + ox, (edge.source.y ?? 0) * scale + oy);
      ctx.lineTo((edge.target.x ?? 0) * scale + ox, (edge.target.y ?? 0) * scale + oy);
    }
    ctx.stroke();
    ctx.globalAlpha = 1;
    for (const node of this.nodes) {
      ctx.fillStyle = node.color;
      ctx.beginPath();
      ctx.arc((node.x ?? 0) * scale + ox, (node.y ?? 0) * scale + oy, Math.max(1.5, node.size * scale), 0, Math.PI * 2);
      ctx.fill();
    }
    // The part of the graph on screen.
    const top = this.toWorld({ x: 0, y: 0 });
    const bottom = this.toWorld({ x: this.width, y: this.height });
    ctx.strokeStyle = this.palette.accent;
    ctx.lineWidth = 1.2;
    ctx.strokeRect(top.x * scale + ox, top.y * scale + oy, (bottom.x - top.x) * scale, (bottom.y - top.y) * scale);
  }
}
