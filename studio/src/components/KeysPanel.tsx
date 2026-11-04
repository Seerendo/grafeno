import { useCallback, useEffect, useState } from "react";

import { describeError, formatTime, saveConnection } from "../api.ts";
import { useStudio } from "../context.ts";
import { Icon } from "../icons.tsx";
import type { AccessKey, Grant, Role } from "../../../clients/typescript/src/index.ts";

const ROLES: Role[] = ["read", "write", "admin"];

/**
 * Who the Studio is connected as and, for owners, the API keys: who may
 * read, write or administer which namespaces.
 */
export function KeysPanel() {
  const { client, connection, namespace, confirm, toast } = useStudio();
  const [me, setMe] = useState<AccessKey | null>(null);
  const [keys, setKeys] = useState<AccessKey[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState("");
  const [grants, setGrants] = useState<Grant[]>([{ namespace, role: "write" }]);
  const [owner, setOwner] = useState(false);
  const [ownOnly, setOwnOnly] = useState(false);
  const [secret, setSecret] = useState<{ name: string; secret: string } | null>(null);

  const load = useCallback(async () => {
    try {
      const who = await client.whoami();
      setMe(who);
      if (who.owner) setKeys(await client.keys());
      setError(null);
    } catch (problem) {
      setError(describeError(problem));
    }
  }, [client]);

  useEffect(() => {
    void load();
  }, [load]);

  const create = async () => {
    try {
      const created = await client.createKey({
        name: name.trim(),
        grants: owner ? [] : grants.filter((grant) => grant.namespace.trim()),
        owner,
        own_only: ownOnly,
      });
      // On a server without a token, the first (owner) key closes it: the
      // Studio switches to that key instead of locking itself out.
      if (!connection.token && created.key.owner) {
        saveConnection({ url: connection.url, token: created.secret }, true);
        toast(`The server now needs a key; the Studio uses “${created.key.name}”. Its secret: ${created.secret}`, "ok");
        window.setTimeout(() => window.location.reload(), 2500);
      }
      setSecret({ name: created.key.name, secret: created.secret });
      setCreating(false);
      setName("");
      setOwner(false);
      setOwnOnly(false);
      setGrants([{ namespace, role: "write" }]);
      await load();
    } catch (problem) {
      setError(describeError(problem));
    }
  };

  const revoke = async (key: AccessKey) => {
    const ok = await confirm({
      title: `Revoke the key “${key.name}”?`,
      message: "Its secret stops working at once. What it recorded keeps its name.",
      confirm: "Revoke",
      danger: true,
    });
    if (!ok) return;
    try {
      await client.revokeKey(key.name);
      toast(`Revoked “${key.name}”.`, "ok");
      await load();
    } catch (problem) {
      setError(describeError(problem));
    }
  };

  const describe = (key: AccessKey) =>
    key.owner ? "owner: everything" : key.grants.map((grant) => `${grant.namespace}: ${grant.role}`).join(" · ");

  return (
    <div className="panel stack">
      <div className="section-head">
        <h2>Access keys</h2>
        {me?.owner && !creating && (
          <button className="small-button" onClick={() => setCreating(true)}>
            <Icon name="plus" size={14} /> New key
          </button>
        )}
      </div>
      {me && (
        <p className="muted small" style={{ margin: 0 }}>
          Connected as <strong className="mono">{me.name}</strong> ({describe(me)}
          {me.own_only ? ", own records only" : ""}).{" "}
          {me.owner
            ? "Keys give agents and people their own access: per namespace, read, write or admin, and optionally only to what they recorded."
            : "Only owners manage keys."}
        </p>
      )}
      {error && <div className="error">{error}</div>}
      {secret && (
        <div className="secret-box stack tight">
          <strong>The secret of “{secret.name}”: copy it now, it is not shown again.</strong>
          <div className="row nowrap">
            <code className="grow-text">{secret.secret}</code>
            <button
              className="small-button"
              onClick={() => {
                void navigator.clipboard?.writeText(secret.secret);
                toast("Secret copied.", "ok");
              }}
            >
              <Icon name="copy" size={14} /> Copy
            </button>
            <button className="ghost icon-button" onClick={() => setSecret(null)} aria-label="Hide the secret">
              <Icon name="x" size={14} />
            </button>
          </div>
        </div>
      )}
      {creating && (
        <form
          className="stack tight key-form"
          onSubmit={(event) => {
            event.preventDefault();
            void create();
          }}
        >
          <label className="field">
            Name <span className="hint">recorded on what it writes (letters, digits, - _ . @)</span>
            <input value={name} onChange={(e) => setName(e.target.value)} placeholder="research-agent" required maxLength={64} />
          </label>
          {!owner && (
            <div className="field">
              Grants <span className="hint">a namespace, or a prefix such as team-*, or * for all</span>
              {grants.map((grant, index) => (
                <div className="row nowrap" key={index}>
                  <input
                    className="grow"
                    value={grant.namespace}
                    onChange={(e) => setGrants(grants.map((g, i) => (i === index ? { ...g, namespace: e.target.value } : g)))}
                    placeholder="namespace"
                  />
                  <select value={grant.role} onChange={(e) => setGrants(grants.map((g, i) => (i === index ? { ...g, role: e.target.value as Role } : g)))}>
                    {ROLES.map((role) => (
                      <option key={role} value={role}>
                        {role}
                      </option>
                    ))}
                  </select>
                  <button type="button" className="ghost icon-button" onClick={() => setGrants(grants.filter((_, i) => i !== index))} aria-label="Remove">
                    <Icon name="x" size={14} />
                  </button>
                </div>
              ))}
              <button type="button" className="small-button" style={{ alignSelf: "flex-start" }} onClick={() => setGrants([...grants, { namespace: "", role: "read" }])}>
                <Icon name="plus" size={14} /> Grant
              </button>
            </div>
          )}
          <label className="check small">
            <input type="checkbox" checked={ownOnly} onChange={(e) => setOwnOnly(e.target.checked)} />
            Only its own records: it may change or delete only what it recorded
          </label>
          <label className="check small">
            <input type="checkbox" checked={owner} onChange={(e) => setOwner(e.target.checked)} />
            Owner: every namespace, and managing keys
          </label>
          <div className="row">
            <button type="button" className="ghost" onClick={() => setCreating(false)}>
              Cancel
            </button>
            <div className="spacer" />
            <button className="primary" type="submit" disabled={!name.trim()}>
              Create key
            </button>
          </div>
        </form>
      )}
      {me?.owner && (
        <div className="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Key</th>
                <th>Access</th>
                <th>Created</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {keys.length === 0 && (
                <tr>
                  <td colSpan={4} className="muted small">
                    No keys yet: only the server's token works.
                  </td>
                </tr>
              )}
              {keys.map((key) => (
                <tr key={key.name}>
                  <td className="mono">{key.name}</td>
                  <td className="small">
                    {describe(key)}
                    {key.own_only && <span className="badge">own records only</span>}
                  </td>
                  <td className="muted small">{key.created_at ? formatTime(key.created_at).slice(0, 10) : ""}</td>
                  <td>
                    <button className="danger ghost small-button" onClick={() => void revoke(key)}>
                      Revoke
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
