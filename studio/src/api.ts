// The connection to a Grafeno server, and small helpers shared by the views.

import { Grafeno, GrafenoError, type Node } from "../../clients/typescript/src/index.ts";
import { desktopFetch, isDesktop } from "./desktop.ts";

export type { Edge, NamespaceInfo, Node, NodeWithText, QueryResult } from "../../clients/typescript/src/index.ts";
export { Grafeno, GrafenoError };

export interface Connection {
  url: string;
  /** A token or API key, or a session's access token. */
  token: string;
  /** A signed-in user's refresh token and name (14.1). */
  refresh?: string;
  user?: string;
}

/** Keeps a session's new tokens wherever the connection was kept. */
function keepTokens(url: string, tokens: { access_token: string; refresh_token: string } | null): void {
  for (const storage of storages()) {
    try {
      const saved = storage.getItem(STORAGE_KEY);
      if (!saved) continue;
      const connection = JSON.parse(saved) as Connection;
      if (connection.url !== url || !connection.refresh) continue;
      if (tokens) storage.setItem(STORAGE_KEY, JSON.stringify({ ...connection, token: tokens.access_token, refresh: tokens.refresh_token }));
      else storage.removeItem(STORAGE_KEY);
    } catch {
      // Not kept: the session still works for this page.
    }
  }
}

const STORAGE_KEY = "grafeno.studio.connection";

function storages(): Storage[] {
  const found: Storage[] = [];
  for (const name of ["sessionStorage", "localStorage"] as const) {
    try {
      found.push(window[name]);
    } catch {
      // Storage can be blocked; the connection is then asked for each time.
    }
  }
  return found;
}

export function loadConnection(): Connection | null {
  for (const storage of storages()) {
    try {
      const saved = storage.getItem(STORAGE_KEY);
      if (saved) return JSON.parse(saved) as Connection;
    } catch {
      // Ignore unreadable or invalid saved connections.
    }
  }
  return null;
}

/** Keeps the connection for this tab, or for every visit with `remember`. */
export function saveConnection(connection: Connection, remember: boolean): void {
  const [session, local] = storages();
  try {
    session?.setItem(STORAGE_KEY, JSON.stringify(connection));
    if (remember) local?.setItem(STORAGE_KEY, JSON.stringify(connection));
    else local?.removeItem(STORAGE_KEY);
  } catch {
    // Not saved: the connection still works for this page.
  }
}

export function forgetConnection(): void {
  for (const storage of storages()) {
    try {
      storage.removeItem(STORAGE_KEY);
    } catch {
      // Nothing to forget.
    }
  }
}

/** The server that served the Studio, when it was `grafeno serve`. */
export function defaultUrl(): string {
  return window.location.origin;
}

export function clientFor(connection: Connection): Grafeno {
  // In the desktop app, requests go through the app (no CORS, and the
  // open database answers in-process).
  return new Grafeno({
    url: connection.url,
    token: connection.refresh ? undefined : connection.token || undefined,
    session: connection.refresh ? { access_token: connection.token, refresh_token: connection.refresh } : undefined,
    onSession: connection.refresh ? (tokens) => keepTokens(connection.url, tokens) : undefined,
    fetch: isDesktop ? desktopFetch : undefined,
  });
}

/** Checks that the server answers and accepts the token. */
export async function probe(connection: Connection): Promise<{ version: string }> {
  const client = clientFor(connection);
  const health = await client.health();
  await client.namespaces();
  return { version: health.version };
}

export function describeError(error: unknown): string {
  if (error instanceof GrafenoError) {
    if (error.status === 401 && (error.code === "session_expired" || error.message.includes("session"))) return "The session ended: sign in again.";
    if (error.status === 401) return error.message || "The server needs a valid token.";
    if (error.status === 0) return `Cannot reach the server: ${error.message}`;
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/** A short name: the `name`, `title` or `label` property, or label and id. */
export function displayName(node: Pick<Node, "id" | "labels" | "props">): string {
  for (const key of ["name", "title", "label"]) {
    const value = node.props?.[key];
    if (typeof value === "string" && value) return value;
  }
  return `${node.labels[0] ?? "Node"} ${node.id.slice(-8)}`;
}

const PALETTE = ["#14b8a6", "#6366f1", "#f59e0b", "#ec4899", "#22c55e", "#0ea5e9", "#a855f7", "#ef4444", "#84cc16", "#f97316"];

/** A stable color per label. */
export function labelColor(label: string): string {
  let hash = 0;
  for (const char of label) hash = (hash * 31 + char.charCodeAt(0)) >>> 0;
  return PALETTE[hash % PALETTE.length];
}

export function formatTime(micros: number | null | undefined): string {
  if (micros === null || micros === undefined) return "—";
  return new Date(micros / 1000).toISOString().replace(".000Z", "Z");
}

/** Navigates within the Studio (`#/view/param`). */
export function go(view: string, param?: string): void {
  window.location.hash = param ? `#/${view}/${encodeURIComponent(param)}` : `#/${view}`;
}
