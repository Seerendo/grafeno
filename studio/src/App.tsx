import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import {
  type Connection,
  type NamespaceInfo,
  clientFor,
  defaultUrl,
  describeError,
  displayName,
  forgetConnection,
  go,
  labelColor,
  loadConnection,
  probe,
  saveConnection,
} from "./api.ts";
import { EdgeForm, NodeForm } from "./components/Forms.tsx";
import { type ConfirmOptions, type NewEdgeOptions, type NewNodeOptions, type Studio, StudioContext, useStudio } from "./context.ts";
import { Icon, type IconName, Logo } from "./icons.tsx";
import { Modal, useNodeSearch } from "./ui.tsx";
import { type DesktopSession, desktop, isDesktop, saveText } from "./desktop.ts";
import { AdminView } from "./views/AdminView.tsx";
import { ExplorerView } from "./views/ExplorerView.tsx";
import { GraphView } from "./views/GraphView.tsx";
import { MemoryView } from "./views/MemoryView.tsx";
import { OverviewView } from "./views/OverviewView.tsx";
import { ScriptsView } from "./views/ScriptsView.tsx";
import { SearchView } from "./views/SearchView.tsx";
import { StartView } from "./views/StartView.tsx";

export { useStudio };

const VIEWS: { id: string; title: string; icon: IconName; hint: string }[] = [
  { id: "overview", title: "Overview", icon: "overview", hint: "Namespaces and what they hold" },
  { id: "explorer", title: "Explorer", icon: "explorer", hint: "Browse, create and edit nodes" },
  { id: "graph", title: "Graph", icon: "graph", hint: "Draw and connect the graph" },
  { id: "scripts", title: "Scripts", icon: "scripts", hint: "Write and run GQ scripts" },
  { id: "search", title: "Search", icon: "search", hint: "Full text, recall and context" },
  { id: "memory", title: "Memory", icon: "memory", hint: "Duplicates, decay, consolidation" },
  { id: "admin", title: "Administration", icon: "admin", hint: "Check, rebuild, export, import" },
];

function parseHash(): { view: string; param?: string } {
  const [raw, param] = window.location.hash.replace(/^#\/?/, "").split("/");
  const view = raw === "query" ? "scripts" : raw;
  const known = VIEWS.find((v) => v.id === view);
  return { view: known ? known.id : "overview", param: param ? decodeURIComponent(param) : undefined };
}

type Theme = "system" | "light" | "dark";

function readSetting(key: string, fallback: string): string {
  try {
    return localStorage.getItem(key) ?? fallback;
  } catch {
    return fallback;
  }
}

function writeSetting(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Settings are a convenience.
  }
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
      <div className="connect-art" aria-hidden="true" />
      <form
        className="connect-card"
        onSubmit={(event) => {
          event.preventDefault();
          onConnect({ url: url.trim().replace(/\/+$/, ""), token: token.trim() }, remember);
        }}
      >
        <div className="connect-brand">
          <Logo size={56} />
          <div>
            <h1>Grafeno Studio</h1>
            <p className="muted">The graph engine for agent memory.</p>
          </div>
        </div>
        <label className="field">
          Server URL
          <input value={url} onChange={(e) => setUrl(e.target.value)} placeholder="http://127.0.0.1:7810" required />
        </label>
        <label className="field">
          Token <span className="hint">if the server was started with --token</span>
          <input type="password" value={token} onChange={(e) => setToken(e.target.value)} autoComplete="off" />
        </label>
        <label className="check small">
          <input type="checkbox" checked={remember} onChange={(e) => setRemember(e.target.checked)} />
          Remember on this browser
        </label>
        {error && <div className="error">{error}</div>}
        <button className="primary big" type="submit">Connect</button>
      </form>
    </div>
  );
}

function NamespaceMenu() {
  const { namespace, namespaces, setNamespace, createNamespace, toast } = useStudio();
  const [open, setOpen] = useState(false);
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [problem, setProblem] = useState<{ message: string; existing?: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => !box.current?.contains(event.target as Node) && setOpen(false);
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);
  const reset = () => {
    setCreating(false);
    setName("");
    setDescription("");
    setProblem(null);
  };
  const choose = (value: string) => {
    setOpen(false);
    reset();
    if (value !== namespace) {
      setNamespace(value);
      toast(`Switched to namespace “${value}”.`, "info");
    }
  };
  const create = async () => {
    const value = name.trim();
    if (!value) return;
    if (namespaces.some((info) => info.name === value)) {
      setProblem({ message: `“${value}” already exists.`, existing: value });
      return;
    }
    setBusy(true);
    try {
      await createNamespace(value, description.trim() || undefined);
      setOpen(false);
      reset();
      toast(`Created namespace “${value}” and switched to it.`, "ok");
    } catch (error) {
      const message = describeError(error);
      setProblem(message.includes("already exists") ? { message: `“${value}” already exists.`, existing: value } : { message });
    } finally {
      setBusy(false);
    }
  };
  const known = namespaces.some((info) => info.name === namespace);
  return (
    <div className="ns-menu" ref={box}>
      <button className="ns-button" onClick={() => setOpen((value) => !value)} title="Switch or create a namespace">
        <span className="ns-dot" />
        <span className="muted small">namespace</span>
        <strong>{namespace}</strong>
        {!known && <span className="muted small">(not created)</span>}
        <Icon name="chevronDown" size={14} />
      </button>
      {open && (
        <div className="menu">
          <div className="muted small menu-note">Switch to</div>
          {namespaces.length === 0 && <div className="muted small menu-note">No namespaces yet: create the first one.</div>}
          {namespaces.map((info) => (
            <button key={info.name} className={info.name === namespace ? "menu-item active" : "menu-item"} onClick={() => choose(info.name)} title={info.description}>
              <span className="grow ellipsis">{info.name}</span>
              <span className="muted small mono">{info.nodes === 0 ? "empty" : `${info.nodes} nodes`}</span>
              {info.name === namespace && <Icon name="check" size={14} />}
            </button>
          ))}
          {creating ? (
            <form
              className="menu-form stack tight"
              onSubmit={(event) => {
                event.preventDefault();
                void create();
              }}
            >
              <input
                autoFocus
                value={name}
                onChange={(e) => {
                  setName(e.target.value);
                  setProblem(null);
                }}
                placeholder="Name of the new namespace"
                maxLength={128}
              />
              <input value={description} onChange={(e) => setDescription(e.target.value)} placeholder="Description (optional)" />
              {problem && (
                <div className="error small">
                  {problem.message}{" "}
                  {problem.existing && (
                    <button type="button" className="link-button" onClick={() => choose(problem.existing!)}>
                      Switch to it
                    </button>
                  )}
                </div>
              )}
              <div className="row">
                <button type="button" className="ghost" onClick={reset}>
                  Cancel
                </button>
                <div className="spacer" />
                <button className="primary" type="submit" disabled={busy || !name.trim()}>
                  Create
                </button>
              </div>
            </form>
          ) : (
            <button className="menu-item" onClick={() => setCreating(true)}>
              <Icon name="plus" size={14} /> New namespace…
            </button>
          )}
        </div>
      )}
    </div>
  );
}

function NewMenu() {
  const { newNode, newEdge } = useStudio();
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => !box.current?.contains(event.target as Node) && setOpen(false);
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);
  const item = (icon: IconName, title: string, hint: string, action: () => void) => (
    <button
      className="menu-item rich"
      onClick={() => {
        setOpen(false);
        action();
      }}
    >
      <Icon name={icon} />
      <span>
        {title}
        <span className="muted small">{hint}</span>
      </span>
    </button>
  );
  return (
    <div className="ns-menu" ref={box}>
      <button className="primary" onClick={() => setOpen((value) => !value)}>
        <Icon name="plus" /> New
      </button>
      {open && (
        <div className="menu right">
          {item("node", "Node", "A person, note, project, memory…", () => newNode({ onCreated: (id) => go("explorer", id) }))}
          {item("link", "Fact", "A relation between two nodes", () => newEdge())}
          {item("scripts", "Script", "Write GQ statements", () => go("scripts"))}
        </div>
      )}
    </div>
  );
}

/** Ctrl+K: jump to a view, run an action or open a node. */
function CommandPalette({ onClose }: { onClose: () => void }) {
  const { newNode, newEdge, namespaces, setNamespace } = useStudio();
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const { results, searching } = useNodeSearch(query, 6);
  const text = query.trim().toLowerCase();
  const commands = [
    ...VIEWS.map((view) => ({ key: `v-${view.id}`, icon: view.icon, title: `Go to ${view.title}`, hint: view.hint, run: () => go(view.id) })),
    { key: "new-node", icon: "node" as IconName, title: "New node", hint: "Create a node", run: () => newNode({ onCreated: (id) => go("explorer", id) }) },
    { key: "new-edge", icon: "link" as IconName, title: "New fact", hint: "Connect two nodes", run: () => newEdge() },
    ...namespaces.map(({ name }) => ({ key: `ns-${name}`, icon: "folder" as IconName, title: `Namespace: ${name}`, hint: "Switch namespace", run: () => setNamespace(name) })),
  ].filter((command) => !text || command.title.toLowerCase().includes(text) || command.hint.toLowerCase().includes(text));
  const nodes = results.map((node) => ({
    key: `n-${node.id}`,
    icon: "node" as IconName,
    title: displayName(node),
    hint: node.labels.join(", "),
    color: labelColor(node.labels[0] ?? ""),
    run: () => go("explorer", node.id),
  }));
  const items = [...commands.slice(0, text ? 8 : 12), ...nodes];
  const pick = (index: number) => {
    const item = items[index];
    if (!item) return;
    onClose();
    item.run();
  };
  return (
    <div className="backdrop top" onMouseDown={(event) => event.target === event.currentTarget && onClose()}>
      <div className="palette" role="dialog" aria-label="Command palette">
        <div className="palette-input">
          <Icon name="search" />
          <input
            autoFocus
            value={query}
            placeholder="Go to a view, create something, or find a node…"
            onChange={(event) => {
              setQuery(event.target.value);
              setActive(0);
            }}
            onKeyDown={(event) => {
              if (event.key === "Escape") onClose();
              else if (event.key === "ArrowDown") {
                event.preventDefault();
                setActive((i) => Math.min(i + 1, items.length - 1));
              } else if (event.key === "ArrowUp") {
                event.preventDefault();
                setActive((i) => Math.max(i - 1, 0));
              } else if (event.key === "Enter") pick(active);
            }}
          />
          <kbd>Esc</kbd>
        </div>
        <div className="palette-list">
          {items.length === 0 && <div className="muted pad small">{searching ? "Searching…" : "Nothing matches."}</div>}
          {items.map((item, index) => (
            <button
              key={item.key}
              className={index === active ? "palette-item active" : "palette-item"}
              onMouseEnter={() => setActive(index)}
              onClick={() => pick(index)}
            >
              {"color" in item ? <span className="dot" style={{ background: item.color as string }} /> : <Icon name={item.icon} />}
              <span className="grow-text">{item.title}</span>
              <span className="muted small">{item.hint}</span>
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}

interface Toast {
  id: number;
  message: string;
  kind: "ok" | "error" | "info";
}

export function App() {
  const [connection, setConnection] = useState<Connection | null>(null);
  const [version, setVersion] = useState<string>("");
  const [error, setError] = useState<string | null>(null);
  const [checking, setChecking] = useState(true);
  const [route, setRoute] = useState(parseHash);
  const [namespaces, setNamespaces] = useState<NamespaceInfo[]>([]);
  const [namespace, setNamespaceState] = useState(() => readSetting("grafeno.studio.namespace", ""));
  const [schema, setSchema] = useState<Studio["schema"]>({ labels: {}, edgeLabels: {} });
  const [revision, setRevision] = useState(0);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [confirming, setConfirming] = useState<(ConfirmOptions & { resolve: (ok: boolean) => void }) | null>(null);
  const [nodeForm, setNodeForm] = useState<NewNodeOptions | null>(null);
  const [edgeForm, setEdgeForm] = useState<NewEdgeOptions | null>(null);
  const [palette, setPalette] = useState(false);
  const [collapsed, setCollapsed] = useState(() => readSetting("grafeno.studio.collapsed", "") === "1");
  const [theme, setTheme] = useState<Theme>(() => readSetting("grafeno.studio.theme", "dark") as Theme);

  useEffect(() => {
    // Dark is the default look; "system" follows the system's choice.
    document.documentElement.dataset.theme = theme;
    writeSetting("grafeno.studio.theme", theme);
  }, [theme]);

  // The database file open in the desktop app, when the Studio runs there.
  const [local, setLocal] = useState<DesktopSession | null>(null);

  const toast = useCallback((message: string, kind: Toast["kind"] = "info") => {
    const id = Date.now() + Math.random();
    setToasts((list) => [...list.slice(-3), { id, message, kind }]);
    window.setTimeout(() => setToasts((list) => list.filter((item) => item.id !== id)), kind === "error" ? 7000 : 3500);
  }, []);

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

  // Try a saved connection, then the server that served the Studio. In the
  // desktop app: the database the app has open, or its start screen.
  useEffect(() => {
    if (isDesktop) {
      void desktop
        .session()
        .then((session) => {
          if (!session) {
            setChecking(false);
            return;
          }
          setLocal(session);
          return connect({ url: session.url, token: "" });
        })
        .catch((problem) => {
          setError(describeError(problem));
          setChecking(false);
        });
      return;
    }
    const saved = loadConnection();
    void connect(saved ?? { url: defaultUrl(), token: "" }, saved ? undefined : false).then(() => {
      if (!saved) setError(null);
    });
  }, [connect]);

  const openLocal = useCallback(
    async (path: string) => {
      setChecking(true);
      try {
        const session = await desktop.open(path);
        setLocal(session);
        await connect({ url: session.url, token: "" });
      } catch (problem) {
        // La apertura fallida conserva la sesión anterior en el motor y la UI.
        const message = describeError(problem);
        setError(message);
        toast(message, "error");
        setChecking(false);
      }
    },
    [connect, toast],
  );

  const closeLocal = useCallback(async () => {
    await desktop.close();
    setLocal(null);
    setConnection(null);
  }, []);

  useEffect(() => {
    const onHash = () => setRoute(parseHash());
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setPalette((open) => !open);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const client = useMemo(() => (connection ? clientFor(connection) : null), [connection]);
  const current = namespace || "default";

  useEffect(() => {
    if (!client) return;
    client
      .namespaces()
      .then((list) => {
        setNamespaces(list);
        // A remembered namespace that was dropped falls back to the first one.
        setNamespaceState((value) => (list.some((info) => info.name === value) ? value : list[0]?.name || value || "default"));
      })
      .catch((problem) => toast(describeError(problem), "error"));
  }, [client, revision, toast]);

  useEffect(() => {
    if (!client) return;
    client
      .namespace(current)
      .stats()
      .then((stats) => setSchema({ labels: stats.node_labels, edgeLabels: stats.edge_labels }))
      .catch(() => setSchema({ labels: {}, edgeLabels: {} }));
  }, [client, current, revision]);

  const setNamespace = useCallback((name: string) => {
    setNamespaceState(name);
    writeSetting("grafeno.studio.namespace", name);
  }, []);

  const changed = useCallback(() => setRevision((value) => value + 1), []);

  // The desktop app's menu: its items arrive as events.
  const menuActions = useRef<(id: string) => void>(() => undefined);
  menuActions.current = (id: string) => {
    const day = new Date().toISOString().slice(0, 10);
    const pick = async (create: boolean) => {
      const path = await desktop.pickDatabase(create);
      if (path) await openLocal(path);
    };
    if (id === "new" || id === "open") void pick(id === "new");
    else if (id === "close" || id === "connect") {
      if (local) void closeLocal();
      else {
        forgetConnection();
        setConnection(null);
      }
    } else if (id === "palette") setPalette(true);
    else if (id.startsWith("theme:")) setTheme(id.slice("theme:".length) as Theme);
    else if (id.startsWith("view:")) go(id.slice("view:".length));
    else if (id === "export" && client) {
      void client
        .export()
        .then((text) => saveText(`grafeno-all-${day}.jsonl`, text, "application/x-ndjson"))
        .then((saved) => saved && toast("Exported every namespace.", "ok"))
        .catch((problem) => toast(describeError(problem), "error"));
    } else if (id === "import" && client) {
      void desktop
        .pickOpen(["jsonl", "ndjson", "json"])
        .then(async (path) => {
          if (!path) return;
          const stats = await client.import(await desktop.readText(path));
          toast(`Imported ${stats.nodes} nodes and ${stats.edges} facts in ${stats.namespaces} namespaces.`, "ok");
          changed();
        })
        .catch((problem) => toast(describeError(problem), "error"));
    }
  };
  useEffect(() => {
    if (!isDesktop) return;
    const stopMenu = desktop.listen<string>("menu", (id) => menuActions.current(id));
    const stopOpen = desktop.listen<string>("open-path", (path) => void openLocal(path));
    return () => {
      stopMenu();
      stopOpen();
    };
  }, [openLocal]);
  const createNamespace = useCallback(
    async (name: string, description?: string) => {
      if (!client) return;
      const info = await client.createNamespace(name, description);
      setNamespaces((list) => [...list, info].sort((a, b) => (a.name < b.name ? -1 : 1)));
      setNamespace(info.name);
      changed();
    },
    [client, setNamespace, changed],
  );
  const confirm = useCallback((options: ConfirmOptions) => new Promise<boolean>((resolve) => setConfirming({ ...options, resolve })), []);

  if (checking && !connection) {
    return (
      <div className="connect">
        <div className="connect-art" aria-hidden="true" />
        <div className="loading">
          <Logo size={48} />
          <span className="muted">Connecting…</span>
        </div>
      </div>
    );
  }
  if ((!connection || !client) && isDesktop) {
    return <StartView error={error} busy={checking} saved={loadConnection()} onOpen={(path) => void openLocal(path)} onConnect={(candidate, remember) => void connect(candidate, remember)} />;
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

  const studio: Studio = {
    client,
    connection,
    namespace: current,
    setNamespace,
    namespaces,
    createNamespace,
    schema,
    revision,
    changed,
    toast,
    confirm,
    newNode: (options = {}) => setNodeForm(options),
    newEdge: (options = {}) => setEdgeForm(options),
  };
  const view = route.view;
  const layout = view === "graph" ? "main canvas-view" : view === "scripts" || view === "explorer" ? "main full" : "main";
  const closeConfirm = (ok: boolean) => {
    confirming?.resolve(ok);
    setConfirming(null);
  };

  return (
    <StudioContext.Provider value={studio}>
      <div className={collapsed ? "app collapsed" : "app"}>
        <nav className="sidebar">
          <a className="sidebar-brand" href="#/overview">
            <Logo size={30} />
            <span className="brand-text">
              Grafeno <span>Studio</span>
            </span>
          </a>
          <div className="nav">
            {VIEWS.map((item) => (
              <a key={item.id} href={`#/${item.id}`} className={item.id === view ? "nav-item active" : "nav-item"} title={collapsed ? item.title : item.hint}>
                <Icon name={item.icon} size={18} />
                <span>{item.title}</span>
              </a>
            ))}
          </div>
          <div className="sidebar-foot">
            <div className="theme-switch" role="group" aria-label="Theme">
              {(["dark", "light", "system"] as Theme[]).map((option) => (
                <button key={option} className={theme === option ? "active" : ""} onClick={() => setTheme(option)} title={`${option} theme`}>
                  <Icon name={option === "light" ? "sun" : option === "dark" ? "moon" : "monitor"} size={14} />
                </button>
              ))}
            </div>
            <button
              className="ghost nav-item collapse-button"
              onClick={() => {
                setCollapsed((value) => !value);
                writeSetting("grafeno.studio.collapsed", collapsed ? "0" : "1");
              }}
              title={collapsed ? "Expand the sidebar" : "Collapse the sidebar"}
            >
              <Icon name={collapsed ? "chevronRight" : "chevronLeft"} size={18} />
              <span>Collapse</span>
            </button>
          </div>
        </nav>
        <header className="topbar">
          <NamespaceMenu />
          <button className="search-trigger" onClick={() => setPalette(true)}>
            <Icon name="search" />
            <span>Search or jump to…</span>
            <kbd>Ctrl K</kbd>
          </button>
          <div className="spacer" />
          <NewMenu />
          <span className="server-pill" title={local ? local.path : connection.url}>
            <span className="live-dot" />
            {local ? local.name : connection.url.replace(/^https?:\/\//, "")} · v{version}
          </span>
          <button
            className="ghost icon-button"
            title={local ? "Close the database" : "Disconnect"}
            onClick={() => {
              if (local) {
                void closeLocal();
                return;
              }
              forgetConnection();
              setConnection(null);
            }}
          >
            <Icon name="logout" />
          </button>
        </header>
        <main className={layout} key={`${current}-${view}`}>
          {view === "overview" && <OverviewView />}
          {view === "explorer" && <ExplorerView selected={route.param} />}
          {view === "graph" && <GraphView seed={route.param} />}
          {view === "scripts" && <ScriptsView />}
          {view === "search" && <SearchView />}
          {view === "memory" && <MemoryView />}
          {view === "admin" && <AdminView />}
        </main>
      </div>

      {nodeForm && <NodeForm options={nodeForm} onClose={() => setNodeForm(null)} />}
      {edgeForm && <EdgeForm options={edgeForm} onClose={() => setEdgeForm(null)} />}
      {palette && <CommandPalette onClose={() => setPalette(false)} />}
      {confirming && (
        <Modal
          title={confirming.title}
          onClose={() => closeConfirm(false)}
          footer={
            <>
              <div className="spacer" />
              <button onClick={() => closeConfirm(false)}>Cancel</button>
              <button className={confirming.danger ? "danger solid" : "primary"} autoFocus onClick={() => closeConfirm(true)}>
                {confirming.confirm ?? "Confirm"}
              </button>
            </>
          }
        >
          {confirming.message && <p className="muted">{confirming.message}</p>}
        </Modal>
      )}
      <div className="toasts" aria-live="polite">
        {toasts.map((item) => (
          <div key={item.id} className={`toast ${item.kind}`} onClick={() => setToasts((list) => list.filter((t) => t.id !== item.id))}>
            <Icon name={item.kind === "error" ? "x" : item.kind === "ok" ? "check" : "command"} size={15} />
            {item.message}
          </div>
        ))}
      </div>
    </StudioContext.Provider>
  );
}
