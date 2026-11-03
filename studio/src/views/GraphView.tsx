import cytoscape, { type Core, type ElementDefinition } from "cytoscape";
import { useCallback, useEffect, useRef, useState } from "react";

import { type Edge, type Node, describeError, displayName, labelColor } from "../api.ts";
import { useStudio } from "../App.tsx";
import { NodeDetail } from "../components/NodeDetail.tsx";

/** Nodes added at most per expansion, so a hub does not flood the view. */
const MAX_NEW = 150;

function cssVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

export function GraphView({ seed }: { seed?: string }) {
  const { client, namespace } = useStudio();
  const container = useRef<HTMLDivElement>(null);
  const cy = useRef<Core | null>(null);
  const [expanded, setExpanded] = useState<string[]>(seed ? [seed] : []);
  const [selected, setSelected] = useState<string | null>(seed ?? null);
  const [search, setSearch] = useState("");
  const [validAt, setValidAt] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [count, setCount] = useState({ nodes: 0, edges: 0 });

  // Create the canvas once.
  useEffect(() => {
    if (!container.current) return;
    const text = cssVar("--text");
    const muted = cssVar("--muted");
    cy.current = cytoscape({
      container: container.current,
      wheelSensitivity: 0.3,
      style: [
        {
          selector: "node",
          style: {
            "background-color": "data(color)",
            label: "data(name)",
            color: text,
            "font-size": 11,
            "text-valign": "bottom",
            "text-margin-y": 4,
            width: 22,
            height: 22,
          },
        },
        { selector: "node.expanded", style: { "border-width": 3, "border-color": text } },
        { selector: "node:selected", style: { "border-width": 4, "border-color": cssVar("--accent") } },
        {
          selector: "edge",
          style: {
            width: 1.5,
            "line-color": muted,
            "target-arrow-color": muted,
            "target-arrow-shape": "triangle",
            "curve-style": "bezier",
            label: "data(label)",
            "font-size": 9,
            color: muted,
            "text-rotation": "autorotate",
          },
        },
      ],
    });
    cy.current.on("tap", "node", (event) => setSelected(event.target.id()));
    cy.current.on("dbltap", "node", (event) => {
      const id = event.target.id();
      setExpanded((current) => (current.includes(id) ? current : [...current, id]));
    });
    return () => cy.current?.destroy();
  }, []);

  // Fetch the neighborhood of every expanded node, as of `validAt`.
  const redraw = useCallback(async () => {
    const graph = cy.current;
    if (!graph) return;
    try {
      const filter = validAt ? { valid_at: validAt } : undefined;
      const nodes = new Map<string, Node>();
      const edges = new Map<string, Edge>();
      for (const id of expanded) {
        const around = await client.namespace(namespace).subgraph([id], { depth: 1, filter });
        let added = 0;
        for (const node of around.nodes) {
          if (!nodes.has(node.id) && (added < MAX_NEW || node.id === id)) {
            nodes.set(node.id, node);
            added += 1;
          }
        }
        for (const edge of around.edges) edges.set(edge.id, edge);
      }
      const elements: ElementDefinition[] = [
        ...[...nodes.values()].map((node) => ({
          group: "nodes" as const,
          data: { id: node.id, name: displayName(node), color: labelColor(node.labels[0] ?? "") },
          classes: expanded.includes(node.id) ? "expanded" : "",
        })),
        ...[...edges.values()]
          .filter((edge) => nodes.has(edge.from) && nodes.has(edge.to))
          .map((edge) => ({
            group: "edges" as const,
            data: { id: edge.id, source: edge.from, target: edge.to, label: edge.label },
          })),
      ];
      graph.elements().remove();
      graph.add(elements);
      graph.layout({ name: "cose", animate: false, padding: 40, nodeRepulsion: () => 9000 }).run();
      // With few nodes, fitting zooms in until the labels are huge.
      if (graph.zoom() > 1.4) {
        graph.zoom(1.4);
        graph.center();
      }
      setCount({ nodes: nodes.size, edges: graph.edges().length });
      setError(null);
    } catch (problem) {
      setError(describeError(problem));
    }
  }, [client, namespace, expanded, validAt]);

  useEffect(() => {
    void redraw();
  }, [redraw]);

  const addFromSearch = async () => {
    const query = search.trim();
    if (!query) return;
    try {
      // An id goes straight in; anything else is a full-text search.
      if (/^[0-9a-f-]{36}$/i.test(query)) {
        setExpanded((current) => [...new Set([...current, query])]);
        return;
      }
      const hits = await client.namespace(namespace).searchText(query, 5);
      if (hits.length === 0) {
        setError(`No node's text matches "${query}".`);
        return;
      }
      setExpanded((current) => [...new Set([...current, ...hits.map((hit) => hit.node)])]);
    } catch (problem) {
      setError(describeError(problem));
    }
  };

  return (
    <div className="graph">
      <div className="toolbar stack" style={{ gap: 8 }}>
        <div className="row">
          <input
            className="grow"
            placeholder="Search text or paste a node id, then Enter"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && void addFromSearch()}
          />
          <button onClick={() => void addFromSearch()}>Add</button>
          <label className="row small" title="Show only facts that were true on this date">
            Valid at
            <input type="date" value={validAt} onChange={(e) => setValidAt(e.target.value)} />
          </label>
          {validAt && <button onClick={() => setValidAt("")}>Now</button>}
          <button onClick={() => cy.current?.fit(undefined, 40)}>Fit</button>
          <button
            onClick={() => {
              setExpanded([]);
              setSelected(null);
            }}
          >
            Clear
          </button>
        </div>
        <div className="muted small">
          {count.nodes} nodes · {count.edges} edges · double-click a node to expand it
          {validAt && ` · facts valid on ${validAt}`}
        </div>
        {error && <div className="error">{error}</div>}
      </div>
      <div className="canvas">
        <div className="cy" ref={container} />
        {expanded.length === 0 && (
          <div className="hint">Search for something, or open a node from the Explorer with “Show in graph”.</div>
        )}
        {selected && (
          <div className="inspector panel">
            <div className="row" style={{ justifyContent: "space-between" }}>
              <button
                onClick={() => setExpanded((current) => (current.includes(selected) ? current : [...current, selected]))}
              >
                Expand
              </button>
              <button onClick={() => setSelected(null)} aria-label="Close">✕</button>
            </div>
            <NodeDetail key={selected} id={selected} onChanged={() => void redraw()} />
          </div>
        )}
      </div>
    </div>
  );
}
