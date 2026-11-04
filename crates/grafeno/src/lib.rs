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

mod access;
pub mod algo;
mod backup;
mod check;
mod db;
mod dump;
mod index;
mod memory;
mod namespace;
mod stats;
mod store;
mod text;
mod traverse;
mod vector;
mod writer;

pub use access::{Grant, Principal, Role, check_principal_name};
pub use backup::BackupStats;
pub use check::{CheckReport, CompactReport, RebuildReport};
pub use db::{Actor, Durability, FileIntegrity, Grafeno, ReadTx, SCHEMA_VERSION, WriteTx};
pub use dump::{DumpStats, EXPORT_FORMAT, EXPORT_VERSION, ImportOptions, Record};
pub use grafeno_core::*;
pub use index::{IndexDef, IndexInfo, IndexRange};
pub use memory::{DEFAULT_IMPORTANCE, MemoryState, MergeReport, names_of, normalize_name};
pub use namespace::{DropStats, MAX_NAMESPACE_LENGTH, check_new_namespace};
pub use stats::{NamespaceInfo, NamespaceStats};
pub use text::tokenize;
pub use traverse::{Direction, EdgeFilter, Neighbor, Path, Subgraph};
pub use writer::{Writer, WriterOptions};

/// A search result: a node and its score (BM25 for text, cosine similarity
/// for vectors). Higher is better.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct Hit {
    pub node: NodeId,
    pub score: f32,
}
