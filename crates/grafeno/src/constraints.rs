//! Constraints on facts: how many current facts of a label a node may have
//! (one per source, per target, or per pair), and properties whose earlier
//! values are kept with the time they held.
//!
//! When a new fact contradicts a current one, the default is to end the
//! old one in valid time where the new one starts (the world changed): its
//! record stays, so `KNOWN AT` still shows what was believed. A constraint
//! can instead only flag the contradiction, or refuse the new fact. Every
//! invalidation and flag is recorded and can be listed.

use grafeno_core::{Edge, EdgeId, Error, Node, NodeId, Result, Timestamp, Value};
use redb::{ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

use crate::db::{ReadTx, WriteTx};
use crate::store::{decode, encode, storage};
use crate::traverse::{Direction, EdgeFilter};

/// `(ns, name) -> Constraint` (JSON).
pub(crate) const CONSTRAINTS: TableDefinition<(&str, &str), &[u8]> =
    TableDefinition::new("constraints");

const EVENTS: &str = "constraint.event";
const HISTORY: &str = "property.history";

/// Which facts of a label may not hold at the same time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// One current fact per source node (a person works at one company).
    Source,
    /// One per target node (a seat has one occupant).
    Target,
    /// One per pair of nodes (one current `rates` between two nodes).
    Pair,
}

/// What a contradicting fact does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnConflict {
    /// End the old fact where the new one starts (or the new one where the
    /// old starts, if the new one is older).
    #[default]
    Invalidate,
    /// Keep both and record the contradiction.
    Flag,
    /// Refuse the new fact.
    Reject,
}

/// A rule on facts or properties.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Constraint {
    /// `CREATE CONSTRAINT FOR ()-[r:works_at]->() REQUIRE ONE PER SOURCE`
    Edge {
        name: String,
        label: String,
        scope: Scope,
        #[serde(default)]
        on_conflict: OnConflict,
    },
    /// `CREATE CONSTRAINT FOR (n:Person) REQUIRE n.city IS FUNCTIONAL`: the
    /// property has one value at a time, and earlier ones are kept with the
    /// time they held.
    Functional {
        name: String,
        label: String,
        key: String,
    },
}

impl Constraint {
    pub fn name(&self) -> &str {
        match self {
            Constraint::Edge { name, .. } | Constraint::Functional { name, .. } => name,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Constraint::Edge { label, .. } | Constraint::Functional { label, .. } => label,
        }
    }

    /// `works_at_one_per_source`, `Person_city_functional`.
    pub fn default_name(&self) -> String {
        match self {
            Constraint::Edge { label, scope, .. } => format!(
                "{label}_one_per_{}",
                match scope {
                    Scope::Source => "source",
                    Scope::Target => "target",
                    Scope::Pair => "pair",
                }
            ),
            Constraint::Functional { label, key, .. } => format!("{label}_{key}_functional"),
        }
    }
}

/// What a constraint did to a fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// The old fact now ends where the new one starts (a new version of
    /// its record; the old record is expired).
    Invalidated,
    /// The new fact, older than the current one, ends where it starts.
    Shortened,
    /// Both started at the same time: the old one was retracted.
    Replaced,
    /// Both kept; the contradiction is recorded.
    Flagged,
}

/// One contradiction and what was done about it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConstraintEvent {
    pub constraint: String,
    pub action: Action,
    /// The fact being written, and the one it contradicted.
    pub new_edge: EdgeId,
    pub old_edge: EdgeId,
    /// For an invalidation: the new version of the old fact, which ends at
    /// `at` (the old record is expired, so `KNOWN AT` keeps what was
    /// believed).
    #[serde(default)]
    pub replacement: Option<EdgeId>,
    /// Where the fact that changed now ends.
    pub at: Option<Timestamp>,
    pub recorded_at: Timestamp,
    pub by: Option<String>,
}

/// A value a property had, and when.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub value: Value,
    /// Since (the node's creation, or the change before), until the change.
    pub from: Timestamp,
    pub to: Timestamp,
    pub by: Option<String>,
}

fn all(
    table: &impl ReadableTable<(&'static str, &'static str), &'static [u8]>,
    ns: &str,
) -> Result<Vec<Constraint>> {
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

fn events_from(entries: Vec<(String, Vec<u8>)>) -> Result<Vec<ConstraintEvent>> {
    let mut out: Vec<ConstraintEvent> = entries
        .into_iter()
        .map(|(_, bytes)| {
            serde_json::from_slice(&bytes).map_err(|e| Error::Serialization(e.to_string()))
        })
        .collect::<Result<_>>()?;
    out.reverse();
    Ok(out)
}

fn history_from(entries: Vec<(String, Vec<u8>)>) -> Result<Vec<HistoryEntry>> {
    entries
        .into_iter()
        .map(|(_, bytes)| {
            serde_json::from_slice(&bytes).map_err(|e| Error::Serialization(e.to_string()))
        })
        .collect()
}

impl ReadTx {
    pub fn constraints(&self, ns: &str) -> Result<Vec<Constraint>> {
        all(&self.constraints, ns)
    }

    /// What constraints did, newest first.
    pub fn constraint_events(&self, ns: &str, limit: usize) -> Result<Vec<ConstraintEvent>> {
        let entries = self.state_entries(ns, EVENTS, "", usize::MAX)?;
        let mut events = events_from(entries)?;
        events.truncate(limit);
        Ok(events)
    }

    /// The earlier values of a functional property of a node, oldest first.
    pub fn property_history(&self, ns: &str, node: NodeId, key: &str) -> Result<Vec<HistoryEntry>> {
        if let Some(view) = self.view() {
            // Nothing of a node or a property the view may not see.
            match self.node(ns, node)? {
                Some(found) if !view.property_hidden(ns, &found.labels, key) => {}
                _ => return Ok(Vec::new()),
            }
        }
        history_from(self.state_entries(ns, HISTORY, &format!("{node}/{key}/"), usize::MAX)?)
    }
}

/// When a fact holds: `[from, to)`, open ends as infinities.
fn span(edge: &Edge) -> (i64, i64) {
    (
        edge.valid_from.map_or(i64::MIN, Timestamp::as_micros),
        edge.valid_to.map_or(i64::MAX, Timestamp::as_micros),
    )
}

impl WriteTx {
    pub fn constraints(&self, ns: &str) -> Result<Vec<Constraint>> {
        all(&self.tx.open_table(CONSTRAINTS).map_err(storage)?, ns)
    }

    /// Declares a constraint; returns how many current contradictions the
    /// facts already have (each is recorded as flagged, nothing is changed).
    pub fn create_constraint(&mut self, ns: &str, constraint: Constraint) -> Result<u64> {
        crate::store::check_label(constraint.label())?;
        crate::namespace::ensure(&self.tx, ns)?;
        if constraint.name().is_empty() {
            return Err(Error::Invalid("a constraint needs a name".into()));
        }
        let existing = self.constraints(ns)?;
        if let Some(same) = existing.iter().find(|c| c.name() == constraint.name()) {
            if *same == constraint {
                return Ok(0);
            }
            return Err(Error::Invalid(format!(
                "a constraint named {:?} already exists",
                constraint.name()
            )));
        }
        if self
            .index_defs(ns)?
            .iter()
            .any(|def| def.name == constraint.name())
        {
            return Err(Error::Invalid(format!(
                "an index is named {:?}",
                constraint.name()
            )));
        }
        self.note(crate::replication::Touch::Constraint(
            ns.to_owned(),
            constraint.name().to_owned(),
        ));
        self.tx
            .open_table(CONSTRAINTS)
            .map_err(storage)?
            .insert((ns, constraint.name()), encode(&constraint)?.as_slice())
            .map_err(storage)?;
        self.constraint_cache = None;
        let Constraint::Edge {
            name, label, scope, ..
        } = &constraint
        else {
            return Ok(0);
        };
        // Contradictions already there: flagged, left as they are.
        let mut found = 0;
        let mut seen = std::collections::HashSet::new();
        let edges = self.edges_with_label(ns, label)?;
        for edge in &edges {
            for other in self.contradictions(ns, edge, *scope)? {
                let pair = if edge.id < other.id {
                    (edge.id, other.id)
                } else {
                    (other.id, edge.id)
                };
                if seen.insert(pair) {
                    found += 1;
                    self.record_event(ns, name, Action::Flagged, pair.1, pair.0, None, None)?;
                }
            }
        }
        Ok(found)
    }

    /// Removes a constraint; whether it existed.
    pub fn drop_constraint(&mut self, ns: &str, name: &str) -> Result<bool> {
        self.constraint_cache = None;
        self.note(crate::replication::Touch::Constraint(
            ns.to_owned(),
            name.to_owned(),
        ));
        Ok(self
            .tx
            .open_table(CONSTRAINTS)
            .map_err(storage)?
            .remove((ns, name))
            .map_err(storage)?
            .is_some())
    }

    pub fn constraint_events(&self, ns: &str, limit: usize) -> Result<Vec<ConstraintEvent>> {
        let mut events = events_from(self.state_entries(ns, EVENTS, "", usize::MAX)?)?;
        events.truncate(limit);
        Ok(events)
    }

    /// What constraints did in this transaction so far; emptied.
    pub fn take_constraint_events(&mut self) -> Vec<ConstraintEvent> {
        std::mem::take(&mut self.constraint_log)
    }

    pub(crate) fn cached_constraints(&mut self, ns: &str) -> Result<Vec<Constraint>> {
        if let Some((cached_ns, cached)) = &self.constraint_cache
            && cached_ns == ns
        {
            return Ok(cached.clone());
        }
        let found = self.constraints(ns)?;
        self.constraint_cache = Some((ns.to_owned(), found.clone()));
        Ok(found)
    }

    fn edges_with_label(&self, ns: &str, label: &str) -> Result<Vec<Edge>> {
        let table = self.tx.open_table(crate::store::EDGES).map_err(storage)?;
        let mut out = Vec::new();
        for item in table.range((ns, 0u128)..).map_err(storage)? {
            let (key, value) = item.map_err(storage)?;
            if key.value().0 != ns {
                break;
            }
            let edge: Edge = decode(value.value())?;
            if edge.label == label {
                out.push(edge);
            }
        }
        Ok(out)
    }

    /// The current facts that `edge` contradicts under `scope` (they
    /// overlap it in valid time).
    fn contradictions(&self, ns: &str, edge: &Edge, scope: Scope) -> Result<Vec<Edge>> {
        let filter = EdgeFilter::any().label(edge.label.clone());
        let (node, direction) = match scope {
            Scope::Source | Scope::Pair => (edge.from, Direction::Out),
            Scope::Target => (edge.to, Direction::In),
        };
        let (start, end) = span(edge);
        let mut out = Vec::new();
        for (id, other_end) in self.neighbor_ids(ns, node, direction, &filter)? {
            if id == edge.id {
                continue;
            }
            let fits = match scope {
                // The same fact again is not a contradiction.
                Scope::Source => other_end != edge.to,
                Scope::Target => other_end != edge.from,
                Scope::Pair => other_end == edge.to,
            };
            if !fits {
                continue;
            }
            let Some(other) = self.edge(ns, id)? else {
                continue;
            };
            if other.is_retracted() {
                continue;
            }
            let (s, e) = span(&other);
            if s < end && start < e {
                out.push(other);
            }
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn record_event(
        &mut self,
        ns: &str,
        constraint: &str,
        action: Action,
        new_edge: EdgeId,
        old_edge: EdgeId,
        at: Option<Timestamp>,
        replacement: Option<EdgeId>,
    ) -> Result<()> {
        let event = ConstraintEvent {
            constraint: constraint.to_owned(),
            action,
            new_edge,
            old_edge,
            replacement,
            at,
            recorded_at: Timestamp::now(),
            by: self.actor.as_ref().map(|actor| actor.name.clone()),
        };
        let bytes = serde_json::to_vec(&event).map_err(|e| Error::Serialization(e.to_string()))?;
        // Keys in time order: the new edge's id is a v7 uuid.
        let key = format!("{}/{}", event.new_edge, event.old_edge);
        self.put_state(ns, EVENTS, &key, &bytes)?;
        self.constraint_log.push(event);
        Ok(())
    }

    /// Applies the edge constraints of the namespace to a fact about to be
    /// written: may change its valid time, and returns the old facts to end
    /// once it is written. Fails for a `reject` constraint.
    pub(crate) fn check_edge(
        &mut self,
        ns: &str,
        edge: &mut Edge,
    ) -> Result<Vec<(String, Edge, Action)>> {
        let constraints = self.cached_constraints(ns)?;
        let mut pending = Vec::new();
        for constraint in constraints {
            let Constraint::Edge {
                name,
                label,
                scope,
                on_conflict,
            } = constraint
            else {
                continue;
            };
            if label != edge.label {
                continue;
            }
            let found = self.contradictions(ns, edge, scope)?;
            if found.is_empty() {
                continue;
            }
            if on_conflict == OnConflict::Reject {
                return Err(Error::Conflict(format!(
                    "{name}: {} already has a current {label} fact ({})",
                    match scope {
                        Scope::Target => edge.to,
                        _ => edge.from,
                    },
                    found[0].id
                )));
            }
            // A fact with no start begins now when it replaces another.
            if on_conflict == OnConflict::Invalidate
                && edge.valid_from.is_none()
                && edge.valid_to.is_none_or(|to| to > edge.recorded_at)
            {
                edge.valid_from = Some(edge.recorded_at);
            }
            for old in found {
                let mut action = on_conflict;
                // An agent limited to its own records may not end others'.
                if action == OnConflict::Invalidate && self.guard_edge(&old).is_err() {
                    action = OnConflict::Flag;
                }
                if action == OnConflict::Flag {
                    pending.push((name.clone(), old, Action::Flagged));
                    continue;
                }
                let (new_start, _) = span(edge);
                let (old_start, _) = span(&old);
                if old_start < new_start {
                    pending.push((name.clone(), old, Action::Invalidated));
                } else if old_start > new_start {
                    // The new fact is older: it ends where the old one starts.
                    let until = old.valid_from.expect("a later start is a set start");
                    if edge.valid_to.is_none_or(|to| to > until) {
                        edge.valid_to = Some(until);
                    }
                    pending.push((name.clone(), old, Action::Shortened));
                } else {
                    pending.push((name.clone(), old, Action::Replaced));
                }
            }
        }
        Ok(pending)
    }

    /// Ends or retracts the old facts `check_edge` found, now that `edge` is
    /// written, and records what was done.
    pub(crate) fn settle_edge(
        &mut self,
        ns: &str,
        edge: &Edge,
        pending: Vec<(String, Edge, Action)>,
    ) -> Result<()> {
        for (name, old, action) in pending {
            let mut replacement = None;
            let at = match action {
                Action::Invalidated => {
                    let at = edge.valid_from.expect("set by check_edge");
                    if old.valid_to.is_none_or(|to| to > at) {
                        // A new version that ends at `at`, and the old record
                        // expired: `KNOWN AT` before now still sees the fact
                        // as it was believed.
                        let now = Timestamp::now();
                        let version = Edge {
                            id: EdgeId::new(),
                            valid_to: Some(at),
                            recorded_at: now,
                            expired_at: None,
                            ..old.clone()
                        };
                        self.retract_edge(ns, old.id)?;
                        replacement = Some(version.id);
                        self.restore_edge(ns, version)?;
                    }
                    Some(at)
                }
                Action::Shortened => edge.valid_to,
                Action::Replaced => {
                    self.retract_edge(ns, old.id)?;
                    None
                }
                Action::Flagged => None,
            };
            self.record_event(ns, &name, action, edge.id, old.id, at, replacement)?;
        }
        Ok(())
    }

    /// Keeps the earlier value of functional properties that change.
    pub(crate) fn record_history(
        &mut self,
        ns: &str,
        node: &Node,
        previous: Option<&Node>,
    ) -> Result<()> {
        let Some(previous) = previous else {
            return Ok(());
        };
        let constraints = self.cached_constraints(ns)?;
        for constraint in constraints {
            let Constraint::Functional { label, key, .. } = constraint else {
                continue;
            };
            if !previous.has_label(&label) {
                continue;
            }
            let Some(old) = previous.props.get(&key) else {
                continue;
            };
            if node.props.get(&key) == Some(old) {
                continue;
            }
            let prefix = format!("{}/{key}/", node.id);
            let from = self
                .state_entries(ns, HISTORY, &prefix, usize::MAX)?
                .last()
                .and_then(|(_, bytes)| serde_json::from_slice::<HistoryEntry>(bytes).ok())
                .map_or(previous.created_at, |entry| entry.to);
            let to = Timestamp::now();
            let entry = HistoryEntry {
                value: old.clone(),
                from,
                to,
                by: self.actor.as_ref().map(|actor| actor.name.clone()),
            };
            let bytes =
                serde_json::to_vec(&entry).map_err(|e| Error::Serialization(e.to_string()))?;
            self.put_state(
                ns,
                HISTORY,
                &format!("{prefix}{:020}", to.as_micros()),
                &bytes,
            )?;
        }
        Ok(())
    }

    pub fn property_history(&self, ns: &str, node: NodeId, key: &str) -> Result<Vec<HistoryEntry>> {
        history_from(self.state_entries(ns, HISTORY, &format!("{node}/{key}/"), usize::MAX)?)
    }
}
