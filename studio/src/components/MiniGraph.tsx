import { useEffect, useRef } from "react";

import { type Edge, type Node, displayName, labelColor } from "../api.ts";
import { useStudio } from "../context.ts";
import { type EdgeInput, GraphEngine, type NodeInput } from "../graph/engine.ts";
import { enginePalette, useThemeVersion } from "../graph/theme.ts";

function collect(value: unknown, nodes: Map<string, Node>, edges: Map<string, Edge>): void {
  if (Array.isArray(value)) {
    for (const item of value) collect(item, nodes, edges);
  } else if (value && typeof value === "object") {
    if ("labels" in value && "created_at" in value) nodes.set((value as Node).id, value as Node);
    else if ("from" in value && "recorded_at" in value) edges.set((value as Edge).id, value as Edge);
  }
}

/** Whether the rows hold nodes, so they can be drawn. */
export function hasGraph(rows: unknown[][]): boolean {
  const nodes = new Map<string, Node>();
  collect(rows, nodes, new Map());
  return nodes.size > 0;
}

const toEdge = (edge: Edge): EdgeInput => ({
  id: edge.id,
  from: edge.from,
  to: edge.to,
  label: edge.label,
  ended: edge.valid_to !== null && edge.valid_to <= Date.now() * 1000,
});

/** Draws the nodes found in query rows, with the facts between them, live. */
export function MiniGraph({ rows, onOpen }: { rows: unknown[][]; onOpen: (id: string) => void }) {
  const { client, namespace } = useStudio();
  const canvas = useRef<HTMLCanvasElement>(null);
  const engine = useRef<GraphEngine | null>(null);
  const open = useRef(onOpen);
  open.current = onOpen;
  const theme = useThemeVersion();

  useEffect(() => {
    if (!canvas.current) return;
    const created = new GraphEngine(
      canvas.current,
      { select: () => undefined, open: (id) => open.current(id), menu: () => undefined, connect: () => undefined },
      enginePalette(),
    );
    engine.current = created;
    return () => {
      created.destroy();
      engine.current = null;
    };
  }, []);

  useEffect(() => {
    engine.current?.setPalette(enginePalette());
  }, [theme]);

  useEffect(() => {
    const nodes = new Map<string, Node>();
    const edges = new Map<string, Edge>();
    collect(rows, nodes, edges);
    const drawn: NodeInput[] = [...nodes.values()].map((node) => ({
      id: node.id,
      name: displayName(node),
      color: labelColor(node.labels[0] ?? ""),
      size: 10,
    }));
    engine.current?.setData(drawn, [...edges.values()].map(toEdge));
    let alive = true;
    // The rows may hold nodes without the edges that join them: fetch those.
    if (nodes.size > 1) {
      client
        .namespace(namespace)
        .subgraph([...nodes.keys()], { depth: 0 })
        .then((sub) => {
          if (!alive) return;
          for (const edge of sub.edges) edges.set(edge.id, edge);
          engine.current?.setData(drawn, [...edges.values()].map(toEdge));
        })
        .catch(() => undefined);
    }
    return () => {
      alive = false;
    };
  }, [rows, client, namespace]);

  return <canvas className="mini-graph" ref={canvas} title="Drag nodes to move them · double-click to open one" />;
}
