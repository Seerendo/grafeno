import cytoscape, { type Core, type ElementDefinition } from "cytoscape";
import { useCallback, useEffect, useRef, useState } from "react";

import { type Edge, type Node, describeError, displayName, formatTime, labelColor } from "../api.ts";
import { cssVar, graphStyle, layoutFor, useThemeVersion } from "../components/MiniGraph.tsx";
import { NodeDetail } from "../components/NodeDetail.tsx";
import { useStudio } from "../context.ts";
import { Icon } from "../icons.tsx";
import { useTask } from "../ui.tsx";

/** Nodes added at most per expansion, so a hub does not flood the view. */
const MAX_NEW = 150;

type Selection = { kind: "node"; id: string } | { kind: "edge"; edge: Edge } | null;

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
  const container = useRef<HTMLDivElement>(null);
  const cy = useRef<Core | null>(null);
  const [expanded, setExpanded] = useState<string[]>(seed ? [seed] : []);
  const [selection, setSelection] = useState<Selection>(seed ? { kind: "node", id: seed } : null);
  const [connectFrom, setConnectFrom] = useState<{ id: string; name: string } | null>(null);
  const [search, setSearch] = useState("");
  const [validAt, setValidAt] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [count, setCount] = useState({ nodes: 0, edges: 0 });
  const edges = useRef(new Map<string, Edge>());
  const names = useRef(new Map<string, string>());
  // Event handlers are bound once; they read the latest state from here.
  const state = useRef({ connectFrom, newEdge });
  state.current = { connectFrom, newEdge };
  const { busy, run } = useTask();
  const theme = useThemeVersion();
  const [colorBy, setColorBy] = useState<ColorBy>("label");
  const [sizeBy, setSizeBy] = useState<SizeBy>("none");
  const [groups, setGroups] = useState<Analysis | null>(null);
  const [scores, setScores] = useState<Analysis | null>(null);
  const labels = useRef(new Map<string, string>());
  // Counts redraws, so painting follows each one.
  const [drawn, setDrawn] = useState(0);

  useEffect(() => {
    cy.current?.style(graphStyle());
  }, [theme]);

  const expand = (id: string) => setExpanded((current) => (current.includes(id) ? current : [...current, id]));

  // Create the canvas once.
  useEffect(() => {
    if (!container.current) return;
    cy.current = cytoscape({ container: container.current, wheelSensitivity: 0.3, style: graphStyle() });
    cy.current.on("tap", "node", (event) => {
      const id = event.target.id();
      const from = state.current.connectFrom;
      if (from) {
        setConnectFrom(null);
        if (from.id !== id) {
          state.current.newEdge({ from, to: { id, name: names.current.get(id) ?? id } });
        }
        return;
      }
      setSelection({ kind: "node", id });
    });
    cy.current.on("tap", "edge", (event) => {
      const edge = edges.current.get(event.target.id());
      if (edge) setSelection({ kind: "edge", edge });
    });
    cy.current.on("tap", (event) => {
      if (event.target === cy.current) {
        setSelection(null);
        setConnectFrom(null);
      }
    });
    cy.current.on("dbltap", "node", (event) => expand(event.target.id()));
    return () => cy.current?.destroy();
  }, []);

  useEffect(() => {
    const graph = cy.current;
    if (!graph) return;
    graph.nodes().removeClass("source");
    if (connectFrom) graph.getElementById(connectFrom.id).addClass("source");
    if (container.current) container.current.style.cursor = connectFrom ? "crosshair" : "";
  }, [connectFrom]);

  // Fetch the neighborhood of every expanded node, as of `validAt`.
  const redraw = useCallback(async () => {
    const graph = cy.current;
    if (!graph) return;
    try {
      const filter = validAt ? { valid_at: validAt } : undefined;
      const nodes = new Map<string, Node>();
      const found = new Map<string, Edge>();
      for (const id of expanded) {
        const around = await client.namespace(namespace).subgraph([id], { depth: 1, filter });
        let added = 0;
        for (const node of around.nodes) {
          if (!nodes.has(node.id) && (added < MAX_NEW || node.id === id)) {
            nodes.set(node.id, node);
            added += 1;
          }
        }
        for (const edge of around.edges) found.set(edge.id, edge);
      }
      edges.current = found;
      names.current = new Map([...nodes.values()].map((node) => [node.id, displayName(node)]));
      labels.current = new Map([...nodes.values()].map((node) => [node.id, node.labels[0] ?? ""]));
      const now = Date.now() * 1000;
      const positions = new Map(graph.nodes().map((node) => [node.id(), { ...node.position() }]));
      const elements: ElementDefinition[] = [
        ...[...nodes.values()].map((node) => ({
          group: "nodes" as const,
          data: { id: node.id, name: displayName(node), color: labelColor(node.labels[0] ?? "") },
          classes: expanded.includes(node.id) ? "expanded" : "",
          position: positions.get(node.id),
        })),
        ...[...found.values()]
          .filter((edge) => nodes.has(edge.from) && nodes.has(edge.to))
          .map((edge) => ({
            group: "edges" as const,
            data: { id: edge.id, source: edge.from, target: edge.to, label: edge.label },
            classes: edge.valid_to !== null && edge.valid_to <= now ? "ended" : "",
          })),
      ];
      graph.elements().remove();
      graph.add(elements);
      // Keep known nodes where they were; lay out only when something is new.
      const fresh = [...nodes.keys()].some((id) => !positions.has(id));
      if (fresh) {
        graph.layout(layoutFor(nodes.size)).run();
        if (graph.zoom() > 1.4) {
          graph.zoom(1.4);
          graph.center();
        }
      }
      setCount({ nodes: nodes.size, edges: graph.edges().length });
      setDrawn((value) => value + 1);
      setError(null);
    } catch (problem) {
      setError(describeError(problem));
    }
  }, [client, namespace, expanded, validAt]);

  useEffect(() => {
    void redraw();
  }, [redraw, revision]);

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
      })
      .catch((problem) => current && setError(describeError(problem)));
    return () => {
      current = false;
    };
  }, [client, namespace, colorBy, sizeBy, revision]);

  // Paint groups and scores onto the drawn nodes.
  useEffect(() => {
    const graph = cy.current;
    if (!graph) return;
    graph.batch(() => {
      graph.nodes().forEach((node) => {
        const group = groups?.values.get(node.id());
        node.data(
          "color",
          group === undefined ? labelColor(labels.current.get(node.id()) ?? "") : cssVar(`--series-${(group % 8) + 1}`),
        );
        const score = scores?.values.get(node.id());
        if (scores && score !== undefined && scores.max > 0) {
          const side = 16 + 34 * Math.sqrt(score / scores.max);
          node.style({ width: side, height: side });
        } else {
          node.removeStyle("width height");
        }
      });
    });
  }, [groups, scores, drawn, theme]);

  const addFromSearch = async () => {
    const query = search.trim();
    if (!query) return;
    try {
      if (/^[0-9a-f-]{36}$/i.test(query)) {
        expand(query);
        return;
      }
      const graph = client.namespace(namespace);
      const byName = await graph.query(
        "MATCH (n) WHERE toLower(coalesce(n.name, n.title, n.label, '')) CONTAINS toLower($q) RETURN n.@id LIMIT 5",
        { q: query },
      );
      const hits = await graph.searchText(query, 5);
      const ids = [...new Set([...(byName.rows as string[][]).map((row) => row[0]), ...hits.map((hit) => hit.node)])];
      if (ids.length === 0) {
        setError(`Nothing is named or says “${query}”.`);
        return;
      }
      setExpanded((current) => [...new Set([...current, ...ids])]);
      setSearch("");
    } catch (problem) {
      setError(describeError(problem));
    }
  };

  const showAll = () =>
    run(async () => {
      const page = await client.namespace(namespace).listNodes({ limit: 60 });
      setExpanded(page.items.map((node) => node.id));
    });

  const edgeAction = async (edge: Edge, action: "invalidate" | "retract" | "delete") => {
    const graph = client.namespace(namespace);
    const title = `${names.current.get(edge.from) ?? "?"} —${edge.label}→ ${names.current.get(edge.to) ?? "?"}`;
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
      setSelection(null);
      changed();
    });
  };

  const selectedNode = selection?.kind === "node" ? selection.id : null;

  return (
    <div className="graph">
      <div className="graph-toolbar">
        <div className="input-icon grow">
          <Icon name="search" />
          <input
            placeholder="Add nodes: search a name or text, or paste an id"
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
        <button
          className="primary"
          onClick={() =>
            newNode({
              onCreated: (id) => {
                expand(id);
                setSelection({ kind: "node", id });
              },
            })
          }
        >
          <Icon name="node" /> Node
        </button>
        <button
          className={connectFrom ? "active" : ""}
          disabled={!selectedNode && !connectFrom}
          onClick={() =>
            connectFrom ? setConnectFrom(null) : selectedNode && setConnectFrom({ id: selectedNode, name: names.current.get(selectedNode) ?? selectedNode })
          }
          title="Select a node, press Connect, then click the target node"
        >
          <Icon name="link" /> {connectFrom ? "Cancel" : "Connect"}
        </button>
        <span className="toolbar-sep" />
        <label className="row small nowrap" title="Color nodes by a graph algorithm run on the whole namespace">
          <Icon name="graph" /> Color
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
        {validAt && <button className="ghost small-button" onClick={() => setValidAt("")}>Now</button>}
        <button className="ghost icon-button" onClick={() => cy.current?.fit(undefined, 40)} title="Fit to screen">
          <Icon name="fit" />
        </button>
        <button
          className="ghost icon-button"
          onClick={() => cy.current?.layout(layoutFor(cy.current.nodes().length, true)).run()}
          title="Lay out again"
        >
          <Icon name="graph" />
        </button>
        <button
          className="ghost icon-button"
          onClick={() => {
            setExpanded([]);
            setSelection(null);
            setConnectFrom(null);
          }}
          title="Clear the canvas"
        >
          <Icon name="eraser" />
        </button>
      </div>
      <div className="canvas">
        <div className="cy" ref={container} />
        <div className="canvas-status small">
          {count.nodes} nodes · {count.edges} facts
          {validAt && ` · valid on ${validAt}`}
          {groups && ` · ${groups.groups} ${colorBy === "wcc" ? "components" : "communities"}`}
          {scores && ` · sized by ${SIZE_BY[sizeBy].title}`}
          {" · "}
          {connectFrom ? <strong className="accent">Click the node that “{connectFrom.name}” points to</strong> : "click to inspect · double-click to expand"}
        </div>
        {error && (
          <div className="canvas-error error" onClick={() => setError(null)}>
            {error}
          </div>
        )}
        {expanded.length === 0 && (
          <div className="hint">
            <div className="empty-art" aria-hidden="true" />
            <p>Search above, press “Show all”, or create a node.</p>
          </div>
        )}
        {selection && (
          <aside className="inspector">
            <div className="inspector-head">
              {selection.kind === "node" ? (
                <>
                  <button className="small-button" onClick={() => expand(selection.id)}>
                    <Icon name="expand" size={14} /> Expand
                  </button>
                  <button
                    className="small-button"
                    onClick={() => setConnectFrom({ id: selection.id, name: names.current.get(selection.id) ?? selection.id })}
                  >
                    <Icon name="link" size={14} /> Connect to…
                  </button>
                </>
              ) : (
                <strong>Fact</strong>
              )}
              <div className="spacer" />
              <button className="ghost icon-button" onClick={() => setSelection(null)} aria-label="Close">
                <Icon name="x" />
              </button>
            </div>
            {selection.kind === "node" ? (
              <NodeDetail key={selection.id} id={selection.id} compact />
            ) : (
              <div className="detail compact">
                <div className="edge-title">
                  <span>{names.current.get(selection.edge.from)}</span>
                  <span className="mono accent">—{selection.edge.label}→</span>
                  <span>{names.current.get(selection.edge.to)}</span>
                </div>
                <dl className="facts small">
                  <dt>@id</dt>
                  <dd className="mono">{selection.edge.id}</dd>
                  <dt>valid</dt>
                  <dd>
                    {selection.edge.valid_from !== null ? formatTime(selection.edge.valid_from).slice(0, 10) : "always"} →{" "}
                    {selection.edge.valid_to !== null ? formatTime(selection.edge.valid_to).slice(0, 10) : "now"}
                  </dd>
                  <dt>recorded</dt>
                  <dd>{formatTime(selection.edge.recorded_at)}</dd>
                </dl>
                {Object.keys(selection.edge.props).length > 0 && <pre>{JSON.stringify(selection.edge.props, null, 2)}</pre>}
                <div className="stack tight">
                  <button onClick={() => void edgeAction(selection.edge, "invalidate")} disabled={busy || (selection.edge.valid_to ?? Infinity) <= Date.now() * 1000}>
                    <Icon name="clock" /> End it today (keeps history)
                  </button>
                  <button onClick={() => void edgeAction(selection.edge, "retract")} disabled={busy}>
                    <Icon name="ban" /> Retract (it was a mistake)
                  </button>
                  <button className="danger ghost" onClick={() => void edgeAction(selection.edge, "delete")} disabled={busy}>
                    <Icon name="trash" /> Delete
                  </button>
                </div>
              </div>
            )}
          </aside>
        )}
      </div>
    </div>
  );
}
