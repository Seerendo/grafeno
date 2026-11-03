//! redb tables and the reads shared by read and write transactions.

use grafeno_core::{EdgeId, Error, NodeId, Result};
use redb::{ReadableTable, TableDefinition};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub(crate) type RecordKey = (&'static str, u128);
pub(crate) type AdjKey = (&'static str, u128, &'static str, u128);
pub(crate) type LabelKey = (&'static str, &'static str, u128);
pub(crate) type PostingKey = (&'static str, &'static str, u128);
pub(crate) type LinkKey = (&'static str, u128, u8);

/// `(ns, node_id) -> Node`
pub(crate) const NODES: TableDefinition<RecordKey, &[u8]> = TableDefinition::new("nodes");
/// `(ns, edge_id) -> Edge`
pub(crate) const EDGES: TableDefinition<RecordKey, &[u8]> = TableDefinition::new("edges");
/// `(ns, from, label, edge_id) -> to`
pub(crate) const ADJ_OUT: TableDefinition<AdjKey, u128> = TableDefinition::new("adj_out");
/// `(ns, to, label, edge_id) -> from`
pub(crate) const ADJ_IN: TableDefinition<AdjKey, u128> = TableDefinition::new("adj_in");
/// `(ns, label, node_id) -> ()`
pub(crate) const NODE_LABELS: TableDefinition<LabelKey, ()> = TableDefinition::new("node_labels");

/// `(ns, node_id) -> text`: the indexed text of a node.
pub(crate) const TEXTS: TableDefinition<RecordKey, &str> = TableDefinition::new("texts");
/// `(ns, node_id) -> number of terms` in the indexed text.
pub(crate) const TEXT_LENGTHS: TableDefinition<RecordKey, u32> =
    TableDefinition::new("text_lengths");
/// `(ns, term, node_id) -> term frequency`
pub(crate) const POSTINGS: TableDefinition<PostingKey, u32> = TableDefinition::new("postings");
/// `ns -> (indexed documents, total terms)`
pub(crate) const TEXT_STATS: TableDefinition<&str, (u64, u64)> = TableDefinition::new("text_stats");

/// `(ns, node_id) -> normalized f32 vector` (little endian).
pub(crate) const VECTORS: TableDefinition<RecordKey, &[u8]> = TableDefinition::new("vectors");
/// `(ns, node_id, layer) -> neighbor ids` (u128, little endian) of the HNSW graph.
pub(crate) const HNSW_LINKS: TableDefinition<LinkKey, &[u8]> = TableDefinition::new("hnsw_links");
/// `ns -> VectorMeta` (JSON).
pub(crate) const VECTOR_META: TableDefinition<&str, &[u8]> = TableDefinition::new("vector_meta");

pub(crate) fn storage(error: impl std::fmt::Display) -> Error {
    Error::Storage(error.to_string())
}

pub(crate) fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|e| Error::Serialization(e.to_string()))
}

pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes).map_err(|e| Error::Serialization(e.to_string()))
}

pub(crate) fn check_namespace(ns: &str) -> Result<()> {
    if ns.is_empty() {
        return Err(Error::Invalid("namespace must not be empty".into()));
    }
    Ok(())
}

pub(crate) fn check_label(label: &str) -> Result<()> {
    if label.is_empty() || label.trim() != label {
        return Err(Error::Invalid(format!("invalid label {label:?}")));
    }
    Ok(())
}

pub(crate) fn read_record<R: DeserializeOwned>(
    table: &impl ReadableTable<RecordKey, &'static [u8]>,
    ns: &str,
    id: u128,
) -> Result<Option<R>> {
    match table.get((ns, id)).map_err(storage)? {
        Some(guard) => decode(guard.value()).map(Some),
        None => Ok(None),
    }
}

/// Every record of a namespace, in id (creation) order.
pub(crate) fn scan_records<R: DeserializeOwned>(
    table: &impl ReadableTable<RecordKey, &'static [u8]>,
    ns: &str,
) -> Result<Vec<R>> {
    let mut out = Vec::new();
    for item in table.range((ns, 0u128)..).map_err(storage)? {
        let (key, value) = item.map_err(storage)?;
        if key.value().0 != ns {
            break;
        }
        out.push(decode(value.value())?);
    }
    Ok(out)
}

/// The edges of `node` in an adjacency table: `(edge, other end)`.
/// With `label`, only edges with that label.
pub(crate) fn adjacency(
    table: &impl ReadableTable<AdjKey, u128>,
    ns: &str,
    node: NodeId,
    label: Option<&str>,
) -> Result<Vec<(EdgeId, NodeId)>> {
    let node = node.as_u128();
    let mut out = Vec::new();
    for item in table
        .range((ns, node, label.unwrap_or(""), 0u128)..)
        .map_err(storage)?
    {
        let (key, value) = item.map_err(storage)?;
        let (key_ns, key_node, key_label, edge) = key.value();
        if key_ns != ns || key_node != node || label.is_some_and(|label| label != key_label) {
            break;
        }
        out.push((EdgeId::from_u128(edge), NodeId::from_u128(value.value())));
    }
    Ok(out)
}

pub(crate) fn nodes_with_label(
    table: &impl ReadableTable<LabelKey, ()>,
    ns: &str,
    label: &str,
) -> Result<Vec<NodeId>> {
    let mut out = Vec::new();
    for item in table.range((ns, label, 0u128)..).map_err(storage)? {
        let (key, _) = item.map_err(storage)?;
        let (key_ns, key_label, node) = key.value();
        if key_ns != ns || key_label != label {
            break;
        }
        out.push(NodeId::from_u128(node));
    }
    Ok(out)
}
