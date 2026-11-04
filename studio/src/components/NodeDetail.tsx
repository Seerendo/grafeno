import { useCallback, useEffect, useState } from "react";

import { type Edge, type Node, type NodeWithText, describeError, displayName, formatTime, go } from "../api.ts";
import { useStudio } from "../context.ts";
import { Icon } from "../icons.tsx";
import { LabelChip, LabelInput, PropsEditor, type PropRow, propsFromRows, rowsFromProps, useTask } from "../ui.tsx";

interface Fact {
  edge: Edge;
  outgoing: boolean;
  other: Node | undefined;
}

type Editing = null | "props" | "text" | "name";

/** A node with its facts; everything on it can be edited in place. */
export function NodeDetail({ id, onChanged, compact }: { id: string; onChanged?: () => void; compact?: boolean }) {
  const { client, namespace, schema, changed, toast, confirm, newEdge, revision } = useStudio();
  const graph = client.namespace(namespace);
  const [node, setNode] = useState<NodeWithText | null>(null);
  const [facts, setFacts] = useState<Fact[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState<Editing>(null);
  const [rows, setRows] = useState<PropRow[]>([]);
  const [text, setText] = useState("");
  const [name, setName] = useState("");
  const [importance, setImportance] = useState(0.5);
  const [adding, setAdding] = useState<string[]>([]);
  const { busy, run } = useTask();

  const load = useCallback(async () => {
    try {
      const found = await graph.getNode(id);
      const around = await graph.subgraph([id], { depth: 1 });
      const nodes = new Map(around.nodes.map((n) => [n.id, n]));
      setNode(found);
      setImportance(found.memory.importance);
      setFacts(
        around.edges
          .filter((edge) => edge.from === id || edge.to === id)
          .map((edge) => ({ edge, outgoing: edge.from === id, other: nodes.get(edge.from === id ? edge.to : edge.from) })),
      );
      setError(null);
    } catch (problem) {
      setNode(null);
      setError(describeError(problem));
    }
    // `graph` changes identity on every render; `client` and `namespace` do not.
  }, [client, namespace, id]);

  useEffect(() => {
    void load();
  }, [load, revision]);

  if (error) return <div className="error">{error}</div>;
  if (!node) return <div className="muted pad">Loading…</div>;

  const write = (work: () => Promise<unknown>, message?: string) =>
    run(async () => {
      await work();
      setEditing(null);
      changed();
      onChanged?.();
      if (message) toast(message, "ok");
    });

  const patchProps = (props: Record<string, unknown>) => {
    // Keys that are gone are removed with `null`.
    for (const key of Object.keys(node.props)) if (!(key in props)) props[key] = null;
    return graph.patchNode(id, { props });
  };

  const remove = async () => {
    const ok = await confirm({
      title: `Delete “${displayName(node)}”?`,
      message: `Its ${facts.length} facts, text and embedding are deleted too. This cannot be undone.`,
      confirm: "Delete node",
      danger: true,
    });
    if (!ok) return;
    await write(() => graph.deleteNode(id), `Deleted ${displayName(node)}.`);
    go(window.location.hash.startsWith("#/graph") ? "graph" : "explorer");
  };

  const factAction = async (fact: Fact, action: "invalidate" | "retract" | "delete") => {
    const label = `${fact.outgoing ? displayName(node) : displayName(fact.other ?? node)} —${fact.edge.label}→ ${
      fact.outgoing ? displayName(fact.other ?? node) : displayName(node)
    }`;
    if (action === "delete") {
      const ok = await confirm({ title: "Delete this fact?", message: `${label}. Its history is lost; to keep it, end it instead.`, confirm: "Delete", danger: true });
      if (!ok) return;
      await write(() => graph.deleteEdge(fact.edge.id), "Fact deleted.");
    } else if (action === "retract") {
      const ok = await confirm({ title: "Retract this fact?", message: `${label} was a mistake. It stays visible to KNOWN AT queries.`, confirm: "Retract" });
      if (!ok) return;
      await write(() => graph.retractEdge(fact.edge.id), "Fact retracted.");
    } else {
      await write(() => graph.invalidateEdge(fact.edge.id, new Date().toISOString()), "Fact ended today.");
    }
  };

  const now = Date.now() * 1000;
  const labels = Object.keys(schema.labels).filter((label) => !node.labels.includes(label));

  return (
    <div className={compact ? "detail compact" : "detail"}>
      <div className="detail-head">
        {editing === "name" ? (
          <form
            className="row"
            onSubmit={(event) => {
              event.preventDefault();
              void write(() => patchProps({ ...node.props, name: name.trim() || null }));
            }}
          >
            <input autoFocus value={name} onChange={(event) => setName(event.target.value)} className="title-input" />
            <button className="primary" type="submit" disabled={busy}>Save</button>
            <button type="button" onClick={() => setEditing(null)}>Cancel</button>
          </form>
        ) : (
          <h2
            className="editable"
            title="Click to rename"
            onClick={() => {
              setName(typeof node.props.name === "string" ? node.props.name : "");
              setEditing("name");
            }}
          >
            {displayName(node)} <Icon name="edit" size={14} className="faint" />
          </h2>
        )}
        <div className="chips">
          {node.labels.map((label) => (
            <LabelChip
              key={label}
              label={label}
              onRemove={node.labels.length > 1 ? () => void write(() => graph.patchNode(id, { remove_labels: [label] })) : undefined}
            />
          ))}
          <div className="add-label">
            <LabelInput
              value={adding}
              suggestions={labels}
              placeholder="+ label"
              onChange={(next) => {
                setAdding([]);
                const fresh = next.filter((label) => !node.labels.includes(label));
                if (fresh.length) void write(() => graph.patchNode(id, { add_labels: fresh }));
              }}
            />
          </div>
        </div>
      </div>

      <div className="detail-actions">
        <button onClick={() => newEdge({ from: { id, name: displayName(node) }, onCreated: () => void load() })}>
          <Icon name="link" /> Add fact
        </button>
        {!window.location.hash.startsWith("#/graph") && (
          <button onClick={() => go("graph", id)}>
            <Icon name="graph" /> Graph
          </button>
        )}
        <div className="spacer" />
        <button className="danger ghost" onClick={() => void remove()} disabled={busy}>
          <Icon name="trash" /> Delete
        </button>
      </div>

      <dl className="facts small">
        <dt>@id</dt>
        <dd className="mono">
          {node.id}{" "}
          <button
            className="ghost icon-button inline"
            aria-label="Copy id"
            onClick={() => void navigator.clipboard?.writeText(node.id).then(() => toast("Id copied.", "info"))}
          >
            <Icon name="copy" size={13} />
          </button>
        </dd>
        <dt>created</dt>
        <dd>{formatTime(node.created_at)}</dd>
        <dt>updated</dt>
        <dd>{formatTime(node.updated_at)}</dd>
        {node.recorded_by && (
          <>
            <dt>recorded by</dt>
            <dd className="mono">{node.recorded_by}</dd>
          </>
        )}
        <dt>importance</dt>
        <dd className="row nowrap">
          <input
            type="range"
            min={0}
            max={1}
            step={0.05}
            value={importance}
            onChange={(event) => setImportance(Number(event.target.value))}
            onPointerUp={() => importance !== node.memory.importance && void write(() => graph.patchNode(id, { importance }))}
            onKeyUp={() => importance !== node.memory.importance && void write(() => graph.patchNode(id, { importance }))}
            aria-label="Importance"
          />
          <span className="mono">{importance.toFixed(2)}</span>
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

      <section>
        <div className="section-head">
          <h3>Properties</h3>
          {editing !== "props" && (
            <button
              className="ghost small-button"
              onClick={() => {
                setRows(rowsFromProps(node.props));
                setEditing("props");
              }}
            >
              <Icon name="edit" size={14} /> Edit
            </button>
          )}
        </div>
        {editing === "props" ? (
          <div className="stack tight">
            <PropsEditor rows={rows} onChange={setRows} />
            <div className="row">
              <button className="primary" disabled={busy} onClick={() => void write(() => patchProps(propsFromRows(rows)), "Properties saved.")}>
                Save
              </button>
              <button onClick={() => setEditing(null)}>Cancel</button>
            </div>
          </div>
        ) : Object.keys(node.props).length === 0 ? (
          <div className="muted small">No properties.</div>
        ) : (
          <table className="kv">
            <tbody>
              {Object.entries(node.props).map(([key, value]) => (
                <tr key={key}>
                  <th className="mono">{key}</th>
                  <td>{typeof value === "string" ? value : <span className="mono">{JSON.stringify(value)}</span>}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>

      <section>
        <div className="section-head">
          <h3>Text</h3>
          {editing !== "text" && (
            <button
              className="ghost small-button"
              onClick={() => {
                setText(node.text ?? "");
                setEditing("text");
              }}
            >
              <Icon name="edit" size={14} /> {node.text ? "Edit" : "Add"}
            </button>
          )}
        </div>
        {editing === "text" ? (
          <div className="stack tight">
            <textarea rows={5} autoFocus value={text} onChange={(event) => setText(event.target.value)} />
            <div className="row">
              <button className="primary" disabled={busy} onClick={() => void write(() => graph.patchNode(id, { text: text.trim() ? text : null }), "Text saved.")}>
                Save
              </button>
              <button onClick={() => setEditing(null)}>Cancel</button>
            </div>
          </div>
        ) : node.text ? (
          <pre className="text-block">{node.text}</pre>
        ) : (
          <div className="muted small">No text: full-text search and recall do not see this node's words.</div>
        )}
      </section>

      <section>
        <div className="section-head">
          <h3>Facts <span className="muted">{facts.length}</span></h3>
        </div>
        {facts.length === 0 && <div className="muted small">No facts yet. Use “Add fact” to connect it.</div>}
        <div className="fact-list">
          {facts.map((fact) => {
            const ended = fact.edge.valid_to !== null && fact.edge.valid_to <= now;
            return (
              <div key={fact.edge.id} className={ended ? "fact ended" : "fact"}>
                <span className="fact-arrow mono">{fact.outgoing ? `→ ${fact.edge.label}` : `← ${fact.edge.label}`}</span>
                {fact.other ? <a href={`#/explorer/${fact.other.id}`}>{displayName(fact.other)}</a> : <span className="muted">?</span>}
                {(fact.edge.valid_from !== null || fact.edge.valid_to !== null) && (
                  <span className="muted small">
                    {fact.edge.valid_from !== null ? formatTime(fact.edge.valid_from).slice(0, 10) : "…"} →{" "}
                    {fact.edge.valid_to !== null ? formatTime(fact.edge.valid_to).slice(0, 10) : "now"}
                  </span>
                )}
                <span className="spacer" />
                <span className="fact-tools">
                  {!ended && (
                    <button className="ghost icon-button" title="It stopped being true today (keeps its history)" onClick={() => void factAction(fact, "invalidate")}>
                      <Icon name="clock" size={14} />
                    </button>
                  )}
                  <button className="ghost icon-button" title="Retract: it was a mistake" onClick={() => void factAction(fact, "retract")}>
                    <Icon name="ban" size={14} />
                  </button>
                  <button className="ghost icon-button danger" title="Delete" onClick={() => void factAction(fact, "delete")}>
                    <Icon name="trash" size={14} />
                  </button>
                </span>
              </div>
            );
          })}
        </div>
      </section>
    </div>
  );
}
