import { useCallback, useEffect, useState } from "react";

import { type Node, describeError, displayName, go, labelColor } from "../api.ts";
import { useStudio } from "../App.tsx";
import { NodeDetail } from "../components/NodeDetail.tsx";

const PAGE = 50;

export function ExplorerView({ selected }: { selected?: string }) {
  const { client, namespace, refreshNamespaces } = useStudio();
  const [labels, setLabels] = useState<Record<string, number>>({});
  const [label, setLabel] = useState<string | undefined>(undefined);
  const [nodes, setNodes] = useState<Node[]>([]);
  const [next, setNext] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reload, setReload] = useState(0);

  useEffect(() => {
    client
      .namespace(namespace)
      .stats()
      .then((stats) => setLabels(stats.node_labels))
      .catch((e) => setError(describeError(e)));
  }, [client, namespace, reload]);

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
  }, [loadPage, reload]);

  const changed = () => {
    setReload((n) => n + 1);
    refreshNamespaces();
  };

  return (
    <div className="stack">
      <h1>Explorer</h1>
      {error && <div className="error">{error}</div>}
      <div className="chips">
        <button className={label === undefined ? "chip active" : "chip"} onClick={() => setLabel(undefined)}>
          All
        </button>
        {Object.entries(labels)
          .sort((a, b) => b[1] - a[1])
          .map(([name, count]) => (
            <button key={name} className={label === name ? "chip active" : "chip"} onClick={() => setLabel(name)}>
              <span className="dot" style={{ background: labelColor(name) }} />
              {name} <span className="muted">{count}</span>
            </button>
          ))}
      </div>
      <div className="split">
        <div className="panel">
          {nodes.length === 0 && <div className="muted small">No nodes{label ? ` with label ${label}` : ""}.</div>}
          <div className="list">
            {nodes.map((node) => (
              <button
                key={node.id}
                className={node.id === selected ? "item selected" : "item"}
                onClick={() => go("explorer", node.id)}
              >
                <span>
                  <span className="dot" style={{ background: labelColor(node.labels[0] ?? "") }} /> {displayName(node)}
                </span>
                <span className="muted small">
                  :{node.labels.join(":")} · {node.id.slice(-12)}
                </span>
              </button>
            ))}
          </div>
          {next && (
            <button style={{ marginTop: 8 }} onClick={() => void loadPage(next)}>
              Load more
            </button>
          )}
        </div>
        <div className="panel">
          {selected ? (
            <NodeDetail key={selected} id={selected} onChanged={changed} />
          ) : (
            <div className="muted">Choose a node to see its properties, text and facts.</div>
          )}
        </div>
      </div>
    </div>
  );
}
