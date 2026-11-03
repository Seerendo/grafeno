//! Vector index: HNSW (Malkov & Yashunin, 2016) stored in redb tables.
//!
//! Vectors are normalized on insert, so the distance is `1 - dot product`
//! (cosine). Each namespace has its own graph and a fixed dimension, set by
//! its first vector.
//!
//! A node's level is derived from its id (the random bits of a UUID v7), so it
//! never has to be stored and the entry point can always be recomputed.

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap, HashSet};

use grafeno_core::{Error, NodeId, Result};
use redb::{ReadableTable, WriteTransaction};
use serde::{Deserialize, Serialize};

use crate::Hit;
use crate::store::{HNSW_LINKS, LinkKey, RecordKey, VECTOR_META, VECTORS, decode, encode, storage};

/// Links per node on layers above 0.
const M: usize = 16;
/// Links per node on layer 0.
const M0: usize = 2 * M;
const EF_CONSTRUCTION: usize = 100;
const MAX_LEVEL: u8 = 16;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct VectorMeta {
    dimension: usize,
    count: u64,
    entry: Option<NodeId>,
}

impl VectorMeta {
    pub(crate) fn dimension(&self) -> usize {
        self.dimension
    }

    pub(crate) fn count(&self) -> u64 {
        self.count
    }
}

fn level_of(node: NodeId) -> u8 {
    let bits = (node.as_u128() as u64) & ((1 << 52) - 1);
    let uniform = (bits as f64 + 1.0) / (1u64 << 52) as f64;
    let level = (-uniform.ln() / (M as f64).ln()).floor();
    (level as u8).min(MAX_LEVEL)
}

fn max_links(layer: u8) -> usize {
    if layer == 0 { M0 } else { M }
}

fn normalize(vector: &[f32]) -> Result<Vec<f32>> {
    if vector.iter().any(|value| !value.is_finite()) {
        return Err(Error::Invalid("vector values must be finite".into()));
    }
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm == 0.0 {
        return Err(Error::Invalid("vector must not be zero".into()));
    }
    // Already normalized (such as an exported vector): keep its exact bits.
    if (norm - 1.0).abs() <= 1e-6 {
        return Ok(vector.to_vec());
    }
    Ok(vector.iter().map(|value| value / norm).collect())
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn vector_bytes(vector: &[f32]) -> Vec<u8> {
    vector
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn bytes_vector(bytes: &[u8]) -> Vec<f32> {
    let (chunks, _) = bytes.as_chunks::<4>();
    chunks
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect()
}

fn links_bytes(links: &[u128]) -> Vec<u8> {
    links.iter().flat_map(|id| id.to_le_bytes()).collect()
}

fn bytes_links(bytes: &[u8]) -> Vec<u128> {
    let (chunks, _) = bytes.as_chunks::<16>();
    chunks
        .iter()
        .map(|chunk| u128::from_le_bytes(*chunk))
        .collect()
}

pub(crate) fn read_meta(
    table: &impl ReadableTable<&'static str, &'static [u8]>,
    ns: &str,
) -> Result<Option<VectorMeta>> {
    match table.get(ns).map_err(storage)? {
        Some(guard) => decode(guard.value()).map(Some),
        None => Ok(None),
    }
}

pub(crate) fn embedding(
    vectors: &impl ReadableTable<RecordKey, &'static [u8]>,
    ns: &str,
    node: NodeId,
) -> Result<Option<Vec<f32>>> {
    Ok(vectors
        .get((ns, node.as_u128()))
        .map_err(storage)?
        .map(|guard| bytes_vector(guard.value())))
}

/// A candidate ordered by distance (smaller is closer).
#[derive(Clone, Copy, Debug)]
struct Scored(f32, u128);

impl PartialEq for Scored {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Scored {}

impl PartialOrd for Scored {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Scored {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0).then(self.1.cmp(&other.1))
    }
}

/// Read access to one namespace's HNSW graph, with a cache of vectors.
struct Space<'a, V, L> {
    ns: &'a str,
    vectors: &'a V,
    links: &'a L,
    cache: &'a mut HashMap<u128, Option<Vec<f32>>>,
}

impl<V, L> Space<'_, V, L>
where
    V: ReadableTable<RecordKey, &'static [u8]>,
    L: ReadableTable<LinkKey, &'static [u8]>,
{
    fn vector(&mut self, id: u128) -> Result<Option<&[f32]>> {
        if !self.cache.contains_key(&id) {
            let vector = self
                .vectors
                .get((self.ns, id))
                .map_err(storage)?
                .map(|guard| bytes_vector(guard.value()));
            self.cache.insert(id, vector);
        }
        Ok(self.cache[&id].as_deref())
    }

    /// `None` when the node has no vector: links may point to removed nodes.
    fn distance(&mut self, query: &[f32], id: u128) -> Result<Option<f32>> {
        Ok(self.vector(id)?.map(|vector| 1.0 - dot(query, vector)))
    }

    fn links(&self, id: u128, layer: u8) -> Result<Vec<u128>> {
        Ok(self
            .links
            .get((self.ns, id, layer))
            .map_err(storage)?
            .map(|guard| bytes_links(guard.value()))
            .unwrap_or_default())
    }

    /// The `ef` nodes closest to `query` on `layer`, closest first.
    fn search_layer(
        &mut self,
        query: &[f32],
        entries: &[Scored],
        ef: usize,
        layer: u8,
    ) -> Result<Vec<Scored>> {
        let mut visited: HashSet<u128> = entries.iter().map(|entry| entry.1).collect();
        let mut candidates: BinaryHeap<Reverse<Scored>> =
            entries.iter().copied().map(Reverse).collect();
        let mut results: BinaryHeap<Scored> = entries.iter().copied().collect();
        while let Some(Reverse(candidate)) = candidates.pop() {
            if results.len() >= ef && results.peek().is_some_and(|worst| candidate.0 > worst.0) {
                break;
            }
            for neighbor in self.links(candidate.1, layer)? {
                if !visited.insert(neighbor) {
                    continue;
                }
                let Some(distance) = self.distance(query, neighbor)? else {
                    continue;
                };
                if results.len() < ef || results.peek().is_some_and(|worst| distance < worst.0) {
                    candidates.push(Reverse(Scored(distance, neighbor)));
                    results.push(Scored(distance, neighbor));
                    if results.len() > ef {
                        results.pop();
                    }
                }
            }
        }
        Ok(results.into_sorted_vec())
    }

    /// Chooses up to `m` neighbors from `candidates` (closest first) with the
    /// paper's heuristic: skip a candidate that is closer to an already chosen
    /// neighbor than to the base, so links spread in different directions.
    /// Skipped candidates fill the remaining slots.
    fn select(&mut self, candidates: &[Scored], m: usize) -> Result<Vec<u128>> {
        let mut chosen: Vec<(u128, Vec<f32>)> = Vec::new();
        let mut skipped = Vec::new();
        for &Scored(distance, id) in candidates {
            if chosen.len() >= m {
                break;
            }
            let Some(vector) = self.vector(id)?.map(<[f32]>::to_vec) else {
                continue;
            };
            if chosen
                .iter()
                .all(|(_, other)| 1.0 - dot(&vector, other) > distance)
            {
                chosen.push((id, vector));
            } else {
                skipped.push(id);
            }
        }
        let mut out: Vec<u128> = chosen.into_iter().map(|(id, _)| id).collect();
        out.extend(skipped.into_iter().take(m.saturating_sub(out.len())));
        Ok(out)
    }

    /// Reduces `links` of `base` to `m`, keeping the best by the heuristic.
    fn prune(&mut self, base: u128, links: &[u128], m: usize) -> Result<Vec<u128>> {
        let Some(base_vector) = self.vector(base)?.map(<[f32]>::to_vec) else {
            return Ok(Vec::new());
        };
        let mut scored = Vec::new();
        for &id in links {
            if id == base {
                continue;
            }
            if let Some(distance) = self.distance(&base_vector, id)? {
                scored.push(Scored(distance, id));
            }
        }
        scored.sort();
        scored.dedup_by_key(|item| item.1);
        self.select(&scored, m)
    }

    /// Greedy descent from the entry point down to `layer + 1`.
    fn descend(&mut self, query: &[f32], entry: NodeId, layer: u8) -> Result<Vec<Scored>> {
        let entry = entry.as_u128();
        let Some(distance) = self.distance(query, entry)? else {
            return Ok(Vec::new());
        };
        let mut entries = vec![Scored(distance, entry)];
        for current in (layer + 1..=level_of(NodeId::from_u128(entry))).rev() {
            entries = self.search_layer(query, &entries, 1, current)?;
        }
        Ok(entries)
    }
}

/// Stores `vector` for `node` and links it into the graph, replacing any
/// previous vector.
pub(crate) fn insert(tx: &WriteTransaction, ns: &str, node: NodeId, vector: &[f32]) -> Result<()> {
    let query = normalize(vector)?;
    remove(tx, ns, node)?;

    let mut meta_table = tx.open_table(VECTOR_META).map_err(storage)?;
    let mut meta = read_meta(&meta_table, ns)?.unwrap_or(VectorMeta {
        dimension: query.len(),
        count: 0,
        entry: None,
    });
    if meta.count > 0 && meta.dimension != query.len() {
        return Err(Error::Invalid(format!(
            "namespace {ns:?} stores vectors of dimension {}, got {}",
            meta.dimension,
            query.len()
        )));
    }
    meta.dimension = query.len();

    let id = node.as_u128();
    let mut vectors = tx.open_table(VECTORS).map_err(storage)?;
    vectors
        .insert((ns, id), vector_bytes(&query).as_slice())
        .map_err(storage)?;
    let mut links = tx.open_table(HNSW_LINKS).map_err(storage)?;
    let level = level_of(node);

    if let Some(entry) = meta.entry {
        let entry_level = level_of(entry);
        let mut cache = HashMap::new();
        let mut space = Space {
            ns,
            vectors: &vectors,
            links: &links,
            cache: &mut cache,
        };
        let mut entries = space.descend(&query, entry, level.min(entry_level))?;

        for layer in (0..=level.min(entry_level)).rev() {
            // Read phase: compute every link list of this layer…
            let mut space = Space {
                ns,
                vectors: &vectors,
                links: &links,
                cache: &mut cache,
            };
            let found = space.search_layer(&query, &entries, EF_CONSTRUCTION, layer)?;
            let own = space.select(&found, M)?;
            let mut updates = vec![(id, own.clone())];
            for &neighbor in &own {
                let mut list = space.links(neighbor, layer)?;
                list.push(id);
                if list.len() > max_links(layer) {
                    list = space.prune(neighbor, &list, max_links(layer))?;
                }
                updates.push((neighbor, list));
            }
            entries = found;
            // …then write them.
            for (node, list) in updates {
                links
                    .insert((ns, node, layer), links_bytes(&list).as_slice())
                    .map_err(storage)?;
            }
        }
        if level > entry_level {
            meta.entry = Some(node);
        }
    } else {
        meta.entry = Some(node);
    }

    meta.count += 1;
    meta_table
        .insert(ns, encode(&meta)?.as_slice())
        .map_err(storage)?;
    Ok(())
}

/// Removes the vector of `node` and reconnects its neighbors. Returns whether
/// there was one.
pub(crate) fn remove(tx: &WriteTransaction, ns: &str, node: NodeId) -> Result<bool> {
    let id = node.as_u128();
    let mut vectors = tx.open_table(VECTORS).map_err(storage)?;
    if vectors.remove((ns, id)).map_err(storage)?.is_none() {
        return Ok(false);
    }
    let mut links = tx.open_table(HNSW_LINKS).map_err(storage)?;
    let mut cache = HashMap::new();
    for layer in 0..=level_of(node) {
        let own = links
            .remove((ns, id, layer))
            .map_err(storage)?
            .map(|guard| bytes_links(guard.value()))
            .unwrap_or_default();
        let mut space = Space {
            ns,
            vectors: &vectors,
            links: &links,
            cache: &mut cache,
        };
        let mut updates = Vec::new();
        for &neighbor in &own {
            // The neighbor inherits the removed node's links as candidates.
            let mut candidates: Vec<u128> = space
                .links(neighbor, layer)?
                .into_iter()
                .filter(|other| *other != id)
                .collect();
            candidates.extend(own.iter().copied().filter(|other| *other != neighbor));
            updates.push((
                neighbor,
                space.prune(neighbor, &candidates, max_links(layer))?,
            ));
        }
        for (neighbor, list) in updates {
            links
                .insert((ns, neighbor, layer), links_bytes(&list).as_slice())
                .map_err(storage)?;
        }
    }

    let mut meta_table = tx.open_table(VECTOR_META).map_err(storage)?;
    let mut meta = read_meta(&meta_table, ns)?
        .ok_or_else(|| Error::Storage(format!("vector metadata missing for {ns:?}")))?;
    meta.count = meta.count.saturating_sub(1);
    if meta.entry == Some(node) {
        meta.entry = highest_node(&vectors, ns)?;
    }
    meta_table
        .insert(ns, encode(&meta)?.as_slice())
        .map_err(storage)?;
    Ok(true)
}

/// The node with the highest level in the namespace (the new entry point).
fn highest_node(
    vectors: &impl ReadableTable<RecordKey, &'static [u8]>,
    ns: &str,
) -> Result<Option<NodeId>> {
    let mut best: Option<(u8, NodeId)> = None;
    for item in vectors.range((ns, 0u128)..).map_err(storage)? {
        let (key, _) = item.map_err(storage)?;
        let (key_ns, id) = key.value();
        if key_ns != ns {
            break;
        }
        let node = NodeId::from_u128(id);
        let level = level_of(node);
        if best.is_none_or(|(top, _)| level > top) {
            best = Some((level, node));
        }
    }
    Ok(best.map(|(_, node)| node))
}

/// The `limit` nodes most similar to `query`, best first. `ef` is the size of
/// the candidate list on layer 0: higher is slower and more accurate.
pub(crate) fn search(
    vectors: &impl ReadableTable<RecordKey, &'static [u8]>,
    links: &impl ReadableTable<LinkKey, &'static [u8]>,
    meta: &impl ReadableTable<&'static str, &'static [u8]>,
    ns: &str,
    query: &[f32],
    limit: usize,
    ef: usize,
) -> Result<Vec<Hit>> {
    let Some(meta) = read_meta(meta, ns)? else {
        return Ok(Vec::new());
    };
    let Some(entry) = meta.entry else {
        return Ok(Vec::new());
    };
    if query.len() != meta.dimension {
        return Err(Error::Invalid(format!(
            "namespace {ns:?} stores vectors of dimension {}, got {}",
            meta.dimension,
            query.len()
        )));
    }
    let query = normalize(query)?;
    let mut cache = HashMap::new();
    let mut space = Space {
        ns,
        vectors,
        links,
        cache: &mut cache,
    };
    let entries = space.descend(&query, entry, 0)?;
    let found = space.search_layer(&query, &entries, ef.max(limit), 0)?;
    Ok(found
        .into_iter()
        .take(limit)
        .map(|Scored(distance, id)| Hit {
            node: NodeId::from_u128(id),
            score: 1.0 - distance,
        })
        .collect())
}

/// Checks the vector index of a namespace: vectors, metadata, links, and that
/// every vector is reachable from the entry point on layer 0 (otherwise
/// searches could never return it). Returns the number of vectors and of
/// dangling links.
pub(crate) fn check(
    vectors: &impl ReadableTable<RecordKey, &'static [u8]>,
    links: &impl ReadableTable<LinkKey, &'static [u8]>,
    meta: &impl ReadableTable<&'static str, &'static [u8]>,
    ns: &str,
    nodes: &HashSet<u128>,
    problems: &mut Vec<String>,
) -> Result<(u64, u64)> {
    let meta = read_meta(meta, ns)?;
    let mut ids = HashSet::new();
    for item in vectors.range((ns, 0u128)..).map_err(storage)? {
        let (key, value) = item.map_err(storage)?;
        let (key_ns, id) = key.value();
        if key_ns != ns {
            break;
        }
        ids.insert(id);
        let node = NodeId::from_u128(id);
        if !nodes.contains(&id) {
            problems.push(format!("vector of missing node {node}"));
        }
        let vector = bytes_vector(value.value());
        if let Some(meta) = &meta
            && vector.len() != meta.dimension
        {
            problems.push(format!(
                "vector of node {node} has dimension {}, expected {}",
                vector.len(),
                meta.dimension
            ));
        }
        let norm = dot(&vector, &vector).sqrt();
        if (norm - 1.0).abs() > 1e-3 {
            problems.push(format!(
                "vector of node {node} is not normalized (norm {norm})"
            ));
        }
    }
    let count = ids.len() as u64;
    match &meta {
        None if count > 0 => problems.push(format!("{count} vectors but no vector metadata")),
        None => {}
        Some(meta) => {
            if meta.count != count {
                problems.push(format!(
                    "vector metadata counts {}, found {count}",
                    meta.count
                ));
            }
            match meta.entry {
                None if count > 0 => {
                    problems.push("vectors exist but there is no entry point".into())
                }
                Some(entry) if !ids.contains(&entry.as_u128()) => {
                    problems.push(format!("entry point {entry} has no vector"))
                }
                _ => {}
            }
        }
    }

    let mut dangling = 0u64;
    for item in links.range((ns, 0u128, 0u8)..).map_err(storage)? {
        let (key, value) = item.map_err(storage)?;
        let (key_ns, id, layer) = key.value();
        if key_ns != ns {
            break;
        }
        let node = NodeId::from_u128(id);
        if !ids.contains(&id) {
            problems.push(format!(
                "links of node {node} without a vector, layer {layer}"
            ));
        }
        if layer > level_of(node) {
            problems.push(format!(
                "links of node {node} on layer {layer}, above its level"
            ));
        }
        dangling += bytes_links(value.value())
            .iter()
            .filter(|neighbor| !ids.contains(neighbor))
            .count() as u64;
    }

    if let Some(entry) = meta.as_ref().and_then(|meta| meta.entry)
        && ids.contains(&entry.as_u128())
    {
        let mut seen = HashSet::from([entry.as_u128()]);
        let mut stack = vec![entry.as_u128()];
        while let Some(id) = stack.pop() {
            let neighbors = links
                .get((ns, id, 0u8))
                .map_err(storage)?
                .map(|guard| bytes_links(guard.value()))
                .unwrap_or_default();
            for neighbor in neighbors {
                if ids.contains(&neighbor) && seen.insert(neighbor) {
                    stack.push(neighbor);
                }
            }
        }
        if (seen.len() as u64) < count {
            problems.push(format!(
                "{} of {count} vectors cannot be reached from the entry point",
                count - seen.len() as u64
            ));
        }
    }
    Ok((count, dangling))
}
