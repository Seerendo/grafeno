import { useEffect, useState } from "react";

import { describeError, formatTime, go, labelColor } from "../api.ts";
import { Icon } from "../icons.tsx";
import { useStudio } from "../context.ts";
import type { NamespaceStats } from "../../../clients/typescript/src/index.ts";

function Bars({ counts }: { counts: Record<string, number> }) {
  const entries = Object.entries(counts).sort((a, b) => b[1] - a[1]);
  const top = entries[0]?.[1] ?? 1;
  if (entries.length === 0) return <div className="muted small">None.</div>;
  return (
    <table>
      <tbody>
        {entries.map(([label, count]) => (
          <tr key={label}>
            <td style={{ width: "30%" }}>
              <span className="dot" style={{ background: labelColor(label) }} /> {label}
            </td>
            <td style={{ width: "15%" }} className="mono">{count}</td>
            <td>
              <div className="bar" style={{ width: `${(count / top) * 100}%` }} />
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

export function OverviewView() {
  const { client, namespace, namespaces, setNamespace, revision, newNode, newEdge } = useStudio();
  const [stats, setStats] = useState<NamespaceStats | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    client.namespace(namespace).stats().then(setStats).catch((e) => setError(describeError(e)));
  }, [client, namespace, revision]);

  return (
    <div className="stack">
      <div className="page-head">
        <div>
          <h1>Overview</h1>
          <p className="muted">What “{namespace}” holds, and where to start.</p>
        </div>
        <div className="row">
          <button onClick={() => newNode({ onCreated: (id) => go("explorer", id) })}>
            <Icon name="node" /> New node
          </button>
          <button onClick={() => newEdge()}>
            <Icon name="link" /> New fact
          </button>
          <button className="primary" onClick={() => go("scripts")}>
            <Icon name="scripts" /> Open scripts
          </button>
        </div>
      </div>
      {error && <div className="error">{error}</div>}
      {stats && (
        <div className="cards">
          {[
            ["Nodes", stats.nodes],
            ["Edges", stats.edges],
            ["Texts", stats.texts],
            ["Vectors", stats.vectors],
            ["Dimension", stats.vector_dimension ?? "—"],
          ].map(([label, value]) => (
            <div key={label} className="panel card">
              <div className="label small">{label}</div>
              <div className="value">{value}</div>
            </div>
          ))}
        </div>
      )}
      <div className="split">
        <div className="panel">
          <h2>Namespaces</h2>
          {namespaces.length === 0 && (
            <p className="muted small">
              The database is empty. Create a node with <strong>New node</strong>, or run a script in{" "}
              <a href="#/scripts">Scripts</a>, such as <code>CREATE (n:Note {"{"}title: 'Hello'{"}"});</code>
            </p>
          )}
          <div className="table-wrap">
            <table>
              <thead>
                <tr>
                  <th>Name</th>
                  <th>Nodes</th>
                  <th>Edges</th>
                  <th>Created</th>
                </tr>
              </thead>
              <tbody>
                {namespaces.map((info) => (
                  <tr
                    key={info.name}
                    className={info.name === namespace ? "clickable selected" : "clickable"}
                    onClick={() => setNamespace(info.name)}
                  >
                    <td>
                      {info.name}
                      {info.description && <div className="muted small">{info.description}</div>}
                    </td>
                    <td className="mono">{info.nodes === 0 ? <span className="muted">empty</span> : info.nodes}</td>
                    <td className="mono">{info.edges}</td>
                    <td className="muted small">{formatTime(info.created_at).slice(0, 10)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
        {stats && (
          <div className="stack">
            <div className="panel">
              <h2>Node labels in “{namespace}”</h2>
              <Bars counts={stats.node_labels} />
            </div>
            <div className="panel">
              <h2>Edge labels</h2>
              <Bars counts={stats.edge_labels} />
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
