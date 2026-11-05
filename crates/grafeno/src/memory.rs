//! Memory primitives: names for entity resolution, importance and access
//! (for decay), and merging duplicate nodes.

use std::collections::BTreeSet;

use grafeno_core::{Error, Node, NodeId, Result, Timestamp, Value};
use redb::ReadableTable;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::db::{ReadTx, WriteTx};
use crate::store::{LabelKey, MEMORY, NAMES, RecordKey, storage};

/// The importance of a node that never got one.
pub const DEFAULT_IMPORTANCE: f32 = 0.5;

/// Properties that name a node, besides the `aliases` list.
pub(crate) const NAME_KEYS: [&str; 3] = ["name", "title", "label"];

/// A name in the form used to compare names: lowercase, without accents,
/// with any run of non-alphanumeric characters as one space
/// ("  Ana  García-López " → "ana garcia lopez").
pub fn normalize_name(name: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in name.nfd().filter(|c| !is_combining_mark(*c)) {
        if c.is_alphanumeric() {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.extend(c.to_lowercase());
        } else {
            space = true;
        }
    }
    out
}

/// The normalized names of a node: its `name`, `title` and `label`
/// properties and the strings in its `aliases` property.
pub fn names_of(node: &Node) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for key in NAME_KEYS {
        if let Some(Value::String(name)) = node.props.get(key) {
            names.insert(normalize_name(name));
        }
    }
    if let Some(Value::Array(aliases)) = node.props.get("aliases") {
        for alias in aliases.iter().filter_map(Value::as_str) {
            names.insert(normalize_name(alias));
        }
    }
    names.remove("");
    names
}

/// How important a memory is and how it has been used. Recall weighs it, and
/// weak memories (unimportant and long unused) can be archived.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryState {
    /// From 0 to 1; [`DEFAULT_IMPORTANCE`] unless set.
    pub importance: f32,
    /// How many times recall returned it to an agent.
    pub accesses: u32,
    /// The last access, or the node's last update if it was never accessed.
    pub last_access: Timestamp,
}

impl MemoryState {
    pub fn default_for(node: &Node) -> Self {
        Self {
            importance: DEFAULT_IMPORTANCE,
            accesses: 0,
            last_access: node.updated_at,
        }
    }

    /// From 1 (used just now) down to 0, halving every `half_life` since the
    /// last access.
    pub fn recency(&self, now: Timestamp, half_life_secs: f64) -> f32 {
        let age = (now.as_micros() - self.last_access.as_micros()).max(0) as f64 / 1e6;
        0.5f64.powf(age / half_life_secs.max(1.0)) as f32
    }

    /// Importance decayed by recency, with a bonus for frequent use:
    /// `importance × recency × (1 + ln(1 + accesses) / 4)`, capped at 1.
    pub fn strength(&self, now: Timestamp, half_life_secs: f64) -> f32 {
        let use_bonus = 1.0 + (1.0 + self.accesses as f32).ln() / 4.0;
        (self.importance * self.recency(now, half_life_secs) * use_bonus).min(1.0)
    }
}

fn read_state(
    table: &impl ReadableTable<RecordKey, (f32, u32, i64)>,
    ns: &str,
    node: &Node,
) -> Result<MemoryState> {
    Ok(match table.get((ns, node.id.as_u128())).map_err(storage)? {
        Some(guard) => {
            let (importance, accesses, last_access) = guard.value();
            MemoryState {
                importance,
                accesses,
                last_access: Timestamp::from_micros(last_access),
            }
        }
        None => MemoryState::default_for(node),
    })
}

pub(crate) fn find_by_name(
    table: &impl ReadableTable<LabelKey, ()>,
    ns: &str,
    name: &str,
) -> Result<Vec<NodeId>> {
    let name = normalize_name(name);
    let mut out = Vec::new();
    if name.is_empty() {
        return Ok(out);
    }
    for item in table.range((ns, name.as_str(), 0u128)..).map_err(storage)? {
        let (key, _) = item.map_err(storage)?;
        let (key_ns, key_name, node) = key.value();
        if key_ns != ns || key_name != name {
            break;
        }
        out.push(NodeId::from_u128(node));
    }
    Ok(out)
}

impl ReadTx {
    /// The memory state of a node (the default one if it was never set).
    pub fn memory(&self, ns: &str, node: &Node) -> Result<MemoryState> {
        read_state(&self.memory, ns, node)
    }

    /// Nodes whose name, title, label or alias is `name`, compared
    /// normalized ([`normalize_name`]).
    pub fn find_by_name(&self, ns: &str, name: &str) -> Result<Vec<NodeId>> {
        find_by_name(&self.names, ns, name)
    }
}

impl WriteTx {
    pub fn memory(&self, ns: &str, node: &Node) -> Result<MemoryState> {
        let table = self.tx.open_table(MEMORY).map_err(storage)?;
        read_state(&table, ns, node)
    }

    pub fn find_by_name(&self, ns: &str, name: &str) -> Result<Vec<NodeId>> {
        let table = self.tx.open_table(NAMES).map_err(storage)?;
        find_by_name(&table, ns, name)
    }

    pub(crate) fn put_memory(&mut self, ns: &str, id: NodeId, state: MemoryState) -> Result<()> {
        self.tx
            .open_table(MEMORY)
            .map_err(storage)?
            .insert(
                (ns, id.as_u128()),
                (
                    state.importance,
                    state.accesses,
                    state.last_access.as_micros(),
                ),
            )
            .map_err(storage)?;
        Ok(())
    }

    /// Sets how important a node is, from 0 to 1.
    pub fn set_importance(&mut self, ns: &str, id: NodeId, importance: f32) -> Result<()> {
        if !(0.0..=1.0).contains(&importance) {
            return Err(Error::Invalid("importance must be between 0 and 1".into()));
        }
        let node = self.node(ns, id)?.ok_or(Error::NodeNotFound(id))?;
        self.guard_node(&node)?;
        let state = MemoryState {
            importance,
            ..self.memory(ns, &node)?
        };
        self.put_memory(ns, id, state)
    }

    /// Records that `ids` were used at `at` (one more access each). Missing
    /// nodes are skipped.
    pub fn touch(&mut self, ns: &str, ids: &[NodeId], at: Timestamp) -> Result<()> {
        for &id in ids {
            let Some(node) = self.node(ns, id)? else {
                continue;
            };
            let state = self.memory(ns, &node)?;
            self.put_memory(
                ns,
                id,
                MemoryState {
                    accesses: state.accesses.saturating_add(1),
                    last_access: state.last_access.max(at),
                    ..state
                },
            )?;
        }
        Ok(())
    }

    /// Writes a node's memory state as given. Used to import exports.
    pub fn restore_memory(&mut self, ns: &str, id: NodeId, state: MemoryState) -> Result<()> {
        self.node(ns, id)?.ok_or(Error::NodeNotFound(id))?;
        self.put_memory(ns, id, state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_normalized() {
        assert_eq!(normalize_name("  Ana  García-López "), "ana garcia lopez");
        assert_eq!(normalize_name("ACME, Inc."), "acme inc");
        assert_eq!(normalize_name("¿?"), "");
    }

    #[test]
    fn strength_decays_with_time_and_grows_with_use() {
        let now = Timestamp::from_secs(1_000_000);
        let day = 86_400.0;
        let fresh = MemoryState {
            importance: 0.8,
            accesses: 0,
            last_access: now,
        };
        let old = MemoryState {
            last_access: Timestamp::from_secs(1_000_000 - 30 * 86_400),
            ..fresh
        };
        assert!((fresh.recency(now, 30.0 * day) - 1.0).abs() < 1e-6);
        assert!((old.recency(now, 30.0 * day) - 0.5).abs() < 1e-3);
        assert!(old.strength(now, 30.0 * day) < fresh.strength(now, 30.0 * day));
        let used = MemoryState {
            accesses: 20,
            ..old
        };
        assert!(used.strength(now, 30.0 * day) > old.strength(now, 30.0 * day));
    }
}
