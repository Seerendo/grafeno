//! Grafeno: a graph engine for agent memory and knowledge.
//!
//! ```no_run
//! use grafeno::{Direction, EdgeFilter, Grafeno, NewEdge, NewNode};
//!
//! let db = Grafeno::open("memory.redb")?;
//! let (ana, rust) = db.write(|tx| {
//!     let ana = tx.create_node("vault", NewNode::new("Person").prop("name", "Ana"))?;
//!     let rust = tx.create_node(
//!         "vault",
//!         NewNode::new("Topic").prop("name", "Rust").text("Rust is a systems language"),
//!     )?;
//!     tx.create_edge("vault", NewEdge::new(ana, "knows_about", rust))?;
//!     Ok((ana, rust))
//! })?;
//!
//! let read = db.begin_read()?;
//! let topics = read.neighbors("vault", ana, Direction::Out, &EdgeFilter::any())?;
//! assert_eq!(topics[0].node, rust);
//! assert_eq!(read.search_text("vault", "systems", 5)?[0].node, rust);
//! # Ok::<(), grafeno::Error>(())
//! ```

mod db;
mod store;
mod text;
mod traverse;
mod vector;

pub use db::{Grafeno, ReadTx, WriteTx};
pub use grafeno_core::*;
pub use text::tokenize;
pub use traverse::{Direction, EdgeFilter, Neighbor, Path, Subgraph};

/// A search result: a node and its score (BM25 for text, cosine similarity
/// for vectors). Higher is better.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct Hit {
    pub node: NodeId,
    pub score: f32,
}
