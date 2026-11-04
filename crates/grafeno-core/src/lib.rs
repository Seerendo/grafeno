//! Grafeno data model: nodes, edges, identifiers and time.
//!
//! No I/O here: storage, indexes and traversals live in the `grafeno` crate.

mod error;
mod id;
mod model;
mod time;

pub use error::{Error, Result};
pub use id::{EdgeId, NodeId};
pub use model::{Edge, NewEdge, NewNode, Node, Props};
pub use serde_json::Value;
pub use time::{Timestamp, days_in_month};
