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
mod changes;
mod check;
mod constraints;
mod db;
mod dump;
mod index;
mod interchange;
mod memory;
mod merge;
mod namespace;
mod resolve;
mod state;
mod stats;
mod store;
mod text;
pub mod textsim;
mod traverse;
mod vector;
mod writer;

pub use access::{Grant, Principal, Role, check_principal_name};
pub use backup::BackupStats;
pub use changes::{Change, ChangeOp, ChangePage, Part, Retention};
pub use check::{CheckReport, CompactReport, RebuildReport};
pub use constraints::{Action, Constraint, ConstraintEvent, HistoryEntry, OnConflict, Scope};
pub use db::{
    Actor, DEFAULT_CACHE, Durability, FileIntegrity, Grafeno, OpenOptions, ReadTx, SCHEMA_VERSION,
    WriteTx,
};
pub use dump::{DumpStats, EXPORT_FORMAT, EXPORT_VERSION, ImportOptions, Record};
pub use grafeno_core::*;
pub use index::{FuzzyHit, IndexDef, IndexInfo, IndexRange};
pub use interchange::{IMPORT_ID, InterchangeOptions, InterchangeStats};
pub use memory::{DEFAULT_IMPORTANCE, MemoryState, names_of, normalize_name};
pub use merge::{
    Conflict, ConflictKind, DEFAULT_STRONG_KEYS, MergeOptions, MergeRecord, MergeReport,
    NodeSnapshot, node_conflicts,
};
pub use namespace::{DropStats, MAX_NAMESPACE_LENGTH, check_new_namespace};
pub use resolve::{Decision, FieldRule, FieldScore, Match, MatchRule, Probe, SCAN_LIMIT, score};
pub use stats::{NamespaceInfo, NamespaceStats};
pub use text::tokenize;
pub use traverse::{Direction, EdgeFilter, Neighbor, Path, Subgraph};
pub use writer::{Histogram, HistogramSnapshot, Lane, Writer, WriterOptions, WriterSnapshot};

/// A search result: a node and its score (BM25 for text, cosine similarity
/// for vectors). Higher is better.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct Hit {
    pub node: NodeId,
    pub score: f32,
}
