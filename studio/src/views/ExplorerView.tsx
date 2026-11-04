import { useCallback, useEffect, useState } from "react";

import { type Node, describeError, displayName, go, labelColor } from "../api.ts";
import { NodeDetail } from "../components/NodeDetail.tsx";
import { useStudio } from "../context.ts";
import { Icon } from "../icons.tsx";
import { Empty, useNodeSearch, useTask } from "../ui.tsx";

const PAGE = 50;

export function ExplorerView({ selected }: { selected?: string }) {
  const { client, namespace, schema, revision, changed, newNode, confirm, toast } = useStudio();
  const [label, setLabel] = useState<string | undefined>(undefined);
  const [nodes, setNodes] = useState<Node[]>([]);
  const [next, setNext] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [checked, setChecked] = useState<Set<string>>(new Set());
  const { results, searching } = useNodeSearch(filter, 50);
  const { busy, run } = useTask();

  const loadPage = useCallback(
    async (after?: string) => {
      try {
        const page = await client.namespace(namespace).listNodes({ label, after, limit: PAGE });
        setNodes((current) => (after ? [...current, ...page.items] : page.items));
        setNext(page.next);
        setError(null);
      } catch (problem) {
        setError(describeError(problem));
      }
    },
    [client, namespace, label],
  );

  useEffect(() => {
    void loadPage();
  }, [loadPage, revision]);

  const searching_ = filter.trim().length > 0;
  const shown = searching_ ? results.filter((node) => !label || node.labels.includes(label)) : nodes;
  const total = Object.values(schema.labels).reduce((sum, count) => sum + count, 0);

  const toggle = (id: string) =>
    setChecked((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const bulkDelete = async () => {
    const ok = await confirm({
      title: `Delete ${checked.size} nodes?`,
      message: "Their facts, texts and embeddings are deleted too. This cannot be undone.",
      confirm: "Delete nodes",
      danger: true,
    });
    if (!ok) return;
    await run(async () => {
      await client.namespace(namespace).batch([...checked].map((id) => ({ op: "delete_node" as const, id })));
      toast(`Deleted ${checked.size} nodes.`, "ok");
      if (selected && checked.has(selected)) go("explorer");
      setChecked(new Set());
      changed();
    });
  };

  const bulkArchive = () =>
    run(async () => {
      const archived = await client.namespace(namespace).archive([...checked]);
      toast(`Archived ${archived} nodes: recall leaves them out.`, "ok");
      setChecked(new Set());
      changed();
    });

  if (total === 0 && !searching_ && nodes.length === 0 && !error) {
    return (
      <div className="page">
        <Empty
          title={`“${namespace}” is empty`}
          action={
            <button className="primary big" onClick={() => newNode({ onCreated: (id) => go("explorer", id) })}>
              <Icon name="plus" /> Create the first node
            </button>
          }
        >
          Nodes are the things you remember: people, notes, projects, memories. Facts connect them.
        </Empty>
      </div>
    );
  }

  return (
    <div className="explorer">
      <section className="explorer-list">
        <div className="list-tools">
          <div className="input-icon">
            <Icon name="search" />
            <input value={filter} onChange={(event) => setFilter(event.target.value)} placeholder="Filter by name or text…" />
          </div>
          <button className="primary icon-button" title="New node" onClick={() => newNode({ labels: label ? [label] : [], onCreated: (id) => go("explorer", id) })}>
            <Icon name="plus" />
          </button>
        </div>
        <div className="chips scroll">
          <button className={label === undefined ? "chip active" : "chip"} onClick={() => setLabel(undefined)}>
            All <span className="faint">{total}</span>
          </button>
          {Object.entries(schema.labels)
            .sort((a, b) => b[1] - a[1])
            .map(([name, count]) => (
              <button key={name} className={label === name ? "chip active" : "chip"} onClick={() => setLabel(label === name ? undefined : name)}>
                <span className="dot" style={{ background: labelColor(name) }} />
                {name} <span className="faint">{count}</span>
              </button>
            ))}
        </div>
        {checked.size > 0 && (
          <div className="bulk-bar">
            <span>{checked.size} selected</span>
            <div className="spacer" />
            <button className="ghost small-button" onClick={() => void bulkArchive()} disabled={busy}>
              <Icon name="archive" size={14} /> Archive
            </button>
            <button className="ghost small-button danger" onClick={() => void bulkDelete()} disabled={busy}>
              <Icon name="trash" size={14} /> Delete
            </button>
            <button className="ghost icon-button" onClick={() => setChecked(new Set())} aria-label="Clear selection">
              <Icon name="x" size={14} />
            </button>
          </div>
        )}
        {error && <div className="error">{error}</div>}
        <div className="node-list">
          {shown.length === 0 && (
            <div className="muted small pad">{searching ? "Searching…" : searching_ ? "No node matches." : `No nodes${label ? ` with label ${label}` : ""}.`}</div>
          )}
          {shown.map((node) => (
            <div key={node.id} className={node.id === selected ? "node-row selected" : "node-row"} onClick={() => go("explorer", node.id)}>
              <input
                type="checkbox"
                checked={checked.has(node.id)}
                onClick={(event) => event.stopPropagation()}
                onChange={() => toggle(node.id)}
                aria-label={`Select ${displayName(node)}`}
              />
              <span className="hex" style={{ background: labelColor(node.labels[0] ?? "") }} />
              <span className="node-row-text">
                <span className="node-name">{displayName(node)}</span>
                <span className="faint small">
                  {node.labels.join(" · ")} · <span className="mono">{node.id.slice(-8)}</span>
                </span>
              </span>
            </div>
          ))}
          {next && !searching_ && (
            <button className="ghost load-more" onClick={() => void loadPage(next)}>
              Load more
            </button>
          )}
        </div>
      </section>
      <section className="explorer-detail">
        {selected ? (
          <NodeDetail key={selected} id={selected} />
        ) : (
          <Empty title="Choose a node">Its properties, text and facts appear here, and you can edit all of them in place.</Empty>
        )}
      </section>
    </div>
  );
}
