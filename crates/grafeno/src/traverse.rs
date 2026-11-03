//! Traversals over a read snapshot: neighbors, k hops, shortest path and subgraph.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};

use grafeno_core::{Edge, EdgeId, Error, Node, NodeId, Result, Timestamp};
use redb::ReadOnlyTable;
use serde::{Deserialize, Serialize};

use crate::ReadTx;
use crate::store::{AdjKey, AdjValue, Adjacent, adjacency};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Out,
    In,
    #[default]
    Both,
}

/// Which edges a traversal follows.
///
/// By default, current knowledge: edges that were not retracted, with any
/// label, regardless of their valid time.
#[derive(Clone, Debug, Default)]
pub struct EdgeFilter {
    labels: Vec<String>,
    valid_at: Option<Timestamp>,
    known_at: Option<Timestamp>,
}

impl EdgeFilter {
    pub fn any() -> Self {
        Self::default()
    }

    /// Only edges with this label (can be repeated).
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.labels.push(label.into());
        self
    }

    /// Only facts that are true at `at`.
    pub fn valid_at(mut self, at: Timestamp) -> Self {
        self.valid_at = Some(at);
        self
    }

    /// What was known at `at`, including edges retracted later.
    pub fn known_at(mut self, at: Timestamp) -> Self {
        self.known_at = Some(at);
        self
    }

    pub fn accepts(&self, edge: &Edge) -> bool {
        let known = match self.known_at {
            Some(at) => edge.known_at(at),
            None => !edge.is_retracted(),
        };
        known
            && self.valid_at.is_none_or(|at| edge.holds_at(at))
            && (self.labels.is_empty() || self.labels.contains(&edge.label))
    }

    /// Like [`EdgeFilter::accepts`], for an adjacency entry. Labels are not
    /// checked here: entries are read by label already.
    pub(crate) fn accepts_entry(&self, entry: &Adjacent) -> bool {
        let known = match self.known_at {
            Some(at) => entry.recorded_at <= at.as_micros() && at.as_micros() < entry.expired_at,
            None => entry.expired_at == i64::MAX,
        };
        known
            && self.valid_at.is_none_or(|at| {
                entry.valid_from <= at.as_micros() && at.as_micros() < entry.valid_to
            })
    }
}

/// A neighbor: the node at the other end and the edge that leads to it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Neighbor {
    pub node: NodeId,
    pub edge: Edge,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Path {
    /// From the origin to the target, both included.
    pub nodes: Vec<NodeId>,
    /// `edges[i]` joins `nodes[i]` and `nodes[i + 1]`.
    pub edges: Vec<EdgeId>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Subgraph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl ReadTx {
    pub fn neighbors(
        &self,
        ns: &str,
        node: NodeId,
        direction: Direction,
        filter: &EdgeFilter,
    ) -> Result<Vec<Neighbor>> {
        let mut out = Vec::new();
        for entry in self.adjacent(ns, node, direction, filter)? {
            let edge = self
                .edge(ns, entry.edge)?
                .ok_or(Error::EdgeNotFound(entry.edge))?;
            out.push(Neighbor {
                node: entry.other,
                edge,
            });
        }
        Ok(out)
    }

    /// The adjacency entries of `node` that pass `filter`, each edge once.
    /// Reads no edge records: the filter's fields are in the entries.
    pub(crate) fn adjacent(
        &self,
        ns: &str,
        node: NodeId,
        direction: Direction,
        filter: &EdgeFilter,
    ) -> Result<Vec<Adjacent>> {
        let tables: Vec<&ReadOnlyTable<AdjKey, AdjValue>> = match direction {
            Direction::Out => vec![&self.out],
            Direction::In => vec![&self.inn],
            Direction::Both => vec![&self.out, &self.inn],
        };
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for table in tables {
            let entries = if filter.labels.is_empty() {
                adjacency(table, ns, node, None)?
            } else {
                let mut entries = Vec::new();
                for label in &filter.labels {
                    entries.extend(adjacency(table, ns, node, Some(label))?);
                }
                entries
            };
            for entry in entries {
                // With `Both`, a self-loop shows up in both tables.
                if filter.accepts_entry(&entry) && seen.insert(entry.edge) {
                    out.push(entry);
                }
            }
        }
        Ok(out)
    }

    /// Nodes `1..=depth` hops away from `start`, with their distance, in BFS order.
    pub fn k_hop(
        &self,
        ns: &str,
        start: NodeId,
        depth: usize,
        direction: Direction,
        filter: &EdgeFilter,
    ) -> Result<Vec<(NodeId, usize)>> {
        let mut distance = HashMap::from([(start, 0usize)]);
        let mut queue = VecDeque::from([start]);
        let mut out = Vec::new();
        while let Some(current) = queue.pop_front() {
            let d = distance[&current];
            if d == depth {
                continue;
            }
            for neighbor in self.adjacent(ns, current, direction, filter)? {
                if let Entry::Vacant(entry) = distance.entry(neighbor.other) {
                    entry.insert(d + 1);
                    out.push((neighbor.other, d + 1));
                    queue.push_back(neighbor.other);
                }
            }
        }
        Ok(out)
    }

    /// The path with the fewest hops from `from` to `to`, of at most `max_depth` hops.
    pub fn shortest_path(
        &self,
        ns: &str,
        from: NodeId,
        to: NodeId,
        max_depth: usize,
        direction: Direction,
        filter: &EdgeFilter,
    ) -> Result<Option<Path>> {
        if from == to {
            return Ok(Some(Path {
                nodes: vec![from],
                edges: Vec::new(),
            }));
        }
        let mut parent: HashMap<NodeId, (NodeId, EdgeId)> = HashMap::new();
        let mut distance = HashMap::from([(from, 0usize)]);
        let mut queue = VecDeque::from([from]);
        while let Some(current) = queue.pop_front() {
            let d = distance[&current];
            if d == max_depth {
                continue;
            }
            for neighbor in self.adjacent(ns, current, direction, filter)? {
                if distance.contains_key(&neighbor.other) {
                    continue;
                }
                distance.insert(neighbor.other, d + 1);
                parent.insert(neighbor.other, (current, neighbor.edge));
                if neighbor.other == to {
                    return Ok(Some(rebuild(&parent, from, to)));
                }
                queue.push_back(neighbor.other);
            }
        }
        Ok(None)
    }

    /// The nodes at most `depth` hops away from `seeds` and the edges among them.
    pub fn subgraph(
        &self,
        ns: &str,
        seeds: &[NodeId],
        depth: usize,
        direction: Direction,
        filter: &EdgeFilter,
    ) -> Result<Subgraph> {
        let mut order: Vec<NodeId> = Vec::new();
        let mut members = HashSet::new();
        for &seed in seeds {
            if members.insert(seed) {
                order.push(seed);
            }
            for (node, _) in self.k_hop(ns, seed, depth, direction, filter)? {
                if members.insert(node) {
                    order.push(node);
                }
            }
        }
        let mut result = Subgraph::default();
        let mut seen_edges = HashSet::new();
        for &id in &order {
            result
                .nodes
                .push(self.node(ns, id)?.ok_or(Error::NodeNotFound(id))?);
            for neighbor in self.neighbors(ns, id, Direction::Out, filter)? {
                if members.contains(&neighbor.node) && seen_edges.insert(neighbor.edge.id) {
                    result.edges.push(neighbor.edge);
                }
            }
        }
        Ok(result)
    }
}

fn rebuild(parent: &HashMap<NodeId, (NodeId, EdgeId)>, from: NodeId, to: NodeId) -> Path {
    let mut nodes = vec![to];
    let mut edges = Vec::new();
    let mut current = to;
    while current != from {
        let (previous, edge) = parent[&current];
        nodes.push(previous);
        edges.push(edge);
        current = previous;
    }
    nodes.reverse();
    edges.reverse();
    Path { nodes, edges }
}
