//! Stores a note and a person, then prints the context recalled for a query.
//!
//! Run with `cargo run -p grafeno-memory --example context`.

use grafeno::{Grafeno, NewEdge, NewNode, Timestamp};
use grafeno_memory::{ContextOptions, RecallQuery, context_for};

fn main() -> grafeno::Result<()> {
    let db = Grafeno::in_memory()?;
    db.write(|tx| {
        let ana = tx.create_node("vault", NewNode::new("Person").prop("name", "Ana"))?;
        let note = tx.create_node(
            "vault",
            NewNode::new("Note")
                .prop("title", "Rust notes")
                .text("Ownership rules and the borrow checker")
                .embedding(vec![0.12, 0.80, 0.33]),
        )?;
        tx.create_edge(
            "vault",
            NewEdge::new(ana, "wrote", note).valid_from(Timestamp::now()),
        )?;
        Ok(())
    })?;

    let read = db.begin_read()?;
    let query = RecallQuery::new()
        .text("borrow checker")
        .embedding(vec![0.10, 0.85, 0.30]);
    let context = context_for(&read, "vault", &query, &ContextOptions::with_budget(800))?;
    print!("{}", context.rendered);
    println!("({} tokens)", context.tokens);
    Ok(())
}
