//! Counters kept up to date by every write, and the statistics built on them.

use std::collections::BTreeMap;

use grafeno_core::Result;
use redb::{ReadableTable, TableDefinition, WriteTransaction};
use serde::Serialize;

use crate::store::{CountKey, EDGE_LABEL_COUNTS, LABEL_COUNTS, NS_COUNTS, storage};

/// A namespace and how much it holds. Namespaces exist while they have nodes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NamespaceInfo {
    pub name: String,
    pub nodes: u64,
    pub edges: u64,
}

/// What a namespace holds, by label and by index.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct NamespaceStats {
    pub name: String,
    pub nodes: u64,
    pub edges: u64,
    /// Nodes per label (a node with two labels counts in both).
    pub node_labels: BTreeMap<String, u64>,
    /// Edges per label, including retracted ones.
    pub edge_labels: BTreeMap<String, u64>,
    /// Nodes with indexed text, and the terms indexed for them.
    pub texts: u64,
    pub text_terms: u64,
    /// Nodes with an embedding, and its dimension.
    pub vectors: u64,
    pub vector_dimension: Option<usize>,
}

pub(crate) fn bump_namespace(
    tx: &WriteTransaction,
    ns: &str,
    nodes: i64,
    edges: i64,
) -> Result<()> {
    let mut table = tx.open_table(NS_COUNTS).map_err(storage)?;
    let (current_nodes, current_edges) = table
        .get(ns)
        .map_err(storage)?
        .map(|guard| guard.value())
        .unwrap_or((0, 0));
    let next = (
        current_nodes.saturating_add_signed(nodes),
        current_edges.saturating_add_signed(edges),
    );
    if next == (0, 0) {
        table.remove(ns).map_err(storage)?;
    } else {
        table.insert(ns, next).map_err(storage)?;
    }
    Ok(())
}

fn bump_label_in(
    tx: &WriteTransaction,
    definition: TableDefinition<CountKey, u64>,
    ns: &str,
    label: &str,
    delta: i64,
) -> Result<()> {
    let mut table = tx.open_table(definition).map_err(storage)?;
    let current = table
        .get((ns, label))
        .map_err(storage)?
        .map(|guard| guard.value())
        .unwrap_or(0);
    let next = current.saturating_add_signed(delta);
    if next == 0 {
        table.remove((ns, label)).map_err(storage)?;
    } else {
        table.insert((ns, label), next).map_err(storage)?;
    }
    Ok(())
}

pub(crate) fn bump_node_label(
    tx: &WriteTransaction,
    ns: &str,
    label: &str,
    delta: i64,
) -> Result<()> {
    bump_label_in(tx, LABEL_COUNTS, ns, label, delta)
}

pub(crate) fn bump_edge_label(
    tx: &WriteTransaction,
    ns: &str,
    label: &str,
    delta: i64,
) -> Result<()> {
    bump_label_in(tx, EDGE_LABEL_COUNTS, ns, label, delta)
}

pub(crate) fn namespaces(
    table: &impl ReadableTable<&'static str, (u64, u64)>,
) -> Result<Vec<NamespaceInfo>> {
    let mut out = Vec::new();
    for item in table.iter().map_err(storage)? {
        let (key, value) = item.map_err(storage)?;
        let (nodes, edges) = value.value();
        out.push(NamespaceInfo {
            name: key.value().to_owned(),
            nodes,
            edges,
        });
    }
    Ok(out)
}

pub(crate) fn label_counts(
    table: &impl ReadableTable<CountKey, u64>,
    ns: &str,
) -> Result<BTreeMap<String, u64>> {
    let mut out = BTreeMap::new();
    for item in table.range((ns, "")..).map_err(storage)? {
        let (key, value) = item.map_err(storage)?;
        let (key_ns, label) = key.value();
        if key_ns != ns {
            break;
        }
        out.insert(label.to_owned(), value.value());
    }
    Ok(out)
}
