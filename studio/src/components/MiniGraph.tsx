import cytoscape, { type Core } from "cytoscape";
import { useEffect, useRef, useState } from "react";

import { type Edge, type Node, displayName, labelColor } from "../api.ts";
import { useStudio } from "../context.ts";

export function cssVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/** Changes when the theme does (chosen, or the system's), to restyle canvases. */
export function useThemeVersion(): number {
  const [version, setVersion] = useState(0);
  useEffect(() => {
    const bump = () => setVersion((value) => value + 1);
    const observer = new MutationObserver(bump);
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    media.addEventListener("change", bump);
    return () => {
      observer.disconnect();
      media.removeEventListener("change", bump);
    };
  }, []);
  return version;
}

/** Cytoscape styles shared by the graph views. */
export function graphStyle(): cytoscape.StylesheetJson {
  const text = cssVar("--text");
  const muted = cssVar("--muted");
  const accent = cssVar("--accent");
  const panel = cssVar("--panel");
  return [
    {
      selector: "node",
      style: {
        "background-color": "data(color)",
        "border-width": 2,
        "border-color": panel,
        label: "data(name)",
        color: text,
        "font-size": 11,
        "font-family": cssVar("--font-sans"),
        "text-valign": "bottom",
        "text-margin-y": 5,
        "text-outline-color": panel,
        "text-outline-width": 2,
        width: 24,
        height: 24,
        shape: "hexagon",
      },
    },
    { selector: "node.expanded", style: { width: 30, height: 30, "border-color": text } },
    { selector: "node:selected", style: { "border-width": 4, "border-color": accent, "overlay-opacity": 0 } },
    { selector: "node.source", style: { "border-width": 4, "border-color": cssVar("--warning"), "border-style": "dashed" } },
    {
      selector: "edge",
      style: {
        width: 1.6,
        "line-color": muted,
        "target-arrow-color": muted,
        "target-arrow-shape": "triangle",
        "arrow-scale": 0.9,
        "curve-style": "bezier",
        label: "data(label)",
        "font-size": 9,
        color: muted,
        "text-rotation": "autorotate",
        "text-background-color": panel,
        "text-background-opacity": 1,
        "text-background-padding": "2px",
      },
    },
    { selector: "edge.ended", style: { "line-style": "dashed", opacity: 0.55 } },
    { selector: "edge:selected", style: { "line-color": accent, "target-arrow-color": accent, color: accent, width: 2.5 } },
  ];
}

function collect(value: unknown, nodes: Map<string, Node>, edges: Map<string, Edge>): void {
  if (Array.isArray(value)) {
    for (const item of value) collect(item, nodes, edges);
  } else if (value && typeof value === "object") {
    if ("labels" in value && "created_at" in value) nodes.set((value as Node).id, value as Node);
    else if ("from" in value && "recorded_at" in value) edges.set((value as Edge).id, value as Edge);
  }
}

/** Force-directed for real graphs; a circle reads better for a handful of nodes. */
export function layoutFor(count: number, animate = false): cytoscape.LayoutOptions {
  if (count <= 6) return { name: "circle", animate, padding: 24, radius: 50 + count * 14, startAngle: Math.PI } as cytoscape.LayoutOptions;
  return { name: "cose", animate, padding: 50, nodeRepulsion: () => 12000, idealEdgeLength: () => 120 } as cytoscape.LayoutOptions;
}

/** Whether the rows hold nodes, so they can be drawn. */
export function hasGraph(rows: unknown[][]): boolean {
  const nodes = new Map<string, Node>();
  collect(rows, nodes, new Map());
  return nodes.size > 0;
}

/** Draws the nodes found in query rows, with the facts between them. */
export function MiniGraph({ rows, onOpen }: { rows: unknown[][]; onOpen: (id: string) => void }) {
  const { client, namespace } = useStudio();
  const host = useRef<HTMLDivElement>(null);
  const cy = useRef<Core | null>(null);
  const theme = useThemeVersion();

  useEffect(() => {
    if (!host.current) return;
    const nodes = new Map<string, Node>();
    const edges = new Map<string, Edge>();
    collect(rows, nodes, edges);
    cy.current = cytoscape({
      container: host.current,
      wheelSensitivity: 0.3,
      style: graphStyle(),
      elements: [
        ...[...nodes.values()].map((node) => ({
          group: "nodes" as const,
          data: { id: node.id, name: displayName(node), color: labelColor(node.labels[0] ?? "") },
        })),
        ...[...edges.values()]
          .filter((edge) => nodes.has(edge.from) && nodes.has(edge.to))
          .map((edge) => ({ group: "edges" as const, data: { id: edge.id, source: edge.from, target: edge.to, label: edge.label } })),
      ],
    });
    const graph = cy.current;
    graph.on("dbltap", "node", (event) => onOpen(event.target.id()));
    // Lay out once the container has its size, and again when it changes.
    const settle = () => {
      graph.resize();
      graph.layout(layoutFor(graph.nodes().length)).run();
      if (graph.zoom() > 1.5) {
        graph.zoom(1.5);
        graph.center();
      }
    };
    const frame = requestAnimationFrame(settle);
    const observer = new ResizeObserver(() => graph.resize());
    observer.observe(host.current);
    let alive = true;
    // The rows may hold nodes without the edges that join them: fetch those.
    if (nodes.size > 1) {
      client
        .namespace(namespace)
        .subgraph([...nodes.keys()], { depth: 0 })
        .then((sub) => {
          if (!alive) return;
          const missing = sub.edges.filter((edge) => !edges.has(edge.id));
          if (missing.length === 0) return;
          graph.add(missing.map((edge) => ({ group: "edges" as const, data: { id: edge.id, source: edge.from, target: edge.to, label: edge.label } })));
          settle();
        })
        .catch(() => undefined);
    }
    return () => {
      alive = false;
      cancelAnimationFrame(frame);
      observer.disconnect();
      graph.destroy();
    };
  }, [rows, onOpen, client, namespace, theme]);

  return <div className="mini-graph" ref={host} />;
}
