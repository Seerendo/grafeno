# Benchmarks and validation

Measured on 2026-11-03 on an AMD Ryzen 7 7435HS with 32 GB of RAM, Windows 11,
Rust 1.99 (release builds). Databases are in memory, so the numbers measure
the engine and redb without disk syncs; a durable commit to a file adds an
fsync per transaction. Runs on this laptop vary by about ±20%: compare
changes larger than that.

```bash
cargo bench -p grafeno --bench engine
```

```bash
cargo run --release -p grafeno --example hnsw_recall -- 20000 128 200
```

## Engine (`benches/engine.rs`)

Corpus: notes of 60 words from a 2 000-word skewed vocabulary, four random
links per note. "Before" is layout version 2 (phase 4), "after" is the
performance pass (layout version 3).

| Operation                                           | Before  | After   |
| --------------------------------------------------- | ------- | ------- |
| Write 100 nodes with text in one transaction        | 26 ms   | 17 ms   |
| Write 1 node with text and a 64-d vector (2k nodes) | 3.0 ms  | 3.5 ms  |
| Write 1 edge (own transaction)                      | 98 µs   | 120 µs  |
| Get a node (10k nodes)                              | 1.4 µs  | 1.6 µs  |
| Neighbors, both directions (10k nodes, 40k edges)   | 26 µs   | 25 µs   |
| k-hop, depth 2                                      | 66 µs   | 16 µs   |
| Shortest path, at most 6 hops                       | 11 ms   | 3.2 ms  |
| BM25, 3-word query, top 10 (10k notes)              | 0.90 ms | 0.27 ms |
| HNSW top 10, 10k uniform 128-d vectors, `ef` 32     | 1.5 ms  | 1.1 ms  |
| HNSW top 10, same, `ef` 64 / 128                    | 2.4 / 3.7 ms | 1.8 / 2.8 ms |

The single-write rows did not change in code paths that matter (getting a
node was not touched at all), so their differences are noise.

## Vector index quality (`examples/hnsw_recall.rs`)

20 000 vectors of dimension 128 around 64 cluster centers (closer to real
embeddings than uniform noise), 200 queries, recall@10 against exact
brute-force results, after the performance pass:

| `ef` | recall@10 | p50     | p99     |
| ---- | --------- | ------- | ------- |
| 16   | 0.956     | 0.32 ms | 0.61 ms |
| 32   | 0.996     | 0.41 ms | 0.72 ms |
| 64   | 1.000     | 0.51 ms | 0.98 ms |
| 128  | 1.000     | 0.79 ms | 1.56 ms |
| 256  | 1.000     | 1.14 ms | 2.09 ms |

Brute force takes about 3 ms per query on the same data. Inserting the
20 000 vectors (batches of 1 000) went from 22.5 s to about 9 s (890 → about
2 200 per second); the integrity check passes with no dangling links.
`ef_construction` 64 instead of 100 gave the same recall and a difference
within the noise, so it stays at 100.

## Where single writes spend their time

Measured with each write in its own transaction, on 2 000 notes:

| Write                         | Time    |
| ----------------------------- | ------- |
| Empty node (commit cost)      | 90 µs   |
| Node with 60 words of text    | 740 µs  |
| Node with a 64-d vector       | 1.8 ms  |

redb is a copy-on-write B-tree: the first change to a page in a transaction
copies it. A text touches one posting page per distinct term, and a vector
rewrites the link lists of about 17 nodes spread over the table. Batching
writes (the HTTP API's `/batch`, or one `write` for many nodes) shares those
copies: 100 notes with text cost 17 ms together, 170 µs each.

## What changed in the performance pass

1. **Traversals read no edge records to filter.** Adjacency entries carry
   the edge's record time and valid time (layout version 3), so `k_hop`,
   `shortest_path`, `subgraph` and recall's graph expansion only read
   adjacency ranges. `neighbors` still reads the record of each edge it
   returns.
2. **Postings carry the text length,** so BM25 ranking needs no lookup per
   matching document.
3. **Vector distances need no copies.** Searches compute dot products
   straight from the stored bytes, with eight accumulators the compiler
   vectorizes.
4. **Write transactions cache the vector graph.** Decoded vectors and link
   lists used by inserts and removes stay in a cache that every write updates
   and that dies with the transaction, so batch inserts stop re-reading the
   same entries.

## Correctness tests

| Test                         | What it shows                                                                 |
| ---------------------------- | ----------------------------------------------------------------------------- |
| `tests/model.rs` (proptest)  | Random sequences of every write (including aborted transactions, valid times and invalidations) leave the engine equal to a plain in-memory model: records, labels, counters, neighbors, complete text search, complete vector search. Traversal filters on adjacency entries agree with the same filters on the edge records, for every direction, valid time, record time and label. The same holds after export → import and after rebuilding the indexes. Passed 3 000 cases. |
| `tests/durability.rs`        | A child process writing transactions is killed three times at arbitrary moments; every reported commit survives, nothing is half-written, and the integrity check passes. |
| `tests/concurrency.rs`       | Four readers running while a writer commits always see whole transactions, and successive snapshots never go back. |
| `src/check.rs` tests         | Damage to each derived structure is detected, and `rebuild_indexes` repairs it. |
| `src/db.rs` tests            | Layout version 1 and 2 files are upgraded on open (counters; new adjacency and postings, old tables dropped); newer layouts are refused. |

## Property indexes (`crates/grafeno-query/examples/index_bench.rs`)

One GQ query on 100,000 `:Person` nodes (ages 0 to 99), in memory, release
build, before and after `CREATE INDEX FOR (p:Person) ON (p.age)`. Reads are
the records counted by `PROFILE`.

| Query | Without index | With index |
| --- | --- | --- |
| `WHERE p.age > 97` (2% of the nodes), `count(p)` | 233 ms, 100,000 reads | 10.7 ms, 6,000 reads |
| `{age: 42}` (1%), `count(p)` | 200 ms, 100,000 reads | 3.6 ms, 2,000 reads |
| `WHERE p.age = 42 … LIMIT 10` | 2.4 ms, 1,024 reads | 0.38 ms, 1,064 reads |

Without an index, a `LIMIT` already stops the scan after the first page of
candidates. A full label scan costs about 2 µs per node, most of it
decoding the node record.

## Remaining limits

- **Vector search reads each visited vector from redb** (about 0.4 µs per
  B-tree lookup). At 20k vectors that is most of a search's time; an
  in-memory copy of a namespace's vectors would remove it, at the cost of
  memory and of keeping it in sync across transactions.
- **Single small transactions** pay for page copies and the commit; clients
  should batch.
- **Edge filters by label alone** (`edges_page` with a label) scan the
  namespace's edges.
