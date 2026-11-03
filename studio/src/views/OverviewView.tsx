import { useEffect, useState } from "react";

import { describeError, labelColor } from "../api.ts";
import { useStudio } from "../App.tsx";
import type { NamespaceInfo, NamespaceStats } from "../../../clients/typescript/src/index.ts";

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
  const { client, namespace, setNamespace } = useStudio();
  const [namespaces, setNamespaces] = useState<NamespaceInfo[]>([]);
  const [stats, setStats] = useState<NamespaceStats | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    client.namespaces().then(setNamespaces).catch((e) => setError(describeError(e)));
  }, [client]);

  useEffect(() => {
    client.namespace(namespace).stats().then(setStats).catch((e) => setError(describeError(e)));
  }, [client, namespace]);

  return (
    <div className="stack">
      <h1>Overview</h1>
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
              The database is empty. Create data in <a href="#/query">Query</a>, for example{" "}
              <code>CREATE (n:Note {"{"}title: 'Hello'{"}"});</code>
            </p>
          )}
          <div className="table-wrap">
            <table>
              <thead>
                <tr>
                  <th>Name</th>
                  <th>Nodes</th>
                  <th>Edges</th>
                </tr>
              </thead>
              <tbody>
                {namespaces.map((info) => (
                  <tr
                    key={info.name}
                    className={info.name === namespace ? "clickable selected" : "clickable"}
                    onClick={() => setNamespace(info.name)}
                  >
                    <td>{info.name}</td>
                    <td className="mono">{info.nodes}</td>
                    <td className="mono">{info.edges}</td>
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
