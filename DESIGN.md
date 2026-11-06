# Grafeno design

Grafeno is a graph database engine written in Rust, built as **memory and a
knowledge base for agents**. Its first client is
[Grafito](https://github.com/Seerendo/grafito), but it does not depend on it:
any agent should be able to use it embedded (like SQLite), as a server, or over
MCP.

## Goals

1. **Property graph**: nodes with one or more labels, typed directed edges,
   JSON properties on both.
2. **Two time axes** on edges (facts):
   - *valid time* (`valid_from` / `valid_to`): when the fact is true in the world;
   - *record time* (`recorded_at` / `expired_at`): when the engine learned it
     and when it was retracted.
   Invalidating or retracting does not delete: history stays queryable.
3. **Provenance**: nodes and edges keep `sources`, the nodes they came from
   (episode, document, chunk, conversation). This allows citing and inheriting
   permissions.
4. **Namespaces**: every piece of data lives in a namespace (user,
   organization, agent). No query crosses namespaces unless asked to.
5. **Hybrid retrieval**: vectors (HNSW) + full text (BM25) + graph expansion,
   fused with RRF, and output that fits a token budget.
6. **Memory primitives** (phase 7): episodes, entities, facts, importance and
   decay, duplicate merging, consolidation. LLM extraction stays outside the
   core (a plugin or the client), so the engine is not tied to a provider.
7. **Interfaces**: Rust API, HTTP/gRPC server, MCP server, Node (napi-rs) and
   Python (PyO3) bindings.

### Out of scope (for now)

- A custom storage engine (pager, WAL): an embedded KV store is used.
- Full Cypher or GQL: GQ covers patterns, filters, aggregates and writes;
  `OPTIONAL MATCH`, `MERGE`, `WITH` and the rest come when needed.
- An external coordinator (etcd, ZooKeeper) or a distributed storage layer:
  replication and failover (phase 13) live in the `grafeno` binary itself.
- Splitting one namespace across servers: phase 13 replicates whole
  databases and, at most, places whole namespaces on different groups.

## Architecture

```
grafeno/
├── crates/
│   ├── grafeno-core      model (Node, Edge, ids, time), errors            [phase 1]
│   ├── grafeno           redb storage, transactions, traversals,          [phases 1-2]
│   │                     full-text index (BM25), vector index (HNSW)
│   ├── grafeno-memory    hybrid recall, token-budgeted context            [phase 2]
│   ├── grafeno-query     GQ: lexer, parser, executor                      [phase 5]
│   └── grafeno-server    `grafeno` binary: HTTP API, MCP over stdio,      [phases 3-5]
│                         shell, administration commands
├── clients/
│   └── typescript        `grafeno-client`, fetch-based HTTP client         [phase 3]
├── studio/               Grafeno Studio, the web interface                 [phase 6]
└── packaging/            Windows installer, .deb/.rpm, systemd unit        [phase 6]
```

The indexes live in the `grafeno` crate, not in a separate one, because they
are written in the same redb transaction as the graph: a node, its text and its
embedding are always consistent, and a failed write leaves no stale entries.

### Storage

[redb](https://github.com/cberner/redb) 4: pure Rust, ACID, one file, MVCC with
one writer and concurrent readers, and an in-memory backend for tests and
short-lived agents.

| Table          | Key                           | Value                      |
| -------------- | ----------------------------- | -------------------------- |
| `nodes`        | `(ns, node_id)`               | `Node` record              |
| `edges`        | `(ns, edge_id)`               | `Edge` record              |
| `adjacency_out` | `(ns, from, label, edge_id)` | `to` + the edge's times, compact (layout 13) |
| `adjacency_in` | `(ns, to, label, edge_id)`    | `from` + the edge's times  |
| `node_labels`  | `(ns, label, node_id)`        | `()`                       |
| `texts`        | `(ns, node_id)`               | indexed text               |
| `text_lengths` | `(ns, node_id)`               | number of terms            |
| `text_postings` | `(ns, term, node_id)`        | term frequency, text length |
| `text_stats`   | `ns`                          | `(documents, total terms)` |
| `vectors`      | `(ns, node_id)`               | normalized `f32` vector    |
| `vectors_q8`   | `(ns, node_id)`               | the same as int8 (layout 13), for searches |
| `hnsw_links`   | `(ns, node_id, layer)`        | neighbor ids               |
| `vector_meta`  | `ns`                          | dimension, count, entry    |
| `meta`         | key, such as `schema_version` | `u64`                      |
| `namespace_counts` | `ns`                      | `(nodes, edges)`           |
| `label_counts` | `(ns, label)`                 | nodes with the label       |
| `edge_label_counts` | `(ns, label)`            | edges with the label       |
| `names`        | `(ns, normalized name, node_id)` | `()`                    |
| `memory`       | `(ns, node_id)`               | `(importance, accesses, last_access)` |
| `indexes`      | `(ns, index name)`            | index definition (JSON)    |
| `index_entries` | `(ns, index, sortable value, node_id)` | `()`              |
| `namespaces`   | `ns`                          | creation time, description (JSON) |
| `principals`   | key name                      | secret hash, grants, owner, `own_only` (JSON) |
| `merges`       | `(ns, merge_id)`              | merge record: who, when, both nodes before, edges moved and dropped |
| `match_rules`  | `(ns, label)`                 | match rule: strong keys, fields with measures and weights, thresholds (JSON) |
| `changes`      | `(ns, seq)`                   | the change log: what a commit changed (a node or edge, created, updated or deleted, its labels, which parts) |
| `constraints`  | `(ns, name)`                  | a constraint on facts (one per source, target or pair) or a functional property (JSON) |
| `state`        | `(ns, kind, key)`             | bytes kept by features on the engine (extraction's text hashes, review queue and decisions) |

- The layout has a version (`SCHEMA_VERSION`, now 12) in `meta`. Opening an
  older file upgrades it in one transaction: version 1 gets its counters
  computed, version 2 gets the new adjacency and posting tables (built from
  the records) and loses the old ones, version 3 gets the names index,
  version 4 the (empty) property index tables, version 5 the namespace
  registry (every namespace with data), version 6 the (empty) principals,
  version 7 the (empty) merge records, and version 8 the (empty) match
  rules and the possibility of fuzzy indexes (whose trigram entries an
  older Grafeno would read as values), version 9 the (empty) state
  table, version 10 the (empty) constraints, and version 11 the (empty)
  change log, which starts with the first commit after. A newer layout is
  refused.
- Adjacency entries hold the edge's record time and valid time
  (`recorded_at`, `expired_at`, `valid_from`, `valid_to`), so traversals
  filter without reading edge records. Postings hold the length of their
  text, so BM25 needs no extra lookup.
- Counters are updated in the same transaction as each write, so listing
  namespaces and their statistics never scans records. A namespace is a
  record of its own: created explicitly or by the first write to it, it
  stays (empty) until it is dropped, which deletes everything in it with
  one range deletion per table.

- Ids are UUID v7 (time-ordered), generated by Grafeno itself and stored as
  `u128`.
- Adjacency lists keyed by `(node, label)` make "neighbors of X through
  `mentions`" one contiguous range.
- Records are serialized as JSON for now. Moving to a binary format is an
  internal change with no effect on the API.

### Time semantics

- `EdgeFilter::any()` (the default) returns **current knowledge**: edges that
  were not retracted, regardless of their valid time.
- `valid_at(t)`: only facts true at `t` (`valid_from <= t < valid_to`).
- `known_at(t)`: what the engine knew at `t` (`recorded_at <= t < expired_at`).
- `invalidate_edge(t)`: the fact stopped being true at `t` (the world changed).
- `retract_edge()`: the fact was a mistake (what we know changed).

### Full-text index

- Tokenizer: Unicode NFD, accents and case removed ("Canción" → "cancion"),
  split on non-alphanumeric characters, one-character terms and English and
  Spanish stopwords dropped. No stemming yet.
- Ranking: BM25 (`k1 = 1.2`, `b = 0.75`).
- Removing a text re-tokenizes the stored copy to find its postings, so a
  tokenizer change requires rebuilding the index.

A hand-written index was chosen over tantivy: it shares the graph's file and
transactions, and it brings no C dependencies. Tantivy remains an option if
richer linguistics (stemming per language, phrase queries) become necessary.

### Vector index

- HNSW (Malkov & Yashunin): `M = 16` (32 on layer 0), `ef_construction = 100`,
  neighbor selection with the paper's diversity heuristic.
- Cosine similarity: vectors are normalized on insert.
- One graph and one fixed dimension per namespace, set by its first vector.
- A node's level comes from the random bits of its id, so it is never stored
  and the entry point can always be recomputed after a delete.
- Deleting a node reconnects each of its neighbors using the removed node's
  links as candidates; links that still point to removed nodes are skipped at
  search time.
- Searches compute distances straight from the stored bytes (no decoding,
  eight-lane dot products). Inserts and removes in one write transaction
  share a cache of decoded vectors and link lists that every write updates
  and that ends with the transaction; a rebuild drops it for its namespace.
- Embeddings are sent by the client. A local model (ort or candle) may be
  offered later for clients without one.

### Hybrid recall and context (`grafeno-memory`)

```
query ─┬─ text ──────▶ BM25 ───┐
       └─ embedding ─▶ HNSW ───┴─▶ RRF ─▶ graph expansion ─▶ label filter ─▶ packing by tokens
```

- `recall`: reciprocal rank fusion (`k = 60`) of both result lists. The best
  `seeds` results then pass `decay^hops` of their score to nodes up to `depth`
  hops away, so nodes without text or embedding (people, companies, tags) are
  found through what mentions them. Every result says why it was recalled
  (`Signal::Text`, `Signal::Vector`, `Signal::Graph`).
- `context_for`: packs recalled nodes, best first, into a token budget; the
  last one that does not fit is truncated. A share of the budget is kept for
  facts: edges between recalled nodes first, then edges that touch one, with
  their valid time. Tokens are estimated (4 characters per token) unless the
  client passes its model's tokenizer.

### Server (phase 3)

One binary, `grafeno`, with two modes. The endpoints, options and MCP setup
are documented in [crates/grafeno-server/README.md](crates/grafeno-server/README.md).

- **HTTP** (`grafeno serve`, axum): JSON under `/v1/{namespace}/…`, an
  optional static bearer token, and engine calls on tokio's blocking pool
  (redb is synchronous). `POST /batch` applies many operations in one
  transaction, with `$name` references between them, so a client can sync a
  note, its chunks and its links atomically. Clients may choose their own
  node ids (`PUT /nodes/{id}`, `upsert_node`), which makes syncing idempotent.
- **MCP** (`grafeno mcp`): newline-delimited JSON-RPC over stdio, written by
  hand (a few hundred lines; no SDK dependency). Seven tools shaped for
  language models: `remember`, `recall`, `context`, `relate`, `update`,
  `forget` and `get_node`. Tool results use names and ISO dates rather than
  raw records, and unnamed memories are named by the start of their text.
- **Docker**: a two-stage image (Debian trixie), port 7810, data in `/data`.

A database file is locked by the process that opens it, so the HTTP and MCP
modes cannot share a file. An MCP mode that forwards to an HTTP server
(`grafeno mcp --url`) would remove that limit.

### Studio and distribution (phase 6)

- **Grafeno Studio** (`studio/`): React, Vite and TypeScript, with
  its own canvas graph engine on d3-force (phase 9.3) and the TypeScript
  client for every call.
  Views: overview, explorer (create and edit nodes, labels, typed
  properties, text, importance and facts in place; bulk archive and
  delete), graph (create nodes, connect them by clicking, inspect and end
  facts, "valid at" time travel), scripts (a SQL-editor-style GQ workspace
  on CodeMirror: tabs kept in the browser, highlighting and completion, run
  at cursor or all statements, errors marked on their line, results as a
  table, JSON or graph), search, memory and administration, plus a command
  palette and light and dark themes. Scripts are split into statements in
  the browser; each runs as its own `POST /query`, so each is its own
  transaction.
- **Logo**: a fragment of graphene, three fused six-carbon rings
  (`docs/assets/logo.svg`, `studio/public/favicon.svg`). The Windows icon
  (`packaging/windows/grafeno.ico`, rendered from the favicon) is embedded
  in `grafeno.exe` by `build.rs` and used by the installer.
- The build is embedded in the binary (`include_dir`, when `studio/dist`
  exists at compile time; otherwise `/studio/` explains how to build it) and
  served at `/studio/` without authentication; the API it calls still needs
  the token, which the Studio asks for. It uses relative paths and hash
  routing, so the same build can be hosted elsewhere with `--cors-origin`.
- **Settings file** (`grafeno.toml`): `db`, `addr`, `token`, `studio`,
  `cors_origins`. `grafeno config init` writes it with a random token; the
  installers call it. `grafeno serve --config` and `grafeno shell --config`
  read it; flags and environment variables take precedence.
- **Services**: on Windows, `grafeno service install|uninstall` registers
  the `Grafeno` service with the Service Control Manager (`windows-service`;
  stops on the SCM's stop request, logs to `grafeno.log`). On Linux, a
  systemd unit runs `grafeno serve --config` as the `grafeno` user, with
  sandboxing (`systemd-analyze security`: 1.3); the server stops gracefully
  on SIGTERM.
- **Packages**: a static musl binary for every Linux distribution, packed
  as `.tar.gz`, `.deb` (`cargo-deb`) and `.rpm` (`cargo-generate-rpm`) by
  `packaging/linux/Dockerfile`; an Inno Setup installer for Windows
  (`packaging/windows/grafeno.iss`); `install.sh` downloads a release and
  checks its SHA-256. `.github/workflows/release.yml` builds them on tags.
- Uninstalling never deletes data or settings.

### Query language and shell (phase 5)

- **GQ** (`grafeno-query`): a hand-written lexer and recursive-descent
  parser (errors carry line and column), and an executor on top of the
  engine API. A pattern starts from its most selective node (a bound
  variable, then `@id`, then labels with properties, then labels) and
  extends both ways through `neighbor_ids`, which reads adjacency entries
  only; edge and node records are read when the pattern needs them.
  `VALID AT` / `KNOWN AT` become the traversal's `EdgeFilter`.
- Writing statements take the write lock, then a read snapshot (which is
  then the latest state), match on the snapshot and write through the
  transaction; any error aborts all of it.
- Grafeno-specific parts: pseudo-properties (`@id`, `@text`, `@embedding`,
  `@valid_from`, …), `RETRACT`, and `SET r.@valid_to` to invalidate a fact.
- **Shell** (`grafeno shell`): a line editor (rustyline) with history,
  statements ending with `;`, psql-style meta-commands, table or JSON
  output, and script mode. The same code drives a file (through the
  library) or a server (through the HTTP API, with `ureq`).
- GQ is also the HTTP API's `POST /v1/{ns}/query` and the MCP `query`
  tool, which is read-only so agents write through the dedicated tools.

### Administration and validation (phase 4)

- **Introspection**: `namespaces()`, `stats(ns)` (counts per label, texts,
  vectors and their dimension), and paged listings of nodes and edges
  (`after` + `limit`, in id order).
- **Integrity**: `check(ns)` compares every derived structure (label index,
  adjacency, postings, text lengths and statistics, vector metadata, HNSW
  links, counters) with the records, and verifies that every vector can be
  reached from the entry point. `rebuild_indexes(ns)` rebuilds them all from
  the records (node, edge, text and vector records are the source of truth).
- **Export and import**: JSON Lines with a header, then each namespace's
  nodes (with text and embedding) and edges, keeping ids and timestamps.
  Import runs in one transaction and replaces records with the same ids.
- All of this is available from the library, the HTTP API
  (`/v1/namespaces`, `/v1/{ns}/stats`, `/check`, `/rebuild`, `/v1/export`,
  `/v1/import`) and the CLI (`grafeno namespaces | stats | check | rebuild |
  export | import`), which opens the file directly, like `pg_dump`.
- **Tests**: a property test against an in-memory model, a kill-the-writer
  durability test, a concurrency test, corruption tests, benchmarks and a
  recall measurement. Results and bottlenecks: [docs/benchmarks.md](docs/benchmarks.md).

Node bindings (napi-rs) were left out: Grafito runs Grafeno as a sidecar
container and talks HTTP, so a native addon would add per-platform builds
without a current user. They remain possible for embedded use.

### Memory (phase 7)

- **Names**: the `names` index holds each node's `name`, `title` and
  `label` properties and its `aliases` (a list of strings), normalized:
  lowercase, without accents or punctuation, spaces collapsed. "Ana García",
  "ana  garcia" and "ANA GARCÍA" are the same key. `find_by_name` and
  `GET /v1/{ns}/resolve` look it up; MCP `remember` with a `name` uses it to
  update an existing entity (with a label in common) instead of creating a
  duplicate.
- **Importance and accesses**: each node has a memory state, importance
  from 0 to 1 (0.5 by default), accesses and the last access (the last update
  until the first access). Only states that differ from the default are
  stored. Recall touches what it returns when asked (`touch`; MCP always
  does).
- **Ranking**: recall multiplies each fused score by `1 + 0.5 × importance +
  0.3 × recency`, where recency halves every 30 days since the last access
  (all configurable, or off). The boost is applied to a pool of
  `max(5 × limit, 100)` candidates, so it can reorder results but not invent
  them.
- **Decay**: strength = importance × recency × (1 + ln(1 + accesses) / 4),
  capped at 1. `weak_memories` lists nodes below a threshold (0.1); nothing
  is deleted automatically. `archive` adds the `Archived` label, which recall
  leaves out unless `include_archived`.
- **Duplicates**: pairs with the same label and a similar normalized name (1
  for equal names, at least 0.7 when one contains the other, word Jaccard
  otherwise, typos counted at 0.8 of a word), the same value of a strong key
  (1), or a cosine similarity of at least 0.92 between embeddings. Pairs
  joined by a `distinct_from` edge, or that the identity rules rule out,
  are not proposed (see 10.5).
- **Merging**: `merge_nodes(keep, merge)` moves the edges to the kept node
  (dropping edges between the two and identical ones), joins labels and
  sources, keeps the kept node's properties and adds the missing ones, turns
  the other names into `aliases`, joins the texts, keeps one embedding,
  combines memory states (highest importance, sum of accesses) and deletes
  the merged node, all in one transaction. It refuses nodes that disagree
  unless forced, and records itself so that it can be undone (see 10.5).
- **Consolidation**: `consolidation_groups` finds entities with at least 3
  memories (`Memory`, `Episode`, `Note`) older than 7 days pointing at them.
  The summary is written by the agent (the engine calls no model):
  `write_summary` stores a `Summary` node with `summarizes` edges to its
  sources and an `about` edge, gives it the highest importance of its
  sources and archives them. Provenance is kept: the sources stay, archived.
- Exposed through the library (`grafeno` and `grafeno-memory::maintenance`),
  HTTP (`/resolve`, `/duplicates`, `/merge`, `/touch`, `/weak`, `/archive`,
  `/consolidation`, `/summaries`, `importance` in `PATCH /nodes/{id}`), MCP
  (`merge`, `maintain`, `consolidate`, `importance` in `remember` and
  `update`), GQ (`n.@importance`, `n.@accesses`, `n.@last_access`, `SET
  n.@importance`) and the Studio's Memory view. Export and import keep memory
  states.

## Integration with Grafito

- When a note syncs, one batch: upsert the `Note` node with Grafito's id,
  replace its chunks and its edges for `[[links]]` (`links_to`) and `#tags`
  (`tagged`). Namespace = the user's vault.
- `search_notes` and the agent loop use `recall` / `context_for`.
- The assistant keeps its own memory per user (preferences, facts), with
  provenance pointing to the conversation.
- `GraphView` can ask the engine for a `subgraph` instead of computing the
  graph in the client.
- Deployment: a `grafeno` container in `docker-compose.yml` (sidecar).

## Phases

1. **Core** (done): model, storage, transactions, traversals (neighbors,
   k hops, shortest path, subgraph).
2. **Retrieval** (done): full-text and vector indexes, hybrid recall,
   `context_for`.
3. **Server** (done): HTTP API with atomic batches, MCP server, TypeScript
   client, Docker image.
4. **Foundations and validation** (done): layout versioning, counters,
   namespaces and statistics, paging, integrity check and rebuild,
   export/import, property/durability/concurrency tests, benchmarks.
5. **Shell and query language** (done): `grafeno shell` (a console like
   `psql`, on a file or a server) and GQ, a Cypher-style language with time
   travel ([docs/query-language.md](docs/query-language.md)).
6. **Studio and installers** (done): Grafeno Studio embedded in the binary;
   Windows installer and service; `.deb`, `.rpm`, `install.sh` and a
   systemd service ([docs/install.md](docs/install.md)).
7. **Memory** (done): entity resolution by normalized names and aliases,
   importance, accesses and decay in ranking, duplicates and merging,
   archiving, consolidation into summaries.
8. **Performance, query language and analysis** (done, in five steps;
   see below).
9. **Desktop application** (done, in five steps; see below): Grafeno
   as a desktop app with its own installers, a new design and a graph you
   can handle directly.
10. **Hardening** (done, in seven steps; see below): the priority
    technical risks, measured and closed before more features build on
    them.
11. **Memory that keeps itself** (done, in five steps): entity
    resolution, incremental extraction and reconciliation of entities,
    automatic invalidation of contradicting facts, hierarchical community
    summaries, continuous ingestion and events.
12. **Compatibility and speed** (done, in five steps): wider
    Cypher and tool compatibility, query optimization under concurrency,
    batched execution, batched and parallel traversal, compact storage and
    vector quantization.
13. **Distributed operation** (planned, in six steps; see below): several
    servers holding the same data, kept in sync by a replicated log with
    Raft; a leader elected among them coordinates writes, the others take
    over when it fails, and reads and large queries spread over all of
    them.
14. **Users, roles and permissions** (planned, in six steps; see below):
    a complete role-based access control, as most databases have: users who
    sign in, roles and privileges granted with `GRANT` and `REVOKE`, rights
    down to labels, properties and records, and an audit log.
15. **Engine and interface apart** (planned, in five steps; see below):
    the engine's installers install only the engine (Windows `.exe`, Linux
    `.deb`, `.rpm` and a `curl` one-liner), as a professional installation
    in the way PostgreSQL's is, with no Docker needed; and the interface
    becomes a separate desktop product with installers of its own, talking
    to engines only through their public API.
16. **Integrations**: Grafito first, Python bindings.

### Phase 8 plan

Each step ends with tests, benchmarks where they apply, documentation and
its own commit.

**8.1 GQ completeness** (done). The executor became a pipeline of clauses
(rows flow from one clause to the next), which the remaining Cypher clauses
need. Reads in a writing statement go through its write transaction, so
later clauses (and `MERGE` on later rows) see earlier writes.

- `OPTIONAL MATCH` (missing parts are `null`), `WITH` (projection and
  aggregation between steps; `WITH … WHERE` filters aggregates, like SQL's
  `HAVING`), `UNWIND`, `UNION` and `UNION ALL`.
- `CASE` (simple and searched), list comprehensions
  (`[x IN list WHERE … | …]`), pattern predicates in `WHERE`
  (`WHERE (a)-[:knows]->(b)`, `WHERE NOT (a)--()`, `EXISTS { … }`).
- `MERGE` (match or create, with `ON CREATE SET` and `ON MATCH SET`) and
  `FOREACH` (writes per list item).
- `CALL procedure(…) YIELD …`: built-in procedures (`db.labels()`,
  `db.relationshipTypes()`, `db.propertyKeys()`, later the algorithms).
- `EXPLAIN` and `PROFILE`: the plan, and rows and time per step.
- Also: `all`, `any`, `none`, `single`, `reduce`, list slices, `^`,
  and `db.search` / `db.searchVector` procedures. Left for later:
  `CALL { … }` subqueries, pattern comprehensions, map projections.

**8.2 Statistics and time** (done). Durations live in `grafeno-query`'s
`temporal` module, on the engine's own calendar helpers (no date library).

- Aggregates: `stdev`, `stdevp`, `variance`, `median`, `percentileCont`,
  `percentileDisc`, `mode`, `count(DISTINCT …)` for all of them.
- A duration type (ISO 8601, such as `P1Y2M10D`, `PT4H`): date and time
  arithmetic (`date + duration`, `b - a`), `duration.between(a, b)`,
  `duration.inDays(a, b)`, components (`d.days`, `d.hours`).
- Date parts and truncation: `t.year`, `t.month`, `t.week`, `t.dayOfWeek`,
  `date.truncate('month', t)`, for grouping by period.
- Valid time in numbers: how long a fact held
  (`duration.between(r.@valid_from, coalesce(r.@valid_to, now()))`), and
  `CALL temporal.series(start, end, step)`, to count what was valid at each
  point of a period in one query.
- Studio: bar and line charts over script results, one shared Y axis,
  categorical colors in a fixed, colorblind-validated order (no pie charts:
  they compare close values badly).
- Also: `VALID AT` / `KNOWN AT` may use row variables, `round(x, digits)`,
  `log`, `exp`, `sign`, `pi()`, `timestamp()`.

**8.3 Property indexes and safe queries** (done). Measured in
[docs/benchmarks.md](docs/benchmarks.md): a 2% range over 100,000 nodes goes
from 233 ms to 11 ms. Values are encoded so that byte order is value order
(numbers as `f64` with flipped bits, strings as UTF-8), and lookups read
inclusive ranges that the query's own conditions then narrow. Queries read
candidates in pages, and limits are checked on every read.

- Declared indexes, as in Postgres and Neo4j:
  `CREATE INDEX FOR (n:Person) ON (n.age)`, `DROP INDEX`, `SHOW INDEXES`,
  also from the Studio. An index table keyed by `(ns, index, sortable
  value, node)` (layout version 5) serves equality, ranges,
  `STARTS WITH` and `IN`; it is kept in the write transaction and checked
  and rebuilt like the other indexes. Uniqueness constraints build on it.
- A planner: starts a pattern from its most selective part (an index, an
  id, the smallest label by its counter), pushes `WHERE` filters into the
  scan, and stops scanning as soon as `LIMIT` is met.
- Guards, so one query cannot take the service down: a time limit, a
  limit on rows read and on rows held in memory (sorts, aggregates,
  `collect`), and cancellation when the client goes away. Defaults in
  `grafeno.toml`; a query over its limits fails with a clear error and
  leaves the server serving others.

**8.4 Concurrent and bulk work** (done, except the shared read caches).
redb has one writer and any number of readers (MVCC); readers already run
in parallel. Numbers are in [docs/benchmarks.md](docs/benchmarks.md).

- **Group commit** (`grafeno::Writer`): a thread takes the writes waiting
  in its queue (up to 128), runs them in arrival order in one transaction
  and commits once. redb allows a savepoint only before a transaction
  touches a table, so a write cannot be undone alone: when one fails, the
  transaction is discarded, that write gets its error, and the others run
  again. Writes are therefore `Fn` (safe to run twice; only the last run
  counts). The server sends every HTTP write through it, GQ writes
  included (`grafeno_query::execute_in`). 16 threads of single-node
  writes on a file: 740 writes/s alone, 8,100 grouped.
- **Durability** (`durability = "eventual"` in `grafeno.toml`,
  `Grafeno::set_durability`): commits skip the disk sync; the writer
  flushes twice a second and on shutdown. A crash loses at most the last
  half second, never part of a commit.
- **Bulk loading** (`grafeno import --bulk`, `POST /v1/import?bulk=true`,
  `Grafeno::import_with`): chunks of 20,000 records with eventual
  durability and one flush, texts indexed per chunk with sorted postings
  (`WriteTx::set_texts`). About 1.3 times faster than one transaction; an
  error keeps the chunks before it. The text postings stay the main cost.
- **Backpressure**: at most `max_requests` (512) requests under `/v1` at
  once, and 4,096 writes waiting; beyond them the server answers 503
  `busy` with `Retry-After: 1`.
- **Not done: shared read caches.** Keeping decoded vectors across
  transactions needs to know, for each snapshot, whether a cached vector
  was written after it; redb does not expose snapshot versions, and the
  measured cost (about 1 ms per vector search at 10k vectors) did not
  justify a second, hand-kept version scheme. It stays in the remaining
  limits.

**8.5 Graph algorithms** (done). On disk, adjacency is already index-free: a
node's edges by label are one contiguous key range, and each entry holds
the neighbor's id and the edge's times. Algorithms that touch the whole
graph work on an in-memory projection instead: a compact adjacency array
(CSR, each node holding the positions of its neighbors) of a namespace or
part of it (labels, edge labels, `VALID AT`), built on demand and reused
while the data does not change.

- Pathfinding: shortest paths weighted by a property (Dijkstra), A*, the
  k shortest paths (Yen), all shortest paths.
- Centrality: degree, PageRank, betweenness, closeness.
- Communities: connected components (weak and strong), label propagation,
  Louvain.
- Similarity: node similarity by shared neighbors (Jaccard, overlap,
  cosine) and k nearest neighbors by embedding.
- Link prediction: common neighbors, Adamic-Adar, resource allocation,
  preferential attachment.
- Node embeddings: FastRP, whose vectors can be written back as node
  embeddings and searched with the vector index.
- Each runs as `CALL algo.<name>(…) YIELD …` in GQ, through HTTP, and from
  the Studio (color by community, size by centrality). Modes: stream the
  results, return statistics, or write them to a property.

As built: `grafeno::algo` holds the projection and the algorithms, which
work on node positions and know nothing of storage. A projection is built
with one range scan of the outgoing adjacency of the namespace (edge
records are read only for a weight property) and keeps both directions as
CSR arrays. The projection cache lives in the database: every commit bumps
a counter twice (odd while committing), and a read transaction records the
counter it saw if no commit ran while it opened, so it can reuse a
projection built for the same snapshot; they are kept up to a budget in
bytes (256 MiB by default, see 10.6). Write
transactions project their own state and never cache. In GQ, statistics
and writing need no special modes: rows are aggregated with `RETURN` or
stored with `SET`, and `POST /v1/{ns}/algo/{name}` with `write` builds that
query. Betweenness, closeness and FastRP spread over the cores; the
others run on one. Every algorithm checks the query's time limit and
cancellation between rounds.

A performance pass (layout version 3) followed phase 4: traversals 3-4×
faster, BM25 3× faster, vector inserts 2.5× faster in batches. Numbers and
the remaining limits are in [docs/benchmarks.md](docs/benchmarks.md).

### Phase 9 plan

The guidelines (2026-11-04): a desktop application with `.exe` and Linux
installers; a complete redesign with new typography and an avant-garde
look; a graph that moves (click, drag freely, interact with its nodes);
namespaces that behave predictably; and the Grafeno icon on every
executable and installer. Shell: Tauri (Rust, the engine runs inside the
app). Direction: *graph lab*. Each step ends with tests, documentation and
its own commit.

**9.1 Namespaces as things you create** (done). Before, a namespace existed only
while it holds a node: the Studio's picker "opens" a name without creating
anything, so an empty `demo` disappears from the list as soon as another
one is chosen, and choosing `demo` again cannot show whether it was
created or only switched to. Namespaces become records of their own.

- A `namespaces` table (layout version 6) with the name, creation time
  and an optional description; the upgrade registers every namespace that
  has data. Writing to an unknown namespace still registers it, so
  libraries and imports keep working; exports carry namespace records
  (export version 2), so empty namespaces survive a backup.
- `ReadTx::namespaces` lists registered namespaces, empty ones included,
  with their counts. `WriteTx::create_namespace` (a `Conflict` error if it
  exists; names without `/`, `\`, `?`, `#`, `%`, control characters or
  outer spaces, up to 128 characters), `describe_namespace` and
  `drop_namespace` (deletes everything in it, after a confirmation in
  every UI). Renaming is left out: an export imported under another name
  does it.
- HTTP `POST /v1/namespaces` (409 `conflict` if it exists), `GET`,
  `PATCH` and `DELETE /v1/namespaces/{ns}`; the shell (`\ns create`,
  `\ns drop NAME --yes`; `\use` says when a namespace does not exist
  yet); GQ `SHOW NAMESPACES`, which MCP agents reach through `query`.
- The Studio separates *switching* (a list of every namespace, the current
  one marked) from *creating* (an explicit "New namespace" with a name,
  which fails visibly if it exists), and says which happened.

**9.2 Graph lab design system** (done). A full redesign of the Studio, which the
desktop app shares.

- Dark first (with a light theme), the graph canvas filling the window
  and the tools in floating panels with blur; a command bar instead of the
  top bar; views as workspaces on the left rail.
- Typography: Space Grotesk for the interface and headings, JetBrains Mono
  for data, ids and code; both bundled (SIL OFL), so the desktop app works
  offline.
- New tokens (color, space, radius, elevation, motion), components rebuilt
  on them (buttons, inputs, menus, tables, dialogs, toasts, inspector), and
  the dataviz palette re-validated for both themes.
- Every view redone on it: overview, explorer, graph, scripts, search,
  memory, administration.

As built: one stylesheet of tokens (`studio/src/styles.css`); dark is the
default theme (`data-theme` is `dark`, `light` or `system`). The rail and
the command bar float as glass over a floor of two soft glows and a dot
grid; the graph view runs under both, its toolbar, inspector and status
floating over the canvas. Interface text in Space Grotesk, data, ids,
code, table heads and section labels in JetBrains Mono, both from
`@fontsource-variable` packages bundled in the build. Nodes get a halo in
their own color. The chart palette kept its slots: the validator passes
both themes on the new surfaces (in light, three slots stay under 3:1
against the surface, relieved by the legend and the table view).

**9.3 A graph you can handle** (done). Cytoscape's static layouts give way to a
live physics simulation (d3-force) drawn on a canvas of its own.

- Drag any node and the rest reacts; release to let it settle, or pin it
  where it is (double-click unpins). The simulation cools down when
  nothing moves and wakes up on any change.
- Hover lights a node's neighborhood; click selects and opens the
  inspector; double-click expands its neighbors, which grow out of it;
  lasso and shift-click select several nodes to move, pin, hide or delete
  together; a context menu on nodes and edges (expand, connect, pin, hide,
  delete, end a fact).
- Connect two nodes by dragging from a node's handle onto another; edges
  drawn with arrows, labels on hover, curved when parallel.
- Zoom and pan with inertia, fit, a minimap, search to fly to a node;
  labels and colors by label, community or property; sizes by centrality
  (phase 8.5); clusters can be collapsed into one node.
- Smooth at a few thousand nodes on screen (canvas, spatial index for
  hits, labels only when legible).

As built: `studio/src/graph/engine.ts` is a framework-free engine (one
canvas, d3-force for links, charge, collision and a weak pull to the
center); the graph view and the result graph of Scripts both use it, and
Cytoscape is gone. Dragging pins what was moved (a dot marks it), and the
simulation reheats while dragging and cools after. Hover and a single
selection light the node's neighborhood and dim the rest; shift-click,
ctrl-click and shift-drag (lasso) select several, which then move,
pin, hide or delete together. A selected node shows a `+` handle: dragging
it onto another node opens the new-fact form with both ends. Right-click
opens a menu for a node, several nodes, a fact or the background.
Parallel facts curve apart, self-loops loop, facts that ended are dashed,
labels are drawn in screen space and only when legible. Zoom follows the
cursor, panning keeps its momentum, fitting keeps clear of the floating
panels, and a minimap shows and moves the view. With communities on
(8.5), a community collapses into one node with its count, and facts in
and out of it merge into one edge per pair; double-click expands it.
Hit testing scans the nodes and edges under the pointer (linear, fine at
the few thousand nodes a view draws). Keys: Delete, Esc, F (frame), H
(hide), P (pin), + and -.

**9.4 Desktop application (Tauri)** (done). A new crate, `grafeno-desktop`
(`apps/desktop`), wraps the Studio in a native window.

- Local mode: the app opens a database file (create, open, recent files)
  and runs the engine inside its own process, serving the HTTP API on a
  random loopback port with a random token that only its window knows; the
  window loads the Studio from it. No service or terminal needed.
- Remote mode: connect to a Grafeno server (the installed service or
  another machine) with its URL and token; saved connections.
- Native menus and shortcuts, file dialogs for import and export, one
  instance per database, window size and position kept, links opened in
  the system browser.

As built (`apps/desktop`, see its README): the window loads the Studio
from Tauri's bundled assets, which keeps one origin (and the Studio's
saved settings and scripts) across launches. Instead of a loopback port,
the Studio's `fetch` goes through one app command, `request`: addresses
under `http://grafeno.local` reach the open database's HTTP router in the
same process (`tower::ServiceExt::oneshot`), and other addresses are sent
by the app, so remote servers need no CORS for it and nothing listens on
the machine. Opening a file holds redb's lock, so a file open in a service
or another program is refused with a clear message. One app instance
(tauri-plugin-single-instance): a second launch focuses the window and
opens its file there. Recent databases live in the app's config folder.
`grafeno-server` gained a `studio` feature (on by default) so the app
does not embed the Studio twice.

**9.5 Installers and icons** (done). One source for every icon: the graphene logo
rendered to `.ico` (16-256 px), `.png` sizes and the Linux icon theme.

- Desktop app installers from the Tauri bundler: Windows NSIS `.exe` (and
  `.msi`), Linux `.deb`, `.rpm` and `.AppImage`, each with the Grafeno
  icon on the installer, the executable, the shortcut, the task bar and the
  application menu (`.desktop` entry).
- The server packages keep their role (service, headless machines) and
  get the icon too: the setup `.exe` and `grafeno.exe` on Windows, a
  `.desktop` entry and icons in the `.deb` and `.rpm`.
- The release workflow builds both families on tags.

As built: `npm run icons` in `apps/desktop` renders `docs/assets/logo.svg`
with `tauri icon` and copies the results to `packaging/windows/grafeno.ico`
and `packaging/linux/icons/`. The app is named **Grafeno Desktop**
(package `grafeno-desktop`), so its Linux packages do not clash with the
server's `grafeno`; its NSIS setup installs for the current user without
administrator rights (the MSI is there for managed machines), and both
installers and the Linux packages register `.redb` files to open in it.
The server's `.deb` and `.rpm` add a **Grafeno Studio** menu entry
(`xdg-open` on the service's Studio) with the icon in the hicolor theme.
The release workflow gained `desktop-windows` and `desktop-linux` jobs
(Ubuntu 22.04, so the AppImage runs on older systems).

Phase 9 is complete.

### Phases 10 to 12 plan

The guidelines (2026-11-04): agent-agnostic and independent of any LLM,
local, and efficient without a GPU. Nothing here calls a model: extraction,
reconciliation, contradiction checks and summaries are rules, statistics
and graph algorithms on the CPU. An agent that has an LLM can still bring
its own results through the API (extracted entities, a summary), and they
are stored with their provenance like any other write.

The risks come first: isolation, integrity and memory correctness are what
the later features (automatic merging, automatic invalidation, continuous
ingestion) would otherwise multiply. Each step ends with tests, a
benchmark where it measures something, documentation and its own commit.

#### Phase 10: hardening

**10.1 Isolation and authorization** (done). One static token gave full
access to every namespace.

- Principals: named API keys (stored hashed), each with grants per
  namespace (or a namespace prefix such as `team-*`): `read`, `write`,
  `admin` (indexes, drop, import), plus a global `owner`. The static token
  of `grafeno.toml` stays as the owner.
- Identity in provenance: every write records the principal that made it
  (`recorded_by` on nodes and edges), queryable as `@recorded_by`.
- Provenance rules: a principal may retract, invalidate or merge only
  what it may write; an optional rule restricts that to records it made
  (agents that must not rewrite each other's memory).
- Every surface checks the same grants: HTTP, MCP, shell, GQ (a query
  only sees its namespace; procedures and algorithms too), export and
  import, the desktop app's bridge.
- Tests: a leak matrix, every endpoint and procedure called with keys of
  the wrong namespace and the wrong role, plus property tests that random
  queries never return another namespace's ids.
- Studio and `grafeno keys create | list | revoke`.

As built: principals live in a `principals` table (layout version 7) with
the SHA-256 of their secret (`gfk_` and 40 hex digits); the server keeps
them in memory by hash and reloads them when one changes. One middleware
maps every `/v1` route to what it needs (any caller, owner, or a role on
the namespace in its path) and serves the request with the caller in a
task-local, so handlers ask for more where the route alone cannot tell:
writing queries need write and index commands admin (from the parsed
statement), creating a namespace needs admin on its name, and listings,
`SHOW NAMESPACES` (through a filter in the query limits) and exports keep
to what the caller may read. Writes run as an `Actor` set on the write
transaction for each job of a group commit: the engine stamps
`recorded_by` and, for own-only actors, refuses changes to others' nodes
and edges (deleting a node checks its edges too; merging checks both nodes
and the moved edges) with `Error::Forbidden`, which GQ and HTTP report as
403. The owner token records nothing. A server without a token stays open
until its first key, which must be an owner; the Studio switches to it.
The desktop app gives each opened file a random owner token of its own.
MCP opens the file itself, so its fences (`--allow`, `--read-only`,
`--as`, `--own-only`) guard against the agent, not the user. Route names
under `/v1` are reserved as namespace names. Tests: a leak matrix over
every namespace route for a key of another namespace and a reader, owner
routes, revoked keys, own-only rules over HTTP and GQ, prefix grants.

**10.2 Data integrity under failure** (done).

- Crash tests extended to bulk imports, migrations, index rebuilds and
  namespace drops (a child process killed at random moments; every
  reported commit survives, nothing is half-done, the check passes).
- Migration fixtures: a committed database file for every layout version,
  opened and upgraded in tests, compared with a fresh build of the same
  data.
- Corruption: flipped bytes and truncated files are detected (open fails
  with a clear error, or the check names what is wrong), never a panic or
  silent wrong answers; `grafeno check --all` for every namespace.
- Online backup: `grafeno backup` and `GET /v1/backup` write a consistent
  copy from a read snapshot while writes go on; restore is opening it.

As built:
- `tests/crash.rs` kills a child process at random moments during a bulk
  import, a one-transaction import, index rebuilds, namespace drops (and
  refills) and a layout upgrade: every namespace checks clean afterwards,
  a one-transaction import is all or nothing, a dropped namespace is gone
  or whole, an interrupted upgrade upgrades on the next open. A bulk
  import commits its chunks with eventual durability, so a crash loses
  the chunks since the last flush, consistently (an *error* keeps them).
- `tests/fixtures/layout-v1..6.redb.gz` were written by the code of each
  past layout version (`tests/fixtures/make.py` builds those commits in
  git worktrees and runs a generator); `tests/upgrade.rs` opens every one
  with the current code and checks counts, text and vector search, the
  names index, valid time, traversals, importance, indexes and empty
  namespaces, then writes and reopens. Every new layout version adds its
  fixture.
- Damage: redb trusts its pages, and reading a damaged one can panic
  inside redb. `Grafeno::verify_file` runs redb's integrity check (every
  page against its checksum, repairing what it can) before a file is
  trusted; `grafeno check` runs it first. `tests/corruption.rs` damages a
  file 150 ways (flipped bytes, zeroed pages, truncation): after
  verification each file is refused or reads back exactly as before;
  nothing crashes and no answer silently differs. The server catches
  panics per request (500, pointing at `grafeno check`).
- Online backup: `ReadTx::backup` copies every table from the snapshot
  into a new file (written beside it and renamed when complete; it
  refuses to run if a table it does not know exists, so a future table
  cannot be forgotten). `grafeno backup OUT` (from a file, or from a
  running server with `--config`), `GET /v1/backup` (owners; the response
  is the file), and a button in the Studio's administration (in the
  desktop app it writes the file directly). Keys are included.

**10.3 HNSW quality after deletions** (done). Deleting a vector
reconnected its neighbors, but quality after many changes was not
measured.

- A benchmark of recall@10 (against exact search) after deleting and
  reinserting 10%, 30% and 50% of the vectors, and after a churn of
  updates; results in `docs/benchmarks.md`.
- What it shows decides the fix: better repair on delete (reconnecting
  through the deleted node's two-hop neighborhood), and a `compact`
  command that rebuilds a namespace's graph when quality drops below a
  threshold (the check can report an estimated recall from a sample).

As built: the measurement found that inserting a vector back under the
same id (and every update, which removes and inserts) could leave a node
linked only to itself, through links left pointing at it; and that
repairs could close neighborhoods into islands and missed nodes that
linked to the removed one without being its neighbors. Inserts now never
choose the node itself; a removal repairs both its neighbors (with a
search from each, `REPAIR_EF` 16) and the nodes two hops away that
pointed at it, gives a neighbor that only the removed node reached a link
from its closest neighbor, and skips ids already removed. Recall@10 with
`ef` 64 stays at 0.99 or more through every state of the benchmark
(numbers in `docs/benchmarks.md`); deletions cost about five times more.
Vectors no search can reach are counted (`unreachable_vectors` in the
check, no longer a problem: a few appear in any changing HNSW), and
`compact_vectors` rebuilds a namespace's graph (`grafeno compact`,
`POST /v1/{ns}/compact`). `vector_recall` estimates recall from a sample
of the namespace's own vectors (`grafeno check --recall N`,
`GET /v1/{ns}/vectors/recall`). `tests/vector_churn.rs` deletes a third,
inserts it back, churns three times and compacts, checking recall and
that every reinserted vector is found.

**10.4 Costly GQ** (done).

- Adversarial tests: cartesian products, long variable-length paths,
  large UNWINDs, deep nesting, huge literals and parameters, sorts and
  collects of millions of rows; each must stop within its limits with a
  `query_limit` error and bounded memory.
- A memory budget for intermediate results (sorts, `collect`, `DISTINCT`,
  aggregates), not only a row count.
- A cost estimate in `EXPLAIN` from the planner's statistics, and an
  optional limit that refuses plans estimated above it before running.
- Fuzzing the parser and planner (random queries from the grammar):
  errors, never panics.

As built: `tests/adversarial.rs` runs 32 hostile queries on a graph of
2 000 nodes and 6 000 edges, each on a thread with a server worker's 2 MiB
stack, and requires each to end within twice its 2 s timeout. The first
run found three ways out. Loops that only compute (`reduce`, list
comprehensions, quantifiers, `UNWIND`) never looked at the clock, so a
`reduce` over a million items ran for minutes: they now count work
(`Budget::work`), checking time and cancellation every thousand items and
every 256 KB copied. Values had no size: a string doubled forty times, a
`replace` that multiplies its input or a list of lists could take every
byte of memory, so `max_memory` (512 MiB) bounds each value built (checked
before a concatenation allocates, and after list and map literals,
comprehensions and functions), and an estimate of the rows held at once,
from a sample of 32, is checked between clauses and inside `UNWIND`. And
deep nesting overflowed the stack (5 000 parentheses, 50 000 `AND`s, 20 000
`NOT`s): the parser counts the depth of the tree it builds, chains of
operators included, and stops at 200, while values stop at 128 lists or
maps deep. Each hostile query now ends with `query_limit` or an error in
at most 3 s, most in well under one.

`EXPLAIN` gains `rows` and `cost` columns, estimated from the counts the
namespace keeps (nodes and edges per label): an anchor's rows come from its
source (one for a bound node or an `@id`, a tenth of the label for an
index lookup, the label's count for a scan), each expansion multiplies by
the average degree for its edge labels (summed over the hops of a
variable-length step), a node label keeps its share of the nodes, and
conditions keep half. Loops in expressions cost their list's length when it
can be told (literals, `range` of literals) and ten otherwise; algorithms
cost what they visit (betweenness, a search from every node). The estimate
ranks an index lookup (hundreds) below a scan (thousands) below a
three-way product or ten-hop paths (tens of billions). `max_cost` refuses
a statement whose total is above it before it reads anything; it is off by
default, as the estimate is only as good as the counts.

`tests/fuzz.rs` (proptest) feeds random token sequences, valid queries with
random edits and arbitrary text to the lexer, parser, planner, `EXPLAIN`
and executor: errors, never panics. Parameters need no limit of their own:
the HTTP body limit bounds them, `serde_json` refuses nesting deeper than
128, and their use is checked like any value's.

**10.5 Correct memory merges** (done). Merging joined two nodes when
asked.

- Identity rules: strong keys (declared unique properties such as email
  or an external id) decide; names and aliases only suggest; conflicting
  strong keys or labels block a merge.
- Contradiction rules: merging two nodes with conflicting functional
  facts (two different `born_on`) reports the conflict instead of keeping
  both silently.
- Provenance: a merge keeps where every fact came from and records itself
  (`merged_from`, by whom, when), and can be undone.
- Tests with tricky cases: namesakes, people and companies with the same
  name, nicknames, typos, translations.

As built (`crates/grafeno/src/merge.rs`): before merging, the engine lists
where the two nodes disagree, as `Conflict`s of four kinds.

- *Strong keys*: `email` and `external_id` by default (`MergeOptions.
  strong_keys`), and the key of every unique constraint on either node's
  labels. Values compare trimmed and lowercased.
- *Labels*: each node has a label the other lacks (`Person` and `Company`
  sharing `Entity`); one label more (`Person` and `Person:Engineer`) is
  fine. `Archived` is left out.
- *Properties*: the same property with different values (two `born_on`),
  names and aliases aside, as those are how duplicates differ.
- *Functional edges*: for labels declared one-at-a-time
  (`MergeOptions.functional`, such as `born_in`), different current
  targets. Ended or retracted facts do not count.

Any conflict refuses the merge with `Error::Conflict` (409 over HTTP, with
the list in `error.conflicts`); `force` merges anyway with the kept node's
values, and the report lists what it overrode. Names never decide alone: in
`find_duplicates`, strong-key and label conflicts take a pair out (two Ana
Garcías with two emails are namesakes), the same strong key proposes one
whatever the names (score 1), and each conflicting fact lowers a pair's
score by 15% and is listed in its reasons and `conflicts`. Typos are found
by filing every name word under itself and its one-letter deletions (two
words one edit apart share a key) and confirmed with an edit distance (one
edit, two from eight letters on). A changed or added last letter is not a
typo but another name (Mario and Maria, Daniel and Daniela). Nicknames and
translations are not guessed: they match through `aliases` or embeddings.

Every merge writes a `MergeRecord` to a `merges` table (layout version 8):
id, kept and merged ids, when, by whom (the transaction's actor), the
conflicts forced over, both nodes as they were (record, text, embedding,
memory state), and the edges moved and dropped. That is the provenance of
every fact the kept node gained. `merges(ns, node)` lists them (`GET
/v1/{ns}/merges?node=`), and `undo_merge` puts everything back: the merged
node with its id, text, embedding and memory, the kept node as it was, the
moved edges on the merged node, the dropped ones restored; edges added to
the kept node since stay. It refuses if the kept node changed after the
merge (those changes would be lost) unless forced, and a merge undoes once.
The record stays, marked undone. Records are not exported (they hold
copies of nodes that an import would duplicate) and go with their
namespace.

`tests/merges.rs` (engine) and `duplicates_follow_identity_rules_in_tricky_cases`
(memory) cover namesakes, the same email written differently, a person and
a company with one name, two birth dates, functional edges, typos in one
word and in a full name, gendered and suffixed names that are not typos,
a nickname and a translation through aliases, and a merge undone exactly
(names index, text index, vectors, counters and the check all agree).

**10.6 RAM under large graphs** (done).

- Benchmarks with 10 million edges and 1 million vectors, in containers
  with 512 MB and 2 GB of memory: peak and steady memory per operation.
- Caps by bytes, not entries, for the projection cache and the vector
  cache; cheaper projections (u32 positions, no edge ids unless asked).
- Measures decide the next steps (vector quantization lands in 12.3).

As built: `crates/grafeno-query/examples/ram_bench.rs` builds 1 M nodes and
10 M edges, and 1 M vectors of dimension 128, and measures each common
operation's resident and peak memory in containers of 2 GiB and 512 MiB
(numbers in `docs/benchmarks.md`). What it found and what changed:

- `Grafeno::open_with(path, &OpenOptions)` sizes the caches: redb's page
  cache (256 MiB by default, it was redb's 1 GiB), the projection cache
  (256 MiB, by bytes instead of four entries) and the vector cache of a
  write transaction (128 MiB, by bytes instead of 262 144 entries).
  `OpenOptions::with_memory(bytes)` splits one budget. The server reads
  `[memory]` in `grafeno.toml`.
- Projections are built in one pass straight into CSR arrays of `u32`
  positions (no intermediate list of edges, no hash map from ids:
  positions are found by binary search in the sorted ids), reserved up
  front. The in-edges point at the out-edges instead of copying them, and
  edge ids are kept only with `ProjectionSpec::edge_ids` (path algorithms
  ask for them). About 12 bytes per edge and 24 per node; `Step::edge` is
  now the edge's number, and `Projection::edge_id` its id.
- `check` streams: one ordered pass over each record table and each
  index, compared by fingerprints, with point lookups only to explain a
  mismatch. Memory: the node ids and the values of unique indexes.
  `rebuild_indexes` reads nodes and edges a page at a time (texts and
  vectors are still held, as they are reindexed from scratch).
- GQ: rows a procedure builds count against `max_memory` while it builds
  them, node and edge values are estimated with their properties, and the
  scoring algorithms take `top: n`.

Measured (`docs/benchmarks.md`): with 10 M edges every operation fits in
512 MiB with small caches, and stays near 300 MiB with the defaults
outside graph algorithms; `check` went from killed at 2 GiB to 249 s; a
million vectors are searched in about 5 ms with 140 MiB.

Left for later, from the numbers: aggregates that stream (counting 10 M
edges in GQ times out; 12.3); vector inserts slow down as the index
grows (each 100 000 took 96 s at first and 809 s at the million) and a
cache that keeps the most used vectors instead of starting over would
help, as would quantization (12.3); projections are rebuilt per query
when the cache is too small for them; the edge-label filter of
projections reads every adjacency entry; and duplicate detection
(`find_duplicates`) still loads every node of the namespace.

**10.7 One writer** (done).

- Measure saturation: write latency (p50, p99, p999) against offered
  load, queue depth and group sizes, with immediate and eventual
  durability, on disk.
- Metrics at `GET /metrics` (Prometheus text): queue depth, commit time,
  group size, rejected writes, query times and limits hit.
- Tuning from the numbers (group size, flush interval, separate queues for
  bulk and interactive writes so an import cannot starve an agent).

As built: the writer has two lanes. Interactive writes are grouped as
before and always served first; bulk writes run one per transaction
between groups, with eventual durability, and a bulk writer waits for
room instead of being refused. `Writer::import` reads an export in
chunks and writes each in the bulk lane, and the server imports through
it (a rebuild and a compaction go through it too): before, an import
opened its own transactions, so the writer, and every agent, waited for
the whole import, and a chunked import switched the whole database to
eventual durability while it ran. Measured (`examples/saturation.rs`,
numbers in `docs/benchmarks.md`): an agent writing 200 times a second
waited up to 12 s during a 300 000-node import, and now 88 ms at p99 with
chunks of 2 000 (the server's default, `[writer] import_chunk`).

Saturation, on a laptop SSD: with immediate durability writes stay near
2 ms up to about 4 000 a second; then groups reach `max_batch`, which
bounds throughput, so it went from 128 to 512 (from about 8 000 to about
11 000 writes a second). With eventual durability writes take about 0.15
ms up to about 8 000 a second and saturate near 16 000; the flush every
500 ms holds the writer for 10 to 25 ms, which is the p99.

The writer keeps counters and histograms (`Writer::metrics`): queue
depth per lane, writes, failures, rejections, retries, groups, writes per
group, commit time and latency per lane. `GET /v1/metrics` serves them
in the Prometheus text format with the server's own: requests by status
class and their time, 503s, GQ queries and their time, and the limits
they hit (timeout, reads, rows, memory, cost, cancelled), and the
projection cache's memory. It needs a secret like the rest of `/v1`. The
writer's options are `[writer]` in `grafeno.toml`.

#### Phase 11: memory that keeps itself

**11.0 Entity resolution and text similarity** (done). Deciding that two records
are the same thing (record linkage), for any kind of entity: the base of
11.1's reconciliation, and useful on its own (contacts from several
systems, products from several suppliers, companies, addresses, authors,
assets).

- Measures on normalized text: Levenshtein, Damerau-Levenshtein, Jaro and
  Jaro-Winkler, Jaccard and Dice over n-grams, longest common
  subsequence; phonetic codes as an option per language; numbers and
  dates with tolerances (a day and a month swapped). As GQ functions
  (`text.jaroWinkler(a, b)` and the like) and in the library.
- Comparison rules declared per label, not coded: which properties
  identify (strong keys, from 10.5), which compare and how, with weights
  and thresholds for "same", "maybe, to review" and "different"
  (Fellegi-Sunter style), and an explanation of every score field by
  field.
- Candidates found at query time, not only in batch: a trigram index (or
  MinHash with LSH) answers "who looks like this?" without comparing
  with everything, so the same rules serve deduplication of a namespace
  and linking a new record as it arrives.
- The graph as evidence: two similar records that share neighbors (an
  address, an employer, contacts) are likelier the same.
- Merges keep the rules and the undo of 10.5. No language model, CPU
  only; an agent may add its own judgment through the API.

As built:

- `grafeno::textsim` holds the measures: Levenshtein, Damerau-Levenshtein
  (optimal string alignment, with a bounded version that stops early),
  edit similarity, Jaro and Jaro-Winkler (checked against the published
  examples), n-grams with word padding, Jaccard and Dice over them, shared
  words, the longest common subsequence, American Soundex, a Spanish
  phonetic key, and date and number similarities with tolerances. A
  `Measure` names one with its settings and compares normalized text. GQ
  exposes them as `text.*` functions (and APOC's names for the common ones).
- A fuzzy index is an `IndexDef` with `fuzzy: true` whose entries, in the
  same table as the others, are the trigrams of the normalized text (of
  each string of a list), tagged apart from values. Creating, dropping,
  checking, rebuilding, exporting and backing up it needs nothing of its
  own; the planner never uses it for equality or ranges. A search reads
  the entries of each trigram of the query (skipping trigrams shared by
  more than 20 000 nodes), takes the nodes sharing the most, and scores
  them with a measure against their value. `CREATE FUZZY INDEX`,
  `db.searchFuzzy`, `GET /v1/{ns}/fuzzy`. Layout version 9 marks files that
  may hold one.
- A `MatchRule` per label (table `match_rules`): strong keys decide (equal
  values trimmed and lowercased: same; different: different); otherwise
  the score is the weighted mean of the fields present on both sides
  (the best pair for lists such as aliases), plus shared neighbors
  (Jaccard of the neighbor sets) as one more field when both sides have
  neighbors (no links known says nothing). `same` and `review` cut the
  score into three decisions, and every `Match` carries its fields:
  measure, both values, similarity and weight. `find_matches` takes its
  candidates from the indexes on the rule's keys (exact and fuzzy), or
  scans the first 20 000 nodes of the label when there are none;
  `match_pairs` matches every node of a label against the others for
  deduplication, leaving out pairs marked `distinct_from`. Exposed as
  `db.match`, `POST /v1/{ns}/match`, `GET /v1/{ns}/matches`, the rules at
  `/v1/{ns}/match-rules` (admin to change), the `find_matches` MCP tool and
  the TypeScript client. `find_duplicates` (10.5) uses the same edit
  distance.
- Measured (`examples/resolve_bench.rs`, numbers in `docs/benchmarks.md`):
  among 100 000 synthetic people, a fuzzy search for a damaged name takes
  20 ms (p50) against 469 ms comparing with every node, and finds the
  true name in the top 10 for 93% of the damaged names. A rule on name
  (Dice and shared words), birth date and city finds 95% of 2 000 damaged
  duplicates as `same` or `review` (81% as `same`), with a precision of
  0.994 for `same`; 9 of 2 000 namesakes were called `same`. With a
  365-day date tolerance instead of 30, 115 namesakes were: a rule is only
  as good as its tolerances, which is why every decision explains itself.
- Not yet: rules travel with exports (they are kept in backups);
  candidate generation by MinHash/LSH for very common names; phonetic
  keys for other languages; a learned weighting (Fellegi-Sunter m and u
  probabilities from labeled pairs).

**11.1 Incremental extraction and reconciliation of entities** (done).

- Extractors without a model: a gazetteer of the namespace's known names
  and aliases (Aho-Corasick over the names index), patterns (emails, URLs,
  dates, amounts, mentions, hashtags, identifiers), and capitalized
  phrases with statistics of how often they recur.
- Incremental: only new or changed text is read, from the change log of
  11.4; each mention becomes a `mentions` edge with its provenance and
  position.
- Reconciliation with the rules of 10.5: a mention links to an existing
  entity when a strong key or an unambiguous name matches, creates a new
  one when nothing does, and is queued for review when it is ambiguous.
- Extractions sent by an agent (from its own LLM) go through the same
  reconciliation.

As built (`grafeno-memory`: `extract` finds, `reconcile` decides):

- Finds carry character offsets in the original text and a normalized
  value. Patterns: emails, URLs (trailing punctuation dropped), `@handles`
  (not inside emails), `#hashtags`, dates (`YYYY-MM-DD`, and `d/m/yyyy`
  day first unless `month_first`, checked for valid months and days),
  amounts with a currency before or after (`S/ 1.250,50` → `PEN 1250.5`:
  the last separator followed by one or two digits is the decimal point),
  and identifier patterns declared with a label (`INV-\d{4}-\d{4}` →
  `Invoice`). Phrases: up to five capitalized words with `de`, `del`,
  `van`, `of` and the like inside, without a leading common word ("El",
  "The") or a sentence's first word followed by a connector ("Hablamos de
  Proyecto Atlas" is "Proyecto Atlas"); a single word that opens a
  sentence is not a phrase. The gazetteer is an Aho-Corasick automaton
  (leftmost-longest) over every normalized name and alias of the entity
  nodes, matched on the normalized text at word boundaries and mapped back
  to the original offsets.
- Sources are the texts of `Memory`, `Note`, `Episode`, `Document`,
  `Chunk`, `Message` and `Summary` nodes by default; entities are the named
  nodes that are not sources. A run hashes each text and keeps the hash in
  the `state` table (layout version 10, a small key-value store per
  namespace for features like this one): only new or changed texts are
  read, in batches of 100 per transaction (in the bulk lane over HTTP).
  Reading a text again replaces the edges extraction wrote for it
  (`extracted: true`) and its occurrences in the review queue.
- Reconciliation: a name with one node links to it. With several, the
  candidate linked to what the same text already links (its neighbors
  among the text's other entities) wins when it is the only one; otherwise
  the name waits in the review queue as `ambiguous`, every occurrence
  grouped under one item. Emails link to the node whose `email` it is
  (through an exact index on `email`), else to an `Email` node; URLs,
  handles, hashtags and identifiers link to a node per value; dates and
  amounts are kept on the text's node (`mentioned_dates`,
  `mentioned_amounts`). An unknown phrase waits as `phrase` until it shows
  up in `min_phrase_sources` (2) different texts, then becomes an `Entity`
  named by its most frequent spelling, with the texts as its `sources`. A
  phrase that became known in the meantime links like a name.
- Mentions are `mentions` edges from the text's node, with the surface,
  `start` and `end` (characters), the `kind` of find and the recorder (the
  transaction's actor). The queue is settled with `link`, `create` or
  `dismiss`; the decision is kept, so the same name links (or is ignored)
  in later runs without asking again.
- An agent's mentions (`ingest`, `POST /v1/{ns}/nodes/{id}/mentions`, the
  `add_mentions` MCP tool) go through the label's match rule when there is
  one (same → link, review → queued with the candidates, different → a new
  node with the agent's properties), else by name; their edges have `kind:
  agent` and stay when the text is extracted again.
- Exposed as `grafeno extract`, `POST /v1/{ns}/extract`, `GET
  /v1/{ns}/review`, `POST /v1/{ns}/review/{key}`, the `extract`, `review`
  and `resolve_review` MCP tools, and the TypeScript client.
- Measured (`examples/extract_bench.rs`, numbers in `docs/benchmarks.md`):
  20 000 memories naming 5 000 entities (3 982 of which share a name with
  another) are read at about 3 200 texts a second; a second run with
  nothing changed takes 0.06 s, and with 1% changed 0.15 s. 99.4% of the
  mentions link to the right entity and none to a wrong one; of 39 713
  mentions of a shared name, the text's other entities settled all but
  263 names, which wait for review.
- Until 11.4's change log, runs find changes by hashing every text; texts
  are read from nodes by label; and the review queue is scanned whole when
  texts change, which is fine for thousands of items, not millions.

**11.2 Automatic invalidation of contradicting facts** (done).

- Edge cardinality constraints: `CREATE CONSTRAINT FOR ()-[r:works_at]->()
  REQUIRE ONE PER SOURCE` (one current fact per source), also per target
  and per pair.
- A new fact that breaks a constraint ends the old one in valid time
  (its `valid_to` becomes the new fact's `valid_from`) instead of failing
  or keeping both; record time keeps the history, so `KNOWN AT` still
  shows what was believed.
- The same for functional properties; a report of what was invalidated,
  and an option to only flag instead of invalidating.

As built (`crates/grafeno/src/constraints.rs`, table `constraints`, layout
version 11):

- `Constraint::Edge { label, scope, on_conflict }` with scope `source`,
  `target` or `pair`, declared in GQ as `CREATE CONSTRAINT [name] FOR
  ()-[r:works_at]->() REQUIRE ONE PER SOURCE [ON CONFLICT INVALIDATE |
  FLAG | REJECT]`. Every `create_edge` checks the constraints of its
  label (read once per transaction): the current, not retracted facts
  that overlap the new one in valid time, from the same source (to
  another target), to the same target (from another source), or between
  the same pair. The same fact again is no contradiction.
- `invalidate`: a new fact without a start begins when it is recorded. An
  old fact that starts earlier ends where the new one starts; it is not
  edited in place but versioned: the old record is expired (record time)
  and a copy with the new `valid_to` is written, so `KNOWN AT` an earlier
  moment still sees the fact as it was believed then. A new fact older
  than a current one (a late report) ends where the current one starts
  (`shortened`); two facts that start at the same moment: the old record
  is retracted (`replaced`). An `own_only` agent cannot end someone else's
  fact: the contradiction is flagged instead. `flag` keeps both; `reject`
  refuses the new fact (409).
- Every action is recorded (`ConstraintEvent`: constraint, action, new
  and old fact, the replacement version, where it now ends, who, when) in
  the state table: `constraint_events`, `GET /v1/{ns}/contradictions`, and
  per transaction `take_constraint_events` (GQ counts them in
  `facts_invalidated` and `conflicts_flagged`; the MCP `relate` tool tells
  the agent what its fact contradicted). Declaring a constraint on facts
  that already contradict each other flags them, and changes nothing.
- `Constraint::Functional { label, key }` (`REQUIRE p.city IS
  FUNCTIONAL`): a node property has no valid time, so each change of the
  property keeps the earlier value with when it held (since the change
  before, or the node's creation) and who changed it: `property_history`,
  GQ `history(n, 'city')`, `GET /v1/{ns}/nodes/{id}/history/{key}`.
- `SHOW CONSTRAINTS` lists unique, edge and functional constraints
  (`name`, `kind`, `label`, `property`, `rule`); `DROP CONSTRAINT` drops
  either kind.
- Measured (`examples/constraint_bench.rs`): 50 000 job changes for
  10 000 people go from 44 000 to 14 000 facts a second with a one
  employer constraint (each write reads the source's other facts of the
  label); every person ends with exactly one current employer, and 40 069
  earlier jobs stay in valid time.

**11.3 Hierarchical community summaries** (done).

- Louvain levels kept as a hierarchy (communities of communities).
- A summary per community without a model: its most central nodes, its
  distinctive terms (TF-IDF against the namespace), label mix, key facts
  and time span; stored as `Summary` nodes linked to their members and
  parent summaries, refreshed incrementally when the community changes.
- Recall and context use them for broad questions (global search over
  summaries, then drill down); an agent may replace a summary's text with
  its own.

As built (`crates/grafeno-memory/src/communities.rs`, and
`algo::louvain_levels` in the engine; no layout change, summaries are
ordinary nodes):

- `louvain_levels` returns the communities after each aggregation of
  Louvain, finest first; each level is a union of communities of the one
  below. `louvain` returns the last of them, as before.
- `refresh_communities` projects the namespace (every node but summaries
  and archived memories, or the labels asked for; every current fact, or
  the edge labels asked for, optionally weighted) and runs it. A community
  gets a summary when it has at least `min_size` (3) nodes and is not the
  same as the one community under it; levels without any are left out and
  the rest numbered from 0.
- A summary is a `Community` node: `name` (its three most central
  members), `level`, `size`, `central` (members by weighted degree inside
  the community), `terms` (TF-IDF of the members' words against the
  namespace; a word in every node is left out), `labels` (the label mix),
  `facts` (the edges inside it between its most central members),
  `from`/`to` (its time span, from the members and the facts' valid
  dates), and a text that says all that, indexed for full-text search.
  Each node links to the lowest summary that holds it (`member_of`); a
  summary to its parent (`part_of`).
- Incremental: the work is split in `plan_communities` (on a read
  transaction: Louvain, centrality, terms) and `apply_communities` (one
  write transaction), so the server runs the plan off the writer. New
  communities are matched to stored summaries by the members they share
  (Jaccard of at least one half, best pairs first, at any level, so a
  community keeps its summary when the levels around it move); a
  fingerprint of the members, their update times and the facts inside
  decides whether a matched summary is rewritten; unmatched summaries are
  deleted; `member_of` and `part_of` edges are written only where they
  differ.
- `set_community_text` puts an agent's own text on a summary (`custom`);
  refreshes keep it, update the rest, and mark it `stale` when the
  community changed; `None` goes back to the generated text.
- `overview` is the global search: BM25 over summaries (`level` to keep
  one), or the top of the hierarchy, biggest first, without a query;
  `community_members` drills down into the communities a summary is made
  of and its direct members, central ones first. Recall leaves summaries
  out unless asked for by label; `ContextOptions::communities` puts the
  ones that match first, as an `## Overview`, in the items' budget (the
  MCP `context` tool uses 3).
- Server: `POST /v1/{ns}/communities/refresh`, `GET
  /v1/{ns}/communities?q=&level=`, `GET /v1/{ns}/communities/{id}`, `PUT
  /v1/{ns}/communities/{id}/text`; MCP `overview`, `summarize_communities`
  and `set_summary`.
- Measured (`examples/community_bench.rs`, 1 000 teams of 50 in 125
  departments, 160 000 edges): the first refresh takes 3.2 s for about
  9 100 summaries in 4 levels; with nothing changed 1.35 s and nothing
  written; after 1% new nodes 2.2 s, with three quarters of the summaries
  untouched (Louvain moves some boundaries, so some communities split or
  merge). Level 1 matches the teams (purity 0.84 against teams, 0.999
  against departments), level 2 the departments (167 summaries, purity
  0.999); a team's topic word finds its summary first for 200 of 200
  teams, in 0.06 ms.

**11.4 Continuous ingestion and events** (done).

- A change log: every commit appends its changes with a sequence number
  (layout change), kept for a configurable window.
- `GET /v1/{ns}/changes?since=` as a stream (server-sent events) and
  webhooks, so agents and services react to writes.
- Streaming ingestion: NDJSON posted continuously, with idempotency keys
  so a retried event is not written twice.
- Extraction (11.1), invalidation (11.2) and summaries (11.3) run from the
  log, in the background, at their own pace.

As built (`crates/grafeno/src/changes.rs`, table `changes`, layout version
12; `crates/grafeno-server/src/events.rs`):

- Every write (`put_node`, `put_edge`, deletes, texts, embeddings) adds
  to the transaction's pending changes, folded per record: created then
  changed stays `created`, anything then deleted is `deleted`, the parts
  (`record`, `text`, `embedding`) add up. At commit they get sequence
  numbers per namespace (after the last one in the table), the commit's
  time and the actor, and are written in the same transaction, so the log
  and the graph never disagree; an aborted or failed write (a job dropped
  from a group commit) leaves nothing. Entries name the record, not its
  content: a follower reads it.
- The window (`Retention`: on, a million entries and seven days per
  namespace by default; `[changes]` in `grafeno.toml`) is kept at commit,
  at most 10 000 entries pruned per commit. `changes(ns, since, limit)`
  returns a page with `last`, `oldest` and `gap` (changes after `since`
  were pruned: start over from the graph). `on_commit` listeners hear each
  commit that appended, with the namespaces and their last numbers.
- `GET /v1/{ns}/changes?since=` is a page, or with `Accept:
  text/event-stream` (or `?stream=true`) a stream of server-sent events:
  `id` is the sequence number (a client resumes with `Last-Event-ID`),
  `event: change`, and one `gap` event when it started behind the window.
  It wakes on each commit (a watch channel fed by `on_commit`). MCP agents
  poll with the `changes` tool.
- Webhooks (`POST /v1/{ns}/webhooks {url, secret, labels, since}`, admin
  role, since a webhook makes the server send requests): a task delivers
  each one's changes in order, 200 at a time, after each commit and every
  2 s, as `{namespace, webhook, changes, last, gap}`, signed with
  HMAC-SHA256 (`X-Grafeno-Signature: sha256=…`) when it has a secret; only
  labels it asks for (the others move its cursor). Its cursor moves only
  on a 2xx; failures wait 2, 4, … up to 600 s and are recorded
  (`failures`, `last_error`).
- `POST /v1/{ns}/ingest` reads NDJSON as it arrives, a node or an edge per
  line, and writes 500 lines per transaction in the writer's bulk lane. A
  line's `key` is kept (state kind `ingest.key`): sent again it is a
  duplicate and writes nothing, and an edge's ends may be `$key` of nodes
  ingested before. Each line fails alone, with its number in `errors`.
- `[background]` names namespaces where extraction and community
  summaries follow the log: a task per namespace and job reads the changes
  after its cursor (state kind `runner`), runs extraction when a source
  text changed and summaries when enough of the graph changed (not their
  own `Community`, `member_of`, `part_of` writes) and the last refresh is
  old enough, then moves its cursor; `GET /v1/background` says what each
  did. Invalidation (11.2) stays in the write: a constraint that held only
  later would let a reader see two current employers.
- Measured: the log costs about 20% of write throughput (35 000 to 29 500
  records a second, `examples/changes_bench.rs`); it reads back at 1.4
  million entries a second; a commit reaches a server-sent events stream
  in 0.15 ms (p99 0.36 ms); 100 000 NDJSON lines ingest at 25 000 a
  second, and sent again at 360 000 a second, all duplicates
  (`grafeno-server/examples/events_bench.rs`).

#### Phase 12: compatibility and speed

**12.1 Wider Cypher and tool compatibility** (done, except the two items
named below).

- The rest of common Cypher: path variables (`p = (…)`, `nodes(p)`,
  `relationships(p)`, `shortestPath`), `CALL { … }` subqueries, `EXISTS
  { … }` and `COUNT { … }`, pattern comprehensions, map projections, `=~`,
  more list, string and math functions; a subset of the openCypher TCK
  run in CI.
- Tools: import and export in CSV and GraphML; a Bolt protocol endpoint
  (a subset) so Neo4j drivers and tools can connect, if the TCK subset is
  far enough along.

As built:

- Paths are a value (`Value::Path`, nodes and edges in written order):
  `p = (…)`, `nodes(p)`, `relationships(p)`, `length(p)`. A match keeps the
  edges of each step, oriented left to right whichever end it started
  from, only used when the pattern names a path. `shortestPath` and
  `allShortestPaths` take one edge pattern and use the breadth-first search
  variable-length edges already use, so `allShortestPaths` gives one path
  per end.
- `CALL { … }` runs once per row, which it sees whole (no need for `WITH x`,
  which also works); what its RETURN gives multiplies the row, and without
  RETURN (writes) the row stays. A name it returns may not exist outside.
  It is one transaction: `IN TRANSACTIONS` and `UNION` inside are refused.
- `COUNT { … }` and pattern comprehensions share `pattern_rows` with
  `EXISTS { … }`; map projections are `n {.a, .*, k: v, var}`; `=~` matches
  the whole string with the `regex` crate, whose time is linear in the text,
  with compiled size bounded and expressions cached per thread. New
  functions are in `morefns.rs`: `reverse`, `tail`, `isEmpty`, `toBoolean`,
  `to…OrNull`, `to…List`, `ltrim`, `rtrim`, `left`, `right`, `char_length`,
  `nullIf`, `asin`, `acos`, `atan`, `atan2`, `cot`, `degrees`, `radians`,
  `haversin`, `isNaN`, `rand`, `randomUUID`, `elementId`.
- Nested `CALL { }` and `FOREACH` count against the parser's depth limit;
  all the new forms run under the query budget
  (`tests/adversarial.rs`).
- `crates/grafeno/src/interchange.rs`: CSV in `neo4j-admin import` headers
  (`id:ID`, `:LABEL`, `name:type`, `:TEXT`; `:START_ID`, `:END_ID`, `:TYPE`,
  `valid_from`, `valid_to`) and GraphML (APOC's `labels` and `label`).
  Exports hold current knowledge: retracted facts stay out. Imports keep
  Grafeno ids, so a re-import updates; other ids map to new nodes,
  remembered per namespace (state kind `import.id`), so a later edges file
  finds them. Bad rows are skipped and reported. `grafeno export` and
  `grafeno import` take `--format csv|graphml`; the server has
  `/v1/{ns}/export.graphml`, `/export/nodes.csv`, `/export/edges.csv` and
  the same under `/import/` (admin role).
- Conformance: `tests/conformance/scenarios.txt` holds 146 scenarios in the
  TCK's form, run by `cargo test`. They are written by hand from the
  language's semantics. **The official openCypher TCK is not vendored or
  run**, so this suite is not a TCK score; five scenarios pin where Grafeno
  differs on purpose.
- **Not built: Bolt.** It depended on the TCK subset being far enough along,
  and without running the official TCK there is no number to judge that by.
- Measured (`examples/interchange_bench.rs`, 200 000 records): CSV exports
  at 397 000 records a second and imports at 27 600; GraphML at 341 000 and
  25 900.

**12.2 Query optimization and performance under concurrency** (done).

- Statistics for the planner (value histograms per index, label and edge
  label counts by direction) and join ordering for patterns with several
  parts; plan caching for repeated queries with parameters.
- Parallel scans for large read queries, and read throughput measured
  with many concurrent readers and a busy writer.

As built:

- `crates/grafeno/src/planstats.rs`: `PlannerStats` per namespace. Per
  value index: entries, distinct values, the 64 most common values with
  their counts, and 64 equal-depth buckets (encoded bounds, so a range is
  estimated by the buckets it covers, half for one it crosses); an
  equality with a value outside the common list gets the remaining entries
  over the remaining distinct values, and nothing when the list holds
  every value. Per edge label: edges, distinct sources and distinct
  targets, so the average degree out and in (one employer per person, 200
  people per company). Two passes over each index and one over each
  adjacency table; 0.3 s for 200 000 people, 4 indexes and 201 000 edges.
- They live in memory, in the `Shared` state every transaction of a
  database holds, and are not stored (no layout change). `ReadTx::
  planner_stats` builds them from the snapshot the first time and again
  when the namespace drifts (nodes or edges off by a tenth and at least
  100, an index created or dropped); while one reader rebuilds, the others
  plan with the previous ones. Statements that write use only what is in
  memory (`cached_planner_stats`), so a build never runs under the write
  lock. `ReadTx::analyze` and `CALL db.analyze()` rebuild them at once.
  An index's estimate is scaled by how much its label grew since.
- `plan.rs`: a pattern starts from a bound node or an `@id` as before;
  otherwise every node's best source (index, label, scan) is estimated in
  rows, with the values the query gives (literals and parameters while
  planning; at run time, the row's values too), and the whole pattern is
  costed from each node: candidates read, then rows × degree at each step
  (by direction, from the statistics) × the share of targets that fit. The
  cheapest wins. `EXPLAIN` shows the estimate (`~2 080 nodes`), and
  `cost.rs` uses the same sources and degrees.
- Join ordering: `order_patterns` runs the patterns of one `MATCH`
  greedily, a pattern sharing a bound variable first, otherwise the
  cheapest. Rows are the same (edge uniqueness was already per pattern);
  their order without `ORDER BY` can change.
- Plan caching: `cache.rs` keeps 1 024 parsed statements by text (least
  recently used out, an eighth at a time; texts over 16 KiB are not kept;
  syntax errors are not kept). Plans are not cached: anchors are chosen
  per run from current statistics, so a cached statement never plans with
  a dropped index or stale counts. `statement_cache_stats()` reports hits.
- Parallel scans: `parallel.rs`. A read-only match whose start is a label
  or an index lookup estimated at 4 096 nodes or more, with no row cap from
  a `LIMIT`, splits the candidate ids into chunks (eight per thread, at
  least 512 ids) that threads take in turn; each chunk's rows return in
  chunk order, so rows and their order equal one thread's. Threads share
  the snapshot (`ReadTx` is `Sync`) and the `Budget`, whose counters are
  now atomic, so reads, time and cancellation count across them. At most
  one thread per CPU works at once over the whole process
  (`set_parallelism`, `scan_threads` in `[query]`); a query that gets none
  runs alone, and work inside a worker never splits again.
- Tests: `grafeno-query/tests/planner.rs` (statistics against known
  distributions and degrees, `db.analyze`, starts by value distribution
  and parameters, a range against a small label, join order, statistics
  rebuilt after drift and new or dropped indexes, the cache with
  parameters, parallel rows equal to serial and limits across threads).
- Measured (`examples/planner_bench.rs`, 200 000 people, against a build of
  12.1 in the same session, alternating): two equalities with the rare
  one written second 354.7 ms → 11.1 ms; a range holding every person
  against a label of 1 000 companies 1 770 ms → 2.7 ms; the big pattern
  written first 2 593 ms → 1.8 ms; 50 000 point queries with a parameter
  17 682 → 18 616 a second; a scan of 200 000 people 743 ms → 161 ms on 16
  threads (706 ms on one). Reads with many readers did not change, as the
  query measured (a point lookup and two hops, about 200 rows) already
  started from its index: best of three runs, 1 reader 1 259 queries a
  second, 16 readers 9 168 (8.5 ms p99 at most), and 7 960 with a busy
  writer committing 10 500 nodes a second meanwhile.

**12.3 Batched execution** (done).

- Execution in batches of rows with column vectors instead of one row at
  a time, for scans, filters and aggregates.

(Vector quantization and compressed postings and adjacency lists, first
planned here, moved to 12.5: the adjacency format is better designed with
12.4's batched traversal, which is what reads it.)

As built:

- `crates/grafeno-query/src/aggregate.rs`. A `MATCH` followed by a
  (non-optional) `RETURN` or `WITH` with aggregates no longer hands its
  rows on: `match_pattern_into` emits them in batches of up to 1 024 to a
  consumer, and the runner folds each batch into the groups. A batch is
  first turned into columns (each row's group key, and one vector per
  aggregate argument) by `Spec::columns`, which reads only the rows, so a
  parallel scan computes them on its worker threads; `Groups::fold` then
  folds the columns in row order. Every other aggregation (after `UNWIND`,
  `WITH`, a procedure) folds its held rows the same way, so there is one
  code path; `Scope::Group` is gone, replaced by `Scope::Folded` (a group's
  first row and its folded aggregates).
- Accumulators: a counter for `count` (with a set of keys for `DISTINCT`),
  running totals for `sum` and `avg` that keep the row-at-a-time results
  exactly (integers checked as they add, so an overflow on the way is still
  an error; floats added in row order; durations apart), the best value for
  `min` and `max` (the first of equal minimums and the last of equal
  maximums, as `min_by` and `max_by` gave), and the values themselves only
  for `collect`, `median`, percentiles, `stdev`, `variance` and `mode`, which
  then reduce as before (`eval::reduce`). Groups keep the order they first
  appeared in.
- `parallel::map_chunks` hands each chunk's result on in chunk order as
  soon as the chunks before it are done (an ordered delivery under a lock)
  instead of joining everything at the end, so a parallel scan feeds the
  fold while it runs, and the results (floating-point sums included) equal
  one thread's. A fold holds at most a batch; `max_rows` still counts every
  row a match returns whole (the collecting path checks the running total).
- Filters still run row by row, inside the match; scans and filters gain
  from building no row maps for the aggregation and from not holding them.
  Expansion is still one anchor at a time: that is 12.4.
- Tests: `grafeno-query/tests/batch.rs` (every aggregate against values
  worked out by hand, `DISTINCT`, nested aggregates, ties of `min` and
  `max`, empty groups, overflow and type errors, durations, `WITH … WHERE`,
  `*`, `ORDER BY` an aggregate, PROFILE rows, 90 000 rows counted under a
  `max_rows` of 2 000, parallel folds equal to serial ones).
- Measured (`examples/batch_bench.rs`, 200 000 people, against 12.2 in the
  same session, best of two runs each): counting a label 291 → 204 ms;
  grouping by a property 609 → 233 ms; a filter and four aggregates 479 ->
  162 ms; one hop grouped by the far end 989 → 650 ms; `collect` per group
  54 → 23 ms; two hops counting 1 990 000 rows 6 309 → 4 825 ms, and with
  the standard limits it now completes instead of failing on `max_rows`.

**12.4 Batched and parallel traversal** (done).

As built:

- `crates/grafeno-query/src/traverse.rs`. A pattern grows from batches of
  64 anchors (one at a time under a row cap, so a `LIMIT` reads no more
  than it needs), step by step: the partial matches of a batch are a
  frontier, and `step` reads the distinct nodes it stands on once, sorted
  by id, then extends every partial match in order. What a step reads is
  kept for the rest of the traversal from that input row: each node's
  edges for that step (already filtered by label, direction and time),
  each target node decoded once, each edge record once, each node's reach
  for a variable-length step (a breadth-first search per source, run once
  however many partial matches stand there).
- The caches are shared by every thread working on the traversal: maps in
  32 shards behind read-write locks, bounded (2^20 edges listed per step,
  2^18 nodes and edge records) and emptied past the bound; a read that
  misses after that reads again. At first each chunk of a parallel scan had
  caches of its own, and nothing was shared: 512 consecutive uids work at
  512 different companies. Shared, the reads of the two-hop count fell from
  4.03 M to 1.12 M.
- Parallel traversal: a frontier over 4 096 partial matches with steps
  left goes on in slices of that size; in a read-only query without a row
  cap, `parallel::map_chunks` (now generic over what it splits) spreads the
  slices over the shared threads and joins their rows in slice order. This
  is what lets one company, or a thousand, with hundreds of employees each
  use every core, where 12.2 only split large sets of anchors.
- Rows and their order equal growing one anchor at a time: each partial
  match's branches stay together and in order, and slices are joined in
  order. `tests/traversal.rs` checks this against the row-capped path
  (which grows one anchor at a time) on eight pattern shapes, and parallel
  against one thread.
- Found on the way, in 12.2's planner: a node whose inline properties read
  another variable of the pattern (`(q {team: p.team})`) could be chosen to
  start it, and failed with an unknown variable. Such nodes no longer start
  a pattern while another node can.
- Not built from the plan: partial matches are still row maps, not
  columns (rows built only at the end), and variable-length steps search
  per source rather than 64 sources per machine word. Rows returned whole
  share nothing between branches and gain nothing (1.5 s either way for
  199 000 rows); the gains are where branches meet.
- Measured (`examples/traversal_bench.rs`, 200 000 people, 1 000 companies,
  5 acquaintances each, against 12.3 in the same session, best of several
  runs): colleagues of 10 000 people counted 5.6 → 2.7 s (reads 4.03 M →
  1.12 M); two hops from every company grouped 9.5 → 2.9 s; reach within 3
  hops of 2 000 people 2.3 → 2.0 s; friends of friends of 20 000 people
  unchanged at 1.1 to 1.2 s, though with 42% fewer reads.

The research that led here: today a match grows from one anchor at a
time: each candidate expands step by step (its partial matches breadth
first, but alone), so 200 000 people who work at 1 000 companies read
those companies' adjacency 200 000 times, decode each company again for
every person, and jump around the adjacency B-tree in id order of the
people rather than of the nodes read. The plan, in two parts:

- **Batched traversal with columnar frontiers.** Take the candidates of a
  start in batches (a thousand or so) and expand the whole batch one step
  at a time, level by level, as a breadth-first search from many sources
  does. The partial matches of a batch are columns (the node at each
  position, the edges of each step, the input row they came from), not
  one row map each. At each step: collect the distinct nodes the frontier
  stands on, sort them by id (adjacency keys are `(ns, node, label,
  edge)`, so reads become a forward walk through the B-tree), read each
  one's adjacency once, filter edges by label, direction and time once per
  node, and join the result back to the partial matches that stand there
  (a hash join from node id to its edges). Targets are decoded once per
  batch, and their labels and properties checked once, however many
  partial matches reach them. Rows are built only at the end, or never:
  12.3's folding consumes the columns directly. Variable-length steps and
  `shortestPath` become level-synchronous searches from a batch of sources
  with a visited bitmap per source (64 sources per machine word, as
  multi-source BFS does), instead of one search per start.
- **Parallel traversal.** When a level's frontier is large enough (the
  same kind of threshold as 12.2's parallel scans, measured), split its
  distinct nodes across the shared threads; each thread reads and filters
  its share and produces its part of the next frontier, and the parts are
  concatenated in frontier order, so the rows and their order stay those
  of one thread, as in 12.2. Searches from many sources split by source
  batch. All threads share the snapshot and the query's budget.
- Measures: one and two hops grouped by the far end, friends of friends,
  variable-length reach and shortest paths from many sources, against
  12.3; results equal to the row-at-a-time matcher (kept for patterns the
  batched one does not cover, such as path variables at first).

**12.5 Compact storage and vector quantization** (done, without the
compressed postings and adjacency blocks; see below).

- Vector quantization (int8, with exact rescoring of the best
  candidates), compressed postings and adjacency lists (delta-encoded ids
  in blocks per node, read whole by 12.4's traversal); memory and speed
  compared with 10.6's numbers. A new layout version, with its upgrade and
  fixture.

As built (layout version 13):

- `vectors_q8`: every vector also kept as int8, its largest absolute value
  (f32) then one signed byte per dimension, that value scaled to 127
  (`vector::quantize`): 4 + d bytes instead of 4·d. Searches walk the HNSW
  graph with these (`dot_q8`, computed from the stored bytes), then rescore
  the `ef` candidates found with the exact vectors and return the best
  `limit`, so the scores returned are exact. Inserts build the graph with
  the exact vectors, as before. The copies follow every write: insert,
  remove, compaction, rebuild, namespace drop and backup.
- `adjacency_out` and `adjacency_in` keep their keys; their values are a
  compact encoding (`store::Adj`, a `redb::Value` whose in-memory type is
  still the tuple, so no reader changed): a byte of flags for which of
  `expired_at`, `valid_from` and `valid_to` are set, the other end (16
  bytes), then `recorded_at` and the set bounds as zigzag varints. A plain
  fact's value takes 25 bytes instead of 48.
- Upgrade: a file of layouts 3 to 12 has each adjacency table renamed,
  copied entry by entry into the new type under its name, and dropped
  (one transaction, streaming, nothing held in memory); every vector gets
  its int8 copy. `tests/fixtures/layout-v12.redb.gz` is new (layout 12 had
  no fixture), so every past layout is upgraded by `upgrade.rs`.
- Measured (`examples/storage_bench.rs`, a file with 100 000 nodes, 300 000
  edges, a third with valid-time ranges, and 50 000 clustered vectors of
  128 dimensions; against 12.4 in the same session): the adjacency tables
  store 26.3 → 21.3 MiB each (19% less; 14% with B-tree metadata); the
  int8 copies add 7.8 MiB next to the 26.2 MiB of exact vectors; the file
  stays at 514 MiB (redb grows it in large steps); recall@10 at `ef` 64 is
  0.996 both ways and the median search 1.35 against 1.40 ms; reading every
  node's edges is no faster (1.25 to 1.40 s before, 1.42 to 1.47 s after:
  decoding varints costs about what the smaller entries save in memory).
- What this buys: with everything in memory, little. A search touches a
  quarter of the vector bytes (plus `ef` exact ones), which pays when the
  vectors do not fit in the page cache. Measured in E2 on 10.6's file of
  1 M vectors of 128 dimensions (2.06 GB), copied twice and opened by each
  build (`ram_bench search`, 1 000 searches per round, two rounds per run,
  runs alternating): in 512 MiB with a 128 MiB page cache, 4.9 to 5.2 ms a
  search before and 3.5 to 4.2 ms after; in 2 GiB with 256 MiB, 5.0 to 6.0
  ms before and 3.2 to 3.7 ms after; resident memory the same (140 and 272
  MiB). Upgrading that file to layout 13 took 4.5 s (the int8 copies of 1 M
  vectors, about 126 MB, fitting in its free pages: the file kept its
  size).
- Not built: compressed postings and adjacency blocks (delta-encoded ids
  per node or per term, read whole). Both replace the one-entry-per-edge
  and one-entry-per-posting tables with blocks rewritten on every change,
  which redesigns the text index, the writes, `check` and `rebuild`; with
  the value encoding above the adjacency is 14% smaller without it.

### Phase 13 plan: distributed operation

The goal: run Grafeno on several servers that hold the same data, so that
the service keeps working when one of them fails, reads scale with the
number of servers, and large queries run on several of them at once,
without giving up what one server guarantees today (a write is
all-or-nothing, a read sees whole transactions, an acknowledged write is
never lost).

The guidelines of phases 10 to 12 hold (local, agent-agnostic, CPU only),
plus: no external coordinator (no etcd, no ZooKeeper, no shared disk), one
binary, and a single server keeps working exactly as now, with the cluster
off. Each step ends with tests (fault injection where it applies), a
measurement, documentation and its own commit.

**Where it starts from.** One process owns the file and is its only
writer; concurrent writes are grouped into one redb transaction (group
commit, 8.4); every write already leaves a change log of what it changed,
record by record, numbered per namespace (11.4); a backup is a consistent
snapshot taken without stopping writers (4); the file has a layout version
and upgrades on open; and 12.3's aggregates fold into accumulators, the
form a distributed query needs to merge partial results.

**The model chosen.** One leader per group of servers (three or five),
elected with Raft, orders every write into a replicated log; a write is
acknowledged once a majority has the entry, and every member applies the
log in the same order to its own redb file. The alternatives were weighed:

- *Shipping redb pages or files* ("physical" in the storage sense): redb
  has no write-ahead log to ship, its page layout differs from file to file,
  and copying whole files only works for catching up, not for each write.
  Backups (snapshots) are used exactly for that: a new or lagging member.
- *Shipping the requests* (statement replication): a GQ write or a `remember`
  call is not deterministic (it draws ids, reads the clock, reads the state
  it runs on), so members could diverge.
- *Shipping the effects* (record-level replication), chosen: the leader runs
  each write group once, and the log entry holds what it did to the records
  (nodes and edges written or deleted, with their final ids and times,
  texts, embeddings, index and schema changes), which is what 11.4's change
  log already captures. Members apply effects, never run statements again,
  and rebuild their derived entries (adjacency, labels, postings, HNSW
  links, int8 copies, counters) from them, so a member's file is the same
  data, verifiable with `check` and by comparing exports.

**13.1 A replicated log of deterministic write groups.**

- Every write group (what the writer thread commits today) is turned, on the
  leader, into a log entry of resolved effects: final ids and timestamps,
  GQ statements reduced to the records they wrote, embeddings and texts as
  values, schema changes (indexes, constraints, principals) as values. An
  entry carries the layout and entry format versions it was written with.
- An `apply` path that takes an entry and performs it in one redb write
  transaction, deriving every index from the records, and stores the log
  position it reached (`applied_index`) in the same transaction, so a crash
  between applying and recording is impossible and replaying never applies
  twice.
- A log table (entries by index and term), its truncation after a snapshot,
  and snapshots built from backups plus the position they hold. A new
  layout version, with its upgrade and fixture.
- Done when: replaying a database's log onto an empty file gives the same
  data (exports equal, `check` clean) as the file that wrote it, for every
  kind of write, including writes that fail halfway (nothing in the log)
  and group commits that mix failing and succeeding writes; property tests
  against the 4.x model.

**13.2 Raft: leader election and log replication.**

- Consensus among three or five members: terms, elections with randomized
  timeouts, log replication with quorum commit, a pre-vote phase so that a
  member that was cut off does not depose a working leader on return, and
  membership changes (joint consensus) to add, remove or replace a member
  without stopping. To evaluate first: `openraft` (an async Rust
  implementation used in production) against writing the protocol, which is
  small but subtle; the choice is recorded here with its reasons.
- The leader's writer proposes each group and acknowledges its writes once
  the entry is committed (a majority has it durably); followers apply
  committed entries in order. Durability settings (immediate, eventual)
  become per-entry fsync policies on every member.
- Snapshot install for a member too far behind (a backup streamed with its
  log position, then the log after it), and log compaction.
- A cluster transport between members (its own port, authenticated with a
  cluster secret and TLS), and `[cluster]` in `grafeno.toml`: the member's
  id, its peers, timeouts. `grafeno cluster init`, `join`, `leave`,
  `status`.
- Done when: a deterministic in-process simulation (members, a network
  that drops, delays, duplicates and reorders messages, crashes and
  restarts, all from a seed) runs thousands of schedules and never loses an
  acknowledged write, never applies two different entries at one index, and
  always elects a leader once a majority can talk.

**13.3 Continuing when an instance fails.**

- Failover: when the leader stops answering, a new one is elected within a
  bounded time and writes resume; the old leader, if it comes back, steps
  down (terms fence it). Writes in flight during the change fail with a
  retryable error or succeed exactly once (idempotency keys, as NDJSON
  ingestion already has).
- Clients find the leader: followers forward writes to it (or answer with
  its address); the TypeScript client, the shell, MCP (`--url`) and the
  Desktop app accept several server addresses and move to another when one
  fails.
- Reads by consistency level: `linearizable` (through the leader, with a
  lease or a read index), `bounded` (any member at most N ms or N entries
  behind), and read-your-writes (a write returns its log position; a read
  that carries it waits until the member has applied it).
- Work that must run once (extraction, community summaries, webhooks, the
  change log's retention, 11.x) runs on the leader only, with its progress
  in the replicated state, so a new leader resumes where the old one
  stopped; server-sent event streams continue their numbering across a
  failover.
- Done when: fault injection on real processes (a docker compose of three
  and five members, killing, pausing and partitioning them with
  `iptables`/`tc`) under a workload of reads and writes, checked by a
  linearizability checker (registers and counters, as Jepsen does), finds
  no anomaly; failover time and lost availability measured.

**13.4 Queries in parallel across instances.**

- Reads spread over members: a server (or the client) balances reads over
  the members that meet the requested consistency.
- Large read queries split across members: every member holds all the data,
  so a query whose start is large (12.2's parallel scans, 12.4's frontiers)
  is cut into ranges of anchor ids; each member runs its ranges at the same
  log position (it waits until it has applied it) and returns partial
  results, 12.3's accumulators made mergeable (counts, integer sums, min,
  max, `collect` in range order; floating-point sums and averages with a
  summation that does not depend on the order, decided in this step so that
  results equal one server's), and the coordinator merges them in range
  order. Queries that return rows stream them back in range order.
- Graph algorithms that already parallelize (PageRank, components) stay on
  one member; distributing them is a separate decision.
- Done when: every query of the conformance and planner suites returns the
  same rows on one server and split across three; speed-up measured for
  scans, two-hop traversals and aggregates with 1, 3 and 5 members.

**13.5 More than one group: namespaces placed on different servers.**

- Write throughput is still one leader's. To go beyond it, a cluster can run
  several Raft groups, each owning whole namespaces (multi-Raft), with a
  small replicated directory saying which group owns which namespace; any
  member routes a request to the right group.
- Moving a namespace between groups without stopping it: snapshot, catch up
  with its change log, a short write pause, switch in the directory.
- Queries that span namespaces (only those that ask, as today) run on each
  owner and merge as in 13.4.
- Done when: write throughput measured with one, two and three groups, and a
  namespace moved under load without losing or duplicating a write.

**13.6 Operating a cluster.**

- Status and metrics per member (role, term, commit and applied positions,
  lag, elections, snapshot transfers) in `/metrics`, `grafeno cluster
  status` and a cluster view in the Studio and the Desktop app.
- Rolling upgrades: members of two consecutive versions coexist; entries
  and snapshots carry their format version, and a leader never writes a
  format a follower cannot read; layout upgrades happen member by member.
- Backups from a follower, restore into a new cluster, and adding a member
  from a backup.
- Installation: a three-member docker compose, systemd units and the
  Windows service with `[cluster]`, and runbooks for losing one member,
  losing a majority, and replacing a disk.
- Done when: a rolling upgrade and every runbook have been run on the
  compose under load, with the linearizability checker watching.

**Measures for the phase** (in `docs/benchmarks.md` and `docs/performance.md`,
against one server): write latency and throughput with three and five
members on one machine and over a real network; failover time; reads per
second with one to five members; speed-up of split queries; time to add a
member from a snapshot of 1 M nodes and 10 M edges; memory per member.

### Phase 14 plan: users, roles and permissions

**Where it starts from (10.1).** Access is by principals: named API keys
(stored as SHA-256), each with grants per namespace or namespace prefix of
one of three fixed roles (`read`, `write`, `admin`), a global `owner` flag,
and an `own_only` rule (change only what you recorded). The token in
`grafeno.toml` is the owner; a server without one is open until its first
key. Every surface (HTTP, MCP, shell, GQ, export, the desktop bridge)
checks the same grants, and writes record who made them (`recorded_by`).
What is missing next to a database such as PostgreSQL or Neo4j: people who
sign in with a password and get a session, roles you define and nest,
privileges finer than three levels (per operation, per label, per
property, per record), `GRANT` and `REVOKE` in the query language, expiry
and rotation of credentials, and a record of who did what to security.

The guidelines hold: local and CPU only (no identity service required),
agent-agnostic (agents and services are accounts like people), and a
single-user setup stays as simple as now: one owner, no ceremony. In a
cluster (phase 13) users, roles, grants and sessions are replicated state,
written through the log and enforced the same on every member. Each step
ends with tests (the 10.1 leak matrix grows with each privilege), a
measurement where it costs something, documentation and its own commit.

**14.1 Accounts and sign-in.**

- Accounts of two kinds: *users* (people: a name, a password, sessions)
  and *service accounts* (agents and applications: API keys only). Both can
  hold API keys; a key belongs to an account, can expire, be rotated and be
  scoped down (a subset of the account's rights, a namespace, read only).
- Passwords hashed with Argon2id (parameters measured on the target
  hardware), a password policy (length, a list of common passwords), and
  throttling and lockout after failed attempts, per account and per address.
- Sign-in over HTTP returns a session (a short-lived token and a refresh
  token, revocable, listed per user); the Studio and the desktop app get a
  sign-in screen instead of pasting a token; the shell signs in with a
  prompt; MCP keeps using keys.
- Bootstrapping: `grafeno users create --superuser` (or the first sign-in
  on an open server) creates the first administrator. The static token
  becomes an optional break-glass credential, off unless configured.
- Migration: every existing principal becomes a service account with the
  same rights (its grants become roles, below); `recorded_by` keeps the
  names it had. A new layout version, with its upgrade and fixture.
- Done when: sign-in, refresh, expiry, revocation, lockout and key scopes
  are tested over HTTP, the shell and the Studio, and the 10.1 matrix passes
  with users, service accounts and scoped keys.

**14.2 Roles and privileges.**

- A catalog of privileges, each on a kind of object:
  - the server: create namespaces, manage accounts, manage roles and
    grants, back up and restore, see metrics and the audit log, operate the
    cluster;
  - a namespace: read (match, search, recall), and separately create,
    update, delete, merge, retract, import, export, manage indexes and
    constraints, run procedures and algorithms, drop;
  - labels inside a namespace: read or write nodes with a label, edges with
    a label (`Person` readable, `Salary` facts not);
  - properties: read or write one property of a label (`Person.email`
    hidden from a role that can read people).
- Roles: built-in ones that map today's levels (`reader`, `editor`,
  `admin` per namespace; `superuser` for the server; `public`, which every
  account has) and roles defined by administrators. Roles are granted to
  accounts and to other roles (a hierarchy, cycles refused); an account's
  rights are the union of its roles'. Privileges can be granted and, where
  a broad grant needs an exception, denied; a denial wins over any grant
  (as in Neo4j), so "read everything in `crm` except `Salary`" is one grant
  and one denial.
- Effective rights are computed once per account and cached, recomputed
  when a grant changes, so checking a request stays a lookup.
- Done when: a property test draws random role graphs, grants and denials
  and checks the computed rights against a direct model; the cost of a
  check is measured with thousands of roles.

**14.3 Managing access in GQ, the API and the tools.**

- GQ statements, run by accounts that may manage access: `CREATE USER`,
  `ALTER USER` (password, status, expiry), `DROP USER`, `CREATE ROLE`,
  `DROP ROLE`, `GRANT` / `DENY` / `REVOKE` privileges `ON` a namespace,
  labels or properties `TO` a role, `GRANT ROLE … TO` an account,
  `SHOW USERS`, `SHOW ROLES`, `SHOW PRIVILEGES [FOR …]`, `SHOW CURRENT
  USER`; `EXPLAIN` of a denied statement says which privilege it lacks.
- The same over HTTP (`/v1/admin/users`, `/roles`, `/grants`), in the CLI
  (`grafeno users`, `grafeno roles`, `grafeno grants`), in the TypeScript
  client, and as views in the Studio and the desktop app.
- Done when: every statement and endpoint is in the conformance and HTTP
  suites, including those refused for lack of the privilege to manage.

**14.4 Enforcement down to labels, properties and records.**

- Reads: nodes and edges a caller may not read are absent from every
  result, as if they did not exist (patterns, `count`, paths, `EXISTS`);
  hidden properties are absent from records, maps and projections, and a
  filter on one finds nothing. The planner adds these restrictions where it
  starts a pattern, so they cost no extra pass.
- Search and memory: full-text, vector (the open decision about filtering
  inside HNSW instead of after it is settled here, with recall measured),
  fuzzy search, `recall` and `context` see only what the caller may read;
  graph algorithms run on the visible subgraph; exports, the change log and
  server-sent events carry only what the subscriber may read.
- Writes: each change is checked against the label, property and operation
  privileges it needs; a statement that would touch something forbidden
  changes nothing and fails with the privilege it lacks.
- Record-level policies, generalizing `own_only`: a role can be limited to
  records that match a condition (`n.@recorded_by = current_user()`,
  `n.tenant = $account.tenant`), for reads, writes or both, as
  PostgreSQL's row-level security does.
- Done when: the leak matrix covers every privilege on every surface, and
  property tests check that random queries under random rights never return
  a hidden node, edge or property, and never change one; the overhead of
  enforcement on reads is measured with and without restrictions.

**14.5 Audit and accountability.**

- An audit log of security events: sign-ins and failures, lockouts, session
  and key creation and revocation, every change to accounts, roles, grants
  and policies, and refused requests, with who, when, from where and what.
  Stored like the change log (numbered, retained by age and size, its own
  privilege to read), queryable in GQ, streamable over server-sent events,
  exportable as JSON lines.
- `recorded_by` names the account (and the key or session) behind every
  write; administrators can list and end any account's sessions.
- Done when: every security event appears exactly once, refused requests
  included, and the log survives restarts, failover and backups.

**14.6 External identities (optional).**

- Sign-in through an external identity provider for organizations that have
  one: OpenID Connect (with its groups mapped to roles), and client
  certificates (mutual TLS) for services. Local accounts keep working with
  none of it, and nothing calls out unless configured.
- Done when: sign-in and group mapping are tested against a local test
  provider, and the server works offline with it configured but
  unreachable (local accounts and existing sessions go on).

**Measures for the phase:** the cost of authorization on reads and writes
(none, a namespace grant, label and property restrictions, record
policies) against 13's numbers; sign-ins per second and their CPU with the
chosen Argon2id parameters; vector search recall and latency with
visibility filters.

### Phase 15 plan: engine and interface apart

**Where it starts from.** The engine and the interface are tied both ways.
The `grafeno` binary embeds Grafeno Studio (the `studio` feature,
`include_dir` of `studio/dist`) and serves it at `/studio/`; its Windows
installer adds Studio shortcuts and opens it after installing, and its
`.deb` and `.rpm` add a `grafeno-studio.desktop` entry. Grafeno Desktop
(9.4, Tauri) links the engine crates (`grafeno`, `grafeno-server`) and runs
the engine inside its own process to open database files, reaching its
HTTP router without a socket. So installing the server brings a web
interface, and installing the desktop app brings a whole engine.

**The goal.** Two products with their own installers, versions and
releases:

- **Grafeno**, the engine: the `grafeno` binary (server, HTTP API, MCP,
  shell, administration commands) and nothing else, for servers and
  machines without a screen. Installed natively, as a database server is
  (PostgreSQL is the model): Windows setup `.exe` (with the service), Linux
  `.deb` and `.rpm` from signed package repositories (with the systemd
  unit), a `curl … | sh` one-liner, and a `.tar.gz` for offline machines.
  Docker stays available and is never required.
- **Grafeno Desktop**, the interface: the Studio as a desktop application,
  Windows `.exe` (and `.msi`), Linux `.deb`, `.rpm` (and `.AppImage`). It
  contains no engine: it talks to engines (on the same machine or others,
  one server or a cluster) only through their public HTTP API, as any
  other client does.

Phases 13 and 14 add views to the Studio as it is (cluster status, sign-in,
users and roles); this phase moves them with it. Each step ends with tests,
documentation and its own commit.

**15.1 The engine alone.**

- The Studio leaves the engine: no `studio` feature, no `include_dir`, no
  `/studio/` route (it answers with a short note pointing to Grafeno
  Desktop), no Node or Vite step in the engine's build.
- The engine's packages install only the engine: the binary, `grafeno.toml`,
  the service (Windows) or systemd unit (Linux), the icon of the binary and
  the documentation. No shortcuts to a web interface, no `.desktop` entry,
  no dependency on a graphical stack (no WebKit or GTK in the `.deb` and
  `.rpm`).
- The binary's size before and after is measured; the release notes say
  where the interface went.
- Done when: the engine builds and passes every test without Node installed,
  and installing its packages on a clean Windows and in clean Debian and
  Fedora containers brings no graphical files or dependencies.

**15.2 A professional installation of the engine.**

Today: `install.sh` (`curl -fsSL …/install.sh | sh`) downloads a release
archive, checks its SHA-256 and installs the binary, and with `--service`
creates the `grafeno` user, `/etc/grafeno/grafeno.toml` and the systemd
service; the `.deb` and `.rpm` do the same from a downloaded file; the
Windows setup (Inno Setup) installs the binary, the configuration and the
service. Missing, next to how PostgreSQL installs: package repositories, a
setup step that initializes an instance, several instances on one machine,
upgrades that protect the data, a guided Windows setup, and signatures.

- Linux, from the package manager: signed APT and DNF/YUM repositories
  (`apt install grafeno`, `dnf install grafeno`), so updates come with the
  system's; packages for x86_64 and aarch64, for current Debian, Ubuntu,
  Fedora, RHEL and their derivatives, with SELinux and AppArmor policies
  where the distribution expects them.
- The one-liner, kept and widened: `curl -fsSL https://… | sh` detects the
  distribution and architecture, and either adds the repository and
  installs the package (the default where one exists) or installs the
  binary from the archive, verifying its checksum and signature. Options
  for a version, a prefix, installing without root (into the user's home,
  with a systemd user service), and unattended use (all choices as flags or
  environment variables).
- Initializing an instance, apart from installing the program, as `initdb`
  and `postgresql-setup` are: `grafeno setup` creates the data directory
  with its permissions, writes `/etc/grafeno/<instance>.toml` (address,
  port, durability, memory, TLS), creates the first administrator (a
  password, with phase 14) and enables and starts the service. Interactive
  by default; every answer as a flag or an answers file for automation.
- Several instances on one machine (as PostgreSQL clusters): a templated
  unit (`grafeno@<name>.service`), each instance with its own data
  directory, configuration, port and logs; `grafeno instances` lists them.
- Upgrades that protect the data: the package's upgrade backs up each
  instance (phase 4's `backup`) before a new version opens it and upgrades its
  layout, keeps the backup until the first clean start, and can roll back
  to the previous version with it. Removing the package keeps the data;
  purging (`apt purge`, an explicit `--purge`) removes it.
- Windows, a guided setup as PostgreSQL's installers have: install folder,
  data folder, port and address, the account the service runs as, the first
  administrator's password, a firewall rule, `grafeno` on the `PATH`; the
  same answers as parameters for a silent install (`/VERYSILENT /DATADIR=…
  /PORT=…`); upgrades in place that keep data and settings; uninstalling
  keeps the data unless asked; a `winget` package.
- Operations a server administrator expects: logs in journald and the Event
  Log (and a file where configured), log rotation, `grafeno doctor` (checks
  permissions, ports, the service, free disk space and the layout version),
  and the paths of everything documented in one table.
- Trust: release archives and packages signed (the repository keys and a
  `minisign` key published), the Windows setup and binary signed with a
  code-signing certificate where one exists, checksums published with
  every release.
- Done when: in clean machines (Debian, Ubuntu, Fedora, RHEL-compatible, on
  x86_64 and aarch64, and Windows 10 and 11), with no Docker, the one-liner,
  the repositories, the packages and the Windows setup each install,
  initialize, upgrade (with the backup taken) and remove the engine,
  interactively and unattended, and `grafeno doctor` passes after each.

**15.3 The public API as the only contract.**

- Everything the interface does goes through the documented HTTP API:
  whatever it reaches today in-process or through app-only commands
  (opening files, import and export, the database's own settings) gets an
  endpoint, or stays in the interface as a file it reads and sends.
- Versioning: the engine reports its version, its API version and its
  capabilities (`GET /v1/about`: authentication modes, cluster, features
  added by later phases); the interface states the API versions it speaks,
  checks them on connecting, and turns off what an older engine lacks
  instead of failing.
- The TypeScript client becomes the published contract (its own version,
  generated types where possible), and a contract test runs the interface's
  client against the real engine binary of each supported version.
- Done when: the interface has no call outside the public API, and the
  contract tests pass against the current engine and the previous release.

**15.4 The desktop application without an engine.**

- Grafeno Desktop stops linking the engine crates. Its sources (the Studio
  and the native shell) live in one place, `apps/desktop`, with no build
  dependency on the engine's workspace.
- Connections: saved servers (an address, a sign-in from phase 14 or a key),
  several open at once, clusters by their members (phase 13), the local
  service found on its default address.
- Local databases, without embedding the engine: when the user opens a
  file, the app asks an installed engine to serve it, by starting the
  `grafeno` binary found on the machine as a child process on a loopback
  port with a token for that session, and stopping it on close. If no
  engine is installed, the app says so and links to the engine's installer.
- Tauri or Electron: Tauri is what 9.4 built (the system web view, a Rust
  shell, installers of a few megabytes); Electron bundles Chromium (one
  rendering engine everywhere, installers of about a hundred megabytes).
  The choice is measured (installer size, memory, start time, rendering of
  the graph view on WebKitGTK) and recorded here; Tauri is kept unless the
  measurements say otherwise.
- Done when: the app, built with no engine crate, opens a local file through
  an installed engine, connects to a remote server and to a cluster, and
  passes its end-to-end tests against the engine binary.

**15.5 Two products, two releases.**

- Separate versions and change logs; separate release workflows, started by
  their own tags (`engine-v…`, `desktop-v…`), each building its installers
  on Windows and Linux, signing them where certificates exist, and
  publishing checksums.
- Install tests per product in clean machines (Windows, Debian, Fedora):
  the engine package brings no graphical files; the desktop package
  installs no service and no engine; installing, upgrading and removing
  either leaves the other untouched, and data stays where it was.
- Documentation split: `docs/install.md` for the engine (every way of
  installing it, initializing instances, upgrading, the paths); the desktop
  app's own guide, with the engine versions it supports.
- Done when: a tagged release of each product builds and passes its install
  tests on its own.

## Measurements and test environments

What Grafeno has been measured to do, where, and how; the base for the
official documentation's charts. The raw numbers are CSV files in
`docs/data/` (one per experiment, described in its `README.md`); the
discussion of each experiment is in `docs/benchmarks.md`, and
`docs/performance.md` presents them with charts (drawn from the CSV files
by `docs/charts/build.py`). Every number below comes from one of three
environments.

### Environments

| Id | What | Hardware and system | Storage | Build |
| --- | --- | --- | --- | --- |
| E1-mem | The engine alone, without a disk | Lenovo laptop (model 83JC): AMD Ryzen 7 7435HS, 8 cores / 16 threads, 31.7 GiB of RAM; Windows 11 Home 10.0.26200 | redb's in-memory backend: no file, no fsync | Rust 1.99.1, release profile, redb 4.4.0 |
| E1-disk | The engine on a file | The same laptop | a file in `%TEMP%` on an NVMe SSD (Micron MTFDKCD512QFM, NTFS); immediate durability is an fsync per commit | as E1-mem |
| E2 | Memory limits | The same laptop, Docker Desktop 29.6.1 on WSL2 (Linux 6.18.54.1, 16 CPUs and 15.5 GiB visible to Docker) | a named Docker volume (ext4 inside WSL2's virtual disk, on the laptop's NVMe); slower than E1-disk for random reads | built in `rust:1.91` with `cargo build --release --locked`, run in `debian:bookworm-slim` with `--memory` equal to `--memory-swap` (a hard limit, no swap): 2 GiB or 512 MiB |

E1 is a laptop on mains power with other programs running: runs vary by
about ±20%, so only larger differences mean something. E2's times are
pessimistic (its disk sits behind a virtual disk); its memory numbers are
the point. Nothing has been measured yet on Linux bare metal, on a
spinning disk, on ARM, or for days at a time.

### Methodology

- **Builds and runs.** Release builds; one run per configuration unless a
  file says otherwise; the date of each run is in `docs/data/README.md`.
  Benchmarks call the library in-process, so HTTP is not in their times
  (its cost has not been measured).
- **Latency under load** (`examples/saturation.rs`). Open load: each write
  is scheduled at a fixed rate whatever the answers, and its latency runs
  from that scheduled time, so a system that falls behind shows it instead
  of slowing the clients down (no coordinated omission). 512 client
  threads; each step lasts 3 s; percentiles are exact over every write of
  the step. Each write is a node and an edge to an earlier node.
- **Memory** (`examples/ram_bench.rs`). Resident memory (`VmRSS`) after an
  operation is "steady"; the most it reached while the operation ran
  (`VmHWM`, reset through `/proc/self/clear_refs` before each operation)
  is "peak". Both include redb's page cache, which lives in the process,
  and not the kernel's file cache. A run that crosses the container's
  limit is killed by the kernel (exit code 137), as a server would be.
- **Recall** (`examples/hnsw_recall.rs`, `hnsw_churn.rs`, `vector_recall`).
  Recall@10 against exact answers from a pass over every vector alive at
  that moment, over 200 queries drawn from the data (clustered vectors,
  closer to real embeddings than uniform noise).
- **Hostile queries** (`tests/adversarial.rs`). Each query runs on a
  thread of its own with the 2 MiB stack of a server worker, under limits
  of 2 s, 2 million reads, 200 000 rows and 64 MiB, on 2 000 nodes and
  6 000 edges; the test fails on a panic, a crash or more than 4 s.
- **Correctness** is shown by tests, not benchmarks (below): property
  tests against a model, crash tests in child processes, fixtures of every
  past layout, fuzzing.

### Headline numbers

| Capability | Result | Env | Data |
| --- | --- | --- | --- |
| Read a node | 1.6 µs | E1-mem | `engine.csv` |
| Neighbors of a node (10k nodes, 40k edges) | 25 µs | E1-mem | `engine.csv` |
| BM25 search, top 10 of 10 000 notes | 0.27 ms | E1-mem | `engine.csv` |
| Vector search, top 10 of 20 000 (128-d, `ef` 64) | recall@10 1.000, p50 0.51 ms, p99 0.98 ms | E1-mem | `hnsw_recall.csv` |
| Vector search, top 10 of 1 000 000 (128-d) | 5 ms, 140 MiB resident in a 512 MiB container | E2 | `vector_search.csv` |
| Recall@10 through deletes, reinserts and churn | 0.99 or more at `ef` 64 (0.929 before 10.3) | E1-mem | `hnsw_churn.csv` |
| A write with immediate durability | about 2 ms up to 4 000 writes/s | E1-disk | `saturation_immediate.csv` |
| Most writes per second, immediate / eventual durability | about 11 000 / 16 000 | E1-disk | `saturation_*.csv` |
| An agent's p99 write latency during a 300 000-node import | 88 ms (12 s before 10.7) | E1-disk | `import_interference.csv` |
| Bulk import, nodes with 30-word texts | about 7 500 nodes/s, steady up to 1 M | E1-disk | `bulk_load.csv` |
| Property index lookup against a label scan (100 000 nodes) | 3.6 ms against 200 ms | E1-mem | `property_index.csv` |
| PageRank, 100 000 nodes and 500 000 edges | 17 ms (projection 102 ms, cached afterwards) | E1-mem | `algorithms.csv` |
| Graph of 1 M nodes and 10 M edges in 512 MiB | every operation completes; peak 509 MiB (a projection with edge ids) | E2 | `ram_graph.csv` |
| Integrity check of 10 M edges | 249 s, 443 MiB peak (killed at 2 GiB before 10.6) | E2 | `check_cost.csv` |
| Projection memory | about 12 bytes per edge, 28 with edge ids | E2 | `projection_size.csv` |
| Hostile GQ queries | 32 of 32 end within their limits, none crashes | E1-mem | `hostile_queries.csv` |
| Fuzzy search for a damaged name among 100 000 people | 20 ms p50 (469 ms comparing with every node) | E1-mem | `resolve_speed.csv` |
| Entity resolution by a match rule (2 000 damaged duplicates, 2 000 namesakes) | 95% found as same or review, precision of `same` 0.994 | E1-mem | `resolve_quality.csv` |
| One current fact per source (50 000 job changes) | 14 000 facts/s (44 000 without); every person one current employer | E1-mem | `constraint_cost.csv` |
| CSV and GraphML of 200 000 records | export 397 000 and 341 000 records/s; import 27 600 and 25 900 | E1-mem | `interchange.csv` |
| Change log on 200 000 records | 29 500 records/s (35 000 without); reads 1.4 M entries/s | E1-mem | `change_log_cost.csv` |
| NDJSON ingestion of 100 000 lines | 25 000 lines/s; a retry 360 000 lines/s, nothing written; commit to event 0.15 ms | E1-mem | `ingest_runs.csv` |
| Community summaries of 50 000 nodes in 4 levels | 3.2 s first, 1.35 s unchanged; teams and departments found (purity 0.999); global search 0.06 ms | E1-mem | `community_*.csv` |
| Extraction of 20 000 memories naming 5 000 entities | 3 200 texts/s; 99.4% of mentions linked right, none wrong; 0.06 s when nothing changed | E1-mem | `extract_*.csv` |
| Queries a planner without statistics got wrong (200 000 people) | 355 ms → 11 ms, 1.8 s → 2.7 ms, 2.6 s → 1.8 ms; a scan 4.4 times faster on 16 threads | E1-mem | `planner_plans.csv`, `parallel_scan.csv` |
| Read queries with 16 readers and a busy writer | 9 200 a second alone, 8 000 with 10 500 writes a second; p99 under 9 ms | E1-mem | `read_concurrency.csv` |
| Aggregates over 200 000 rows, folded as they come (12.3) | 2.3 to 2.9 times faster grouped; 2 M rows counted within the standard limits (failed before) | E1-mem | `batch_aggregation.csv` |
| Traversals from many starts, batched and parallel (12.4) | two hops from 10 000 people 5.6 → 2.7 s, from every company 9.5 → 2.9 s; reads down 30 to 72% | E1-mem | `traversal.csv` |
| Compact adjacency and int8 vectors (12.5) | adjacency 19% smaller; searches walk a quarter of the vector bytes at the same recall (0.996) and speed in memory | E1-disk | `compact_storage.csv` |
| Vector search, 1 M vectors in 512 MiB, int8 copies (12.5) | 3.5 to 4.2 ms (4.9 to 5.2 ms before), same resident memory | E2 | `vector_search_int8.csv` |

### Guarantees and the tests that hold them

`cargo test --workspace` runs about 210 Rust tests (unit and
integration); the TypeScript client has 7 end-to-end tests against the
real binary. The ones that carry each guarantee:

| Guarantee | Tests | How |
| --- | --- | --- |
| The engine does what a plain model does | `grafeno/tests/model.rs` | proptest: random sequences of every write, aborted transactions, valid and record times; records, labels, counters, neighbors, text and vector search compared with an in-memory model, also after export → import and after a rebuild (3 000 cases) |
| Commits survive crashes | `grafeno/tests/durability.rs`, `crash.rs` | a child process killed at random moments during writes, imports, rebuilds, namespace drops and upgrades; every reported commit is there, nothing is half-written, the check passes |
| Old files open | `grafeno/tests/upgrade.rs` and `fixtures/layout-v1` to `v12` | a database written by the commit of every past layout version, upgraded on open, then written and reopened |
| Damage is found, not trusted | `grafeno/tests/corruption.rs`, `src/check.rs` tests | files with flipped bytes, zeroed pages or cut short are refused or read back exactly as they were, never a crash or a different answer; damage to each derived index is reported and repaired by a rebuild |
| Backups are consistent | `grafeno/tests/backup.rs` | a backup taken while writes go on is the snapshot it started from |
| Readers see whole transactions | `grafeno/tests/concurrency.rs` | readers during commits never see part of one, and snapshots never go back |
| Group commit is correct | `grafeno/tests/writer.rs` | concurrent writes all commit; a failing write leaves the others; a full queue answers busy; imports through the bulk lane let interactive writes in; metrics count what happened |
| Vector search keeps its quality | `grafeno/tests/vector_churn.rs` | deletes, reinserts, three rounds of churn and a compaction keep recall@10 at 0.95 or more, and reinserted vectors are found |
| Records are matched by rule, with reasons | `grafeno/tests/resolve.rs`, `property_index.rs` (fuzzy), `src/textsim.rs` tests | measures against published values; strong keys decide either way; typos, swapped dates and namesakes; neighbors as evidence (and no links as no evidence); scans without indexes; pairs for deduplication; fuzzy indexes through updates, check and export |
| Contradicting facts are ended, not lost | `grafeno/tests/constraints.rs` | invalidation as a new version (`KNOWN AT` sees what was believed), late facts shortened, facts without dates starting now, the same fact again, flag, reject, target and pair scopes, `own_only` agents flagged, existing contradictions, property history |
| Common Cypher behaves as expected | `grafeno-query/tests/conformance.rs` (hand-written scenarios, not the official TCK), `cypher_more.rs`, `adversarial.rs`, `fuzz.rs` | 146 scenarios over MATCH, paths, RETURN, WITH, UNION, aggregation, nulls, numbers, strings, lists, subqueries, writes and errors; differences from Cypher pinned; nesting and budgets for the new forms |
| Graphs go out and come back | `grafeno/tests/interchange.rs`, `grafeno-server/tests/http.rs` | CSV and GraphML round trips with types, text and dates; re-imports that update; files from other tools with their own ids; bad rows reported |
| Every write is in the change log | `grafeno/tests/changes.rs`, `grafeno-server/tests/events.rs`, `upgrade.rs` | entries folded per record and numbered per namespace, nothing from aborted or failed writes in a group commit, the window and the gap, listeners; pages and server-sent events resuming after `Last-Event-ID`; NDJSON with keys, references and failing lines, retries as duplicates; signed webhooks, label filters, failures kept in place; extraction and summaries from the log; layout 11 upgrades with an empty log |
| Communities summarize themselves | `grafeno-memory/tests/communities.rs`, `grafeno-server/tests/http.rs` and `mcp.rs` community tests | a summary per community naming its center and topic, refreshes that change nothing, members joining in place, an agent's text kept and marked stale, levels as communities of communities, global search, drill-down, recall without summaries, context with an overview |
| Texts link to what they mention | `grafeno-memory/tests/extraction.rs`, `src/extract.rs` tests | patterns and their normalized values and offsets, phrases and sentence openers, the gazetteer on normalized text; known names, emails by index, value nodes, new names after two texts, namesakes settled by context or queued, decisions that last, dismissals, incremental runs, an agent's mentions through match rules |
| Merges are right and reversible | `grafeno/tests/merges.rs`, `grafeno-memory/tests/maintenance.rs` | strong keys, labels, conflicting facts and functional edges; namesakes, typos, nicknames, translations; a merge undone exactly (indexes, counters and the check agree) |
| Isolation between principals | `grafeno-server/tests/access.rs` | a matrix of keys, grants and endpoints: no read or write outside a key's grants, `own_only` keys change only their own records |
| GQ follows Cypher | `grafeno-query/tests/cypher.rs`, `queries.rs`, `statistics.rs`, `indexes.rs`, `algorithms.rs` | clauses, functions, aggregates, indexes, planner choices and algorithms on known graphs |
| The planner estimates from the data, and parallel scans change nothing | `grafeno-query/tests/planner.rs` | statistics against known distributions and degrees; starts chosen by value distribution, parameters and pattern cost; join order; statistics rebuilt after drift and index changes; a cached statement after its index is dropped; parallel rows and order equal one thread's, and limits hold across threads |
| Folded aggregates equal row-at-a-time ones and hold no rows | `grafeno-query/tests/batch.rs` | every aggregate with DISTINCT, nesting, ties, empty groups, overflow and duration sums; 90 000 rows counted under a `max_rows` of 2 000; parallel folds equal to one thread's |
| Batched and parallel traversal give the same rows in the same order | `grafeno-query/tests/traversal.rs` | eight pattern shapes (both directions from a middle anchor, edge variables and properties, targets filtered by earlier nodes, edge uniqueness, variable length with paths, bound nodes, row filters) against one anchor at a time; a hub's 12 000 two-hop rows on one and four threads; shared nodes read once; `max_rows` across slices |
| int8 searches keep their quality and compact entries read back exactly | `grafeno/tests/quantized.rs`, `src/store.rs` and `src/vector.rs` tests | recall@10 of 0.95 or more with exact scores; the copies through removal, replacement, compaction, backup and namespace drop; adjacency values round trip at every bound; quantized dot products within 0.02 |
| Costly queries stop | `grafeno-query/tests/adversarial.rs` | 32 hostile queries within their limits; nesting just under the limit runs on a 2 MiB stack; `max_cost` refuses; procedure rows count against `max_memory` |
| Nothing a client sends crashes the parser | `grafeno-query/tests/fuzz.rs` | proptest: random tokens, valid queries with random edits, arbitrary text, through the lexer, parser, planner, `EXPLAIN` and executor |
| The API keeps its contract | `grafeno-server/tests/http.rs`, `mcp.rs`, `shell.rs`, `clients/typescript/test` | every endpoint and MCP tool, limits, conflicts (409 with details), metrics, the CLI shell, and the client against the real binary |

### Reproducing

```bash
cargo bench -p grafeno --bench engine
cargo run --release -p grafeno --example hnsw_recall -- 20000 128 200
cargo run --release -p grafeno --example hnsw_churn
cargo run --release -p grafeno --example concurrency
cargo run --release -p grafeno --example bulk_load -- 200000
cargo run --release -p grafeno --example algo_bench
cargo run --release -p grafeno --example saturation -- 3
cargo run --release -p grafeno-query --example index_bench
cargo run --release -p grafeno --example resolve_bench -- 100000
cargo run --release -p grafeno-memory --example extract_bench -- 5000 20000
cargo run --release -p grafeno --example constraint_bench -- 10000 50000
cargo run --release -p grafeno-memory --example community_bench -- 1000 50
cargo run --release -p grafeno --example interchange_bench -- 100000
cargo run --release -p grafeno-query --example planner_bench -- 200000
cargo run --release -p grafeno-query --example batch_bench -- 200000
cargo run --release -p grafeno-query --example traversal_bench -- 200000
cargo run --release -p grafeno --example storage_bench -- 100000 50000
cargo run --release -p grafeno --example changes_bench -- 100000
cargo run --release -p grafeno-server --example events_bench -- 100000
cargo test --release -p grafeno-query --test adversarial -- --nocapture
```

The memory runs (E2) build the benchmark for Linux and run it with a hard
limit; the file lives on a named volume:

```bash
docker run --rm -v "$PWD:/src:ro" -v grafeno-target:/target -e CARGO_TARGET_DIR=/target -w /src rust:1.91 cargo build --release --locked -p grafeno-query --example ram_bench
docker run --rm --memory 2g --memory-swap 2g -v grafeno-target:/target -v grafeno-data:/data debian:bookworm-slim /target/release/examples/ram_bench build /data/big.redb 1_000_000 10_000_000
docker run --rm --memory 512m --memory-swap 512m -e CACHE_MB=128 -e PROJECTION_CACHE_MB=64 -v grafeno-target:/target -v grafeno-data:/data debian:bookworm-slim /target/release/examples/ram_bench ops /data/big.redb
```

(`vectors DB COUNT DIM` and `search DB DIM` measure vectors the same way.)

### What the measurements leave open

- Counting every edge of 10 M in GQ took longer than the 30 s limit.
  Since 12.3 aggregates fold as rows come and hold none, but each edge is
  each edge was matched one at a time; 12.4 batches the traversal and
  shares its reads. Not measured again in E2.
- Vector inserts slow down as the index grows: each 100 000 took 96 s at
  first and 809 s at the millionth (E2); a vector cache that keeps the
  most used entries, larger transactions and quantization (12.3) are the
  levers.
- With a projection cache smaller than a projection, every algorithm call
  rebuilds it (about 60 s on 10 M edges in E2) and GQ algorithms hit the
  30 s timeout.
- Opening a large file after a crash takes minutes while redb repairs it
  (461 s for 6.4 GB in E2).
- With eventual durability, the flush every 500 ms holds the writer for
  10 to 25 ms, which is the p99 of a write.
- A parallel scan of 200 000 candidates gains about 4.4 times on 16
  threads, not 16: each candidate still decodes its node and clones its
  row one at a time (12.3 is the lever).
- Not yet measured: Linux bare metal, spinning disks, ARM, multi-day
  runs, and HTTP end to end under load.

## Open decisions

- License (Grafito is AGPL-3.0; an engine for any agent may prefer
  MIT/Apache-2.0).
- Binary record format (postcard, rkyv).
- Whether Grafito's graph moves entirely to Grafeno or is only mirrored.
- Filtered vector search (labels, ACL) inside HNSW instead of post-filtering.
- Whether an application in front (Grafito) signs its users in itself and
  calls Grafeno as one service account, or maps them to Grafeno accounts
  (phase 14 allows both).
- On Windows, building needs the MSVC toolchain (Visual Studio Build Tools):
  tokio's `windows-sys` does not build with the self-contained GNU toolchain.
- Phase 13: `openraft` or a Raft of our own; the cluster transport (HTTP/2
  with the existing stack, or gRPC); the default read consistency
  (linearizable or bounded); and how floating-point aggregates merge across
  members so that results equal one server's.
- Phase 14: whether denials exist (they make exceptions to broad grants
  easy, as in Neo4j, but PostgreSQL does without them); whether hidden
  properties are absent or masked; session tokens signed (stateless,
  checked on any member) or stored (revocable at once); and the names of
  the built-in roles.
- Phase 15: Tauri or Electron for Grafeno Desktop; whether the interface is
  also published as static files for a web server of one's own (never
  served by the engine); whether the desktop app may download an engine
  when none is installed or only points to its installer; where the package
  repositories are hosted (GitHub Pages, a bucket, a service such as
  Cloudsmith or packagecloud) and who holds the signing keys; and a macOS
  build, not planned yet.
