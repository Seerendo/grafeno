import { createContext, useCallback, useContext, useEffect, useMemo, useState } from "react";

import {
  type Connection,
  type Grafeno,
  clientFor,
  defaultUrl,
  describeError,
  forgetConnection,
  loadConnection,
  probe,
  saveConnection,
} from "./api.ts";
import { AdminView } from "./views/AdminView.tsx";
import { ExplorerView } from "./views/ExplorerView.tsx";
import { GraphView } from "./views/GraphView.tsx";
import { MemoryView } from "./views/MemoryView.tsx";
import { OverviewView } from "./views/OverviewView.tsx";
import { QueryView } from "./views/QueryView.tsx";
import { SearchView } from "./views/SearchView.tsx";

interface Studio {
  client: Grafeno;
  connection: Connection;
  namespace: string;
  setNamespace: (name: string) => void;
  namespaces: string[];
  /** Reloads the namespace list (after writes that may add or empty one). */
  refreshNamespaces: () => void;
}

const StudioContext = createContext<Studio | null>(null);

export function useStudio(): Studio {
  const studio = useContext(StudioContext);
  if (!studio) throw new Error("useStudio outside the Studio");
  return studio;
}

const VIEWS = [
  { id: "overview", title: "Overview" },
  { id: "explorer", title: "Explorer" },
  { id: "graph", title: "Graph" },
  { id: "query", title: "Query" },
  { id: "search", title: "Search" },
  { id: "memory", title: "Memory" },
  { id: "admin", title: "Administration" },
] as const;

type ViewId = (typeof VIEWS)[number]["id"];

function parseHash(): { view: ViewId; param?: string } {
  const [view, param] = window.location.hash.replace(/^#\/?/, "").split("/");
  const known = VIEWS.find((v) => v.id === view);
  return { view: known ? known.id : "overview", param: param ? decodeURIComponent(param) : undefined };
}

function Logo() {
  return (
    <svg width="22" height="22" viewBox="0 0 32 32" aria-hidden="true">
      <path d="M16 3l11 6.5v13L16 29 5 22.5v-13z" fill="none" stroke="currentColor" strokeWidth="2.5" />
      <circle cx="16" cy="16" r="4" fill="currentColor" />
    </svg>
  );
}

function ConnectScreen({ initial, error, onConnect }: {
  initial: Connection;
  error: string | null;
  onConnect: (connection: Connection, remember: boolean) => void;
}) {
  const [url, setUrl] = useState(initial.url);
  const [token, setToken] = useState(initial.token);
  const [remember, setRemember] = useState(false);
  return (
    <div className="connect">
      <form
        className="panel stack"
        onSubmit={(event) => {
          event.preventDefault();
          onConnect({ url: url.trim().replace(/\/+$/, ""), token: token.trim() }, remember);
        }}
      >
        <div className="brand">
          <Logo /> Grafeno Studio
        </div>
        <p className="muted small">Connect to a running <code>grafeno serve</code>.</p>
        <label className="field">
          Server URL
          <input value={url} onChange={(e) => setUrl(e.target.value)} placeholder="http://127.0.0.1:7810" required />
        </label>
        <label className="field">
          Token <span className="muted small">(if the server was started with --token)</span>
          <input type="password" value={token} onChange={(e) => setToken(e.target.value)} autoComplete="off" />
        </label>
        <label className="row small">
          <input type="checkbox" checked={remember} onChange={(e) => setRemember(e.target.checked)} />
          Remember on this browser
        </label>
        {error && <div className="error">{error}</div>}
        <button className="primary" type="submit">Connect</button>
      </form>
    </div>
  );
}

export function App() {
  const [connection, setConnection] = useState<Connection | null>(null);
  const [version, setVersion] = useState<string>("");
  const [error, setError] = useState<string | null>(null);
  const [checking, setChecking] = useState(true);
  const [route, setRoute] = useState(parseHash);
  const [namespaces, setNamespaces] = useState<string[]>([]);
  const [namespace, setNamespaceState] = useState<string>(() => {
    try {
      return localStorage.getItem("grafeno.studio.namespace") ?? "";
    } catch {
      return "";
    }
  });

  const connect = useCallback(async (candidate: Connection, remember?: boolean) => {
    setChecking(true);
    try {
      const { version } = await probe(candidate);
      if (remember !== undefined) saveConnection(candidate, remember);
      setVersion(version);
      setConnection(candidate);
      setError(null);
    } catch (problem) {
      setError(describeError(problem));
      setConnection(null);
    } finally {
      setChecking(false);
    }
  }, []);

  // Try a saved connection, then the server that served the Studio.
  useEffect(() => {
    const saved = loadConnection();
    void connect(saved ?? { url: defaultUrl(), token: "" }, saved ? undefined : false).then(() => {
      if (!saved) setError(null);
    });
  }, [connect]);

  useEffect(() => {
    const onHash = () => setRoute(parseHash());
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);

  const client = useMemo(() => (connection ? clientFor(connection) : null), [connection]);

  const refreshNamespaces = useCallback(() => {
    if (!client) return;
    client
      .namespaces()
      .then((list) => {
        const names = list.map((info) => info.name);
        setNamespaces(names);
        setNamespaceState((current) => current || names[0] || "default");
      })
      .catch((problem) => setError(describeError(problem)));
  }, [client]);

  useEffect(refreshNamespaces, [refreshNamespaces]);

  const setNamespace = useCallback((name: string) => {
    setNamespaceState(name);
    try {
      localStorage.setItem("grafeno.studio.namespace", name);
    } catch {
      // Not remembered: only a convenience.
    }
  }, []);

  if (checking && !connection) {
    return <div className="connect muted">Connecting…</div>;
  }
  if (!connection || !client) {
    return (
      <ConnectScreen
        initial={{ url: defaultUrl(), token: "" }}
        error={error}
        onConnect={(candidate, remember) => void connect(candidate, remember)}
      />
    );
  }

  const studio: Studio = { client, connection, namespace: namespace || "default", setNamespace, namespaces, refreshNamespaces };
  const view = route.view;

  return (
    <StudioContext.Provider value={studio}>
      <div className="app">
        <header className="topbar">
          <div className="brand">
            <Logo /> Grafeno Studio
          </div>
          <label className="row small">
            <span className="muted">Namespace</span>
            <input
              list="namespaces"
              value={studio.namespace}
              onChange={(e) => setNamespace(e.target.value)}
              style={{ width: 180 }}
              aria-label="Namespace"
            />
            <datalist id="namespaces">
              {namespaces.map((name) => (
                <option key={name} value={name} />
              ))}
            </datalist>
          </label>
          <div className="spacer" />
          <span className="muted small" title={connection.url}>
            {connection.url.replace(/^https?:\/\//, "")} · v{version}
          </span>
          <button
            onClick={() => {
              forgetConnection();
              setConnection(null);
            }}
          >
            Disconnect
          </button>
        </header>
        <nav className="sidebar">
          {VIEWS.map((item) => (
            <a key={item.id} href={`#/${item.id}`} className={item.id === view ? "active" : ""}>
              {item.title}
            </a>
          ))}
        </nav>
        <main className={view === "graph" ? "main full" : "main"}>
          {error && <div className="error" onClick={() => setError(null)}>{error}</div>}
          {view === "overview" && <OverviewView />}
          {view === "explorer" && <ExplorerView selected={route.param} />}
          {view === "graph" && <GraphView seed={route.param} />}
          {view === "query" && <QueryView />}
          {view === "search" && <SearchView />}
          {view === "memory" && <MemoryView />}
          {view === "admin" && <AdminView />}
        </main>
      </div>
    </StudioContext.Provider>
  );
}
