//! Graph algorithms over an in-memory projection of a namespace.
//!
//! A [`Projection`] holds the nodes of a namespace (or of some labels) and
//! the edges among them as compressed adjacency arrays (CSR): the
//! neighbors of a node are one slice of a shared array, in both
//! directions. Algorithms that visit the whole graph many times (PageRank,
//! Louvain, betweenness) then read no storage at all.
//!
//! [`ReadTx::projection`] builds one on demand and keeps it in a small
//! cache shared by every read transaction of the database, until the next
//! commit changes the data.
//!
//! Nodes are numbered `0..len()` in id order (creation order); algorithms
//! take and return these positions, and [`Projection::id`] turns them back
//! into node ids.

mod centrality;
mod community;
mod embedding;
mod paths;
mod similarity;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use grafeno_core::{Edge, EdgeId, Error, NodeId, Result, Timestamp};
use redb::ReadableTable;

pub use centrality::{PageRank, betweenness, closeness, degree, page_rank};
pub use community::{
    Louvain, Triangles, label_propagation, louvain, modularity, strongly_connected, triangles,
    weakly_connected,
};
pub use embedding::{FastRp, fast_rp};
pub use paths::{
    WeightedPath, all_shortest_paths, astar, distances, k_shortest_paths, shortest_path,
};
pub use similarity::{
    LinkMetric, SimilarityMetric, link_score, neighbor_similarity, predict_links, similar_neighbors,
};

use crate::store::{
    AdjKey, AdjValue, Adjacent, LabelKey, RecordKey, decode, nodes_with_label, storage,
};
use crate::traverse::{Direction, EdgeFilter};
use crate::{ReadTx, WriteTx};

/// Which part of a namespace a projection holds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ProjectionSpec {
    /// Nodes with any of these labels; every node when empty.
    pub labels: Vec<String>,
    /// Edges with any of these labels; every edge when empty.
    pub edge_labels: Vec<String>,
    /// Only facts true at this time.
    pub valid_at: Option<Timestamp>,
    /// What was known at this time; current knowledge (no retracted
    /// edges) when `None`.
    pub known_at: Option<Timestamp>,
    /// The edge property that weighs each edge. Edges without it, or with
    /// a value that is not a number, weigh 1.
    pub weight: Option<String>,
}

impl ProjectionSpec {
    fn filter(&self) -> EdgeFilter {
        let mut filter = EdgeFilter::any();
        if let Some(at) = self.valid_at {
            filter = filter.valid_at(at);
        }
        if let Some(at) = self.known_at {
            filter = filter.known_at(at);
        }
        filter
    }
}

/// One direction of the adjacency: the edges of node `i` are
/// `start[i]..start[i + 1]` in the other arrays.
#[derive(Debug, Default)]
struct Csr {
    start: Vec<usize>,
    other: Vec<u32>,
    edge: Vec<EdgeId>,
    /// Empty when the projection is not weighted.
    weight: Vec<f64>,
}

impl Csr {
    fn build(len: usize, edges: &[Link], weighted: bool, reverse: bool) -> Self {
        let mut start = vec![0usize; len + 1];
        for link in edges {
            let from = if reverse { link.to } else { link.from };
            start[from as usize + 1] += 1;
        }
        for i in 0..len {
            start[i + 1] += start[i];
        }
        let mut next = start.clone();
        let mut other = vec![0u32; edges.len()];
        let mut edge = vec![EdgeId::from_u128(0); edges.len()];
        let mut weight = if weighted {
            vec![0.0; edges.len()]
        } else {
            Vec::new()
        };
        for link in edges {
            let (from, to) = if reverse {
                (link.to, link.from)
            } else {
                (link.from, link.to)
            };
            let at = next[from as usize];
            next[from as usize] += 1;
            other[at] = to;
            edge[at] = link.edge;
            if weighted {
                weight[at] = link.weight;
            }
        }
        Self {
            start,
            other,
            edge,
            weight,
        }
    }

    fn range(&self, node: u32) -> std::ops::Range<usize> {
        self.start[node as usize]..self.start[node as usize + 1]
    }
}

struct Link {
    from: u32,
    to: u32,
    edge: EdgeId,
    weight: f64,
}

/// One edge seen from a node: the node at the other end, the edge, and its
/// weight (1 when the projection is not weighted).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Step {
    pub node: u32,
    pub edge: EdgeId,
    pub weight: f64,
}

/// The nodes of a namespace and the edges among them, in memory.
#[derive(Debug, Default)]
pub struct Projection {
    ids: Vec<NodeId>,
    index: HashMap<NodeId, u32>,
    out: Csr,
    inn: Csr,
    weighted: bool,
}

impl Projection {
    /// A projection of these nodes and edges `(from, to, edge, weight)`.
    /// Edges with an end outside `ids` are left out.
    pub fn from_edges(
        mut ids: Vec<NodeId>,
        edges: impl IntoIterator<Item = (NodeId, NodeId, EdgeId, f64)>,
        weighted: bool,
    ) -> Self {
        ids.sort_unstable();
        ids.dedup();
        let index: HashMap<NodeId, u32> = ids
            .iter()
            .enumerate()
            .map(|(at, id)| (*id, at as u32))
            .collect();
        let links: Vec<Link> = edges
            .into_iter()
            .filter_map(|(from, to, edge, weight)| {
                Some(Link {
                    from: *index.get(&from)?,
                    to: *index.get(&to)?,
                    edge,
                    weight,
                })
            })
            .collect();
        let out = Csr::build(ids.len(), &links, weighted, false);
        let inn = Csr::build(ids.len(), &links, weighted, true);
        Self {
            ids,
            index,
            out,
            inn,
            weighted,
        }
    }

    /// How many nodes it holds.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// How many edges it holds.
    pub fn edge_count(&self) -> usize {
        self.out.other.len()
    }

    /// Whether edges carry weights from a property.
    pub fn is_weighted(&self) -> bool {
        self.weighted
    }

    /// The id of the node at `position`.
    pub fn id(&self, position: u32) -> NodeId {
        self.ids[position as usize]
    }

    /// The position of a node, if the projection holds it.
    pub fn position(&self, id: NodeId) -> Option<u32> {
        self.index.get(&id).copied()
    }

    /// The edges of `node` in `direction`. With `Both`, a self-loop shows
    /// up once.
    pub fn neighbors(&self, node: u32, direction: Direction) -> impl Iterator<Item = Step> + '_ {
        let out = match direction {
            Direction::Out | Direction::Both => self.out.range(node),
            Direction::In => 0..0,
        };
        let inn = match direction {
            Direction::In | Direction::Both => self.inn.range(node),
            Direction::Out => 0..0,
        };
        let both = direction == Direction::Both;
        let step = |csr: &Csr, at: usize| Step {
            node: csr.other[at],
            edge: csr.edge[at],
            weight: if self.weighted { csr.weight[at] } else { 1.0 },
        };
        out.map(move |at| step(&self.out, at)).chain(
            inn.filter(move |&at| !(both && self.inn.other[at] == node))
                .map(move |at| step(&self.inn, at)),
        )
    }

    /// The nodes the out-edges of `node` lead to.
    pub(crate) fn targets(&self, node: u32) -> &[u32] {
        &self.out.other[self.out.range(node)]
    }

    /// How many edges `node` has in `direction` (a self-loop counts once
    /// with `Both`).
    pub fn degree(&self, node: u32, direction: Direction) -> usize {
        match direction {
            Direction::Out => self.out.range(node).len(),
            Direction::In => self.inn.range(node).len(),
            Direction::Both => self.neighbors(node, direction).count(),
        }
    }

    /// The distinct neighbors of every node in `direction`, sorted, without
    /// the node itself: the simple graph that similarity, link prediction
    /// and triangle counting work on.
    pub(crate) fn neighbor_sets(&self, direction: Direction) -> Vec<Vec<u32>> {
        (0..self.len() as u32)
            .map(|node| {
                let mut set: Vec<u32> = self
                    .neighbors(node, direction)
                    .map(|step| step.node)
                    .filter(|&other| other != node)
                    .collect();
                set.sort_unstable();
                set.dedup();
                set
            })
            .collect()
    }
}

/// Builds a projection from the tables of a read or write transaction.
fn project(
    nodes: &impl ReadableTable<RecordKey, &'static [u8]>,
    labels: &impl ReadableTable<LabelKey, ()>,
    out: &impl ReadableTable<AdjKey, AdjValue>,
    edges: &impl ReadableTable<RecordKey, &'static [u8]>,
    ns: &str,
    spec: &ProjectionSpec,
) -> Result<Projection> {
    let ids = if spec.labels.is_empty() {
        let mut ids = Vec::new();
        for item in nodes.range((ns, 0u128)..).map_err(storage)? {
            let (key, _) = item.map_err(storage)?;
            let (key_ns, id) = key.value();
            if key_ns != ns {
                break;
            }
            ids.push(NodeId::from_u128(id));
        }
        ids
    } else {
        let mut ids = Vec::new();
        for label in &spec.labels {
            ids.extend(nodes_with_label(labels, ns, label)?);
        }
        ids
    };
    let filter = spec.filter();
    let mut links = Vec::new();
    for item in out.range((ns, 0u128, "", 0u128)..).map_err(storage)? {
        let (key, value) = item.map_err(storage)?;
        let (key_ns, from, label, edge) = key.value();
        if key_ns != ns {
            break;
        }
        if !spec.edge_labels.is_empty() && !spec.edge_labels.iter().any(|l| l == label) {
            continue;
        }
        let (to, recorded_at, expired_at, valid_from, valid_to) = value.value();
        let entry = Adjacent {
            edge: EdgeId::from_u128(edge),
            other: NodeId::from_u128(to),
            recorded_at,
            expired_at,
            valid_from,
            valid_to,
        };
        if !filter.accepts_entry(&entry) {
            continue;
        }
        let weight = match &spec.weight {
            None => 1.0,
            Some(key) => match edges.get((ns, edge)).map_err(storage)? {
                Some(guard) => decode::<Edge>(guard.value())?
                    .props
                    .get(key)
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(1.0),
                None => 1.0,
            },
        };
        links.push((NodeId::from_u128(from), entry.other, entry.edge, weight));
    }
    Ok(Projection::from_edges(ids, links, spec.weight.is_some()))
}

// ---------------------------------------------------------------- cache

/// Projections kept between read transactions.
const CACHED: usize = 4;

/// What every transaction of a database shares: a commit counter and the
/// projection cache.
#[derive(Default)]
pub(crate) struct Shared {
    /// Odd while a commit is under way; it grows by two with each commit.
    generation: AtomicU64,
    projections: Mutex<Vec<Cached>>,
}

struct Cached {
    generation: u64,
    ns: String,
    spec: ProjectionSpec,
    projection: Arc<Projection>,
}

impl Shared {
    /// Runs a commit, so that readers know the data changed.
    pub(crate) fn committing<T>(&self, commit: impl FnOnce() -> T) -> T {
        self.generation.fetch_add(1, Ordering::SeqCst);
        let result = commit();
        self.generation.fetch_add(1, Ordering::SeqCst);
        result
    }

    /// Opens a snapshot with `begin`, and tells which generation it sees,
    /// if no commit ran meanwhile.
    pub(crate) fn snapshot<T>(&self, begin: impl FnOnce() -> T) -> (T, Option<u64>) {
        let before = self.generation.load(Ordering::SeqCst);
        let snapshot = begin();
        let after = self.generation.load(Ordering::SeqCst);
        let generation = (before == after && before.is_multiple_of(2)).then_some(before);
        (snapshot, generation)
    }

    fn get(&self, generation: u64, ns: &str, spec: &ProjectionSpec) -> Option<Arc<Projection>> {
        let cached = self.projections.lock().unwrap_or_else(|e| e.into_inner());
        cached
            .iter()
            .find(|c| c.generation == generation && c.ns == ns && &c.spec == spec)
            .map(|c| c.projection.clone())
    }

    fn put(&self, generation: u64, ns: &str, spec: &ProjectionSpec, projection: Arc<Projection>) {
        let mut cached = self.projections.lock().unwrap_or_else(|e| e.into_inner());
        // Older generations will not be asked for again by new readers.
        cached.retain(|c| c.generation >= generation);
        if cached.len() == CACHED {
            cached.remove(0);
        }
        cached.push(Cached {
            generation,
            ns: ns.to_owned(),
            spec: spec.clone(),
            projection,
        });
    }
}

impl ReadTx {
    /// The projection of `ns` that `spec` describes, from the cache when
    /// this snapshot's data has been projected before.
    pub fn projection(&self, ns: &str, spec: &ProjectionSpec) -> Result<Arc<Projection>> {
        if let Some(generation) = self.generation
            && let Some(hit) = self.shared.get(generation, ns, spec)
        {
            return Ok(hit);
        }
        let projection = Arc::new(project(
            &self.nodes,
            &self.labels,
            &self.out,
            &self.edges,
            ns,
            spec,
        )?);
        if let Some(generation) = self.generation {
            self.shared.put(generation, ns, spec, projection.clone());
        }
        Ok(projection)
    }
}

impl WriteTx {
    /// [`ReadTx::projection`] of the data as written so far in this
    /// transaction (never cached).
    pub fn projection(&self, ns: &str, spec: &ProjectionSpec) -> Result<Arc<Projection>> {
        use crate::store::{ADJ_OUT, EDGES, NODE_LABELS, NODES};
        let nodes = self.tx.open_table(NODES).map_err(storage)?;
        let labels = self.tx.open_table(NODE_LABELS).map_err(storage)?;
        let out = self.tx.open_table(ADJ_OUT).map_err(storage)?;
        let edges = self.tx.open_table(EDGES).map_err(storage)?;
        Ok(Arc::new(project(&nodes, &labels, &out, &edges, ns, spec)?))
    }
}

/// Stops an algorithm when it returns true (a time limit or a cancelled
/// request). Checked between rounds and every few hundred nodes.
pub type Stop<'a> = &'a (dyn Fn() -> bool + Sync);

/// A [`Stop`] that never stops.
pub fn never() -> bool {
    false
}

pub(crate) fn stopped() -> Error {
    Error::Invalid("the algorithm was stopped".into())
}

pub(crate) fn check(stop: Stop, i: usize) -> Result<()> {
    if i.is_multiple_of(256) && stop() {
        return Err(stopped());
    }
    Ok(())
}

/// A small fast random generator (SplitMix64) so results repeat for a seed.
pub(crate) struct Random(u64);

impl Random {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number in `0..n`.
    pub(crate) fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A projection from edges between positions (ids `1..=n`).
    pub(crate) fn graph(n: usize, edges: &[(u32, u32)]) -> Projection {
        weighted(
            n,
            &edges.iter().map(|&(a, b)| (a, b, 1.0)).collect::<Vec<_>>(),
            false,
        )
    }

    pub(crate) fn weighted(n: usize, edges: &[(u32, u32, f64)], weighted: bool) -> Projection {
        let ids: Vec<NodeId> = (1..=n as u128).map(NodeId::from_u128).collect();
        Projection::from_edges(
            ids,
            edges.iter().enumerate().map(|(at, &(a, b, w))| {
                (
                    NodeId::from_u128(a as u128 + 1),
                    NodeId::from_u128(b as u128 + 1),
                    EdgeId::from_u128(at as u128 + 1),
                    w,
                )
            }),
            weighted,
        )
    }

    #[test]
    fn neighbors_in_each_direction() {
        let p = graph(3, &[(0, 1), (1, 2), (2, 2)]);
        let out: Vec<u32> = p.neighbors(1, Direction::Out).map(|s| s.node).collect();
        let inn: Vec<u32> = p.neighbors(1, Direction::In).map(|s| s.node).collect();
        let both: Vec<u32> = p.neighbors(2, Direction::Both).map(|s| s.node).collect();
        assert_eq!(out, vec![2]);
        assert_eq!(inn, vec![0]);
        assert_eq!(both, vec![2, 1], "the self-loop once");
        assert_eq!(p.edge_count(), 3);
        assert_eq!(p.degree(2, Direction::Both), 2);
    }
}
