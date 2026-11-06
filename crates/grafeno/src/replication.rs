//! The replication log (phase 13.1): every committed write transaction, as
//! the effects it had, so that other servers can apply it and hold the same
//! data.
//!
//! A write transaction notes the keys of the primary records it touches
//! (nodes, edges, texts, embeddings, memory states, index and constraint
//! definitions, principals, namespaces, match rules, state entries, merge
//! records). At commit their final values are read and appended to the log
//! as one [`Entry`], in the same redb transaction as the writes: values,
//! not requests, so ids, clock readings and everything a GQ statement or a
//! constraint decided are already resolved. Work that rewrites a whole
//! namespace (dropping it, rebuilding its indexes, compacting its vectors)
//! is logged as that operation instead of record by record.
//!
//! [`WriteTx::apply_entry`] performs an entry on another database: it writes
//! the records and derives everything else from them (adjacency, labels,
//! names, property indexes, postings, the HNSW graph, counters), writes the
//! change log entries the leader's commit wrote (so every member's change
//! log is the same), runs none of the rules that decided them (constraints,
//! property history), and records the position it reached in the same
//! transaction, so an entry is never applied twice nor half applied.
//!
//! The log is off until [`crate::Grafeno::enable_replication_log`] turns it
//! on for good (it is kept in the file): a single server pays nothing.

use std::collections::{BTreeSet, HashSet};

use grafeno_core::{Edge, EdgeId, Error, Node, NodeId, Result, Timestamp};
use redb::{ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::db::{ReadTx, SCHEMA_VERSION, WriteTx};
use crate::store::{EDGES, MEMORY, META, NODES, decode, encode, storage};

/// `index -> Entry` (JSON).
pub(crate) const REPLICATION_LOG: TableDefinition<u64, &[u8]> =
    TableDefinition::new("replication_log");

/// The version of [`Entry`]'s format.
pub const ENTRY_FORMAT: u32 = 1;

pub(crate) const ENABLED: &str = "replication_enabled";
const FIRST: &str = "replication_first";
const LAST: &str = "replication_last";
const APPLIED: &str = "replication_applied";

/// A key of a primary record, noted when a transaction writes it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Touch {
    Node(String, u128),
    Edge(String, u128),
    Text(String, u128),
    Vector(String, u128),
    Memory(String, u128),
    Index(String, String),
    Constraint(String, String),
    Principal(String),
    Namespace(String),
    MatchRule(String, String),
    State(String, String, String),
    Merge(String, u128),
    Bulk(Bulk),
}

/// Work logged as itself rather than record by record.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Bulk {
    DropNamespace { ns: String },
    RebuildIndexes { ns: String },
    CompactVectors { ns: String },
}

/// What a write transaction touched, in the order it first touched it.
#[derive(Default)]
pub(crate) struct Capture {
    keys: Vec<Touch>,
    seen: HashSet<Touch>,
    /// Inside bulk work, which is logged as itself.
    paused: u32,
}

/// One effect: a record's value after the transaction (`None`: it is gone),
/// or bulk work.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Node {
        ns: String,
        id: NodeId,
        node: Option<Node>,
    },
    Edge {
        ns: String,
        id: EdgeId,
        edge: Option<Edge>,
    },
    Text {
        ns: String,
        id: NodeId,
        text: Option<String>,
    },
    Vector {
        ns: String,
        id: NodeId,
        embedding: Option<Vec<f32>>,
    },
    Memory {
        ns: String,
        id: NodeId,
        state: Option<(f32, u32, i64)>,
    },
    Index {
        ns: String,
        name: String,
        def: Option<Json>,
    },
    Constraint {
        ns: String,
        name: String,
        constraint: Option<Json>,
    },
    Principal {
        name: String,
        principal: Option<Json>,
    },
    Namespace {
        ns: String,
        meta: Option<Json>,
    },
    MatchRule {
        ns: String,
        label: String,
        rule: Option<Json>,
    },
    State {
        ns: String,
        kind: String,
        key: String,
        value: Option<Vec<u8>>,
    },
    Merge {
        ns: String,
        /// The merge id, in decimal (JSON has no 128-bit integers).
        id: String,
        record: Option<Json>,
    },
    Bulk(Bulk),
}

/// A committed write transaction, as its effects.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub format: u32,
    /// The layout version of the database that wrote it.
    pub layout: u64,
    /// The commit's clock: the change log entries it makes carry it.
    pub at: Timestamp,
    pub ops: Vec<Op>,
    /// The change log entries the commit wrote, per namespace (without
    /// their sequence numbers, given again in the same order on apply), so
    /// that every member's change log is the same.
    #[serde(default)]
    pub changes: Vec<(String, crate::changes::Change)>,
}

/// Where a database stands in its replication log.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct LogPosition {
    pub enabled: bool,
    /// The first entry still kept (0 when none is).
    pub first: u64,
    /// The last entry written.
    pub last: u64,
    /// The last entry applied to the data (equal to `last` on the server
    /// that wrote them).
    pub applied: u64,
}

fn meta_get(table: &impl ReadableTable<&'static str, u64>, key: &str) -> Result<u64> {
    Ok(table
        .get(key)
        .map_err(storage)?
        .map_or(0, |guard| guard.value()))
}

fn json_of(bytes: &[u8]) -> Result<Json> {
    serde_json::from_slice(bytes).map_err(|e| Error::Serialization(e.to_string()))
}

fn bytes_of(value: &Json) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|e| Error::Serialization(e.to_string()))
}

impl Touch {
    fn ns(&self) -> Option<&str> {
        match self {
            Touch::Node(ns, _)
            | Touch::Edge(ns, _)
            | Touch::Text(ns, _)
            | Touch::Vector(ns, _)
            | Touch::Memory(ns, _)
            | Touch::Index(ns, _)
            | Touch::Constraint(ns, _)
            | Touch::Namespace(ns)
            | Touch::MatchRule(ns, _)
            | Touch::State(ns, _, _)
            | Touch::Merge(ns, _) => Some(ns),
            Touch::Principal(_) | Touch::Bulk(_) => None,
        }
    }
}

impl WriteTx {
    /// Notes that this transaction wrote a primary record (when the log is
    /// on, and outside bulk work).
    pub(crate) fn note(&mut self, touch: Touch) {
        if let Some(capture) = self.capture.as_mut()
            && capture.paused == 0
            && capture.seen.insert(touch.clone())
        {
            capture.keys.push(touch);
        }
    }

    /// Runs bulk work, logged as `bulk` instead of the records it writes.
    pub(crate) fn bulk<T>(
        &mut self,
        bulk: Bulk,
        work: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        if let Some(capture) = self.capture.as_mut() {
            if capture.paused == 0 {
                capture.keys.push(Touch::Bulk(bulk));
            }
            capture.paused += 1;
        }
        let result = work(self);
        if let Some(capture) = self.capture.as_mut() {
            capture.paused -= 1;
        }
        result
    }

    /// The final value of a touched record.
    fn after_image(&self, touch: &Touch) -> Result<Op> {
        let tx = &self.tx;
        Ok(match touch {
            Touch::Node(ns, id) => Op::Node {
                ns: ns.clone(),
                id: NodeId::from_u128(*id),
                node: tx
                    .open_table(NODES)
                    .map_err(storage)?
                    .get((ns.as_str(), *id))
                    .map_err(storage)?
                    .map(|guard| decode(guard.value()))
                    .transpose()?,
            },
            Touch::Edge(ns, id) => Op::Edge {
                ns: ns.clone(),
                id: EdgeId::from_u128(*id),
                edge: tx
                    .open_table(EDGES)
                    .map_err(storage)?
                    .get((ns.as_str(), *id))
                    .map_err(storage)?
                    .map(|guard| decode(guard.value()))
                    .transpose()?,
            },
            Touch::Text(ns, id) => Op::Text {
                ns: ns.clone(),
                id: NodeId::from_u128(*id),
                text: self.text(ns, NodeId::from_u128(*id))?,
            },
            Touch::Vector(ns, id) => Op::Vector {
                ns: ns.clone(),
                id: NodeId::from_u128(*id),
                embedding: self.embedding(ns, NodeId::from_u128(*id))?,
            },
            Touch::Memory(ns, id) => Op::Memory {
                ns: ns.clone(),
                id: NodeId::from_u128(*id),
                state: tx
                    .open_table(MEMORY)
                    .map_err(storage)?
                    .get((ns.as_str(), *id))
                    .map_err(storage)?
                    .map(|guard| guard.value()),
            },
            Touch::Index(ns, name) => Op::Index {
                ns: ns.clone(),
                name: name.clone(),
                def: tx
                    .open_table(crate::index::INDEXES)
                    .map_err(storage)?
                    .get((ns.as_str(), name.as_str()))
                    .map_err(storage)?
                    .map(|guard| json_of(guard.value()))
                    .transpose()?,
            },
            Touch::Constraint(ns, name) => Op::Constraint {
                ns: ns.clone(),
                name: name.clone(),
                constraint: tx
                    .open_table(crate::constraints::CONSTRAINTS)
                    .map_err(storage)?
                    .get((ns.as_str(), name.as_str()))
                    .map_err(storage)?
                    .map(|guard| json_of(guard.value()))
                    .transpose()?,
            },
            Touch::Principal(name) => Op::Principal {
                name: name.clone(),
                principal: tx
                    .open_table(crate::access::PRINCIPALS)
                    .map_err(storage)?
                    .get(name.as_str())
                    .map_err(storage)?
                    .map(|guard| json_of(guard.value()))
                    .transpose()?,
            },
            Touch::Namespace(ns) => Op::Namespace {
                ns: ns.clone(),
                meta: tx
                    .open_table(crate::namespace::NAMESPACES)
                    .map_err(storage)?
                    .get(ns.as_str())
                    .map_err(storage)?
                    .map(|guard| json_of(guard.value()))
                    .transpose()?,
            },
            Touch::MatchRule(ns, label) => Op::MatchRule {
                ns: ns.clone(),
                label: label.clone(),
                rule: tx
                    .open_table(crate::resolve::MATCH_RULES)
                    .map_err(storage)?
                    .get((ns.as_str(), label.as_str()))
                    .map_err(storage)?
                    .map(|guard| json_of(guard.value()))
                    .transpose()?,
            },
            Touch::State(ns, kind, key) => Op::State {
                ns: ns.clone(),
                kind: kind.clone(),
                key: key.clone(),
                value: self.state(ns, kind, key)?,
            },
            Touch::Merge(ns, id) => Op::Merge {
                ns: ns.clone(),
                id: id.to_string(),
                record: tx
                    .open_table(crate::merge::MERGES)
                    .map_err(storage)?
                    .get((ns.as_str(), *id))
                    .map_err(storage)?
                    .map(|guard| json_of(guard.value()))
                    .transpose()?,
            },
            Touch::Bulk(bulk) => Op::Bulk(bulk.clone()),
        })
    }

    /// Appends what this transaction did to the log, as the entry after
    /// the last one, and returns the commit clock to stamp its changes with.
    pub(crate) fn log_entry(&mut self) -> Result<Option<Timestamp>> {
        let Some(capture) = self.capture.take() else {
            return Ok(None);
        };
        if capture.keys.is_empty() {
            return Ok(None);
        }
        let mut ops = Vec::with_capacity(capture.keys.len());
        let mut namespaces = BTreeSet::new();
        for touch in &capture.keys {
            if let Some(ns) = touch.ns() {
                namespaces.insert(ns.to_owned());
            }
            ops.push(self.after_image(touch)?);
        }
        // Namespaces registered on the way (by a first write to them) keep
        // the creation time they got here.
        for ns in namespaces {
            if !capture.seen.contains(&Touch::Namespace(ns.clone())) {
                ops.push(self.after_image(&Touch::Namespace(ns))?);
            }
        }
        let at = Timestamp::now();
        let entry = Entry {
            format: ENTRY_FORMAT,
            layout: SCHEMA_VERSION,
            at,
            ops,
            changes: self.pending_changes.entries().to_vec(),
        };
        let mut meta = self.tx.open_table(META).map_err(storage)?;
        let index = meta_get(&meta, LAST)? + 1;
        if meta_get(&meta, FIRST)? == 0 {
            meta.insert(FIRST, index).map_err(storage)?;
        }
        meta.insert(LAST, index).map_err(storage)?;
        meta.insert(APPLIED, index).map_err(storage)?;
        drop(meta);
        self.tx
            .open_table(REPLICATION_LOG)
            .map_err(storage)?
            .insert(index, encode(&entry)?.as_slice())
            .map_err(storage)?;
        Ok(Some(at))
    }

    /// Applies the entry at `index` of another database's log: writes its
    /// records, derives the rest, keeps the entry in this database's log
    /// and records the position. An entry already applied is skipped
    /// (`false`); one past the next is an error (entries are missing).
    pub fn apply_entry(&mut self, index: u64, entry: &Entry) -> Result<bool> {
        if entry.format > ENTRY_FORMAT || entry.layout > SCHEMA_VERSION {
            return Err(Error::Invalid(format!(
                "log entry {index} has format {} and layout {}, newer than this Grafeno \
                 ({ENTRY_FORMAT} and {SCHEMA_VERSION})",
                entry.format, entry.layout
            )));
        }
        let applied = meta_get(&self.tx.open_table(META).map_err(storage)?, APPLIED)?;
        if index <= applied {
            return Ok(false);
        }
        if index != applied + 1 {
            return Err(Error::Invalid(format!(
                "log entry {index} cannot follow {applied}: entries are missing"
            )));
        }
        // What this applies is logged as the entry itself, not again.
        let capture = self.capture.take();
        self.applying = true;
        self.commit_at = Some(entry.at);
        let result = entry.ops.iter().try_for_each(|op| self.apply_op(op));
        // The change log gets the leader's entries, not those derived here.
        self.pending_changes = crate::changes::Pending::from_entries(entry.changes.clone());
        self.applying = false;
        self.capture = None;
        drop(capture);
        result?;
        let mut meta = self.tx.open_table(META).map_err(storage)?;
        if meta_get(&meta, FIRST)? == 0 {
            meta.insert(FIRST, index).map_err(storage)?;
        }
        let last = meta_get(&meta, LAST)?.max(index);
        meta.insert(LAST, last).map_err(storage)?;
        meta.insert(APPLIED, index).map_err(storage)?;
        meta.insert(ENABLED, 1).map_err(storage)?;
        drop(meta);
        self.tx
            .open_table(REPLICATION_LOG)
            .map_err(storage)?
            .insert(index, encode(entry)?.as_slice())
            .map_err(storage)?;
        Ok(true)
    }

    fn apply_op(&mut self, op: &Op) -> Result<()> {
        match op {
            Op::Node { ns, id, node } => {
                let previous = self.node(ns, *id)?;
                match (node, previous) {
                    (Some(node), previous) => self.put_node(ns, node, previous.as_ref())?,
                    (None, Some(previous)) => self.remove_node_record(ns, &previous)?,
                    (None, None) => {}
                }
            }
            Op::Edge { ns, id, edge } => {
                let existed = self.edge(ns, *id)?.is_some();
                match edge {
                    Some(edge) => {
                        self.put_edge(ns, edge)?;
                        if !existed {
                            crate::stats::bump_namespace(&self.tx, ns, 0, 1)?;
                            crate::stats::bump_edge_label(&self.tx, ns, &edge.label, 1)?;
                        }
                    }
                    None if existed => self.delete_edge(ns, *id)?,
                    None => {}
                }
            }
            Op::Text { ns, id, text } => {
                if self.text(ns, *id)? != *text {
                    match text {
                        Some(text) => crate::text::index(&self.tx, ns, *id, text)?,
                        None => {
                            crate::text::remove(&self.tx, ns, *id)?;
                        }
                    }
                    self.log_node_part(ns, *id, crate::changes::Part::Text)?;
                }
            }
            Op::Vector { ns, id, embedding } => {
                if self.embedding(ns, *id)? != *embedding {
                    match embedding {
                        Some(embedding) => {
                            crate::vector::insert(&self.tx, &mut self.vectors, ns, *id, embedding)?
                        }
                        None => {
                            crate::vector::remove(&self.tx, &mut self.vectors, ns, *id)?;
                        }
                    }
                    self.log_node_part(ns, *id, crate::changes::Part::Embedding)?;
                }
            }
            Op::Memory { ns, id, state } => {
                let mut table = self.tx.open_table(MEMORY).map_err(storage)?;
                match state {
                    Some(state) => {
                        table
                            .insert((ns.as_str(), id.as_u128()), *state)
                            .map_err(storage)?;
                    }
                    None => {
                        table.remove((ns.as_str(), id.as_u128())).map_err(storage)?;
                    }
                }
            }
            Op::Index { ns, name, def } => {
                let current = self
                    .index_defs(ns)?
                    .into_iter()
                    .find(|existing| existing.name == *name);
                let wanted: Option<crate::IndexDef> = def
                    .as_ref()
                    .map(|def| serde_json::from_value(def.clone()))
                    .transpose()
                    .map_err(|e| Error::Serialization(e.to_string()))?;
                if current != wanted {
                    if current.is_some() {
                        self.drop_index(ns, name)?;
                    }
                    if let Some(wanted) = wanted {
                        self.create_index(ns, wanted)?;
                    }
                }
            }
            Op::Constraint {
                ns,
                name,
                constraint,
            } => {
                self.constraint_cache = None;
                let mut table = self
                    .tx
                    .open_table(crate::constraints::CONSTRAINTS)
                    .map_err(storage)?;
                match constraint {
                    Some(value) => {
                        table
                            .insert((ns.as_str(), name.as_str()), bytes_of(value)?.as_slice())
                            .map_err(storage)?;
                    }
                    None => {
                        table
                            .remove((ns.as_str(), name.as_str()))
                            .map_err(storage)?;
                    }
                }
            }
            Op::Principal { name, principal } => {
                let mut table = self
                    .tx
                    .open_table(crate::access::PRINCIPALS)
                    .map_err(storage)?;
                match principal {
                    Some(value) => {
                        table
                            .insert(name.as_str(), bytes_of(value)?.as_slice())
                            .map_err(storage)?;
                    }
                    None => {
                        table.remove(name.as_str()).map_err(storage)?;
                    }
                }
            }
            Op::Namespace { ns, meta } => {
                let mut table = self
                    .tx
                    .open_table(crate::namespace::NAMESPACES)
                    .map_err(storage)?;
                match meta {
                    Some(value) => {
                        table
                            .insert(ns.as_str(), bytes_of(value)?.as_slice())
                            .map_err(storage)?;
                    }
                    None => {
                        table.remove(ns.as_str()).map_err(storage)?;
                    }
                }
            }
            Op::MatchRule { ns, label, rule } => {
                let mut table = self
                    .tx
                    .open_table(crate::resolve::MATCH_RULES)
                    .map_err(storage)?;
                match rule {
                    Some(value) => {
                        table
                            .insert((ns.as_str(), label.as_str()), bytes_of(value)?.as_slice())
                            .map_err(storage)?;
                    }
                    None => {
                        table
                            .remove((ns.as_str(), label.as_str()))
                            .map_err(storage)?;
                    }
                }
            }
            Op::State {
                ns,
                kind,
                key,
                value,
            } => match value {
                Some(value) => self.put_state(ns, kind, key, value)?,
                None => {
                    self.delete_state(ns, kind, key)?;
                }
            },
            Op::Merge { ns, id, record } => {
                let id: u128 = id
                    .parse()
                    .map_err(|_| Error::Serialization(format!("bad merge id {id:?}")))?;
                let mut table = self.tx.open_table(crate::merge::MERGES).map_err(storage)?;
                match record {
                    Some(value) => {
                        table
                            .insert((ns.as_str(), id), bytes_of(value)?.as_slice())
                            .map_err(storage)?;
                    }
                    None => {
                        table.remove((ns.as_str(), id)).map_err(storage)?;
                    }
                }
            }
            Op::Bulk(Bulk::DropNamespace { ns }) => {
                if self.namespace(ns)?.is_some() {
                    self.drop_namespace(ns)?;
                }
            }
            Op::Bulk(Bulk::RebuildIndexes { ns }) => {
                self.rebuild_indexes(ns)?;
            }
            Op::Bulk(Bulk::CompactVectors { ns }) => {
                self.compact_vectors(ns)?;
            }
        }
        Ok(())
    }

    /// Drops the log's entries up to `through` (kept in a snapshot): a
    /// member that needs them catches up from a backup instead.
    pub fn truncate_log(&mut self, through: u64) -> Result<u64> {
        let mut meta = self.tx.open_table(META).map_err(storage)?;
        let (first, last) = (meta_get(&meta, FIRST)?, meta_get(&meta, LAST)?);
        let through = through.min(last);
        if first == 0 || through < first {
            return Ok(0);
        }
        let mut log = self.tx.open_table(REPLICATION_LOG).map_err(storage)?;
        let mut dropped = 0;
        for index in first..=through {
            if log.remove(index).map_err(storage)?.is_some() {
                dropped += 1;
            }
        }
        let next = if through == last { 0 } else { through + 1 };
        meta.insert(FIRST, next).map_err(storage)?;
        Ok(dropped)
    }
}

impl ReadTx {
    /// Where this database stands in its replication log.
    pub fn log_position(&self) -> Result<LogPosition> {
        let meta = self.tx.open_table(META).map_err(storage)?;
        Ok(LogPosition {
            enabled: meta_get(&meta, ENABLED)? == 1,
            first: meta_get(&meta, FIRST)?,
            last: meta_get(&meta, LAST)?,
            applied: meta_get(&meta, APPLIED)?,
        })
    }

    /// Up to `limit` log entries after `after`, in order.
    pub fn log_entries(&self, after: u64, limit: usize) -> Result<Vec<(u64, Entry)>> {
        let table = self.tx.open_table(REPLICATION_LOG).map_err(storage)?;
        let mut out = Vec::new();
        for item in table.range(after.saturating_add(1)..).map_err(storage)? {
            if out.len() >= limit {
                break;
            }
            let (index, value) = item.map_err(storage)?;
            out.push((index.value(), decode(value.value())?));
        }
        Ok(out)
    }
}
