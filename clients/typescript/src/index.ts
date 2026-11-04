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
  /** The API key (agent) that created it, when known. */
  recorded_by?: string;
}

/** How much a node matters and how often it is used, for ranking and decay. */
export interface MemoryState {
  /** From 0 to 1; 0.5 by default. */
  importance: number;
  accesses: number;
  /** Microseconds since the epoch; the last update until the first access. */
  last_access: number;
}

export interface NodeWithText extends Node {
  /** The indexed text, when the node has one. */
  text?: string;
  memory: MemoryState;
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
  /** The API key (agent) that recorded it, when known. */
  recorded_by?: string;
}

export type Role = "read" | "write" | "admin";

/** A role on a namespace, or on every name starting with a prefix (`team-*`, `*`). */
export interface Grant {
  namespace: string;
  role: Role;
}

/** An API key, as the server shows it (never its secret). */
export interface AccessKey {
  name: string;
  grants: Grant[];
  /** Every right on every namespace, and managing keys. */
  owner: boolean;
  /** May change only nodes and facts it recorded itself. */
  own_only: boolean;
  description?: string;
  /** Microseconds since the epoch; absent for the server's own token. */
  created_at?: number;
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
  /** From 0 to 1. */
  importance?: number;
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
  /** Also return archived memories. */
  include_archived?: boolean;
  /** `true` (default) weighs importance and recency, `false` ranks by relevance alone. */
  ranking?: boolean | Ranking;
  /** Record an access to each returned node. */
  touch?: boolean;
}

export interface Ranking {
  importance_weight?: number;
  recency_weight?: number;
  half_life_days?: number;
}

export type Signal =
  | { kind: "text"; rank: number; score: number }
  | { kind: "vector"; rank: number; similarity: number }
  | { kind: "graph"; seed: Id; hops: number }
  | { kind: "memory"; importance: number; recency: number };

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

/** A page of a listing; pass `next` as `after` to get the following one. */
export interface Page<T> {
  items: T[];
  next: Id | null;
}

export interface ListOptions {
  label?: string;
  after?: Id;
  /** 1 to 1000; default 100. */
  limit?: number;
}

/** A namespace: created explicitly or by the first write, listed even when empty. */
export interface NamespaceInfo {
  name: string;
  nodes: number;
  edges: number;
  /** Microseconds since the epoch. */
  created_at: number;
  description?: string;
}

export interface NamespaceStats {
  name: string;
  nodes: number;
  edges: number;
  node_labels: Record<string, number>;
  edge_labels: Record<string, number>;
  texts: number;
  text_terms: number;
  vectors: number;
  vector_dimension: number | null;
}

export interface CheckReport {
  namespace: string;
  nodes: number;
  edges: number;
  texts: number;
  vectors: number;
  dangling_links: number;
  /** Empty when the namespace is consistent. */
  problems: string[];
}

export interface RebuildReport {
  namespace: string;
  nodes: number;
  edges: number;
  texts: number;
  vectors: number;
  dropped_edges: number;
  dropped_texts: number;
  dropped_vectors: number;
}

export interface DumpStats {
  namespaces: number;
  nodes: number;
  edges: number;
}

/** A graph algorithm the server can run (`GET /v1/algorithms`). */
export interface AlgorithmInfo {
  name: string;
  arguments: string;
  columns: string[];
  description: string;
}

/** How to run a graph algorithm over HTTP. */
export interface AlgorithmOptions {
  /** Arguments before the settings: node ids, `k`. */
  args?: unknown[];
  /** Projection and algorithm settings: `labels`, `edgeLabels`, `weight`, `direction`, … */
  settings?: Record<string, unknown>;
  /** Store the result in this node property (or `@embedding`) instead of returning rows. */
  write?: string;
  /** With `write`, the column to store (the one after `node` by default). */
  column?: string;
}

/** What a writing query changed. */
export interface WriteStats {
  nodes_created: number;
  nodes_deleted: number;
  edges_created: number;
  edges_deleted: number;
  edges_retracted: number;
  properties_set: number;
  labels_added: number;
  labels_removed: number;
}

/** The result of a GQ query. Nodes and edges come as their records, times as ISO strings. */
export interface PlanStep {
  step: number;
  operation: string;
  details: string;
  /** Rows after the step. */
  rows?: number;
  /** Records read: nodes, edges, adjacency and index entries. */
  reads?: number;
  time_ms?: number;
}

export interface QueryResult {
  columns: string[];
  rows: unknown[][];
  stats: WriteStats;
  /** What each step did, for statements that start with PROFILE. */
  profile?: PlanStep[];
}

export interface BatchResult {
  results: Record<string, unknown>[];
  /** The ids given to each `$name`. */
  refs: Record<string, Id>;
}

export interface Duplicate {
  a: Node;
  b: Node;
  /** From 0 to 1; 1 is the same normalized name. */
  score: number;
  reasons: string[];
}

export interface MergeReport {
  kept: Id;
  merged: Id;
  edges_moved: number;
  edges_dropped: number;
}

export interface WeakMemory {
  node: Node;
  /** importance × recency × use, from 0 to 1. */
  strength: number;
  memory: MemoryState;
}

export interface ConsolidationGroup {
  /** The entity the memories are about. */
  entity: Node;
  memories: Node[];
}

export interface SummaryInput {
  text: string;
  /** The memories summarized. */
  sources: Id[];
  /** The entity they are about. */
  about?: Id;
  name?: string;
  /** Default: `["Summary"]`. */
  labels?: string[];
  embedding?: number[];
  /** Archive the sources (default true). */
  archive_sources?: boolean;
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
    // Browsers refuse `fetch` called as a method of another object
    // ("Illegal invocation"), so the global one is bound to `globalThis`.
    this.#fetch = options.fetch ?? fetch.bind(globalThis);
  }

  /** Operations on one namespace (a user, organization or agent). */
  namespace(name: string): Namespace {
    return new Namespace(this, name);
  }

  health(): Promise<{ status: string; version: string }> {
    return this.request("GET", "/health");
  }

  /** Every namespace that has nodes, in name order. */
  namespaces(): Promise<NamespaceInfo[]> {
    return this.request("GET", "/v1/namespaces");
  }

  /** Creates an empty namespace; fails with status 409 (`conflict`) if it exists. */
  createNamespace(name: string, description?: string): Promise<NamespaceInfo> {
    return this.request("POST", "/v1/namespaces", { name, description });
  }

  /** One namespace; fails with status 404 if it does not exist. */
  getNamespace(name: string): Promise<NamespaceInfo> {
    return this.request("GET", `/v1/namespaces/${encodeURIComponent(name)}`);
  }

  /** Sets the description of a namespace (`null` removes it). */
  describeNamespace(name: string, description: string | null): Promise<NamespaceInfo> {
    return this.request("PATCH", `/v1/namespaces/${encodeURIComponent(name)}`, { description });
  }

  /** Deletes a namespace and everything in it. It cannot be undone. */
  dropNamespace(name: string): Promise<{ nodes: number; edges: number }> {
    return this.request("DELETE", `/v1/namespaces/${encodeURIComponent(name)}`);
  }

  /** Who this client is: the owner token, or an API key with its grants. */
  whoami(): Promise<AccessKey> {
    return this.request("GET", "/v1/whoami");
  }

  /** The API keys (owners only). */
  keys(): Promise<AccessKey[]> {
    return this.request("GET", "/v1/keys");
  }

  /** Creates an API key; its secret is returned only here. */
  createKey(key: { name: string; grants?: Grant[]; owner?: boolean; own_only?: boolean; description?: string }): Promise<{
    key: AccessKey;
    secret: string;
  }> {
    return this.request("POST", "/v1/keys", key);
  }

  /** Revokes an API key: its secret stops working at once. */
  async revokeKey(name: string): Promise<void> {
    await this.request("DELETE", `/v1/keys/${encodeURIComponent(name)}`);
  }

  /** The graph algorithms `CALL algo.*` and `algorithm()` can run. */
  algorithms(): Promise<AlgorithmInfo[]> {
    return this.request("GET", "/v1/algorithms");
  }

  /** A JSON Lines export of `namespaces` (all of them by default). */
  /** A consistent copy of the whole database file, taken while the server keeps serving (owners only). */
  async backup(): Promise<ArrayBuffer> {
    return this.#send("GET", "/v1/backup").then((response) => response.arrayBuffer());
  }

  async export(namespaces: string[] = []): Promise<string> {
    const query = namespaces.length ? `?ns=${encodeURIComponent(namespaces.join(","))}` : "";
    return this.#send("GET", `/v1/export${query}`).then((response) => response.text());
  }

  /** Imports a JSON Lines export in one transaction: all of it or nothing. */
  /**
   * Imports an export. `bulk` loads it in chunks: faster for large exports,
   * but an error keeps the chunks before it.
   */
  async import(dump: string, options: { bulk?: boolean } = {}): Promise<DumpStats> {
    const path = options.bulk ? "/v1/import?bulk=true" : "/v1/import";
    const response = await this.#send("POST", path, dump, "application/x-ndjson");
    return (await response.json()) as DumpStats;
  }

  /** Sends a request and parses the JSON response. */
  async request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const response = await this.#send(
      method,
      path,
      body === undefined ? undefined : JSON.stringify(body),
      "application/json",
    );
    const text = await response.text();
    return (text ? JSON.parse(text) : undefined) as T;
  }

  /** Sends a request; throws `GrafenoError` unless the response is a success. */
  async #send(method: string, path: string, body?: string, contentType?: string): Promise<Response> {
    const headers: Record<string, string> = {};
    if (body !== undefined && contentType) headers["content-type"] = contentType;
    if (this.#token) headers.authorization = `Bearer ${this.#token}`;
    let response: Response;
    try {
      response = await this.#fetch(this.#url + path, { method, headers, body });
    } catch (error) {
      throw new GrafenoError(0, "network", `request to Grafeno failed: ${String(error)}`);
    }
    if (!response.ok) {
      const text = await response.text();
      let error: { code?: string; message?: string } | undefined;
      try {
        error = (JSON.parse(text) as { error?: { code?: string; message?: string } }).error;
      } catch {
        error = undefined;
      }
      throw new GrafenoError(response.status, error?.code ?? "http", error?.message ?? (text || response.statusText));
    }
    return response;
  }
}

function listQuery(options: ListOptions): string {
  const params = new URLSearchParams();
  if (options.label !== undefined) params.set("label", options.label);
  if (options.after !== undefined) params.set("after", options.after);
  if (options.limit !== undefined) params.set("limit", String(options.limit));
  return params.size ? `?${params}` : "";
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

  /** A page of nodes in creation order, only those with `label` if given. */
  listNodes(options: ListOptions = {}): Promise<Page<Node>> {
    return this.#call("GET", `/nodes${listQuery(options)}`);
  }

  /** A page of edges (retracted ones included), only those with `label` if given. */
  listEdges(options: ListOptions = {}): Promise<Page<Edge>> {
    return this.#call("GET", `/edges${listQuery(options)}`);
  }

  /** Every node, following the pages. */
  async *allNodes(options: Omit<ListOptions, "after"> = {}): AsyncGenerator<Node> {
    let after: Id | undefined;
    do {
      const page = await this.listNodes({ ...options, after });
      yield* page.items;
      after = page.next ?? undefined;
    } while (after);
  }

  stats(): Promise<NamespaceStats> {
    return this.#call("GET", "/stats");
  }

  /** Checks that indexes and counters agree with the records. */
  check(): Promise<CheckReport> {
    return this.#call("GET", "/check");
  }

  /** Rebuilds the namespace's indexes and counters from its records. */
  rebuild(): Promise<RebuildReport> {
    return this.#call("POST", "/rebuild");
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

  /**
   * Runs one GQ statement (Cypher-like), such as
   * `MATCH (p:Person)-[:works_at]->(c) RETURN p.name, c.name`. Statements
   * with writes run in one transaction.
   */
  query(query: string, params: Record<string, unknown> = {}): Promise<QueryResult> {
    return this.#call("POST", "/query", { query, params });
  }

  /**
   * Runs a graph algorithm (`pageRank`, `louvain`, `shortestPath`, …). Rows hold
   * nodes as `{id, labels, name}`; with `write`, one row with the nodes written.
   */
  algorithm(name: string, options: AlgorithmOptions = {}): Promise<QueryResult> {
    return this.#call("POST", `/algo/${encodeURIComponent(name)}`, options);
  }

  /** Applies every operation in one transaction: all of them or none. */
  batch(ops: BatchOp[]): Promise<BatchResult> {
    return this.#call("POST", "/batch", { ops });
  }

  // Memory

  /** Nodes with this name or alias, ignoring case, accents and punctuation. */
  resolve(name: string, label?: string): Promise<Node[]> {
    return this.#call("GET", `/resolve${queryString({ name, label })}`);
  }

  /** Likely duplicates, best first: by name, and by meaning when `vectors` is not false. */
  duplicates(options: { label?: string; min_score?: number; vectors?: boolean; limit?: number } = {}): Promise<Duplicate[]> {
    return this.#call("GET", `/duplicates${queryString(options)}`);
  }

  /** Folds `merge` into `keep`: edges move, labels and missing properties join, `merge` is deleted. */
  merge(keep: Id, merge: Id): Promise<MergeReport> {
    return this.#call("POST", "/merge", { keep, merge });
  }

  /** Records an access to each node now. */
  async touch(ids: Id[]): Promise<void> {
    await this.#call("POST", "/touch", { ids });
  }

  /** Memories whose strength fell below the threshold, weakest first. */
  weak(options: { half_life_days?: number; threshold?: number; label?: string; limit?: number } = {}): Promise<WeakMemory[]> {
    return this.#call("GET", `/weak${queryString(options)}`);
  }

  /** Adds the `Archived` label, which recall skips. Returns how many were archived. */
  async archive(ids: Id[]): Promise<number> {
    const result = await this.#call<{ archived: number }>("POST", "/archive", { ids });
    return result.archived;
  }

  /** Groups of older memories about one entity, ready to be summarized. */
  consolidation(
    options: { older_than_days?: number; min_size?: number; limit?: number; labels?: string[] } = {},
  ): Promise<ConsolidationGroup[]> {
    const { labels, ...rest } = options;
    return this.#call("GET", `/consolidation${queryString({ ...rest, labels: labels?.join(",") })}`);
  }

  /** Stores a summary of several memories and (by default) archives them. */
  summarize(input: SummaryInput): Promise<{ id: Id }> {
    return this.#call("POST", "/summaries", input);
  }
}

function queryString(values: Record<string, string | number | boolean | undefined>): string {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(values)) {
    if (value !== undefined) params.set(key, String(value));
  }
  return params.size ? `?${params}` : "";
}
