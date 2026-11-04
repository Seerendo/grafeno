import { useEffect, useState } from "react";

import type { Connection } from "../api.ts";
import { type RecentDatabase, desktop } from "../desktop.ts";
import { Icon, Logo } from "../icons.tsx";

/**
 * The desktop app's start: open or create a database file (the engine runs
 * inside the app), or connect to a Grafeno server.
 */
export function StartView({
  error,
  busy,
  saved,
  onOpen,
  onConnect,
}: {
  error: string | null;
  busy: boolean;
  saved: Connection | null;
  onOpen: (path: string) => void;
  onConnect: (connection: Connection, remember: boolean) => void;
}) {
  const [recent, setRecent] = useState<RecentDatabase[]>([]);
  const [url, setUrl] = useState(saved?.url ?? "http://127.0.0.1:7810");
  const [token, setToken] = useState(saved?.token ?? "");
  const [remember, setRemember] = useState(saved !== null);

  const refresh = () => void desktop.recent().then(setRecent).catch(() => setRecent([]));
  useEffect(refresh, []);

  const pick = async (create: boolean) => {
    const path = await desktop.pickDatabase(create);
    if (path) onOpen(path);
  };

  return (
    <div className="connect start">
      <div className="connect-art" aria-hidden="true" />
      <div className="start-card">
        <div className="connect-brand">
          <Logo size={60} />
          <div>
            <h1>Grafeno</h1>
            <p className="muted">The graph engine for agent memory and knowledge.</p>
          </div>
        </div>
        {error && <div className="error">{error}</div>}
        <div className="start-grid">
          <section className="stack tight">
            <h3>On this computer</h3>
            <p className="muted small">A database file. The engine runs inside the app: no service, no port.</p>
            <div className="row">
              <button className="primary" disabled={busy} onClick={() => void pick(true)}>
                <Icon name="plus" /> New database…
              </button>
              <button disabled={busy} onClick={() => void pick(false)}>
                <Icon name="folder" /> Open…
              </button>
            </div>
            <div className="recent-list">
              {recent.length === 0 && <div className="muted small">No recent databases yet.</div>}
              {recent.map((item) => (
                <div key={item.path} className={item.exists ? "recent-item" : "recent-item missing"}>
                  <button className="recent-open" disabled={busy || !item.exists} onClick={() => onOpen(item.path)} title={item.path}>
                    <span className="ns-dot" />
                    <span className="recent-text">
                      <strong>{item.name}</strong>
                      <span className="muted small mono ellipsis">{item.exists ? item.path : `missing: ${item.path}`}</span>
                    </span>
                  </button>
                  <button
                    className="ghost icon-button"
                    title="Forget it (the file stays)"
                    onClick={() => void desktop.forgetRecent(item.path).then(refresh)}
                  >
                    <Icon name="x" size={14} />
                  </button>
                </div>
              ))}
            </div>
          </section>
          <form
            className="stack tight"
            onSubmit={(event) => {
              event.preventDefault();
              onConnect({ url: url.trim().replace(/\/+$/, ""), token: token.trim() }, remember);
            }}
          >
            <h3>A Grafeno server</h3>
            <p className="muted small">The installed service, or one on another machine.</p>
            <label className="field">
              Server URL
              <input value={url} onChange={(e) => setUrl(e.target.value)} placeholder="http://127.0.0.1:7810" required />
            </label>
            <label className="field">
              Token <span className="hint">grafeno config token, on the server</span>
              <input value={token} onChange={(e) => setToken(e.target.value)} type="password" autoComplete="off" />
            </label>
            <label className="check small">
              <input type="checkbox" checked={remember} onChange={(e) => setRemember(e.target.checked)} />
              Remember this server
            </label>
            <button type="submit" disabled={busy}>
              <Icon name="link" /> Connect
            </button>
          </form>
        </div>
      </div>
    </div>
  );
}
