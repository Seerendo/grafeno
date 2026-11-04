// The desktop application (Tauri): when the Studio runs in its window,
// `window.__TAURI__` is there, and these helpers reach the app's commands.
// In a browser they are absent and the Studio works as before.

interface TauriGlobal {
  core: { invoke: <T>(command: string, args?: Record<string, unknown>) => Promise<T> };
  event: { listen: <T>(event: string, handler: (event: { payload: T }) => void) => Promise<() => void> };
}

const tauri = (window as unknown as { __TAURI__?: TauriGlobal }).__TAURI__;

/** Whether the Studio runs inside the Grafeno desktop app. */
export const isDesktop = tauri !== undefined;

/** The address of the database opened in the app (never leaves the process). */
export const LOCAL_URL = "http://grafeno.local";

export interface DesktopSession {
  url: string;
  path: string;
  name: string;
}

export interface RecentDatabase {
  path: string;
  name: string;
  exists: boolean;
}

function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!tauri) return Promise.reject(new Error("not in the desktop app"));
  return tauri.core.invoke<T>(command, args);
}

export const desktop = {
  session: () => invoke<DesktopSession | null>("session"),
  open: (path: string) => invoke<DesktopSession>("open_database", { path }),
  close: () => invoke<void>("close_database"),
  recent: () => invoke<RecentDatabase[]>("recent"),
  forgetRecent: (path: string) => invoke<void>("forget_recent", { path }),
  pickDatabase: (create: boolean) => invoke<string | null>("pick_database", { create }),
  pickSave: (name: string, extension: string) => invoke<string | null>("pick_save", { name, extension }),
  pickOpen: (extensions: string[]) => invoke<string | null>("pick_open", { extensions }),
  writeText: (path: string, contents: string) => invoke<void>("write_text", { path, contents }),
  readText: (path: string) => invoke<string>("read_text", { path }),
  openExternal: (url: string) => invoke<void>("open_external", { url }),
  backupTo: (path: string) => invoke<number>("backup_database", { path }),
  listen: <T>(event: string, handler: (payload: T) => void): (() => void) => {
    let stop: (() => void) | null = null;
    let stopped = false;
    void tauri?.event.listen<T>(event, (message) => handler(message.payload)).then((unlisten) => {
      if (stopped) unlisten();
      else stop = unlisten;
    });
    return () => {
      stopped = true;
      stop?.();
    };
  },
};

interface Reply {
  status: number;
  headers: Record<string, string>;
  body: string;
}

/**
 * `fetch` through the app: requests to the open database run in-process,
 * others go out from the app itself, so no server needs CORS for it.
 */
export const desktopFetch: typeof fetch = async (input, init) => {
  const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
  const headers: Record<string, string> = {};
  new Headers(init?.headers).forEach((value, name) => {
    headers[name] = value;
  });
  const body = init?.body == null ? null : typeof init.body === "string" ? init.body : await new Response(init.body).text();
  let reply: Reply;
  try {
    reply = await invoke<Reply>("request", { method: init?.method ?? "GET", url, headers, body });
  } catch (error) {
    throw new TypeError(String(error));
  }
  const empty = reply.status === 204 || reply.status === 205 || reply.status === 304;
  return new Response(empty ? null : reply.body, { status: reply.status, headers: reply.headers });
};

/** Saves text to a file the user picks: a dialog in the app, a download in a browser. */
export async function saveText(name: string, text: string, type: string): Promise<boolean> {
  if (isDesktop) {
    const extension = name.includes(".") ? name.slice(name.lastIndexOf(".") + 1) : "txt";
    const path = await desktop.pickSave(name, extension);
    if (!path) return false;
    await desktop.writeText(path, text);
    return true;
  }
  const url = URL.createObjectURL(new Blob([text], { type }));
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  link.click();
  URL.revokeObjectURL(url);
  return true;
}

// Web links open in the system browser, not in the app's window.
if (isDesktop) {
  document.addEventListener("click", (event) => {
    const link = event.target instanceof Element ? event.target.closest("a[href]") : null;
    const href = link?.getAttribute("href") ?? "";
    if (/^https?:\/\//.test(href)) {
      event.preventDefault();
      void desktop.openExternal(href);
    }
  });
}
