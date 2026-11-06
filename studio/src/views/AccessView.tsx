import { useCallback, useEffect, useState } from "react";

import type { Account, PrivilegeCatalog, PrivilegeObject, RoleDefinition } from "../../../clients/typescript/src/index.ts";
import { describeError } from "../api.ts";
import { useStudio } from "../context.ts";

type ObjectKind = PrivilegeObject["on"];

/** A privilege as `SHOW PRIVILEGES FOR` lists it. */
interface Held {
  source: string;
  access: "grant" | "deny";
  privilege: string;
  on: string;
}

const LEVELS = ["reader", "editor", "admin"];

/** The object a description from the server stands for, for revoking it. */
function objectOf(on: string): PrivilegeObject | null {
  if (on === "server") return { on: "server" };
  let match = on.match(/^namespace (.+)$/);
  if (match) return { on: "namespace", namespace: match[1] };
  match = on.match(/^(nodes|edges) (\S+) in (.+)$/);
  if (match) return { on: "label", label: match[2], edges: match[1] === "edges", namespace: match[3] };
  match = on.match(/^property (\S+)\.(\S+) in (.+)$/);
  if (match) return { on: "property", label: match[1], property: match[2], namespace: match[3] };
  return null;
}

function status(account: Account): string {
  if (account.disabled) return "suspended";
  if (account.expires_at && account.expires_at <= Date.now() * 1000) return "expired";
  return "active";
}

/**
 * Users, roles and grants (14.3): who the caller is, the accounts and the
 * roles they hold (with `manage_accounts`), and each role's privileges,
 * granted, denied and revoked here (with `manage_roles`).
 */
export function AccessView() {
  const { client, toast, confirm } = useStudio();
  const [me, setMe] = useState<Awaited<ReturnType<typeof client.whoami>> | null>(null);
  const [accounts, setAccounts] = useState<Account[] | null>(null);
  const [roles, setRoles] = useState<RoleDefinition[] | null>(null);
  const [catalog, setCatalog] = useState<PrivilegeCatalog | null>(null);
  const [selected, setSelected] = useState<string>("");
  const [held, setHeld] = useState<Held[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const [busy, setBusy] = useState(false);
  const [newRole, setNewRole] = useState({ name: "", description: "" });
  const [grant, setGrant] = useState({
    kind: "namespace" as ObjectKind,
    privileges: [] as string[],
    namespace: "",
    label: "",
    property: "",
    edges: false,
    deny: false,
  });

  const refresh = useCallback(() => setRevision((r) => r + 1), []);

  useEffect(() => {
    let live = true;
    (async () => {
      try {
        const [who, list, defined, privileges] = await Promise.all([
          client.whoami(),
          client.accounts().catch(() => null),
          client.roles().catch(() => null),
          client.privilegeCatalog(),
        ]);
        if (!live) return;
        setMe(who);
        setAccounts(list);
        setRoles(defined);
        setCatalog(privileges);
        setError(null);
        setSelected((current) => current || defined?.[0]?.name || "");
      } catch (problem) {
        if (live) setError(describeError(problem));
      }
    })();
    return () => {
      live = false;
    };
  }, [client, revision]);

  useEffect(() => {
    if (!selected || !roles) {
      setHeld([]);
      return;
    }
    let live = true;
    client
      .privilegesOf(selected)
      .then((result) => {
        if (!live) return;
        setHeld(
          result.rows.map((row) => ({
            source: String(row[0]),
            access: row[1] === "deny" ? "deny" : "grant",
            privilege: String(row[2]),
            on: String(row[3]),
          })),
        );
      })
      .catch((problem) => live && toast(describeError(problem), "error"));
    return () => {
      live = false;
    };
  }, [client, selected, roles, revision, toast]);

  const act = async (work: () => Promise<unknown>, done?: string) => {
    setBusy(true);
    try {
      const result = await work();
      const message =
        done ??
        ((result as { rows?: unknown[][] } | undefined)?.rows?.[0]?.[0] as string | undefined) ??
        "Done";
      toast(message, "ok");
      refresh();
    } catch (problem) {
      toast(describeError(problem), "error");
    } finally {
      setBusy(false);
    }
  };

  const objectFor = (): PrivilegeObject | null => {
    const namespace = grant.namespace.trim();
    switch (grant.kind) {
      case "server":
        return { on: "server" };
      case "namespace":
        return namespace ? { on: "namespace", namespace } : null;
      case "label":
        return namespace && grant.label.trim() ? { on: "label", namespace, label: grant.label.trim(), edges: grant.edges } : null;
      case "property":
        return namespace && grant.label.trim() && grant.property.trim()
          ? { on: "property", namespace, label: grant.label.trim(), property: grant.property.trim() }
          : null;
    }
  };

  const choices =
    catalog && grant.kind === "namespace" && !grant.deny
      ? [...catalog.namespace, ...LEVELS]
      : catalog
        ? catalog[grant.kind]
        : [];
  const object = objectFor();
  const ownRole = roles?.find((role) => role.name === selected);

  return (
    <div className="stack">
      <h1>Access</h1>
      {error && <div className="error">{error}</div>}

      {me && (
        <div className="panel stack">
          <h2>You</h2>
          <p className="muted small" style={{ margin: 0 }}>
            <strong>{me.name}</strong>
            {me.superuser ? " — a superuser: everything" : ""}
            {me.via ? ` · signed in with a ${me.via.kind}` : ""}
            {me.scope ? " · a scoped key" : ""}
          </p>
          {Array.isArray((me as { effective_roles?: string[] }).effective_roles) && (
            <div className="chips">
              {((me as { effective_roles?: string[] }).effective_roles ?? []).map((role) => (
                <span className="chip" key={role}>
                  {role}
                </span>
              ))}
            </div>
          )}
        </div>
      )}

      {accounts === null && roles === null && me && !error && (
        <div className="panel">
          <p className="muted" style={{ margin: 0 }}>
            Managing users needs the <code>manage_accounts</code> privilege, and managing roles{" "}
            <code>manage_roles</code>. A superuser can grant them with{" "}
            <code>GRANT manage_roles ON SERVER TO a_role</code>.
          </p>
        </div>
      )}

      {accounts && (
        <div className="panel stack">
          <h2>Accounts</h2>
          <div style={{ overflowX: "auto" }}>
            <table>
              <thead>
                <tr>
                  <th>Name</th>
                  <th>Kind</th>
                  <th>Roles</th>
                  <th>Status</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {accounts.map((account) => (
                  <tr key={account.name}>
                    <td>
                      {account.name}
                      {account.superuser && <span className="badge">superuser</span>}
                    </td>
                    <td>{account.kind}</td>
                    <td>
                      <div className="chips">
                        {(account.roles ?? []).map((role) => (
                          <span className="chip" key={role}>
                            {role}
                            <button
                              className="chip-x"
                              title={`Take ${role} from ${account.name}`}
                              disabled={busy}
                              onClick={() => void act(() => client.revokeRoles([role], [account.name]))}
                            >
                              ×
                            </button>
                          </span>
                        ))}
                        {roles && roles.length > 0 && (
                          <select
                            value=""
                            disabled={busy}
                            aria-label={`Give ${account.name} a role`}
                            onChange={(event) =>
                              event.target.value && void act(() => client.grantRoles([event.target.value], [account.name]))
                            }
                          >
                            <option value="">+ role</option>
                            {roles
                              .filter((role) => !(account.roles ?? []).includes(role.name))
                              .map((role) => (
                                <option key={role.name} value={role.name}>
                                  {role.name}
                                </option>
                              ))}
                          </select>
                        )}
                      </div>
                    </td>
                    <td>{status(account)}</td>
                    <td>
                      <button
                        disabled={busy || account.name === me?.name}
                        onClick={() =>
                          void act(
                            () => client.changeAccount(account.name, { disabled: !account.disabled }),
                            account.disabled ? `${account.name} is active` : `${account.name} is suspended`,
                          )
                        }
                      >
                        {account.disabled ? "Activate" : "Suspend"}
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}

      {roles && (
        <div className="panel stack">
          <h2>Roles</h2>
          <p className="muted small" style={{ margin: 0 }}>
            A role holds privileges, built-in levels and other roles. An account's rights are the union of its roles'; a
            denial wins over any grant. A role named <code>public</code> is everyone's.
          </p>
          <div className="row">
            <input
              placeholder="new role"
              value={newRole.name}
              onChange={(event) => setNewRole({ ...newRole, name: event.target.value })}
            />
            <input
              placeholder="description"
              value={newRole.description}
              onChange={(event) => setNewRole({ ...newRole, description: event.target.value })}
            />
            <button
              disabled={busy || !newRole.name.trim()}
              onClick={() =>
                void act(async () => {
                  await client.putRole(newRole.name.trim(), { description: newRole.description || undefined });
                  setSelected(newRole.name.trim());
                  setNewRole({ name: "", description: "" });
                }, `Created ${newRole.name.trim()}`)
              }
            >
              Create
            </button>
          </div>
          {roles.length > 0 && (
            <div className="chips">
              {roles.map((role) => (
                <button
                  key={role.name}
                  className={role.name === selected ? "chip active" : "chip"}
                  onClick={() => setSelected(role.name)}
                >
                  {role.name}
                </button>
              ))}
            </div>
          )}
          {ownRole && (
            <div className="stack">
              <h3>
                {ownRole.name}
                {ownRole.description ? <span className="muted small"> — {ownRole.description}</span> : null}
              </h3>
              <table>
                <thead>
                  <tr>
                    <th>Access</th>
                    <th>Privilege</th>
                    <th>On</th>
                    <th>From</th>
                    <th />
                  </tr>
                </thead>
                <tbody>
                  {held.length === 0 && (
                    <tr>
                      <td colSpan={5} className="muted">
                        No privileges yet.
                      </td>
                    </tr>
                  )}
                  {held.map((item, index) => {
                    const target = objectOf(item.on);
                    return (
                      <tr key={index}>
                        <td>{item.access === "deny" ? <span className="badge danger">deny</span> : "grant"}</td>
                        <td>{item.privilege}</td>
                        <td>{item.on}</td>
                        <td>{item.source === ownRole.name ? "" : item.source}</td>
                        <td>
                          {item.source === ownRole.name && target && (
                            <button
                              disabled={busy}
                              onClick={() =>
                                void act(() =>
                                  client.revoke([item.privilege], target, [ownRole.name], item.access === "deny" ? "denials" : "grants"),
                                )
                              }
                            >
                              Revoke
                            </button>
                          )}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>

              <div className="stack">
                <h3>Grant or deny</h3>
                <div className="row">
                  <select
                    value={grant.kind}
                    onChange={(event) => setGrant({ ...grant, kind: event.target.value as ObjectKind, privileges: [] })}
                  >
                    <option value="namespace">a namespace</option>
                    <option value="label">nodes or edges with a label</option>
                    <option value="property">a property</option>
                    <option value="server">the server</option>
                  </select>
                  {grant.kind !== "server" && (
                    <input
                      placeholder="namespace or pattern (team-*, *)"
                      value={grant.namespace}
                      onChange={(event) => setGrant({ ...grant, namespace: event.target.value })}
                    />
                  )}
                  {(grant.kind === "label" || grant.kind === "property") && (
                    <input
                      placeholder="label"
                      value={grant.label}
                      onChange={(event) => setGrant({ ...grant, label: event.target.value })}
                    />
                  )}
                  {grant.kind === "property" && (
                    <input
                      placeholder="property"
                      value={grant.property}
                      onChange={(event) => setGrant({ ...grant, property: event.target.value })}
                    />
                  )}
                  {grant.kind === "label" && (
                    <label className="small">
                      <input
                        type="checkbox"
                        checked={grant.edges}
                        onChange={(event) => setGrant({ ...grant, edges: event.target.checked })}
                      />{" "}
                      edges
                    </label>
                  )}
                  <label className="small">
                    <input
                      type="checkbox"
                      checked={grant.deny}
                      onChange={(event) =>
                        setGrant({
                          ...grant,
                          deny: event.target.checked,
                          privileges: grant.privileges.filter((p) => !LEVELS.includes(p)),
                        })
                      }
                    />{" "}
                    deny
                  </label>
                </div>
                <div className="chips">
                  {choices.map((privilege) => {
                    const on = grant.privileges.includes(privilege);
                    return (
                      <button
                        key={privilege}
                        className={on ? "chip active" : "chip"}
                        title={LEVELS.includes(privilege) ? "a built-in level" : undefined}
                        onClick={() =>
                          setGrant({
                            ...grant,
                            privileges: on ? grant.privileges.filter((p) => p !== privilege) : [...grant.privileges, privilege],
                          })
                        }
                      >
                        {privilege}
                      </button>
                    );
                  })}
                </div>
                <div className="row">
                  <button
                    className={grant.deny ? "danger" : undefined}
                    disabled={busy || !object || grant.privileges.length === 0}
                    onClick={() =>
                      object &&
                      void act(() => client.grant(grant.privileges, object, [ownRole.name], { deny: grant.deny }))
                    }
                  >
                    {grant.deny ? "Deny" : "Grant"} to {ownRole.name}
                  </button>
                  <button
                    className="danger"
                    disabled={busy}
                    onClick={async () => {
                      const sure = await confirm({
                        title: `Delete the role ${ownRole.name}?`,
                        message: "Every account and role that has it loses it.",
                        confirm: "Delete",
                        danger: true,
                      });
                      if (sure) {
                        await act(() => client.deleteRole(ownRole.name), `Deleted ${ownRole.name}`);
                        setSelected("");
                      }
                    }}
                  >
                    Delete role
                  </button>
                </div>
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
