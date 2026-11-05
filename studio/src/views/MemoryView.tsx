import { useCallback, useEffect, useState } from "react";

import { describeError, displayName, formatTime } from "../api.ts";
import { useStudio } from "../context.ts";
import { GrafenoError } from "../../../clients/typescript/src/index.ts";
import type { Conflict, ConsolidationGroup, Duplicate, MergeRecord, Node, WeakMemory } from "../../../clients/typescript/src/index.ts";

function describeConflict(conflict: Conflict): string {
  const show = (value: unknown) => (typeof value === "string" ? value : JSON.stringify(value));
  switch (conflict.kind) {
    case "strong_key":
      return `different ${conflict.key}: ${show(conflict.kept)} and ${show(conflict.merged)}`;
    case "labels":
      return `different labels: ${show(conflict.kept)} and ${show(conflict.merged)}`;
    case "edge":
      return `conflicting ${conflict.key} facts`;
    default:
      return `${conflict.key}: ${show(conflict.kept)} and ${show(conflict.merged)}`;
  }
}

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
  const { client, namespace, confirm, changed } = useStudio();
  const graph = client.namespace(namespace);
  const [duplicates, setDuplicates] = useState<Duplicate[] | null>(null);
  const [weak, setWeak] = useState<WeakMemory[] | null>(null);
  const [groups, setGroups] = useState<ConsolidationGroup[] | null>(null);
  const [merges, setMerges] = useState<MergeRecord[] | null>(null);
  const [threshold, setThreshold] = useState(0.1);
  const [olderThan, setOlderThan] = useState(7);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      const [found, fading, grouped, recent] = await Promise.all([
        graph.duplicates({ limit: 50 }),
        graph.weak({ threshold, limit: 50 }),
        graph.consolidation({ older_than_days: olderThan, limit: 20 }),
        graph.merges({ limit: 10 }),
      ]);
      setDuplicates(found);
      setMerges(recent);
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
      changed();
      await load();
    } catch (problem) {
      setError(describeError(problem));
    } finally {
      setBusy(false);
    }
  };

  const merge = async (keep: Node, other: Node) => {
    const ok = await confirm({
      title: `Merge “${displayName(other)}” into “${displayName(keep)}”?`,
      message: `Its facts move to “${displayName(keep)}”, its name becomes an alias, and it is deleted.`,
      confirm: "Merge",
    });
    if (!ok) return;
    void act(async () => {
      let force = false;
      try {
        await graph.mergeConflicts(keep.id, other.id).then(async (conflicts) => {
          if (conflicts.length === 0) return;
          const identity = conflicts.some((c) => c.kind === "strong_key" || c.kind === "labels");
          force = await confirm({
            title: identity ? "They look like different things" : "They disagree on some facts",
            message: `${conflicts.map(describeConflict).join("; ")}. Merge anyway? “${displayName(keep)}” keeps its values; the merge can be undone.`,
            confirm: "Merge anyway",
            danger: identity,
          });
          if (!force) throw new GrafenoError(0, "cancelled", "Merge cancelled.");
        });
      } catch (problem) {
        if (problem instanceof GrafenoError && problem.code === "cancelled") return problem.message;
        throw problem;
      }
      const report = await graph.merge(keep.id, other.id, { force });
      return `Merged: ${report.edges_moved} facts moved, ${report.edges_dropped} dropped.`;
    });
  };

  const undo = (record: MergeRecord) =>
    void act(async () => {
      try {
        await graph.undoMerge(record.id);
      } catch (problem) {
        if (!(problem instanceof GrafenoError && problem.status === 409 && problem.message.includes("changed after"))) throw problem;
        const ok = await confirm({
          title: "The kept node changed since",
          message: `Undoing restores “${displayName(record.kept_before.node)}” as it was before the merge and loses those changes.`,
          confirm: "Undo anyway",
          danger: true,
        });
        if (!ok) return "Undo cancelled.";
        await graph.undoMerge(record.id, true);
      }
      return `“${displayName(record.merged_before.node)}” is back.`;
    });

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
                {pair.conflicts.length > 0 && (
                  <div className="small warn">Disagree: {pair.conflicts.map(describeConflict).join("; ")}</div>
                )}
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

      {merges && merges.length > 0 && (
        <div className="panel stack">
          <h2>Recent merges</h2>
          <div className="list">
            {merges.map((record) => (
              <div key={record.id} className="memory-row">
                <div className="stack" style={{ gap: 2 }}>
                  <div>
                    “{displayName(record.merged_before.node)}” into “{displayName(record.kept_before.node)}”
                  </div>
                  <div className="muted small">
                    {formatTime(record.at)}
                    {record.by && ` · by ${record.by}`}
                    {record.forced && ` · forced over ${record.conflicts.map(describeConflict).join("; ")}`}
                    {record.undone_at !== null && ` · undone ${formatTime(record.undone_at)}`}
                  </div>
                </div>
                {record.undone_at === null && (
                  <button disabled={busy} onClick={() => undo(record)}>Undo</button>
                )}
              </div>
            ))}
          </div>
        </div>
      )}

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
              onClick={async () => {
                if (!(await confirm({ title: `Archive all ${weak.length} weak memories?`, message: "They are kept, but recall leaves them out.", confirm: "Archive" }))) return;
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
