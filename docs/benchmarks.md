# Benchmarks and validation

Measured on 2026-11-03 on an AMD Ryzen 7 7435HS with 32 GB of RAM, Windows 11,
Rust 1.99 (release builds). Databases are in memory, so the numbers measure
the engine and redb without disk syncs; a durable commit to a file adds an
fsync per transaction.

```bash
cargo bench -p grafeno --bench engine
```

```bash
cargo run --release -p grafeno --example hnsw_recall -- 20000 128 200
```

## Engine (`benches/engine.rs`)

Corpus: notes of 60 words from a 2 000-word skewed vocabulary, four random
links per note.

| Operation                                           | Time      |
| --------------------------------------------------- | --------- |
| Write 100 nodes with text in one transaction        | 26 ms     |
| Write 1 node with text and a 64-d vector (2k nodes) | 3.0 ms    |
| Write 1 edge (own transaction)                      | 98 µs     |
| Get a node (10k nodes)                              | 1.4 µs    |
| Neighbors, both directions (10k nodes, 40k edges)   | 26 µs     |
| k-hop, depth 2                                      | 66 µs     |
| Shortest path, at most 6 hops                       | 11 ms     |
| BM25, 3-word query, top 10 (10k notes)              | 0.9 ms    |
| HNSW top 10, 10k uniform 128-d vectors, `ef` 32     | 1.5 ms    |
| HNSW top 10, same, `ef` 64 / 128                    | 2.4 / 3.7 ms |

## Vector index quality (`examples/hnsw_recall.rs`)

20 000 vectors of dimension 128 around 64 cluster centers (closer to real
embeddings than uniform noise), 200 queries, recall@10 against exact
brute-force results:

| `ef` | recall@10 | p50     | p99     |
| ---- | --------- | ------- | ------- |
| 16   | 0.957     | 0.43 ms | 0.95 ms |
| 32   | 0.996     | 0.54 ms | 1.17 ms |
| 64   | 1.000     | 0.62 ms | 1.25 ms |
| 128  | 1.000     | 0.89 ms | 1.77 ms |
| 256  | 1.000     | 1.20 ms | 2.16 ms |

Brute force takes 2.9 ms per query on the same data. Inserting the 20 000
vectors took 22.5 s (about 890 per second), and the integrity check passed
with no dangling links.

## Correctness tests

| Test                         | What it shows                                                                 |
| ---------------------------- | ----------------------------------------------------------------------------- |
| `tests/model.rs` (proptest)  | Random sequences of every write (including aborted transactions) leave the engine equal to a plain in-memory model: records, labels, counters, neighbors, complete text search, complete vector search. The same holds after export → import and after rebuilding the indexes. Passed 3 000 cases. |
| `tests/durability.rs`        | A child process writing transactions is killed three times at arbitrary moments; every reported commit survives, nothing is half-written, and the integrity check passes. |
| `tests/concurrency.rs`       | Four readers running while a writer commits always see whole transactions, and successive snapshots never go back. |
| `src/check.rs` tests         | Damage to each derived structure is detected, and `rebuild_indexes` repairs it. |
| `src/db.rs` tests            | Databases from layout version 1 get their counters; newer layouts are refused. |

## What the numbers say

The engine is correct under every test above, and fast for reads by id,
neighbors and full-text search. These are the bottlenecks found:

1. **Vector inserts (~1 ms each).** Each insert searches the graph with
   `ef_construction = 100`, reading and decoding every visited vector from
   redb, with a cache that only lives for that insert. A cache shared across
   a transaction, and storing vectors as raw `f32` (no copy on read), should
   help batch inserts most.
2. **Traversals decode whole edges.** `neighbors` reads and JSON-decodes the
   full edge record of every adjacency entry to apply the filter, so a
   6-hop shortest path over a 40k-edge graph takes 11 ms. Keeping the
   filter's fields (retracted, valid time) in the adjacency value, or a
   binary record format, would avoid most of it.
3. **Small transactions.** About 90 µs per commit even in memory: clients
   should batch writes (the HTTP API's `/batch` exists for this).
4. **Vector search vs brute force** is only about 5× at 20k vectors. The gap
   grows with size, but the per-vector redb read is the main cost here too.
