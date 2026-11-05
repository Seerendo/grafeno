# Historial de desarrollo / Development history

[Español](../README.md) · [English](../README.en.md)

Detalle de las fases descritas en el README original, conservado en su idioma
original. La presentación y los pasos para empezar están en los README enlazados.

The phases below are preserved from the original README. The linked READMEs
provide the current introduction and getting-started instructions.

- **Phase 1, core:** storage on [redb](https://github.com/cberner/redb),
  transactions, neighbors, k hops, shortest path, subgraph.
- **Phase 2, retrieval:** BM25 full-text index, HNSW vector index, hybrid
  recall (RRF + graph expansion) and `context_for`.
- **Phase 3, server:** the `grafeno` binary with an HTTP API (`grafeno serve`)
  and an MCP server for agents (`grafeno mcp`), a TypeScript client and a
  Docker image. See [crates/grafeno-server](../crates/grafeno-server/README.md).
- **Phase 4, foundations and validation:** namespaces and statistics, paged
  listings, integrity check and index rebuild, export/import, and property,
  durability and concurrency tests plus benchmarks
  ([docs/benchmarks.md](benchmarks.md)).
- **Phase 5, shell and query language:** `grafeno shell`, a console like
  `psql` for a file or a server, and [GQ](query-language.md), a
  Cypher-style language with time travel (`VALID AT`, `KNOWN AT`).
- **Phase 6, Studio and installers:** Grafeno Studio, the web interface
  (explorer, graph view with time travel, query editor, search and context
  lab, administration) served by `grafeno serve` at `/studio/`; a Windows
  installer with a Windows service; `.deb` and `.rpm` packages with a
  systemd service; `install.sh` for any Linux. See
  [docs/install.md](install.md).
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
  `PROFILE` ([docs/query-language.md](query-language.md)).
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
  databases ([apps/desktop/README.md](../apps/desktop/README.md)).
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
