/**
 * TypeScript client for the Grafeno HTTP API (`grafeno serve`).
 *
 * Field names follow the API as is (snake_case). Times are microseconds since
 * the Unix epoch in responses; requests also accept ISO 8601 strings.
 *
 * ```ts
 * const grafeno = new Grafeno({ url: "http://127.0.0.1:7810", token: process.env.GRAFENO_TOKEN });
 * const vault = grafeno.namespace("vault-123");
 * const { id } = await vault.createNode({ labels: ["Note"], props: { title: "Rust" }, text: "…" });
 * const context = await vault.context({ text: "borrow checker", budget: 800 });
 * ```
 */

export type Id = string;
/** Microseconds since the Unix epoch, or an ISO 8601 date or date-time. */
export type TimeInput = number | string;
export type Props = Record<string, unknown>;
export type Direction = "out" | "in" | "both";

export interface Node {
  id: Id;
  labels: string[];
  props: Props;
  sources: Id[];
  created_at: number;
  updated_at: number;
}

export interface NodeWithText extends Node {
  /** The indexed text, when the node has one. */
  text?: string;
}

export interface Edge {
  id: Id;
  label: string;
  from: Id;
  to: Id;
  props: Props;
  sources: Id[];
  valid_from: number | null;
  valid_to: number | null;
  recorded_at: number;
  expired_at: number | null;
}

export interface NodeInput {
  /** Only for upserts: your own id. */
  id?: Id;
  labels: string[];
  props?: Props;
  sources?: Id[];
  /** Indexed for full-text search. */
  text?: string;
  /** Indexed for vector search; one dimension per namespace. */
  embedding?: number[];
}

/** `text` or `embedding` set to `null` removes them. */
export interface NodePatch {
  /** Merged into the current properties; `null` values remove properties. */
  props?: Props;
  add_labels?: string[];
  remove_labels?: string[];
  text?: string | null;
  embedding?: number[] | null;
}

export interface EdgeInput {
  /** A node id, or in a batch a `$name` given earlier. */
  from: string;
  label: string;
  to: string;
  props?: Props;
  sources?: Id[];
  valid_from?: TimeInput;
  valid_to?: TimeInput;
}

export interface EdgeFilter {
  labels?: string[];
  valid_at?: TimeInput;
  known_at?: TimeInput;
}

export interface Neighbor {
  node: Id;
  edge: Edge;
}

export interface Path {
  nodes: Id[];
  edges: Id[];
}

export interface Subgraph {
  nodes: Node[];
  edges: Edge[];
}

export interface Hit {
  node: Id;
  score: number;
}

export interface Expansion {
  depth?: number;
  direction?: Direction;
  filter?: EdgeFilter;
  decay?: number;
  seeds?: number;
}

export interface RecallInput {
  text?: string;
  embedding?: number[];
  limit?: number;
  labels?: string[];
  candidates?: number;
  /** `true` (default) expands with default options, `false` disables it. */
  expand?: boolean | Expansion;
}

export type Signal =
  | { kind: "text"; rank: number; score: number }
  | { kind: "vector"; rank: number; similarity: number }
  | { kind: "graph"; seed: Id; hops: number };

export interface Recalled {
  node: Node;
  score: number;
  signals: Signal[];
}

export interface ContextInput extends RecallInput {
  budget?: number;
  fact_share?: number;
  max_facts?: number;
  facts?: EdgeFilter;
}

export interface ContextItem {
  node: Id;
  name: string;
  labels: string[];
  text: string;
  score: number;
  sources: Id[];
  truncated: boolean;
}

export interface ContextFact {
  edge: Id;
  from: Id;
  label: string;
  to: Id;
  text: string;
}

export interface Context {
  items: ContextItem[];
  facts: ContextFact[];
  tokens: number;
  /** Ready to paste into a prompt. */
  rendered: string;
}

export type BatchOp =
  | { op: "create_node"; ref?: string; node: NodeInput }
  | { op: "upsert_node"; ref?: string; node: NodeInput & { id: Id } }
  | { op: "update_node"; id: string; patch: NodePatch }
  | { op: "delete_node"; id: string }
  | { op: "create_edge"; edge: EdgeInput }
  | { op: "delete_edge"; id: Id }
  | { op: "delete_edges_from"; from: string; label?: string }
  | { op: "invalidate_edge"; id: Id; at: TimeInput }
  | { op: "retract_edge"; id: Id };

export interface BatchResult {
  results: Record<string, unknown>[];
  /** The ids given to each `$name`. */
  refs: Record<string, Id>;
}

export interface GrafenoOptions {
  /** Base URL, such as `http://127.0.0.1:7810`. */
  url: string;
  /** Bearer token, if the server was started with one. */
  token?: string;
  /** A custom `fetch`, for tests or other runtimes. */
  fetch?: typeof fetch;
}

/** An error returned by the server (or a network failure, with status 0). */
export class GrafenoError extends Error {
  readonly status: number;
  readonly code: string;

  constructor(status: number, code: string, message: string) {
    super(message);
    this.name = "GrafenoError";
    this.status = status;
    this.code = code;
  }
}

export class Grafeno {
  readonly #url: string;
  readonly #token: string | undefined;
  readonly #fetch: typeof fetch;

  constructor(options: GrafenoOptions) {
    this.#url = options.url.replace(/\/+$/, "");
    this.#token = options.token;
    this.#fetch = options.fetch ?? fetch;
  }

  /** Operations on one namespace (a user, organization or agent). */
  namespace(name: string): Namespace {
    return new Namespace(this, name);
  }

  health(): Promise<{ status: string; version: string }> {
    return this.request("GET", "/health");
  }

  /** Sends a request and parses the JSON response. */
  async request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const headers: Record<string, string> = {};
    if (body !== undefined) headers["content-type"] = "application/json";
    if (this.#token) headers.authorization = `Bearer ${this.#token}`;
    let response: Response;
    try {
      response = await this.#fetch(this.#url + path, {
        method,
        headers,
        body: body === undefined ? undefined : JSON.stringify(body),
      });
    } catch (error) {
      throw new GrafenoError(0, "network", `request to Grafeno failed: ${String(error)}`);
    }
    const text = await response.text();
    const data: unknown = text ? JSON.parse(text) : undefined;
    if (!response.ok) {
      const error = (data as { error?: { code?: string; message?: string } } | undefined)?.error;
      throw new GrafenoError(response.status, error?.code ?? "http", error?.message ?? response.statusText);
    }
    return data as T;
  }
}

export class Namespace {
  readonly #client: Grafeno;
  readonly #base: string;
  readonly name: string;

  constructor(client: Grafeno, name: string) {
    this.#client = client;
    this.name = name;
    this.#base = `/v1/${encodeURIComponent(name)}`;
  }

  #call<T>(method: string, path: string, body?: unknown): Promise<T> {
    return this.#client.request<T>(method, this.#base + path, body);
  }

  // Nodes

  /** Creates a node, or upserts it when `input.id` is set. */
  createNode(input: NodeInput): Promise<{ id: Id; created: boolean }> {
    return this.#call("POST", "/nodes", input);
  }

  /** Creates the node with this id or replaces it entirely (edges are kept). */
  putNode(id: Id, input: Omit<NodeInput, "id">): Promise<{ id: Id; created: boolean }> {
    return this.#call("PUT", `/nodes/${encodeURIComponent(id)}`, input);
  }

  getNode(id: Id): Promise<NodeWithText> {
    return this.#call("GET", `/nodes/${encodeURIComponent(id)}`);
  }

  /** Every node of the namespace, or only those with `label`. */
  listNodes(label?: string): Promise<Node[]> {
    const query = label === undefined ? "" : `?label=${encodeURIComponent(label)}`;
    return this.#call("GET", `/nodes${query}`);
  }

  patchNode(id: Id, patch: NodePatch): Promise<NodeWithText> {
    return this.#call("PATCH", `/nodes/${encodeURIComponent(id)}`, patch);
  }

  /** Deletes the node, its edges, text and embedding. */
  async deleteNode(id: Id): Promise<void> {
    await this.#call("DELETE", `/nodes/${encodeURIComponent(id)}`);
  }

  neighbors(
    id: Id,
    options: { direction?: Direction; labels?: string[]; validAt?: TimeInput; knownAt?: TimeInput } = {},
  ): Promise<Neighbor[]> {
    const params = new URLSearchParams();
    if (options.direction) params.set("direction", options.direction);
    if (options.labels?.length) params.set("label", options.labels.join(","));
    if (options.validAt !== undefined) params.set("valid_at", String(options.validAt));
    if (options.knownAt !== undefined) params.set("known_at", String(options.knownAt));
    const query = params.size ? `?${params}` : "";
    return this.#call("GET", `/nodes/${encodeURIComponent(id)}/neighbors${query}`);
  }

  // Edges

  createEdge(input: EdgeInput): Promise<{ id: Id }> {
    return this.#call("POST", "/edges", input);
  }

  getEdge(id: Id): Promise<Edge> {
    return this.#call("GET", `/edges/${encodeURIComponent(id)}`);
  }

  async deleteEdge(id: Id): Promise<void> {
    await this.#call("DELETE", `/edges/${encodeURIComponent(id)}`);
  }

  /** The fact stopped being true at `at`; its history is kept. */
  invalidateEdge(id: Id, at: TimeInput): Promise<Edge> {
    return this.#call("POST", `/edges/${encodeURIComponent(id)}/invalidate`, { at });
  }

  /** The fact was a mistake; it stays visible to `known_at` queries. */
  retractEdge(id: Id): Promise<Edge> {
    return this.#call("POST", `/edges/${encodeURIComponent(id)}/retract`);
  }

  // Traversals

  async kHop(
    start: Id,
    options: { depth?: number; direction?: Direction; filter?: EdgeFilter } = {},
  ): Promise<{ node: Id; hops: number }[]> {
    return this.#call("POST", "/k-hop", { start, ...options });
  }

  async path(
    from: Id,
    to: Id,
    options: { max_depth?: number; direction?: Direction; filter?: EdgeFilter } = {},
  ): Promise<Path | null> {
    const result = await this.#call<{ path: Path | null }>("POST", "/path", { from, to, ...options });
    return result.path;
  }

  subgraph(
    seeds: Id[],
    options: { depth?: number; direction?: Direction; filter?: EdgeFilter } = {},
  ): Promise<Subgraph> {
    return this.#call("POST", "/subgraph", { seeds, ...options });
  }

  // Retrieval

  searchText(query: string, limit = 10): Promise<Hit[]> {
    return this.#call("POST", "/search/text", { query, limit });
  }

  searchVector(embedding: number[], limit = 10, ef?: number): Promise<Hit[]> {
    return this.#call("POST", "/search/vector", { embedding, limit, ef });
  }

  /** Hybrid recall: full text and vectors fused, then graph expansion. */
  recall(input: RecallInput): Promise<Recalled[]> {
    return this.#call("POST", "/recall", input);
  }

  /** Recalled nodes and facts packed into a token budget. */
  context(input: ContextInput): Promise<Context> {
    return this.#call("POST", "/context", input);
  }

  /** Applies every operation in one transaction: all of them or none. */
  batch(ops: BatchOp[]): Promise<BatchResult> {
    return this.#call("POST", "/batch", { ops });
  }
}
