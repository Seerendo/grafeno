# Grafeno

A graph engine in Rust for agent memory and knowledge: a property graph with
namespaces, facts with valid time and record time, provenance, full-text and
vector search, and hybrid recall that returns context within a token budget.
First client: [Grafito](https://github.com/Seerendo/grafito).

The design, phases and open decisions are in [DESIGN.md](DESIGN.md).

## Status

- **Phase 1, core:** storage on [redb](https://github.com/cberner/redb),
  transactions, neighbors, k hops, shortest path, subgraph.
- **Phase 2, retrieval:** BM25 full-text index, HNSW vector index, hybrid
  recall (RRF + graph expansion) and `context_for`.

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

```bash
cargo test
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
| `grafeno`        | Database, transactions, traversals, BM25 and HNSW indexes     |
| `grafeno-memory` | Hybrid recall and token-budgeted context                      |
