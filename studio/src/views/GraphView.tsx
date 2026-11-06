import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { type Edge, type Node, describeError, displayName, formatTime, labelColor } from "../api.ts";
import { NodeDetail } from "../components/NodeDetail.tsx";
import { useStudio } from "../context.ts";
import { type EdgeInput, GraphEngine, type NodeInput, type Target } from "../graph/engine.ts";
import { loadExpandedGraph } from "../graph/expansion.ts";
import { cssVar, enginePalette, useThemeVersion } from "../graph/theme.ts";
import { Icon, type IconName } from "../icons.tsx";
import { useTask } from "../ui.tsx";

/** Radius of a node, and of one whose neighbors are shown. */
const SIZE = 11;
const SIZE_EXPANDED = 13;

/** Graph algorithms that color nodes by group, run on the whole namespace. */
const COLOR_BY = {
  label: { title: "Label" },
  louvain: { title: "Community (Louvain)", settings: {} },
  labelPropagation: { title: "Community (label propagation)", settings: {} },
  wcc: { title: "Connected component", settings: {} },
} as const;

/** Graph algorithms that size nodes by a score. */
const SIZE_BY = {
  none: { title: "Same size" },
  pageRank: { title: "PageRank", settings: { direction: "both" } },
  degree: { title: "Degree", settings: {} },
  betweenness: { title: "Betweenness", settings: { direction: "both", sample: 300 } },
} as const;

type ColorBy = keyof typeof COLOR_BY;
type SizeBy = keyof typeof SIZE_BY;

/** Group or score per node id, from one algorithm run. */
type Analysis = { values: Map<string, number>; groups: number; max: number };

type Menu = { x: number; y: number; target: Target; nodes: string[] };

const CLUSTER = "cluster:";

async function analyze(
  graph: ReturnType<ReturnType<typeof useStudio>["client"]["namespace"]>,
  name: string,
  settings: Record<string, unknown>,
): Promise<Analysis> {
  const result = await graph.algorithm(name, { settings });
  const values = new Map<string, number>();
  for (const row of result.rows as [{ id: string }, number][]) values.set(row[0].id, Number(row[1]));
  const all = [...values.values()];
  return { values, groups: new Set(all).size, max: Math.max(0, ...all) };
}

export function GraphView({ seed }: { seed?: string }) {
  const { client, namespace, revision, newNode, newEdge, confirm, changed, toast } = useStudio();
  const host = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const minimap = useRef<HTMLCanvasElement>(null);
  const toolbar = useRef<HTMLDivElement>(null);
  const engine = useRef<GraphEngine | null>(null);
  const rootRequest = useRef(0);
  const [expanded, setExpanded] = useState<string[]>(seed ? [seed] : []);
  const [nodes, setNodes] = useState(new Map<string, Node>());
  const [edges, setEdges] = useState(new Map<string, Edge>());
  const [hidden, setHidden] = useState(new Set<string>());
  const [collapsed, setCollapsed] = useState(new Set<number>());
  const [selected, setSelected] = useState<string[]>(seed ? [seed] : []);
  const [selectedEdge, setSelectedEdge] = useState<string | null>(null);
  const [menu, setMenu] = useState<Menu | null>(null);
  const [search, setSearch] = useState("");
  const [validAt, setValidAt] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [colorBy, setColorBy] = useState<ColorBy>("label");
  const [sizeBy, setSizeBy] = useState<SizeBy>("none");
  const [groups, setGroups] = useState<Analysis | null>(null);
  const [scores, setScores] = useState<Analysis | null>(null);
  const [showMap, setShowMap] = useState(true);
  const { busy, run } = useTask();
  const theme = useThemeVersion();

  const expand = useCallback((id: string) => {
    rootRequest.current++;
    setExpanded((current) => (current.includes(id) ? current : [...current, id]));
  }, []);

  useEffect(() => () => { rootRequest.current++; }, [client, namespace]);
  const names = useMemo(() => new Map([...nodes.values()].map((node) => [node.id, displayName(node)])), [nodes]);
  const nameOf = (id: string) => names.get(id) ?? id;

  // Handlers bound once read the latest values from here.
  const latest = useRef({ expand, newEdge, nameOf, collapsed, setCollapsed });
  latest.current = { expand, newEdge, nameOf, collapsed, setCollapsed };

  // The engine lives as long as the view.
  useEffect(() => {
    if (!canvas.current) return;
    const created = new GraphEngine(
      canvas.current,
      {
        select: (ids, edge) => {
          setSelected(ids);
          setSelectedEdge(edge);
          setMenu(null);
        },
        open: (id) => {
          if (id.startsWith(CLUSTER)) {
            const group = Number(id.slice(CLUSTER.length));
            latest.current.setCollapsed((current) => new Set([...current].filter((c) => c !== group)));
          } else {
            latest.current.expand(id);
          }
        },
        menu: (target, x, y, ids) => setMenu({ target, x, y, nodes: ids }),
        connect: (from, to) => {
          if (from.startsWith(CLUSTER) || to.startsWith(CLUSTER)) return;
          const { nameOf: name } = latest.current;
          latest.current.newEdge({ from: { id: from, name: name(from) }, to: { id: to, name: name(to) } });
        },
      },
      enginePalette(),
    );
    engine.current = created;
    created.attachMinimap(minimap.current);
    // For checks in the browser console while developing.
    if (import.meta.env.DEV) (window as unknown as { graphEngine?: GraphEngine }).graphEngine = created;
    return () => {
      created.destroy();
      engine.current = null;
    };
  }, []);

  useEffect(() => {
    engine.current?.setPalette(enginePalette());
  }, [theme]);

  // Keep fitting clear of the floating toolbar and inspector.
  const inspectorOpen = selected.length > 0 || selectedEdge !== null;
  useEffect(() => {
    const update = () => {
      const area = host.current?.getBoundingClientRect();
      const bar = toolbar.current?.getBoundingClientRect();
      if (!area || !bar) return;
      engine.current?.setInsets({
        top: bar.bottom - area.top + 8,
        left: bar.left - area.left,
        right: inspectorOpen ? 410 : 12,
        bottom: 40,
      });
    };
    update();
    const observer = new ResizeObserver(update);
    if (host.current) observer.observe(host.current);
    if (toolbar.current) observer.observe(toolbar.current);
    return () => observer.disconnect();
  }, [inspectorOpen]);

  // Una expansión nueva invalida la anterior, también al limpiar o cambiar filtros.
  useEffect(() => {
    const controller = new AbortController();
    const { signal } = controller;
    setLoading(true);
    const filter = validAt ? { valid_at: validAt } : undefined;
    void loadExpandedGraph(client.namespace(namespace), expanded, filter, signal)
      .then((result) => {
        if (signal.aborted) return;
        setNodes(result.nodes);
        setEdges(result.edges);
        setError(null);
      })
      .catch((problem) => {
        if (!signal.aborted) setError(describeError(problem));
      })
      .finally(() => {
        if (!signal.aborted) setLoading(false);
      });
    return () => controller.abort();
  }, [client, namespace, expanded, validAt, revision]);

  // Run the chosen algorithms on the whole namespace (again after changes).
  useEffect(() => {
    let current = true;
    const graph = client.namespace(namespace);
    const color = COLOR_BY[colorBy];
    const size = SIZE_BY[sizeBy];
    Promise.all([
      "settings" in color ? analyze(graph, colorBy, color.settings) : null,
      "settings" in size ? analyze(graph, sizeBy, size.settings) : null,
    ])
      .then(([byGroup, byScore]) => {
        if (!current) return;
        setGroups(byGroup);
        setScores(byScore);
        if (!byGroup) setCollapsed(new Set());
      })
      .catch((problem) => current && setError(describeError(problem)));
    return () => {
      current = false;
    };
  }, [client, namespace, colorBy, sizeBy, revision]);

  // What the engine draws: visible nodes, collapsed communities as one node
  // each, and the facts between what is left.
  const drawn = useMemo(() => {
    const now = Date.now() * 1000;
    const groupOf = (id: string) => groups?.values.get(id);
    const placeOf = (id: string): string | null => {
      if (hidden.has(id)) return null;
      const group = groupOf(id);
      return group !== undefined && collapsed.has(group) ? `${CLUSTER}${group}` : id;
    };
    const colorOf = (id: string) => {
      const group = groupOf(id);
      return group === undefined ? labelColor(nodes.get(id)?.labels[0] ?? "") : cssVar(`--series-${(group % 8) + 1}`);
    };
    const sizeOf = (id: string) => {
      const score = scores?.values.get(id);
      if (scores && score !== undefined && scores.max > 0) return 8 + 18 * Math.sqrt(score / scores.max);
      return expanded.includes(id) ? SIZE_EXPANDED : SIZE;
    };
    const out: NodeInput[] = [];
    const clusters = new Map<number, number>();
    for (const node of nodes.values()) {
      const place = placeOf(node.id);
      if (place === null) continue;
      if (place !== node.id) {
        const group = groupOf(node.id)!;
        clusters.set(group, (clusters.get(group) ?? 0) + 1);
        continue;
      }
      out.push({ id: node.id, name: displayName(node), color: colorOf(node.id), size: sizeOf(node.id), ring: expanded.includes(node.id) });
    }
    for (const [group, count] of clusters) {
      out.push({
        id: `${CLUSTER}${group}`,
        name: `Community ${group}`,
        color: cssVar(`--series-${(group % 8) + 1}`),
        size: Math.min(34, 12 + Math.sqrt(count) * 4),
        count,
      });
    }
    const links: EdgeInput[] = [];
    const merged = new Map<string, EdgeInput & { n: number }>();
    for (const edge of edges.values()) {
      const from = placeOf(edge.from);
      const to = placeOf(edge.to);
      if (!from || !to || !nodes.has(edge.from) || !nodes.has(edge.to)) continue;
      const ended = edge.valid_to !== null && edge.valid_to <= now;
      if (from === edge.from && to === edge.to) {
        links.push({ id: edge.id, from, to, label: edge.label, ended });
        continue;
      }
      if (from === to) continue;
      // Facts into or out of a collapsed community become one edge.
      const key = `${from}>${to}`;
      const known = merged.get(key);
      if (known) {
        known.n += 1;
        known.label = `${known.n} facts`;
      } else {
        merged.set(key, { id: `${CLUSTER}${key}`, from, to, label: edge.label, n: 1 });
      }
    }
    links.push(...merged.values());
    return { nodes: out, edges: links };
    // The theme changes the series colors, read from CSS.
  }, [nodes, edges, hidden, collapsed, groups, scores, expanded, theme]);

  useEffect(() => {
    engine.current?.setData(drawn.nodes, drawn.edges);
  }, [drawn]);

  // The node the view was opened on starts selected, once it is drawn.
  const seeded = useRef(false);
  useEffect(() => {
    if (!seed || seeded.current || !drawn.nodes.some((node) => node.id === seed)) return;
    seeded.current = true;
    engine.current?.select([seed]);
  }, [seed, drawn]);

  // ------------------------------------------------------------ actions

  const addFromSearch = async () => {
    const query = search.trim();
    if (!query) return;
    const request = ++rootRequest.current;
    try {
      if (/^[0-9a-f-]{36}$/i.test(query)) {
        expand(query);
        return;
      }
      // A node already drawn: fly to it.
      const lower = query.toLowerCase();
      const onScreen = [...nodes.values()].find((node) => !hidden.has(node.id) && displayName(node).toLowerCase().includes(lower));
      if (onScreen && engine.current?.screenOf(onScreen.id)) {
        engine.current.focus(onScreen.id);
        setSearch("");
        return;
      }
      const graph = client.namespace(namespace);
      const byName = await graph.query(
        "MATCH (n) WHERE toLower(coalesce(n.name, n.title, n.label, '')) CONTAINS toLower($q) RETURN n.@id LIMIT 5",
        { q: query },
      );
      if (request !== rootRequest.current) return;
      const hits = await graph.searchText(query, 5);
      if (request !== rootRequest.current) return;
      const ids = [...new Set([...(byName.rows as string[][]).map((row) => row[0]), ...hits.map((hit) => hit.node)])];
      if (ids.length === 0) {
        setError(`Nothing is named or says “${query}”.`);
        return;
      }
      setExpanded((current) => [...new Set([...current, ...ids])]);
      setSearch("");
    } catch (problem) {
      if (request === rootRequest.current) setError(describeError(problem));
    }
  };

  const showAll = () => {
    const request = ++rootRequest.current;
    return run(async () => {
      try {
        const page = await client.namespace(namespace).listNodes({ limit: 60 });
        if (request !== rootRequest.current) return;
        setHidden(new Set());
        setExpanded(page.items.map((node) => node.id));
      } catch (problem) {
        if (request === rootRequest.current) throw problem;
      }
    });
  };

  const clear = () => {
    rootRequest.current++;
    setExpanded([]);
    setHidden(new Set());
    setCollapsed(new Set());
    setSelected([]);
    setSelectedEdge(null);
    engine.current?.select([]);
  };

  const hide = (ids: string[]) => {
    rootRequest.current++;
    setHidden((current) => new Set([...current, ...ids.filter((id) => !id.startsWith(CLUSTER))]));
    setSelected([]);
    engine.current?.select([]);
  };

  const collapseGroupOf = (id: string) => {
    const group = groups?.values.get(id);
    if (group === undefined) return;
    setCollapsed((current) => new Set([...current, group]));
    setSelected([]);
    engine.current?.select([]);
  };

  const deleteNodes = async (ids: string[]) => {
    const real = ids.filter((id) => !id.startsWith(CLUSTER));
    if (real.length === 0) return;
    const ok = await confirm({
      title: real.length === 1 ? `Delete “${nameOf(real[0])}”?` : `Delete ${real.length} nodes?`,
      message: "Their facts are deleted with them. It cannot be undone.",
      confirm: "Delete",
      danger: true,
    });
    if (!ok) return;
    rootRequest.current++;
    await run(async () => {
      const graph = client.namespace(namespace);
      for (const id of real) await graph.deleteNode(id);
      setExpanded((current) => current.filter((id) => !real.includes(id)));
      setSelected([]);
      toast(real.length === 1 ? "Node deleted." : `${real.length} nodes deleted.`, "ok");
      changed();
    });
  };

  const edgeAction = async (edge: Edge, action: "invalidate" | "retract" | "delete") => {
    const graph = client.namespace(namespace);
    const title = `${nameOf(edge.from)} —${edge.label}→ ${nameOf(edge.to)}`;
    if (action !== "invalidate") {
      const ok = await confirm({
        title: action === "delete" ? "Delete this fact?" : "Retract this fact?",
        message: action === "delete" ? `${title}. Its history is lost.` : `${title} was a mistake; KNOWN AT queries still see it.`,
        confirm: action === "delete" ? "Delete" : "Retract",
        danger: action === "delete",
      });
      if (!ok) return;
    }
    await run(async () => {
      if (action === "delete") await graph.deleteEdge(edge.id);
      else if (action === "retract") await graph.retractEdge(edge.id);
      else await graph.invalidateEdge(edge.id, new Date().toISOString());
      toast(action === "invalidate" ? "Fact ended today." : action === "delete" ? "Fact deleted." : "Fact retracted.", "ok");
      setSelectedEdge(null);
      changed();
    });
  };

  const onKey = (event: React.KeyboardEvent) => {
    if (event.target instanceof Element && event.target.closest("input, textarea, select, .inspector")) return;
    const key = event.key.toLowerCase();
    if (key === "escape") {
      setMenu(null);
      setSelected([]);
      setSelectedEdge(null);
      engine.current?.select([]);
    } else if (key === "delete" && selected.length > 0) {
      void deleteNodes(selected);
    } else if (key === "f") {
      engine.current?.fit(true, selected.length > 1 ? selected : undefined);
    } else if (key === "h" && selected.length > 0) {
      hide(selected);
    } else if (key === "p" && selected.length > 0) {
      const pinned = selected.every((id) => engine.current?.isPinned(id));
      engine.current?.pin(selected, !pinned);
    } else if (key === "+" || key === "=") {
      engine.current?.zoomBy(1.3);
    } else if (key === "-") {
      engine.current?.zoomBy(1 / 1.3);
    } else {
      return;
    }
    event.preventDefault();
  };

  // ------------------------------------------------------------ menus

  type Item = { icon: IconName; label: string; run: () => void; danger?: boolean; hint?: string } | "sep";
  const menuItems = (current: Menu): Item[] => {
    const e = engine.current;
    if (current.target.kind === "node") {
      const id = current.target.id;
      if (id.startsWith(CLUSTER)) {
        const group = Number(id.slice(CLUSTER.length));
        return [{ icon: "expand", label: "Expand the community", run: () => setCollapsed((c) => new Set([...c].filter((g) => g !== group))) }];
      }
      const ids = current.nodes.length > 1 ? current.nodes : [id];
      const pinned = ids.every((one) => e?.isPinned(one));
      if (ids.length > 1) {
        return [
          { icon: "fit", label: `Frame these ${ids.length} nodes`, hint: "F", run: () => e?.fit(true, ids) },
          { icon: "expand", label: "Expand all of them", run: () => { rootRequest.current++; setExpanded((c) => [...new Set([...c, ...ids])]); } },
          { icon: "pin", label: pinned ? "Unpin them" : "Pin them", hint: "P", run: () => e?.pin(ids, !pinned) },
          { icon: "eyeOff", label: "Hide them", hint: "H", run: () => hide(ids) },
          "sep",
          { icon: "trash", label: `Delete ${ids.length} nodes`, danger: true, hint: "Del", run: () => void deleteNodes(ids) },
        ];
      }
      return [
        { icon: "expand", label: "Expand neighbors", hint: "double-click", run: () => expand(id) },
        { icon: "link", label: "Connect to…", hint: "drag the +", run: () => newEdge({ from: { id, name: nameOf(id) } }) },
        { icon: "pin", label: pinned ? "Unpin" : "Pin here", hint: "P", run: () => e?.pin([id], !pinned) },
        { icon: "target", label: "Center on it", run: () => e?.focus(id) },
        { icon: "eyeOff", label: "Hide", hint: "H", run: () => hide([id]) },
        ...(groups?.values.has(id) ? [{ icon: "cluster" as IconName, label: "Collapse its community", run: () => collapseGroupOf(id) }] : []),
        "sep",
        { icon: "trash", label: "Delete node", danger: true, hint: "Del", run: () => void deleteNodes([id]) },
      ];
    }
    if (current.target.kind === "edge") {
      const edge = edges.get(current.target.id);
      if (!edge) return [];
      return [
        { icon: "clock", label: "End it today", run: () => void edgeAction(edge, "invalidate") },
        { icon: "ban", label: "Retract (a mistake)", run: () => void edgeAction(edge, "retract") },
        "sep",
        { icon: "trash", label: "Delete fact", danger: true, run: () => void edgeAction(edge, "delete") },
      ];
    }
    return [
      { icon: "fit", label: "Fit to screen", hint: "F", run: () => e?.fit(true) },
      { icon: "graph", label: "Lay out again", run: () => e?.reheat() },
      { icon: "pin", label: "Unpin all", run: () => e?.unpinAll() },
      ...(hidden.size > 0 ? [{ icon: "undo" as IconName, label: `Show ${hidden.size} hidden`, run: () => setHidden(new Set()) }] : []),
      ...(collapsed.size > 0 ? [{ icon: "cluster" as IconName, label: "Expand every community", run: () => setCollapsed(new Set()) }] : []),
      ...(groups && collapsed.size === 0
        ? [{ icon: "cluster" as IconName, label: "Collapse every community", run: () => setCollapsed(new Set(groups.values.values())) }]
        : []),
      "sep",
      { icon: "node", label: "New node", run: () => newNode({ onCreated: (id) => expand(id) }) },
    ];
  };

  useEffect(() => {
    if (!menu) return;
    const close = (event: MouseEvent) => {
      if (!(event.target instanceof Element && event.target.closest(".context-menu"))) setMenu(null);
    };
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [menu]);

  const edge = selectedEdge ? edges.get(selectedEdge) : undefined;
  const single = selected.length === 1 && !selected[0].startsWith(CLUSTER) ? selected[0] : null;
  const visible = drawn.nodes.length;

  return (
    <div className="graph" aria-busy={loading} ref={host} tabIndex={-1} onKeyDown={onKey}>
      <div className="canvas">
        <canvas className="graph-canvas" ref={canvas} aria-label="Graph canvas" />
      </div>
      <div className="graph-toolbar" ref={toolbar}>
        <div className="input-icon grow">
          <Icon name="search" />
          <input
            placeholder="Find a node: on screen, by name or text, or paste an id"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && void addFromSearch()}
          />
        </div>
        <button onClick={() => void addFromSearch()}>Add</button>
        <button onClick={() => void showAll()} disabled={busy} title="Draw the first 60 nodes and their facts">
          Show all
        </button>
        <span className="toolbar-sep" />
        <button className="primary" onClick={() => newNode({ onCreated: (id) => expand(id) })}>
          <Icon name="node" /> Node
        </button>
        <span className="toolbar-sep" />
        <label className="row small nowrap" title="Color nodes by a graph algorithm run on the whole namespace">
          Color
          <select value={colorBy} onChange={(e) => setColorBy(e.target.value as ColorBy)}>
            {Object.entries(COLOR_BY).map(([key, option]) => (
              <option key={key} value={key}>
                {option.title}
              </option>
            ))}
          </select>
        </label>
        <label className="row small nowrap" title="Size nodes by a centrality score over the whole namespace">
          Size
          <select value={sizeBy} onChange={(e) => setSizeBy(e.target.value as SizeBy)}>
            {Object.entries(SIZE_BY).map(([key, option]) => (
              <option key={key} value={key}>
                {option.title}
              </option>
            ))}
          </select>
        </label>
        <span className="toolbar-sep" />
        <label className="row small nowrap" title="Show only facts that were true on this date">
          <Icon name="clock" /> Valid at
          <input type="date" value={validAt} onChange={(e) => setValidAt(e.target.value)} />
        </label>
        {validAt && (
          <button className="ghost small-button" onClick={() => setValidAt("")}>
            Now
          </button>
        )}
      </div>

      <div className="graph-controls" role="toolbar" aria-label="View">
          <button className="ghost icon-button" onClick={() => engine.current?.zoomBy(1 / 1.3)} title="Zoom out (−)">
            <Icon name="zoomOut" />
          </button>
          <button className="ghost icon-button" onClick={() => engine.current?.zoomBy(1.3)} title="Zoom in (+)">
            <Icon name="zoomIn" />
          </button>
          <button className="ghost icon-button" onClick={() => engine.current?.fit(true)} title="Fit to screen (F, or double-click the background)">
            <Icon name="fit" />
          </button>
          <button className="ghost icon-button" onClick={() => engine.current?.reheat()} title="Lay out again">
            <Icon name="graph" />
          </button>
          <button className="ghost icon-button" onClick={() => engine.current?.unpinAll()} title="Unpin every node">
            <Icon name="pin" />
          </button>
          <button className={showMap ? "ghost icon-button active" : "ghost icon-button"} onClick={() => setShowMap((v) => !v)} title="Minimap">
            <Icon name="map" />
          </button>
          <button className="ghost icon-button" onClick={clear} title="Clear the canvas">
            <Icon name="eraser" />
          </button>
      </div>

      <div className="canvas-status small">
        {visible} nodes · {drawn.edges.length} facts
        {hidden.size > 0 && ` · ${hidden.size} hidden`}
        {validAt && ` · valid on ${validAt}`}
        {groups && ` · ${groups.groups} ${colorBy === "wcc" ? "components" : "communities"}`}
        {scores && ` · sized by ${SIZE_BY[sizeBy].title}`}
        <span className="faint"> · drag to move and pin · shift-drag to select · drag the + to connect · right-click for more</span>
      </div>
      <canvas className="minimap" ref={minimap} hidden={!showMap || visible === 0} aria-label="Minimap" />

      {error && (
        <div className="canvas-error error" onClick={() => setError(null)}>
          {error}
        </div>
      )}
      {expanded.length === 0 && (
        <div className="hint">
          <div className="empty-art" aria-hidden="true" />
          <p>Find a node above, press “Show all”, or create a node.</p>
        </div>
      )}

      {menu && (
        <div className="context-menu menu" style={{ left: menu.x, top: menu.y }} onContextMenu={(e) => e.preventDefault()}>
          {menuItems(menu).map((item, index) =>
            item === "sep" ? (
              <div key={`sep-${index}`} className="menu-sep" />
            ) : (
              <button
                key={item.label}
                className={item.danger ? "menu-item danger-item" : "menu-item"}
                onClick={() => {
                  setMenu(null);
                  item.run();
                }}
              >
                <span className="row nowrap">
                  <Icon name={item.icon} size={15} /> {item.label}
                </span>
                {item.hint && <kbd>{item.hint}</kbd>}
              </button>
            ),
          )}
        </div>
      )}

      {inspectorOpen && (
        <aside className="inspector">
          <div className="inspector-head">
            {single && (
              <>
                <button className="small-button" onClick={() => expand(single)}>
                  <Icon name="expand" size={14} /> Expand
                </button>
                <button className="small-button" onClick={() => newEdge({ from: { id: single, name: nameOf(single) } })}>
                  <Icon name="link" size={14} /> Connect to…
                </button>
                <button
                  className="small-button"
                  onClick={() => engine.current?.pin([single], !engine.current.isPinned(single))}
                  title="Pin or unpin (P)"
                >
                  <Icon name="pin" size={14} />
                </button>
              </>
            )}
            {selected.length > 1 && <strong>{selected.length} nodes</strong>}
            {edge && <strong>Fact</strong>}
            <div className="spacer" />
            <button
              className="ghost icon-button"
              onClick={() => {
                setSelected([]);
                setSelectedEdge(null);
                engine.current?.select([]);
              }}
              aria-label="Close"
            >
              <Icon name="x" />
            </button>
          </div>
          {single && <NodeDetail key={single} id={single} compact />}
          {selected.length > 1 && (
            <div className="stack tight">
              <p className="muted small">Drag any of them to move them together. Right-click for more.</p>
              <button onClick={() => engine.current?.fit(true, selected)}>
                <Icon name="fit" /> Frame them
              </button>
              <button onClick={() => engine.current?.pin(selected, true)}>
                <Icon name="pin" /> Pin them
              </button>
              <button onClick={() => engine.current?.pin(selected, false)}>Unpin them</button>
              <button onClick={() => hide(selected)}>
                <Icon name="eyeOff" /> Hide them
              </button>
              <button className="danger ghost" onClick={() => void deleteNodes(selected)} disabled={busy}>
                <Icon name="trash" /> Delete them
              </button>
            </div>
          )}
          {selected.length === 1 && selected[0].startsWith(CLUSTER) && (
            <div className="stack tight">
              <p className="muted small">A collapsed community. Double-click it to expand it.</p>
              <button onClick={() => setCollapsed((c) => new Set([...c].filter((g) => `${CLUSTER}${g}` !== selected[0])))}>
                <Icon name="expand" /> Expand it
              </button>
            </div>
          )}
          {edge && (
            <div className="detail compact">
              <div className="edge-title">
                <span>{nameOf(edge.from)}</span>
                <span className="mono accent">—{edge.label}→</span>
                <span>{nameOf(edge.to)}</span>
              </div>
              <dl className="facts small">
                <dt>@id</dt>
                <dd className="mono">{edge.id}</dd>
                <dt>valid</dt>
                <dd>
                  {edge.valid_from !== null ? formatTime(edge.valid_from).slice(0, 10) : "always"} →{" "}
                  {edge.valid_to !== null ? formatTime(edge.valid_to).slice(0, 10) : "now"}
                </dd>
                <dt>recorded</dt>
                <dd>{formatTime(edge.recorded_at)}</dd>
                {edge.recorded_by && (
                  <>
                    <dt>recorded by</dt>
                    <dd className="mono">{edge.recorded_by}</dd>
                  </>
                )}
              </dl>
              {Object.keys(edge.props).length > 0 && <pre>{JSON.stringify(edge.props, null, 2)}</pre>}
              <div className="stack tight">
                <button onClick={() => void edgeAction(edge, "invalidate")} disabled={busy || (edge.valid_to ?? Infinity) <= Date.now() * 1000}>
                  <Icon name="clock" /> End it today (keeps history)
                </button>
                <button onClick={() => void edgeAction(edge, "retract")} disabled={busy}>
                  <Icon name="ban" /> Retract (it was a mistake)
                </button>
                <button className="danger ghost" onClick={() => void edgeAction(edge, "delete")} disabled={busy}>
                  <Icon name="trash" /> Delete
                </button>
              </div>
            </div>
          )}
        </aside>
      )}
    </div>
  );
}
