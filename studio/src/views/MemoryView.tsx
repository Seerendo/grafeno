import { useCallback, useEffect, useState } from "react";

import { describeError, displayName, formatTime } from "../api.ts";
import { useStudio } from "../App.tsx";
import type { ConsolidationGroup, Duplicate, Node, WeakMemory } from "../../../clients/typescript/src/index.ts";

function NodeLink({ node }: { node: Node }) {
  return (
    <a href={`#/explorer/${node.id}`} title={node.id}>
      {displayName(node)} <span className="muted">({node.labels.join(", ")})</span>
    </a>
  );
}

/**
 * Memory upkeep: likely duplicates to merge, weak memories to archive, and
 * groups of older memories an agent could summarize.
 */
export function MemoryView() {
  const { client, namespace } = useStudio();
  const graph = client.namespace(namespace);
  const [duplicates, setDuplicates] = useState<Duplicate[] | null>(null);
  const [weak, setWeak] = useState<WeakMemory[] | null>(null);
  const [groups, setGroups] = useState<ConsolidationGroup[] | null>(null);
  const [threshold, setThreshold] = useState(0.1);
  const [olderThan, setOlderThan] = useState(7);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      const [found, fading, grouped] = await Promise.all([
        graph.duplicates({ limit: 50 }),
        graph.weak({ threshold, limit: 50 }),
        graph.consolidation({ older_than_days: olderThan, limit: 20 }),
      ]);
      setDuplicates(found);
      setWeak(fading);
      setGroups(grouped);
      setError(null);
    } catch (problem) {
      setError(describeError(problem));
    }
    // `graph` changes identity on every render; `client` and `namespace` do not.
  }, [client, namespace, threshold, olderThan]);

  useEffect(() => {
    void load();
  }, [load]);

  const act = async (work: () => Promise<string>) => {
    setBusy(true);
    try {
      setNotice(await work());
      await load();
    } catch (problem) {
      setError(describeError(problem));
    } finally {
      setBusy(false);
    }
  };

  const merge = (keep: Node, other: Node) => {
    if (!window.confirm(`Merge "${displayName(other)}" into "${displayName(keep)}"? "${displayName(other)}" is deleted.`)) return;
    void act(async () => {
      const report = await graph.merge(keep.id, other.id);
      return `Merged: ${report.edges_moved} facts moved, ${report.edges_dropped} dropped.`;
    });
  };

  const distinct = (pair: Duplicate) =>
    void act(async () => {
      await graph.createEdge({ from: pair.a.id, label: "distinct_from", to: pair.b.id });
      return `"${displayName(pair.a)}" and "${displayName(pair.b)}" are marked as different.`;
    });

  return (
    <div className="stack">
      <h1>Memory</h1>
      {error && <div className="error">{error}</div>}
      {notice && <div className="ok small" onClick={() => setNotice(null)}>{notice}</div>}

      <div className="panel stack">
        <h2>Possible duplicates {duplicates && <span className="muted">({duplicates.length})</span>}</h2>
        <p className="muted small" style={{ margin: 0 }}>
          Nodes with the same label and a similar name (ignoring case and accents) or meaning. Merging moves the facts to the
          kept node and turns the other name into an alias.
        </p>
        {duplicates?.length === 0 && <div className="muted small">No duplicates found.</div>}
        <div className="list">
          {duplicates?.map((pair) => (
            <div key={`${pair.a.id}-${pair.b.id}`} className="memory-row">
              <div className="stack" style={{ gap: 2 }}>
                <div>
                  <NodeLink node={pair.a} /> <span className="muted">≈</span> <NodeLink node={pair.b} />
                </div>
                <div className="muted small">
                  {pair.score.toFixed(2)} · {pair.reasons.join("; ")}
                </div>
              </div>
              <div className="row">
                <button disabled={busy} onClick={() => merge(pair.a, pair.b)} title={`Keep ${displayName(pair.a)}`}>
                  Keep left
                </button>
                <button disabled={busy} onClick={() => merge(pair.b, pair.a)} title={`Keep ${displayName(pair.b)}`}>
                  Keep right
                </button>
                <button disabled={busy} onClick={() => distinct(pair)}>Different</button>
              </div>
            </div>
          ))}
        </div>
      </div>

      <div className="panel stack">
        <div className="row">
          <h2 style={{ margin: 0 }}>Weak memories {weak && <span className="muted">({weak.length})</span>}</h2>
          <div className="spacer" />
          <label className="row small">
            <span className="muted">Strength below</span>
            <input type="number" min={0} max={1} step={0.05} value={threshold} onChange={(e) => setThreshold(Number(e.target.value))} style={{ width: 80 }} />
          </label>
        </div>
        <p className="muted small" style={{ margin: 0 }}>
          Strength combines importance, how recently a memory was used and how often. Archived memories are kept but left out of
          recall.
        </p>
        {weak?.length === 0 && <div className="muted small">Nothing is fading.</div>}
        <div className="list">
          {weak?.map((item) => (
            <div key={item.node.id} className="memory-row">
              <div className="stack" style={{ gap: 2 }}>
                <NodeLink node={item.node} />
                <div className="muted small">
                  strength {item.strength.toFixed(2)} · importance {item.memory.importance.toFixed(2)} · {item.memory.accesses} accesses ·
                  last {formatTime(item.memory.last_access).slice(0, 10)}
                </div>
              </div>
              <button
                disabled={busy}
                onClick={() => void act(async () => `${await graph.archive([item.node.id])} archived.`)}
              >
                Archive
              </button>
            </div>
          ))}
        </div>
        {weak && weak.length > 1 && (
          <div className="row">
            <button
              disabled={busy}
              onClick={() => {
                if (!window.confirm(`Archive all ${weak.length} weak memories?`)) return;
                void act(async () => `${await graph.archive(weak.map((item) => item.node.id))} archived.`);
              }}
            >
              Archive all
            </button>
          </div>
        )}
      </div>

      <div className="panel stack">
        <div className="row">
          <h2 style={{ margin: 0 }}>Ready to consolidate {groups && <span className="muted">({groups.length})</span>}</h2>
          <div className="spacer" />
          <label className="row small">
            <span className="muted">Older than (days)</span>
            <input type="number" min={0} value={olderThan} onChange={(e) => setOlderThan(Number(e.target.value))} style={{ width: 80 }} />
          </label>
        </div>
        <p className="muted small" style={{ margin: 0 }}>
          Several older memories about one entity. An agent can summarize them with the <code>consolidate</code> MCP tool (or{" "}
          <code>POST /v1/{"{ns}"}/summaries</code>): the summary replaces them in recall and links to them.
        </p>
        {groups?.length === 0 && <div className="muted small">No groups yet.</div>}
        {groups?.map((group) => (
          <div key={group.entity.id} className="stack" style={{ gap: 4 }}>
            <div>
              <NodeLink node={group.entity} /> <span className="muted small">· {group.memories.length} memories</span>
            </div>
            <ul className="small" style={{ margin: 0 }}>
              {group.memories.map((memory) => (
                <li key={memory.id}>
                  <NodeLink node={memory} />
                </li>
              ))}
            </ul>
          </div>
        ))}
      </div>
    </div>
  );
}
