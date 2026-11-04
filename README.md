<p align="center"><img src="docs/assets/logo.svg" width="96" alt="Grafeno logo: three fused carbon rings of a graphene lattice"></p>

# Grafeno

A graph engine in Rust for agent memory and knowledge: a property graph with
namespaces, facts with valid time and record time, provenance, full-text and
vector search, hybrid recall that returns context within a token budget,
a Cypher-like query language (GQ) with statistics and property indexes, and
graph algorithms (paths, centrality, communities, similarity, embeddings).
First client: [Grafito](https://github.com/Seerendo/grafito).

The design, phases and open decisions are in [DESIGN.md](DESIGN.md).

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
