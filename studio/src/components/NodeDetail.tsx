import { useCallback, useEffect, useState } from "react";

import { type Edge, type Node, type NodeWithText, describeError, displayName, formatTime, go, labelColor } from "../api.ts";
import { useStudio } from "../App.tsx";

interface Fact {
  edge: Edge;
  outgoing: boolean;
  other: Node | undefined;
}

/** A node with its facts, and forms to change or delete it. */
export function NodeDetail({ id, onChanged }: { id: string; onChanged?: () => void }) {
  const { client, namespace } = useStudio();
  const graph = client.namespace(namespace);
  const [node, setNode] = useState<NodeWithText | null>(null);
  const [facts, setFacts] = useState<Fact[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState(false);
  const [propsText, setPropsText] = useState("");
  const [text, setText] = useState("");
  const [importance, setImportance] = useState("0.5");
  const [saving, setSaving] = useState(false);

  const load = useCallback(async () => {
    try {
      const found = await graph.getNode(id);
      const around = await graph.subgraph([id], { depth: 1 });
      const nodes = new Map(around.nodes.map((n) => [n.id, n]));
      setNode(found);
      setFacts(
        around.edges
          .filter((edge) => edge.from === id || edge.to === id)
          .map((edge) => ({
            edge,
            outgoing: edge.from === id,
            other: nodes.get(edge.from === id ? edge.to : edge.from),
          })),
      );
      setPropsText(JSON.stringify(found.props, null, 2));
      setText(found.text ?? "");
      setImportance(String(Math.round(found.memory.importance * 100) / 100));
      setError(null);
    } catch (problem) {
      setNode(null);
      setError(describeError(problem));
    }
    // `graph` changes identity on every render; `client` and `namespace` do not.
  }, [client, namespace, id]);

  useEffect(() => {
    setEditing(false);
    void load();
  }, [load]);

  if (error) return <div className="error">{error}</div>;
  if (!node) return <div className="muted">Loading…</div>;

  const save = async () => {
    setSaving(true);
    try {
      const props = JSON.parse(propsText || "{}") as Record<string, unknown>;
      if (typeof props !== "object" || props === null || Array.isArray(props)) {
        throw new Error("Properties must be a JSON object.");
      }
      // Keys that are gone are removed with `null`.
      for (const key of Object.keys(node.props)) {
        if (!(key in props)) props[key] = null;
      }
      const level = Number(importance);
      if (!Number.isFinite(level) || level < 0 || level > 1) throw new Error("Importance must be between 0 and 1.");
      await graph.patchNode(id, { props, text: text.trim() ? text : null, importance: level });
      setEditing(false);
      await load();
      onChanged?.();
    } catch (problem) {
      setError(describeError(problem));
    } finally {
      setSaving(false);
    }
  };

  const remove = async () => {
    if (!window.confirm(`Delete "${displayName(node)}" and its ${facts.length} facts? This cannot be undone.`)) return;
    try {
      await graph.deleteNode(id);
      onChanged?.();
      go("explorer");
    } catch (problem) {
      setError(describeError(problem));
    }
  };

  return (
    <div className="stack">
      <div className="row">
        <h2 style={{ margin: 0 }}>{displayName(node)}</h2>
        <div className="chips">
          {node.labels.map((label) => (
            <span key={label} className="chip">
              <span className="dot" style={{ background: labelColor(label) }} />
              {label}
            </span>
          ))}
        </div>
      </div>
      <dl className="facts small">
        <dt>@id</dt>
        <dd className="mono">{node.id}</dd>
        <dt>created</dt>
        <dd>{formatTime(node.created_at)}</dd>
        <dt>updated</dt>
        <dd>{formatTime(node.updated_at)}</dd>
        <dt>importance</dt>
        <dd>
          <span className="meter" title={String(node.memory.importance)}>
            <span style={{ width: `${node.memory.importance * 100}%` }} />
          </span>{" "}
          {node.memory.importance.toFixed(2)}
        </dd>
        <dt>accesses</dt>
        <dd>
          {node.memory.accesses}
          {node.memory.accesses > 0 && <span className="muted"> · last {formatTime(node.memory.last_access)}</span>}
        </dd>
        {node.sources.length > 0 && (
          <>
            <dt>sources</dt>
            <dd>
              {node.sources.map((source) => (
                <div key={source}>
                  <a href={`#/explorer/${source}`} className="mono">{source}</a>
                </div>
              ))}
            </dd>
          </>
        )}
      </dl>

      {editing ? (
        <div className="stack">
          <label className="field">
            Properties (JSON)
            <textarea className="code" rows={8} value={propsText} onChange={(e) => setPropsText(e.target.value)} />
          </label>
          <label className="field">
            Text (@text, searchable)
            <textarea rows={4} value={text} onChange={(e) => setText(e.target.value)} />
          </label>
          <label className="field">
            Importance (0 to 1: ranks higher and fades slower)
            <input type="number" min={0} max={1} step={0.05} value={importance} onChange={(e) => setImportance(e.target.value)} style={{ width: 120 }} />
          </label>
          <div className="row">
            <button className="primary" disabled={saving} onClick={() => void save()}>Save</button>
            <button onClick={() => setEditing(false)}>Cancel</button>
          </div>
        </div>
      ) : (
        <>
          <div>
            <h3>Properties</h3>
            <pre>{JSON.stringify(node.props, null, 2)}</pre>
          </div>
          {node.text && (
            <div>
              <h3>Text</h3>
              <pre>{node.text}</pre>
            </div>
          )}
        </>
      )}

      <div>
        <h3>Facts ({facts.length})</h3>
        {facts.length === 0 && <div className="muted small">No current facts.</div>}
        <div className="list">
          {facts.map(({ edge, outgoing, other }) => (
            <div key={edge.id} className="row small" style={{ padding: "4px 0", borderBottom: "1px solid var(--border)" }}>
              <span className="mono">{outgoing ? `-[:${edge.label}]->` : `<-[:${edge.label}]-`}</span>
              {other ? <a href={`#/explorer/${other.id}`}>{displayName(other)}</a> : <span className="muted">?</span>}
              {(edge.valid_from !== null || edge.valid_to !== null) && (
                <span className="muted">
                  {edge.valid_from !== null ? formatTime(edge.valid_from).slice(0, 10) : "…"} →{" "}
                  {edge.valid_to !== null ? formatTime(edge.valid_to).slice(0, 10) : "now"}
                </span>
              )}
            </div>
          ))}
        </div>
      </div>

      <div className="row">
        <button onClick={() => go("graph", id)}>Show in graph</button>
        {!editing && <button onClick={() => setEditing(true)}>Edit</button>}
        <button className="danger" onClick={() => void remove()}>Delete</button>
      </div>
    </div>
  );
}
