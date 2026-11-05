<p align="center"><img src="docs/assets/logo.svg" width="96" alt="Grafeno logo: three fused carbon rings of a graphene lattice"></p>

# Grafeno

A graph engine in Rust for agent memory and knowledge: a property graph with
namespaces, facts with valid time and record time, provenance, full-text and
vector search, hybrid recall that returns context within a token budget,
a Cypher-like query language (GQ) with statistics and property indexes, and
graph algorithms (paths, centrality, communities, similarity, embeddings).
First client: [Grafito](https://github.com/Seerendo/grafito).

The design, phases and open decisions are in [DESIGN.md](DESIGN.md);
measured speed, memory and limits, with charts, in
[docs/performance.md](docs/performance.md).

## Status

- **Phase 1, core:** storage on [redb](https://github.com/cberner/redb),
  transactions, neighbors, k hops, shortest path, subgraph.
- **Phase 2, retrieval:** BM25 full-text index, HNSW vector index, hybrid
  recall (RRF + graph expansion) and `context_for`.
- **Phase 3, server:** the `grafeno` binary with an HTTP API (`grafeno serve`)
  and an MCP server for agents (`grafeno mcp`), a TypeScript client and a
  Docker image. See [crates/grafeno-server](crates/grafeno-server/README.md).
- **Phase 4, foundations and validation:** namespaces and statistics, paged
  listings, integrity check and index rebuild, export/import, and property,
  durability and concurrency tests plus benchmarks
  ([docs/benchmarks.md](docs/benchmarks.md)).
- **Phase 5, shell and query language:** `grafeno shell`, a console like
  `psql` for a file or a server, and [GQ](docs/query-language.md), a
  Cypher-style language with time travel (`VALID AT`, `KNOWN AT`).
- **Phase 6, Studio and installers:** Grafeno Studio, the web interface
  (explorer, graph view with time travel, query editor, search and context
  lab, administration) served by `grafeno serve` at `/studio/`; a Windows
  installer with a Windows service; `.deb` and `.rpm` packages with a
  systemd service; `install.sh` for any Linux. See
  [docs/install.md](docs/install.md).
- **Phase 7, memory:** entities found by normalized names and aliases (MCP
  `remember` updates them instead of duplicating), importance, accesses and
  decay in recall ranking, duplicate detection and merging, archiving, and
  consolidation of old memories into summaries; a Memory view in the Studio.
- **Studio redesign:** a new look with light and dark themes; nodes, labels,
  properties, text, importance and facts are created and edited from the
  interface (explorer, graph and dialogs); a script workspace with tabs,
  GQ highlighting and completion, run-at-cursor or run-all, and results as a
  table, JSON or graph; a command palette (Ctrl+K). The logo is a piece of
  graphene: three fused carbon rings.
- **Phase 8.1, complete GQ:** `OPTIONAL MATCH`, `WITH`, `UNWIND`, `UNION`,
  `MERGE`, `FOREACH`, `CALL` procedures, `CASE`, list comprehensions,
  quantifiers, `reduce`, pattern predicates in `WHERE`, `EXPLAIN` and
  `PROFILE` ([docs/query-language.md](docs/query-language.md)).
- **Phase 8.2, statistics and time:** `stdev`, `variance`, `median`,
  percentiles and `mode`; durations with calendar arithmetic,
  `duration.between`, date parts and `date.truncate` for grouping by period;
  `temporal.series` with per-row `VALID AT` to count what was true at each
  point; bar and line charts in the Studio's script results.
- **Phase 8.3, indexes and safe queries:** `CREATE INDEX`, unique
  constraints, a planner that starts from indexes or the smallest label and
  stops early for `LIMIT`; per-query limits (time, records read, rows held)
  and cancellation, so one query cannot take the service down; an indexes
  panel in the Studio.
- **Phase 8.4, concurrent and bulk work:** group commit for concurrent
  writes (11 times the writes per second on disk), eventual durability,
  bulk imports in chunks, and 503 with `Retry-After` when the server is
  full.
- **Phase 8.5, graph algorithms:** `CALL algo.*` for weighted paths
  (Dijkstra, A*, Yen, all shortest paths), centrality (degree, PageRank,
  betweenness, closeness), communities (components, label propagation,
  Louvain, triangles), similarity, link prediction and FastRP embeddings,
  on a cached in-memory projection; over HTTP too, and in the Studio's
  graph (color by community, size by centrality).
- **Phase 9.1, namespaces you create:** namespaces are records of their
  own (layout version 6): empty ones stay listed, creating an existing one
  is a clear conflict, and dropping one deletes everything in it; over
  HTTP, the shell (`\ns create`, `\ns drop`), GQ (`SHOW NAMESPACES`) and
  the Studio.
- **Phase 9.2, graph lab design:** a new design for the Studio, dark first,
  with a floating rail and command bar over the canvas, Space Grotesk and
  JetBrains Mono bundled for offline use, and glowing nodes.
- **Phase 9.3, a graph you can handle:** the Studio's graph is a live
  physics simulation on its own canvas: drag and pin nodes, lasso and
  move groups, connect by dragging, context menus, a minimap, and
  communities that collapse into one node.
- **Phase 9.4, desktop application:** `apps/desktop`, a Tauri app that
  opens database files with the engine inside the app (no service, no
  port) or connects to a server, with native menus, dialogs and recent
  databases ([apps/desktop/README.md](apps/desktop/README.md)).
- **Phase 9.5, installers and icons:** Grafeno Desktop installers for
  Windows (`.exe`, `.msi`) and Linux (`.deb`, `.rpm`, `.AppImage`) with
  `.redb` files opening in it, every icon rendered from one source, and a
  Grafeno Studio menu entry with its icon in the server's Linux packages.
- **Phase 10.1, isolation and authorization:** API keys with grants per
  namespace (read, write, admin, prefixes), owners, and keys limited to
  their own records; every write records who made it (`@recorded_by`);
  every route checked by one policy and tested against leaks; `grafeno
  keys`, MCP fences (`--allow`, `--read-only`, `--as`) and a keys panel in
  the Studio.
- **Phase 10.2, integrity under failure:** crash tests during imports,
  rebuilds, drops and upgrades; databases written by every past layout
  version open and upgrade; damaged files are refused by a checksum
  check (`grafeno check`) instead of crashing; online backups (`grafeno
  backup`, `GET /v1/backup`, the Studio).
- **Phase 10.3, vector search under change:** recall@10 measured after
  deletions, reinserts and churn; four repair bugs fixed (a reinserted
  vector could become its own only neighbor), so recall stays at 0.99 or
  more; `grafeno compact` and a recall estimate (`grafeno check --recall`).
- **Phase 10.4, costly queries:** 32 hostile queries (products, long paths,
  huge `UNWIND`s and reduces, deep nesting) all stop within their limits; a
  memory limit for values and held rows, time checked inside computing
  loops, a nesting limit, cost estimates in `EXPLAIN` with an optional
  `max_cost`, and a fuzz test that the parser and planner never panic.
- **Phase 10.5, correct merges:** merges refuse nodes that disagree (two
  emails, a person and a company, two birth dates) unless forced; duplicate
  detection follows the same identity rules and catches typos; every merge
  is recorded (who, when, both nodes as they were) and can be undone.
- **Phase 10.6, memory under large graphs:** measured with 10 M edges and
  1 M vectors in 2 GiB and 512 MiB containers; caches sized by bytes
  (`[memory]`), projections at about 12 bytes per edge, a streaming
  `check` ten times faster, and GQ algorithms bounded by `max_memory` and
  `top`.
- **Phase 10.7, one writer:** saturation measured (about 2 ms a write up
  to 4 000 a second with immediate durability, 11 000 at most); imports
  go through a bulk lane, so an agent writing during a large import waits
  88 ms at p99 instead of the whole import; `GET /v1/metrics` for
  Prometheus. Phase 10 (hardening) is complete.
- **Phase 11.0, entity resolution:** text similarity in GQ (Levenshtein,
  Damerau, Jaro-Winkler, n-grams, Soundex and a Spanish phonetic key),
  fuzzy trigram indexes (`CREATE FUZZY INDEX`, about 23 times faster than
  comparing with every node), and match rules per label with strong
  keys, weighted fields, neighbors as evidence and explained decisions
  (`db.match`, `POST /v1/{ns}/match`, the `find_matches` MCP tool).
- **Phase 11.1, extraction:** memories are read for what they mention
  (known names through a gazetteer, emails, URLs, handles, hashtags,
  dates, amounts, identifiers, recurring new names) and linked by
  `mentions` edges; namesakes are settled by context or queued for
  review; runs read only what changed (`grafeno extract`, the `extract`
  MCP tool); agents can send their own mentions.
- **Phase 11.2, contradicting facts:** edge constraints (`REQUIRE ONE PER
  SOURCE`, `TARGET` or `PAIR`) end the old fact in valid time when a new
  one contradicts it, as a new version so `KNOWN AT` keeps what was
  believed; or flag, or reject; functional properties keep their
  earlier values (`history(n, 'city')`).
- **Phase 11.3, community summaries:** Louvain levels kept as a
  hierarchy, each community summarized without a model (central members,
  distinctive terms, label mix, key facts, time span) as a `Community`
  node; refreshes rewrite only what changed and keep an agent's own text;
  `overview` answers broad questions and drills down.
- **Phase 11.4, following the writes:** a change log numbered per
  namespace (layout 12), streamed as server-sent events
  (`GET /v1/{ns}/changes`), pushed to signed webhooks, polled by the MCP
  `changes` tool; NDJSON ingestion with idempotency keys; extraction and
  community summaries can follow the log in the background
  (`[background]`).
- **Phase 12.1, wider Cypher and tools:** path variables, `shortestPath`,
  `CALL { … }`, `COUNT { … }`, pattern comprehensions, map projections,
  `=~` and more functions; import and export in CSV and GraphML; 146
  hand-written conformance scenarios in the TCK's form (the official TCK is
  not run, and Bolt is not built).
- **Phase 12.2, query optimization and concurrency:** planner statistics
  (common values and buckets per index, edge degrees by direction) kept in
  memory and rebuilt as data drifts; starts and pattern order by estimated
  cost; parsed statements cached; large read scans split across threads.

## Installing

**Grafeno Desktop**, the desktop application (database files with the
engine inside, or a connection to a server):

| System | Install |
| ------ | ------- |
| Windows | `grafeno-desktop-<version>-windows-x64-setup.exe` or `.msi` |
| Debian, Ubuntu | `sudo apt install ./grafeno-desktop_<version>_amd64.deb` |
| Fedora, RHEL | `sudo dnf install ./grafeno-desktop-<version>-1.x86_64.rpm` |
| Any Linux | `grafeno-desktop-<version>-x86_64.AppImage` |

**The Grafeno server**, as a service for agents and applications:

| System | Install |
| ------ | ------- |
| Windows | `grafeno-<version>-windows-x64-setup.exe` |
| Debian, Ubuntu | `sudo apt install ./grafeno_<version>-1_amd64.deb` |
| Fedora, RHEL | `sudo dnf install ./grafeno-<version>-1.x86_64.rpm` |
| Any Linux | `curl -fsSL https://raw.githubusercontent.com/Seerendo/grafeno/main/install.sh \| sh` |

Then open Grafeno Studio at <http://127.0.0.1:7810/studio/>, with the token
from `grafeno config token`. Details, Docker and settings:
[docs/install.md](docs/install.md).

```text
$ grafeno shell --db memory.redb -n vault
vault=> MATCH (p:Person)-[w:works_at]->(c) VALID AT '2022-01-01'
vault->   RETURN p.name, c.name, date(w.@valid_from) AS since;
 p.name | c.name | since
--------+--------+------------
 Ana    | Acme   | 2020-01-01
(1 row)
```

Graph algorithms run from GQ too
([docs/query-language.md](docs/query-language.md#graph-algorithms)):

```text
vault=> CALL algo.pageRank({labels: 'Person'}) YIELD node, score
vault->   RETURN node.name, score LIMIT 3;
vault=> CALL algo.louvain() YIELD node, community SET node.community = community;
```

Give an agent long-term memory over MCP, for example in Claude Code:

```bash
cargo install --path crates/grafeno-server
```

```bash
claude mcp add grafeno -- grafeno mcp --db ~/.grafeno/memory.redb
```

Or embed it as a library:

```rust
use grafeno::{Grafeno, NewEdge, NewNode, Timestamp};
use grafeno_memory::{ContextOptions, RecallQuery, context_for};

let db = Grafeno::open("memory.redb")?;
db.write(|tx| {
    let ana = tx.create_node("vault", NewNode::new("Person").prop("name", "Ana"))?;
    let note = tx.create_node(
        "vault",
        NewNode::new("Note")
            .prop("title", "Rust notes")
            .text("Ownership rules and the borrow checker")
            .embedding(vec![0.12, 0.80, 0.33]),
    )?;
    tx.create_edge("vault", NewEdge::new(ana, "wrote", note).valid_from(Timestamp::now()))?;
    Ok(())
})?;

let read = db.begin_read()?;
let query = RecallQuery::new()
    .text("borrow checker")
    .embedding(vec![0.10, 0.85, 0.30]);
let context = context_for(&read, "vault", &query, &ContextOptions::with_budget(800))?;
println!("{}", context.rendered);
// ## Memory
// [1] Rust notes (Note)
// Ownership rules and the borrow checker
//
// [2] Ana (Person)
//
// ## Facts
// - Ana wrote Rust notes (from 2026-11-03)
```

## Development

On Windows, install Visual Studio Build Tools (C++ workload) and use the MSVC
Rust toolchain.

```bash
cargo test
```

```bash
cd clients/typescript && npm test
```

```bash
cargo clippy --all-targets
```

```bash
cargo run -p grafeno-memory --example context
```

| Crate            | Contents                                                     |
| ---------------- | ------------------------------------------------------------ |
| `grafeno-core`   | Model: `Node`, `Edge`, UUID v7 ids, `Timestamp`              |
| `grafeno`        | Database, transactions, traversals, BM25, HNSW and property indexes, group commit, graph algorithms (`algo`) |
| `grafeno-memory` | Hybrid recall and token-budgeted context                      |
| `grafeno-query`  | GQ: parser, planner and executor of the query language, `CALL algo.*` |
| `grafeno-server` | The `grafeno` binary: HTTP API, MCP server, shell, admin CLI   |
| `clients/typescript` | `grafeno-client`, the TypeScript HTTP client              |
| `apps/desktop`   | `grafeno-desktop`, the desktop application (Tauri)            |
