//! Export and import: a JSON Lines dump of nodes (with their text and
//! embedding) and edges, for backups, migrations and test data.
//!
//! The first line is a header; then, per namespace, its nodes and then its
//! edges, so every edge comes after both of its ends:
//!
//! ```text
//! {"type":"header","format":"grafeno-export","version":1,"exported_at":1793664000000000}
//! {"type":"node","ns":"vault","node":{"id":"…","labels":["Note"],…},"text":"…","embedding":[…]}
//! {"type":"edge","ns":"vault","edge":{"id":"…","label":"links_to","from":"…","to":"…",…}}
//! {"type":"index","ns":"vault","index":{"name":"Note_title","label":"Note","key":"title","unique":false}}
//! ```
//!
//! Records keep their ids and timestamps. Embeddings are exported normalized.

use std::collections::BTreeSet;
use std::io::{BufRead, Write};

use grafeno_core::{Edge, Error, Node, NodeId, Result, Timestamp};
use serde::{Deserialize, Serialize};

use crate::db::{Grafeno, ReadTx};
use crate::index::IndexDef;
use crate::memory::MemoryState;
use crate::store::{decode, storage};

pub const EXPORT_FORMAT: &str = "grafeno-export";
/// The newest export version this build writes and reads.
pub const EXPORT_VERSION: u32 = 1;

/// One line of an export.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Record {
    Header {
        format: String,
        version: u32,
        exported_at: Timestamp,
    },
    Node {
        ns: String,
        node: Node,
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        embedding: Option<Vec<f32>>,
        /// Present when it differs from the default state.
        #[serde(skip_serializing_if = "Option::is_none")]
        memory: Option<MemoryState>,
    },
    Edge {
        ns: String,
        edge: Edge,
    },
    /// A declared property index, written before the namespace's nodes.
    Index {
        ns: String,
        index: IndexDef,
    },
}

/// A line as read. Deserializing it directly (instead of through serde's
/// internally tagged enums, which buffer numbers as `f64`) keeps embeddings
/// bit for bit.
#[derive(Deserialize)]
struct RawRecord {
    #[serde(rename = "type")]
    kind: String,
    format: Option<String>,
    version: Option<u32>,
    exported_at: Option<Timestamp>,
    ns: Option<String>,
    node: Option<Node>,
    text: Option<String>,
    embedding: Option<Vec<f32>>,
    memory: Option<MemoryState>,
    edge: Option<Edge>,
    index: Option<IndexDef>,
}

impl RawRecord {
    fn into_record(self) -> std::result::Result<Record, String> {
        let missing = |field: &str| format!("a {} record needs `{field}`", self.kind);
        Ok(match self.kind.as_str() {
            "header" => Record::Header {
                format: self.format.clone().ok_or_else(|| missing("format"))?,
                version: self.version.ok_or_else(|| missing("version"))?,
                exported_at: self.exported_at.unwrap_or(Timestamp::from_micros(0)),
            },
            "node" => Record::Node {
                ns: self.ns.clone().ok_or_else(|| missing("ns"))?,
                node: self.node.clone().ok_or_else(|| missing("node"))?,
                text: self.text,
                embedding: self.embedding,
                memory: self.memory,
            },
            "edge" => Record::Edge {
                ns: self.ns.clone().ok_or_else(|| missing("ns"))?,
                edge: self.edge.clone().ok_or_else(|| missing("edge"))?,
            },
            "index" => Record::Index {
                ns: self.ns.clone().ok_or_else(|| missing("ns"))?,
                index: self.index.clone().ok_or_else(|| missing("index"))?,
            },
            other => return Err(format!("unknown record type {other:?}")),
        })
    }
}

/// How much an export or import moved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DumpStats {
    pub namespaces: usize,
    pub nodes: u64,
    pub edges: u64,
}

fn write_record(out: &mut impl Write, record: &Record) -> Result<()> {
    serde_json::to_writer(&mut *out, record).map_err(|e| Error::Serialization(e.to_string()))?;
    out.write_all(b"\n")
        .map_err(|e| Error::Storage(format!("cannot write the export: {e}")))
}

impl ReadTx {
    /// Writes `namespaces` (every namespace when empty) as JSON Lines. The
    /// export is consistent: it comes from this transaction's snapshot.
    pub fn export(&self, namespaces: &[String], out: &mut impl Write) -> Result<DumpStats> {
        let names: Vec<String> = if namespaces.is_empty() {
            self.namespaces()?
                .into_iter()
                .map(|info| info.name)
                .collect()
        } else {
            namespaces.to_vec()
        };
        write_record(
            out,
            &Record::Header {
                format: EXPORT_FORMAT.into(),
                version: EXPORT_VERSION,
                exported_at: Timestamp::now(),
            },
        )?;
        let mut stats = DumpStats {
            namespaces: names.len(),
            ..DumpStats::default()
        };
        for ns in &names {
            for index in self.index_defs(ns)? {
                write_record(
                    out,
                    &Record::Index {
                        ns: ns.clone(),
                        index,
                    },
                )?;
            }
            for item in self.nodes.range((ns.as_str(), 0u128)..).map_err(storage)? {
                let (key, value) = item.map_err(storage)?;
                if key.value().0 != ns {
                    break;
                }
                let node: Node = decode(value.value())?;
                let id: NodeId = node.id;
                let memory = self.memory(ns, &node)?;
                let record = Record::Node {
                    ns: ns.clone(),
                    text: self.text(ns, id)?,
                    embedding: self.embedding(ns, id)?,
                    memory: (memory != MemoryState::default_for(&node)).then_some(memory),
                    node,
                };
                write_record(out, &record)?;
                stats.nodes += 1;
            }
            for item in self.edges.range((ns.as_str(), 0u128)..).map_err(storage)? {
                let (key, value) = item.map_err(storage)?;
                if key.value().0 != ns {
                    break;
                }
                let edge: Edge = decode(value.value())?;
                write_record(
                    out,
                    &Record::Edge {
                        ns: ns.clone(),
                        edge,
                    },
                )?;
                stats.edges += 1;
            }
        }
        out.flush()
            .map_err(|e| Error::Storage(format!("cannot write the export: {e}")))?;
        Ok(stats)
    }
}

impl Grafeno {
    /// Reads an export and writes it in one transaction: all of it or nothing.
    /// Records whose ids already exist replace them.
    pub fn import(&self, input: impl BufRead) -> Result<DumpStats> {
        self.write(|tx| {
            let mut stats = DumpStats::default();
            let mut namespaces = BTreeSet::new();
            let mut header = false;
            for (index, line) in input.lines().enumerate() {
                let number = index + 1;
                let at = |message: String| Error::Invalid(format!("line {number}: {message}"));
                let line = line.map_err(|e| at(e.to_string()))?;
                if line.trim().is_empty() {
                    continue;
                }
                let raw: RawRecord = serde_json::from_str(&line).map_err(|e| at(e.to_string()))?;
                match raw.into_record().map_err(at)? {
                    Record::Header {
                        format, version, ..
                    } => {
                        if header {
                            return Err(at("a second header".into()));
                        }
                        if format != EXPORT_FORMAT {
                            return Err(at(format!("not a Grafeno export (format {format:?})")));
                        }
                        if version > EXPORT_VERSION {
                            return Err(at(format!(
                                "export version {version} is newer than this Grafeno reads \
                                 ({EXPORT_VERSION})"
                            )));
                        }
                        header = true;
                    }
                    _ if !header => return Err(at("the export must start with a header".into())),
                    Record::Node {
                        ns,
                        node,
                        text,
                        embedding,
                        memory,
                    } => {
                        let id = node.id;
                        tx.restore_node(&ns, node, text.as_deref(), embedding.as_deref())
                            .map_err(|e| at(e.to_string()))?;
                        if let Some(memory) = memory {
                            tx.restore_memory(&ns, id, memory)
                                .map_err(|e| at(e.to_string()))?;
                        }
                        namespaces.insert(ns);
                        stats.nodes += 1;
                    }
                    Record::Index { ns, index } => {
                        tx.create_index(&ns, index).map_err(|e| at(e.to_string()))?;
                    }
                    Record::Edge { ns, edge } => {
                        tx.restore_edge(&ns, edge).map_err(|e| at(e.to_string()))?;
                        namespaces.insert(ns);
                        stats.edges += 1;
                    }
                }
            }
            if !header {
                return Err(Error::Invalid("the export is empty".into()));
            }
            stats.namespaces = namespaces.len();
            Ok(stats)
        })
    }
}
