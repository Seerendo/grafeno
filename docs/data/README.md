# Measurement data

The numbers behind `DESIGN.md` ("Measurements") and
[benchmarks](../benchmarks.md), one CSV per experiment, for charts and for
comparing later runs. Environments (E1-mem, E1-disk, E2) are described in
`DESIGN.md`; each file says which it used. Times are in the unit its column
names (`_us`, `_ms`, `_s`); memory is in MiB (2^20 bytes); empty cells were
not measured or do not apply. Every number comes from one run of a release
build unless the file says otherwise; on E1, runs vary by about ±20%.

| File | Experiment | Source | Environment | Date | Suggested chart |
| --- | --- | --- | --- | --- | --- |
| `engine.csv` | Core operations before and after the performance pass (layout 2 → 3) | `crates/grafeno/benches/engine.rs` | E1-mem | 2026-11-03 | horizontal bars, before / after, log scale |
| `single_writes.csv` | Cost of one write in its own transaction | engine bench | E1-mem | 2026-11-03 | bars |
| `hnsw_recall.csv` | Recall@10 and latency against `ef`, 20 000 vectors of 128 dimensions | `crates/grafeno/examples/hnsw_recall.rs` | E1-mem | 2026-11-03 | recall and p99 against `ef` (two axes, or two panels) |
| `hnsw_churn.csv` | Recall@10 through deletes, reinserts and churn, before and after 10.3 | `crates/grafeno/examples/hnsw_churn.rs` | E1-mem | 2026-11-04 | lines per state |
| `property_index.csv` | GQ queries with and without a property index, 100 000 nodes | `crates/grafeno-query/examples/index_bench.rs` | E1-mem | 2026-11-03 | paired bars, log scale |
| `concurrent_writes.csv` | 16 writers and 4 readers, with and without group commit | `crates/grafeno/examples/concurrency.rs` | E1-disk | 2026-11-03 | grouped bars |
| `bulk_load.csv` | Imports of 200 000 and 1 000 000 nodes | `crates/grafeno/examples/bulk_load.rs` | E1-disk | 2026-11-03 | bars, nodes per second |
| `algorithms.csv` | Graph algorithms on 100 000 nodes and 500 000 edges | `crates/grafeno/examples/algo_bench.rs` | E1-mem | 2026-11-04 | horizontal bars, log scale |
| `hostile_queries.csv` | 32 hostile GQ queries and the limit that stopped each | `crates/grafeno-query/tests/adversarial.rs` | E1-mem | 2026-11-05 | bars colored by outcome |
| `ram_graph.csv` | Memory and time per operation, 1 M nodes and 10 M edges, 2 GiB and 512 MiB | `crates/grafeno-query/examples/ram_bench.rs` | E2 | 2026-11-05 | steady and peak bars per operation, one panel per container |
| `ram_graph_before_10_6.csv` | The same operations before the changes of 10.6 | `ram_bench.rs` | E2 | 2026-11-05 | before / after |
| `check_cost.csv` | `check` on 10 M edges, three implementations | `ram_bench.rs` | E2 | 2026-11-05 | bars |
| `projection_size.csv` | Projection memory with and without edge ids | `ram_bench.rs` | E2 | 2026-11-05 | bars |
| `vector_insert_progress.csv` | Inserting 1 M vectors of 128 dimensions, per 100 000 | `ram_bench.rs vectors` | E2 (2 GiB) | 2026-11-05 | step seconds against vectors inserted |
| `vector_insert_cache.csv` | 300 000 vectors with a 128 MiB and a 1 GiB vector cache | `ram_bench.rs vectors` | E2 (2 GiB) | 2026-11-05 | grouped bars per 100 000 |
| `vector_search.csv` | 1 000 searches on 1 M and 300 000 vectors | `ram_bench.rs search` | E2 | 2026-11-05 | bars, ms per search and MiB |
| `saturation_immediate.csv` | Write latency against offered load, immediate durability, `max_batch` 128 and 512 | `crates/grafeno/examples/saturation.rs` | E1-disk | 2026-11-05 | p50 / p99 / p999 against offered load (log–log), throughput against offered load |
| `saturation_eventual.csv` | The same with eventual durability | `saturation.rs` | E1-disk | 2026-11-05 | as above |
| `resolve_speed.csv` | Fuzzy index against a scan, and `find_matches`, 100 000 people | `crates/grafeno/examples/resolve_bench.rs` | E1-mem | 2026-11-05 | bars, log scale |
| `resolve_quality.csv` | A match rule's decisions on 2 000 damaged duplicates and 2 000 namesakes | `resolve_bench.rs` | E1-mem | 2026-11-05 | grouped bars of shares per kind |
| `constraint_cost.csv` | 50 000 job changes with and without an edge constraint | `crates/grafeno/examples/constraint_bench.rs` | E1-mem | 2026-11-05 | bars |
| `community_refresh.csv` | Refreshes of community summaries: first, unchanged, 1% new nodes | `crates/grafeno-memory/examples/community_bench.rs` | E1-mem | 2026-11-05 | bars |
| `community_levels.csv` | Summaries per level and their purity against planted teams and departments | `community_bench.rs` | E1-mem | 2026-11-05 | table |
| `extract_runs.csv` | Extraction runs: first, unchanged, 1% changed | `crates/grafeno-memory/examples/extract_bench.rs` | E1-mem | 2026-11-05 | bars, log scale |
| `extract_quality.csv` | Links against the known mentions of 20 000 memories | `extract_bench.rs` | E1-mem | 2026-11-05 | a table, or stat tiles |
| `import_interference.csv` | An agent's write latency during a 300 000-node import | `saturation.rs` | E1-disk | 2026-11-05 | bars of p99 per import mode, log scale |

`python docs/charts/build.py` draws the charts of
[performance](../performance.md) from these files (light and dark SVG in
`docs/assets/charts/`). To add a run, keep the columns, add rows with a new date (or a new file
for a new environment), and say in `DESIGN.md` what changed.
