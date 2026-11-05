//! Property indexes: declared per namespace on a `(label, property)` pair,
//! kept in the same transaction as every node write.
//!
//! Entries are keyed by `(ns, index, value, node)`, where `value` is encoded
//! so that byte order is value order: equality, ranges and string prefixes
//! are key ranges. Only booleans, numbers and strings are indexed (lists,
//! maps and `null` are not). Numbers are compared as `f64`, so an index can
//! return a few extra nodes for integers beyond 2^53; callers check the
//! property itself, and the index only narrows the search.
//!
//! A fuzzy index (`fuzzy: true`) keeps, instead of the value, the
//! trigrams of the normalized text (of a string, or of each string of a
//! list): it finds the nodes whose text looks like a query
//! ([`ReadTx::fuzzy_search`]), typos and word order included, without
//! comparing with every node. Its entries share the table, tagged apart.

use std::collections::HashMap;
use std::ops::Bound;

use grafeno_core::{Error, Node, NodeId, Result};
use redb::{ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::store::{
    NODE_LABELS, NODES, check_label, decode, encode, nodes_with_label, read_record, storage,
};
use crate::{ReadTx, WriteTx};

pub(crate) type IndexKey = (&'static str, &'static str, &'static [u8], u128);

/// `(ns, index name) -> IndexDef` (JSON).
pub(crate) const INDEXES: TableDefinition<(&str, &str), &[u8]> = TableDefinition::new("indexes");
/// `(ns, index name, encoded value, node) -> ()`
pub(crate) const INDEX_ENTRIES: TableDefinition<IndexKey, ()> =
    TableDefinition::new("index_entries");

/// A declared index: nodes with `label`, by their property `key`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexDef {
    pub name: String,
    pub label: String,
    pub key: String,
    /// No two nodes with the label may share a value.
    #[serde(default)]
    pub unique: bool,
    /// Trigrams of the text, for [`ReadTx::fuzzy_search`], instead of the
    /// value (not for lookups by value; never unique).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fuzzy: bool,
}

impl IndexDef {
    /// The default name: `Person_age` (`Person_email_unique` for a
    /// constraint).
    pub fn default_name(label: &str, key: &str, unique: bool) -> String {
        if unique {
            format!("{label}_{key}_unique")
        } else {
            format!("{label}_{key}")
        }
    }

    /// `Person_name_fuzzy`.
    pub fn default_fuzzy_name(label: &str, key: &str) -> String {
        format!("{label}_{key}_fuzzy")
    }

    /// An index (not unique, not fuzzy) under its default name.
    pub fn new(label: &str, key: &str) -> Self {
        Self {
            name: Self::default_name(label, key, false),
            label: label.into(),
            key: key.into(),
            unique: false,
            fuzzy: false,
        }
    }

    /// A fuzzy index under its default name.
    pub fn fuzzy(label: &str, key: &str) -> Self {
        Self {
            name: Self::default_fuzzy_name(label, key),
            fuzzy: true,
            ..Self::new(label, key)
        }
    }
}

/// The tag of fuzzy-index entries: a trigram follows.
const GRAM: u8 = 4;

/// The trigrams a fuzzy index keeps for a value: of the normalized text of
/// a string, or of each string in a list.
pub(crate) fn grams_of(value: &Json) -> Vec<String> {
    let mut grams = std::collections::BTreeSet::new();
    let mut add = |text: &str| {
        grams.extend(crate::textsim::ngrams(&crate::normalize_name(text), 3));
    };
    match value {
        Json::String(s) => add(s),
        Json::Array(items) => items.iter().filter_map(Json::as_str).for_each(&mut add),
        _ => {}
    }
    grams.into_iter().collect()
}

fn gram_bytes(gram: &str) -> Vec<u8> {
    let mut out = vec![GRAM];
    out.extend_from_slice(gram.as_bytes());
    out
}

/// A node that looks like a fuzzy search's text.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct FuzzyHit {
    pub node: NodeId,
    /// From 0 to 1 (see [`ReadTx::fuzzy_search`]).
    pub score: f64,
}

/// A trigram shared by more nodes than this says little about a query
/// and costs a long read: its entries are left out of the candidate count.
const COMMON_GRAM: usize = 20_000;

/// An index and how many nodes it holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct IndexInfo {
    #[serde(flatten)]
    pub def: IndexDef,
    pub entries: u64,
}

/// Which values to look up.
#[derive(Clone, Debug, PartialEq)]
pub enum IndexRange {
    Equal(Json),
    /// Between two values of the same kind (numbers, strings or booleans);
    /// an unbounded end stays within that kind.
    Between {
        lower: Bound<Json>,
        upper: Bound<Json>,
    },
    /// Strings that start with this text.
    Prefix(String),
}

const BOOL: u8 = 1;
const NUMBER: u8 = 2;
const STRING: u8 = 3;

fn kind(value: &Json) -> Option<u8> {
    match value {
        Json::Bool(_) => Some(BOOL),
        Json::Number(_) => Some(NUMBER),
        Json::String(_) => Some(STRING),
        _ => None,
    }
}

/// The sortable bytes of a value, or `None` if it is not indexed.
pub(crate) fn encode_value(value: &Json) -> Option<Vec<u8>> {
    match value {
        Json::Bool(b) => Some(vec![BOOL, u8::from(*b)]),
        Json::Number(n) => {
            let f = n.as_f64()?;
            if !f.is_finite() {
                return None;
            }
            // Flip the bits so that byte order is numeric order (and -0 = 0).
            let f = if f == 0.0 { 0.0 } else { f };
            let bits = f.to_bits();
            let bits = if bits >> 63 == 1 {
                !bits
            } else {
                bits ^ (1 << 63)
            };
            let mut out = vec![NUMBER];
            out.extend_from_slice(&bits.to_be_bytes());
            Some(out)
        }
        Json::String(s) => {
            let mut out = vec![STRING];
            out.extend_from_slice(s.as_bytes());
            Some(out)
        }
        _ => None,
    }
}

/// The byte range a lookup covers. Bounds are always inclusive: exclusion
/// is left to the caller's own check (numbers are compared as `f64` here).
fn byte_range(range: &IndexRange) -> Option<(Vec<u8>, Vec<u8>)> {
    match range {
        IndexRange::Equal(value) => {
            let bytes = encode_value(value)?;
            Some((bytes.clone(), bytes))
        }
        IndexRange::Prefix(prefix) => {
            let mut lower = vec![STRING];
            lower.extend_from_slice(prefix.as_bytes());
            let mut upper = lower.clone();
            // 0xFF never appears in UTF-8, so it sorts after every
            // continuation of the prefix.
            upper.push(0xFF);
            Some((lower, upper))
        }
        IndexRange::Between { lower, upper } => {
            let bound_kind = |bound: &Bound<Json>| match bound {
                Bound::Included(value) | Bound::Excluded(value) => kind(value),
                Bound::Unbounded => None,
            };
            let tag = bound_kind(lower).or_else(|| bound_kind(upper))?;
            if [lower, upper]
                .iter()
                .any(|bound| bound_kind(bound).is_some_and(|other| other != tag))
            {
                return None;
            }
            let low = match lower {
                Bound::Included(value) | Bound::Excluded(value) => encode_value(value)?,
                Bound::Unbounded => vec![tag],
            };
            let high = match upper {
                Bound::Included(value) | Bound::Excluded(value) => encode_value(value)?,
                Bound::Unbounded => vec![tag, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
            };
            Some((low, high))
        }
    }
}

/// The index entries a node should have under these definitions.
pub(crate) fn entries_of<'a>(defs: &'a [IndexDef], node: &Node) -> Vec<(&'a IndexDef, Vec<u8>)> {
    let mut out = Vec::new();
    for def in defs.iter().filter(|def| node.has_label(&def.label)) {
        let Some(value) = node.props.get(&def.key) else {
            continue;
        };
        if def.fuzzy {
            out.extend(grams_of(value).iter().map(|gram| (def, gram_bytes(gram))));
        } else if let Some(bytes) = encode_value(value) {
            out.push((def, bytes));
        }
    }
    out
}

/// Nodes of fuzzy index `name` that share trigrams with `text`, best first:
/// candidates by trigrams in common, then scored with `measure` against
/// the node's value (the best of a list). Reads `node` for each candidate.
fn fuzzy(
    entries: &impl ReadableTable<IndexKey, ()>,
    node: impl Fn(NodeId) -> Result<Option<Node>>,
    def: &IndexDef,
    ns: &str,
    text: &str,
    measure: crate::textsim::Measure,
    limit: usize,
) -> Result<Vec<FuzzyHit>> {
    let query = crate::normalize_name(text);
    let grams = crate::textsim::ngrams(&query, 3);
    let mut shared: HashMap<u128, u32> = HashMap::new();
    for gram in &grams {
        let bytes = gram_bytes(gram);
        let mut seen = Vec::new();
        for item in entries
            .range(
                (ns, def.name.as_str(), bytes.as_slice(), 0u128)
                    ..=(ns, def.name.as_str(), bytes.as_slice(), u128::MAX),
            )
            .map_err(storage)?
        {
            seen.push(item.map_err(storage)?.0.value().3);
            if seen.len() > COMMON_GRAM {
                break;
            }
        }
        if seen.len() > COMMON_GRAM {
            continue;
        }
        for id in seen {
            *shared.entry(id).or_default() += 1;
        }
    }
    // The candidates sharing the most trigrams, then the measure on them.
    let mut candidates: Vec<(u128, u32)> = shared.into_iter().collect();
    candidates.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    candidates.truncate(limit.saturating_mul(10).max(50));
    let mut hits = Vec::new();
    for (id, _) in candidates {
        let id = NodeId::from_u128(id);
        let Some(found) = node(id)? else {
            continue;
        };
        let texts: Vec<&str> = match found.props.get(&def.key) {
            Some(Json::String(s)) => vec![s.as_str()],
            Some(Json::Array(items)) => items.iter().filter_map(Json::as_str).collect(),
            _ => Vec::new(),
        };
        let score = texts
            .iter()
            .map(|value| measure.compare(text, value))
            .fold(0.0, f64::max);
        hits.push(FuzzyHit { node: id, score });
    }
    hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.node.cmp(&b.node)));
    hits.truncate(limit);
    Ok(hits)
}

fn fuzzy_def(defs: Vec<IndexDef>, label: &str, key: &str) -> Result<IndexDef> {
    defs.into_iter()
        .find(|def| def.fuzzy && def.label == label && def.key == key)
        .ok_or_else(|| {
            Error::Invalid(format!(
                "no fuzzy index on :{label}({key}); create one with CREATE FUZZY INDEX FOR (n:{label}) ON (n.{key})"
            ))
        })
}

fn defs_in(
    table: &impl ReadableTable<(&'static str, &'static str), &'static [u8]>,
    ns: &str,
) -> Result<Vec<IndexDef>> {
    let mut out = Vec::new();
    for item in table.range((ns, "")..).map_err(storage)? {
        let (key, value) = item.map_err(storage)?;
        if key.value().0 != ns {
            break;
        }
        out.push(decode(value.value())?);
    }
    Ok(out)
}

fn lookup(
    table: &impl ReadableTable<IndexKey, ()>,
    ns: &str,
    name: &str,
    range: &IndexRange,
    limit: usize,
) -> Result<Vec<NodeId>> {
    let Some((low, high)) = byte_range(range) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for item in table
        .range((ns, name, low.as_slice(), 0u128)..=(ns, name, high.as_slice(), u128::MAX))
        .map_err(storage)?
    {
        if out.len() >= limit {
            break;
        }
        let (key, _) = item.map_err(storage)?;
        out.push(NodeId::from_u128(key.value().3));
    }
    Ok(out)
}

fn count(table: &impl ReadableTable<IndexKey, ()>, ns: &str, name: &str) -> Result<u64> {
    let empty: &[u8] = &[];
    let top: &[u8] = &[0xFF; 16];
    let mut entries = 0;
    for item in table
        .range((ns, name, empty, 0u128)..=(ns, name, top, u128::MAX))
        .map_err(storage)?
    {
        item.map_err(storage)?;
        entries += 1;
    }
    Ok(entries)
}

impl ReadTx {
    /// The indexes of a namespace, in name order.
    pub fn index_defs(&self, ns: &str) -> Result<Vec<IndexDef>> {
        defs_in(&self.indexes, ns)
    }

    /// The indexes with how many nodes each holds (counts every entry).
    pub fn indexes(&self, ns: &str) -> Result<Vec<IndexInfo>> {
        self.index_defs(ns)?
            .into_iter()
            .map(|def| {
                let entries = count(&self.index_entries, ns, &def.name)?;
                Ok(IndexInfo { def, entries })
            })
            .collect()
    }

    /// Up to `limit` nodes with `label` whose property `key` looks like
    /// `text`, through the fuzzy index on them, best first. Candidates are
    /// the nodes sharing the most trigrams with the normalized text; each
    /// is scored with `measure` (Dice over trigrams, Jaro-Winkler, …)
    /// against its value. Fails if there is no fuzzy index on `(label,
    /// key)`.
    pub fn fuzzy_search(
        &self,
        ns: &str,
        label: &str,
        key: &str,
        text: &str,
        measure: crate::textsim::Measure,
        limit: usize,
    ) -> Result<Vec<FuzzyHit>> {
        let def = fuzzy_def(self.index_defs(ns)?, label, key)?;
        fuzzy(
            &self.index_entries,
            |id| self.node(ns, id),
            &def,
            ns,
            text,
            measure,
            limit,
        )
    }

    /// Up to `limit` nodes in index `name` whose value is in `range`, in
    /// value order. An unknown index gives none.
    pub fn index_lookup(
        &self,
        ns: &str,
        name: &str,
        range: &IndexRange,
        limit: usize,
    ) -> Result<Vec<NodeId>> {
        lookup(&self.index_entries, ns, name, range, limit)
    }
}

impl WriteTx {
    pub fn index_defs(&self, ns: &str) -> Result<Vec<IndexDef>> {
        defs_in(&self.tx.open_table(INDEXES).map_err(storage)?, ns)
    }

    /// [`ReadTx::fuzzy_search`] as written so far in this transaction.
    pub fn fuzzy_search(
        &self,
        ns: &str,
        label: &str,
        key: &str,
        text: &str,
        measure: crate::textsim::Measure,
        limit: usize,
    ) -> Result<Vec<FuzzyHit>> {
        let def = fuzzy_def(self.index_defs(ns)?, label, key)?;
        let table = self.tx.open_table(INDEX_ENTRIES).map_err(storage)?;
        fuzzy(
            &table,
            |id| self.node(ns, id),
            &def,
            ns,
            text,
            measure,
            limit,
        )
    }

    /// [`ReadTx::index_lookup`] as written so far in this transaction.
    pub fn index_lookup(
        &self,
        ns: &str,
        name: &str,
        range: &IndexRange,
        limit: usize,
    ) -> Result<Vec<NodeId>> {
        lookup(
            &self.tx.open_table(INDEX_ENTRIES).map_err(storage)?,
            ns,
            name,
            range,
            limit,
        )
    }

    /// Declares an index and fills it from the nodes that have the label.
    /// Returns how many nodes it holds. An identical definition is kept as
    /// it is; another index with the same name or on the same label and
    /// property is an error, as are duplicate values in a unique index.
    pub fn create_index(&mut self, ns: &str, def: IndexDef) -> Result<u64> {
        check_label(&def.label)?;
        crate::namespace::ensure(&self.tx, ns)?;
        if def.key.is_empty() || def.name.is_empty() {
            return Err(Error::Invalid(
                "an index needs a name and a property".into(),
            ));
        }
        let existing = self.index_defs(ns)?;
        if let Some(same) = existing.iter().find(|other| other.name == def.name) {
            if *same == def {
                let table = self.tx.open_table(INDEX_ENTRIES).map_err(storage)?;
                return count(&table, ns, &def.name);
            }
            return Err(Error::Invalid(format!(
                "an index named {:?} already exists",
                def.name
            )));
        }
        if def.fuzzy && def.unique {
            return Err(Error::Invalid("a fuzzy index cannot be unique".into()));
        }
        if let Some(other) = existing.iter().find(|other| {
            other.label == def.label
                && other.key == def.key
                && other.unique == def.unique
                && other.fuzzy == def.fuzzy
        }) {
            return Err(Error::Invalid(format!(
                "index {:?} already covers :{}({})",
                other.name, def.label, def.key
            )));
        }
        let ids = nodes_with_label(
            &self.tx.open_table(NODE_LABELS).map_err(storage)?,
            ns,
            &def.label,
        )?;
        let nodes = self.tx.open_table(NODES).map_err(storage)?;
        let mut seen: HashMap<Vec<u8>, (NodeId, Json)> = HashMap::new();
        let mut entries = Vec::new();
        for id in ids {
            let Some(node) = read_record::<Node>(&nodes, ns, id.as_u128())? else {
                continue;
            };
            let Some(value) = node.props.get(&def.key) else {
                continue;
            };
            if def.fuzzy {
                for (_, bytes) in entries_of(std::slice::from_ref(&def), &node) {
                    entries.push((bytes, id));
                }
                continue;
            }
            let Some(bytes) = encode_value(value) else {
                continue;
            };
            if def.unique {
                if let Some((other, other_value)) = seen.get(&bytes)
                    && other_value == value
                {
                    return Err(Error::Invalid(format!(
                        "cannot make :{}({}) unique: nodes {other} and {id} both have {value}",
                        def.label, def.key
                    )));
                }
                seen.insert(bytes.clone(), (id, value.clone()));
            }
            entries.push((bytes, id));
        }
        drop(nodes);
        let mut table = self.tx.open_table(INDEX_ENTRIES).map_err(storage)?;
        for (bytes, id) in &entries {
            table
                .insert((ns, def.name.as_str(), bytes.as_slice(), id.as_u128()), ())
                .map_err(storage)?;
        }
        drop(table);
        self.tx
            .open_table(INDEXES)
            .map_err(storage)?
            .insert((ns, def.name.as_str()), encode(&def)?.as_slice())
            .map_err(storage)?;
        Ok(entries.len() as u64)
    }

    /// Removes an index and its entries. Returns whether it existed.
    pub fn drop_index(&mut self, ns: &str, name: &str) -> Result<bool> {
        let existed = self
            .tx
            .open_table(INDEXES)
            .map_err(storage)?
            .remove((ns, name))
            .map_err(storage)?
            .is_some();
        let empty: &[u8] = &[];
        self.tx
            .open_table(INDEX_ENTRIES)
            .map_err(storage)?
            .retain_in((ns, name, empty, 0u128).., |key, _| {
                key.0 != ns || key.1 != name
            })
            .map_err(storage)?;
        Ok(existed)
    }

    /// Brings the index entries of a node up to date: `node` is its new
    /// state (`None` when deleted), `previous` the old one (`None` when new).
    pub(crate) fn index_node(
        &mut self,
        ns: &str,
        node: Option<&Node>,
        previous: Option<&Node>,
    ) -> Result<()> {
        let defs = self.index_defs(ns)?;
        if defs.is_empty() {
            return Ok(());
        }
        let old = previous
            .map(|node| entries_of(&defs, node))
            .unwrap_or_default();
        let new = node.map(|node| entries_of(&defs, node)).unwrap_or_default();
        let id = node
            .or(previous)
            .map(|node| node.id)
            .expect("a node to index");
        let mut table = self.tx.open_table(INDEX_ENTRIES).map_err(storage)?;
        for (def, bytes) in &old {
            if !new.contains(&(def, bytes.clone())) {
                table
                    .remove((ns, def.name.as_str(), bytes.as_slice(), id.as_u128()))
                    .map_err(storage)?;
            }
        }
        for (def, bytes) in &new {
            if old.contains(&(def, bytes.clone())) {
                continue;
            }
            if def.unique {
                let value = node
                    .and_then(|node| node.props.get(&def.key))
                    .expect("indexed");
                for item in table
                    .range(
                        (ns, def.name.as_str(), bytes.as_slice(), 0u128)
                            ..=(ns, def.name.as_str(), bytes.as_slice(), u128::MAX),
                    )
                    .map_err(storage)?
                {
                    let (key, _) = item.map_err(storage)?;
                    let other = key.value().3;
                    if other == id.as_u128() {
                        continue;
                    }
                    // Equal bytes are equal values except for huge integers:
                    // compare the values themselves.
                    let nodes = self.tx.open_table(NODES).map_err(storage)?;
                    let same = read_record::<Node>(&nodes, ns, other)?
                        .is_some_and(|other| other.props.get(&def.key) == Some(value));
                    if same {
                        return Err(Error::Invalid(format!(
                            "{} must be unique among :{} nodes: node {} already has {value}",
                            def.key,
                            def.label,
                            NodeId::from_u128(other)
                        )));
                    }
                }
            }
            table
                .insert((ns, def.name.as_str(), bytes.as_slice(), id.as_u128()), ())
                .map_err(storage)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn encoding_keeps_order() {
        let values = [
            json!(-1e9),
            json!(-2.5),
            json!(-1),
            json!(0),
            json!(0.5),
            json!(3),
            json!(1e12),
        ];
        let encoded: Vec<Vec<u8>> = values.iter().map(|v| encode_value(v).unwrap()).collect();
        let mut sorted = encoded.clone();
        sorted.sort();
        assert_eq!(encoded, sorted);
        assert_eq!(encode_value(&json!(-0.0)), encode_value(&json!(0)));
        assert!(encode_value(&json!("a")).unwrap() < encode_value(&json!("ab")).unwrap());
        assert!(encode_value(&json!(true)).unwrap() < encode_value(&json!(1)).unwrap());
        assert!(encode_value(&json!([1])).is_none() && encode_value(&json!(null)).is_none());
    }
}
