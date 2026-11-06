import { useEffect, useState } from "react";

import { describeError } from "../api.ts";
import { useStudio } from "../context.ts";

/** A member's view of one Raft group (`GET /v1/cluster`). */
interface Status {
  id: number;
  role: string;
  term: number;
  leader: number | null;
  commit: number;
  applied: number;
  last_index: number;
  snapshot_index: number;
  voters: number[];
  outgoing: number[];
  elections: number;
  leader_changes: number;
  snapshots_sent: number;
  snapshots_installed: number;
  peers: { id: number; matched: number; lag: number }[];
  format: number;
  members: Record<string, { version: string; format: number; layout: number }>;
}

interface Group {
  id: number;
  voters: number[];
  held: boolean;
  status: Status | null;
}

interface Placement {
  namespace: string;
  group: number;
  moving_to: number | null;
}

/** The cluster as the member this Studio talks to sees it, refreshed every two seconds. */
export function ClusterView() {
  const { client, namespaces, changed, confirm, toast } = useStudio();
  const [groups, setGroups] = useState<Group[] | null>(null);
  const [placement, setPlacement] = useState<Placement[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [single, setSingle] = useState(false);
  const [moving, setMoving] = useState<{ namespace: string; group: number }>({
    namespace: "",
    group: 0,
  });
  const [busy, setBusy] = useState(false);
  const [tick, setTick] = useState(0);

  useEffect(() => {
    const timer = window.setInterval(() => setTick((t) => t + 1), 2000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    let live = true;
    (async () => {
      try {
        const status = await client.request<Status>("GET", "/v1/cluster");
        const more = await client
          .request<Group[]>("GET", "/v1/cluster/groups")
          .catch(() => [] as Group[]);
        const placed = await client
          .request<Placement[]>("GET", "/v1/cluster/placement")
          .catch(() => [] as Placement[]);
        if (!live) return;
        setGroups(
          more.length > 0
            ? more
            : [{ id: 0, voters: status.voters, held: true, status }],
        );
        setPlacement(placed);
        setSingle(false);
        setError(null);
      } catch (problem) {
        if (!live) return;
        const message = describeError(problem);
        if (/not.*cluster|404|400/i.test(message)) setSingle(true);
        else setError(message);
      }
    })();
    return () => {
      live = false;
    };
  }, [client, tick]);

  if (single) {
    return (
      <div className="stack">
        <h1>Cluster</h1>
        <div className="panel">
          <p className="muted" style={{ margin: 0 }}>
            This server runs alone. Give it a <code>[cluster]</code> section in{" "}
            <code>grafeno.toml</code> (see the installation guide) to run it
            with others: they hold the same data, elect a leader, and keep
            serving when one fails.
          </p>
        </div>
      </div>
    );
  }

  const here = groups?.find((group) => group.status)?.status;
  const members = here
    ? Object.entries(here.members).sort(([a], [b]) => Number(a) - Number(b))
    : [];
  const groupOf = (ns: string) =>
    placement.find((p) => p.namespace === ns)?.group ?? 0;

  return (
    <div className="stack">
      <h1>Cluster</h1>
      {error && <div className="error">{error}</div>}
      {!groups && !error && <p className="muted">Loading…</p>}

      {groups && (
        <div className="panel stack">
          <h2>Raft groups{here ? `, as member ${here.id} sees them` : ""}</h2>
          <div style={{ overflowX: "auto" }}>
            <table>
              <thead>
                <tr>
                  <th>Group</th>
                  <th>Voters</th>
                  <th>Here</th>
                  <th>Leader</th>
                  <th>Term</th>
                  <th>Commit</th>
                  <th>Applied</th>
                  <th>Snapshot</th>
                  <th>Format</th>
                  <th>Elections</th>
                  <th>Leader changes</th>
                  <th>Snapshots sent / installed</th>
                </tr>
              </thead>
              <tbody>
                {groups.map((group) => {
                  const status = group.status;
                  return (
                    <tr key={group.id}>
                      <td>{group.id}</td>
                      <td>{group.voters.join(", ")}</td>
                      <td>{status ? status.role : "not held"}</td>
                      <td>{status?.leader ?? "—"}</td>
                      <td>{status?.term ?? "—"}</td>
                      <td>{status?.commit ?? "—"}</td>
                      <td>
                        {status ? status.applied : "—"}
                        {status && status.commit > status.applied ? (
                          <span className="muted small">
                            {" "}
                            ({status.commit - status.applied} behind)
                          </span>
                        ) : null}
                      </td>
                      <td>{status?.snapshot_index ?? "—"}</td>
                      <td title="The newest entry format every voter reads, as the leader works it out (followers hear only from the leader)">
                        {status
                          ? status.role === "leader"
                            ? status.format
                            : "leader's"
                          : "—"}
                      </td>
                      <td>{status?.elections ?? "—"}</td>
                      <td>{status?.leader_changes ?? "—"}</td>
                      <td>
                        {status
                          ? `${status.snapshots_sent} / ${status.snapshots_installed}`
                          : "—"}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
          {groups.some(
            (group) =>
              group.status?.role === "leader" && group.status.peers.length > 0,
          ) && (
            <>
              <h3>
                Lag of the other members (on the groups this member leads)
              </h3>
              <table>
                <thead>
                  <tr>
                    <th>Group</th>
                    <th>Member</th>
                    <th>Holds up to</th>
                    <th>Entries behind</th>
                  </tr>
                </thead>
                <tbody>
                  {groups.flatMap((group) =>
                    (group.status?.role === "leader"
                      ? group.status.peers
                      : []
                    ).map((peer) => (
                      <tr key={`${group.id}-${peer.id}`}>
                        <td>{group.id}</td>
                        <td>{peer.id}</td>
                        <td>{peer.matched}</td>
                        <td
                          style={
                            peer.lag > 100
                              ? { color: "var(--danger, #c33)" }
                              : undefined
                          }
                        >
                          {peer.lag}
                        </td>
                      </tr>
                    )),
                  )}
                </tbody>
              </table>
            </>
          )}
        </div>
      )}

      {members.length > 0 && (
        <div className="panel stack">
          <h2>Members</h2>
          <p className="muted small" style={{ margin: 0 }}>
            What each member said it runs. During a rolling upgrade members of
            two versions coexist; the leader writes no entry format a voter
            cannot read.
          </p>
          <table>
            <thead>
              <tr>
                <th>Member</th>
                <th>Version</th>
                <th>Entry format</th>
                <th>Layout</th>
              </tr>
            </thead>
            <tbody>
              {members.map(([id, info]) => (
                <tr key={id}>
                  <td>
                    {id}
                    {here && Number(id) === here.id ? (
                      <span className="muted small"> (this one)</span>
                    ) : null}
                  </td>
                  <td>{info.version}</td>
                  <td>{info.format}</td>
                  <td>{info.layout}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {groups && groups.length > 1 && (
        <div className="panel stack">
          <h2>Where namespaces live</h2>
          <table>
            <thead>
              <tr>
                <th>Namespace</th>
                <th>Group</th>
              </tr>
            </thead>
            <tbody>
              {namespaces.map((info) => {
                const placed = placement.find((p) => p.namespace === info.name);
                return (
                  <tr key={info.name}>
                    <td>{info.name}</td>
                    <td>
                      {groupOf(info.name)}
                      {placed?.moving_to != null ? (
                        <span className="muted small">
                          {" "}
                          (moving to {placed.moving_to})
                        </span>
                      ) : null}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          <div className="row">
            <select
              value={moving.namespace}
              onChange={(e) =>
                setMoving({ ...moving, namespace: e.target.value })
              }
            >
              <option value="">Namespace…</option>
              {namespaces.map((info) => (
                <option key={info.name} value={info.name}>
                  {info.name}
                </option>
              ))}
            </select>
            <select
              value={moving.group}
              onChange={(e) =>
                setMoving({ ...moving, group: Number(e.target.value) })
              }
            >
              {groups.map((group) => (
                <option key={group.id} value={group.id}>
                  group {group.id}
                </option>
              ))}
            </select>
            <button
              disabled={
                busy ||
                !moving.namespace ||
                groupOf(moving.namespace) === moving.group
              }
              onClick={async () => {
                const ok = await confirm({
                  title: `Move “${moving.namespace}” to group ${moving.group}?`,
                  message:
                    "It keeps serving while it is copied; its writes pause for a moment at the end (clients retry them).",
                  confirm: "Move",
                });
                if (!ok) return;
                setBusy(true);
                try {
                  const result = await client.request<{
                    paused_ms?: number;
                    records?: number;
                  }>("POST", "/v1/cluster/placement", moving);
                  toast(
                    `Moved: ${result.records ?? 0} records, writes paused ${Math.round(result.paused_ms ?? 0)} ms`,
                    "ok",
                  );
                  changed();
                  setTick((t) => t + 1);
                } catch (problem) {
                  toast(describeError(problem), "error");
                } finally {
                  setBusy(false);
                }
              }}
            >
              {busy ? "Moving…" : "Move"}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
