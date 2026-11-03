//! Agent memory on top of Grafeno: hybrid recall and token-budgeted context.
//!
//! [`recall`] fuses full-text (BM25) and vector results with reciprocal rank
//! fusion, then spreads part of each top result's score to its graph
//! neighbors. [`context_for`] packs the recalled nodes, and the facts that
//! connect them, into a prompt-ready text that fits a token budget.
//!
//! ```no_run
//! use grafeno::{Grafeno, NewNode};
//! use grafeno_memory::{ContextOptions, RecallQuery, context_for};
//!
//! let db = Grafeno::open("memory.redb")?;
//! db.write(|tx| {
//!     tx.create_node("agent", NewNode::new("Fact").text("The user prefers short answers"))
//! })?;
//!
//! let read = db.begin_read()?;
//! let query = RecallQuery::new().text("how should I answer the user?");
//! let context = context_for(&read, "agent", &query, &ContextOptions::with_budget(500))?;
//! println!("{}", context.rendered);
//! # Ok::<(), grafeno::Error>(())
//! ```

mod context;
mod recall;

pub use context::{
    Context, ContextFact, ContextItem, ContextOptions, context_for, estimate_tokens,
};
pub use recall::{Expansion, RecallQuery, Recalled, Signal, recall};
