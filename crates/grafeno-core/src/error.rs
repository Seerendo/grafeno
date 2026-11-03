use crate::{EdgeId, NodeId};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("node {0} not found")]
    NodeNotFound(NodeId),
    #[error("edge {0} not found")]
    EdgeNotFound(EdgeId),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("serialization error: {0}")]
    Serialization(String),
}

pub type Result<T> = std::result::Result<T, Error>;
