//! Communities and clustering: connected components, label propagation,
//! Louvain, modularity and triangles.

use std::collections::HashMap;

use grafeno_core::Result;

use super::{Projection, Random, Stop, check, stopped};
use crate::traverse::Direction;

/// Numbers groups `0, 1, 2, …` in the order their first node appears, so
/// results do not depend on internal representatives.
fn renumber(groups: &[u32]) -> Vec<u32> {
    let mut seen = HashMap::new();
    groups
        .iter()
        .map(|group| {
            let next = seen.len() as u32;
            *seen.entry(*group).or_insert(next)
        })
        .collect()
}

/// Weakly connected components: nodes joined by edges in either direction
/// share a component.
pub fn weakly_connected(p: &Projection) -> Vec<u32> {
    let mut parent: Vec<u32> = (0..p.len() as u32).collect();
    fn root(parent: &mut [u32], mut node: u32) -> u32 {
        while parent[node as usize] != node {
            let up = parent[parent[node as usize] as usize];
            parent[node as usize] = up;
            node = up;
        }
        node
    }
    for node in 0..p.len() as u32 {
        for step in p.neighbors(node, Direction::Out) {
            let (a, b) = (root(&mut parent, node), root(&mut parent, step.node));
            if a != b {
                parent[a.max(b) as usize] = a.min(b);
            }
        }
    }
    let roots: Vec<u32> = (0..p.len() as u32).map(|n| root(&mut parent, n)).collect();
    renumber(&roots)
}

/// Strongly connected components: nodes that reach each other following
/// edge directions (Tarjan's algorithm, without recursion).
pub fn strongly_connected(p: &Projection) -> Vec<u32> {
    let n = p.len();
    const UNSEEN: u32 = u32::MAX;
    let mut index = vec![UNSEEN; n];
    let mut low = vec![0u32; n];
    let mut on_stack = vec![false; n];
    let mut stack = Vec::new();
    let mut component = vec![UNSEEN; n];
    let mut next_index = 0u32;
    let mut next_component = 0u32;
    // Each frame: the node and how many of its out-edges were visited.
    let mut frames: Vec<(u32, usize)> = Vec::new();
    for start in 0..n as u32 {
        if index[start as usize] != UNSEEN {
            continue;
        }
        frames.push((start, 0));
        while let Some(frame) = frames.last_mut() {
            let (node, visited) = *frame;
            frame.1 += 1;
            let v = node as usize;
            if visited == 0 {
                index[v] = next_index;
                low[v] = next_index;
                next_index += 1;
                stack.push(node);
                on_stack[v] = true;
            }
            match p.targets(node).get(visited).copied() {
                Some(target) => {
                    let w = target as usize;
                    if index[w] == UNSEEN {
                        frames.push((target, 0));
                    } else if on_stack[w] {
                        low[v] = low[v].min(index[w]);
                    }
                }
                None => {
                    frames.pop();
                    if let Some(&(parent, _)) = frames.last() {
                        low[parent as usize] = low[parent as usize].min(low[v]);
                    }
                    if low[v] == index[v] {
                        while let Some(member) = stack.pop() {
                            on_stack[member as usize] = false;
                            component[member as usize] = next_component;
                            if member == node {
                                break;
                            }
                        }
                        next_component += 1;
                    }
                }
            }
        }
    }
    renumber(&component)
}

/// Label propagation: every node starts in its own community and, in
/// turn (in an order shuffled each round), joins the one that weighs most
/// among its neighbors, until nothing changes. Ties keep the current
/// community, or pick one at random with `seed`. Edges count in both
/// directions. Returns the communities and the rounds run.
pub fn label_propagation(
    p: &Projection,
    max_iterations: usize,
    seed: u64,
    stop: Stop,
) -> Result<(Vec<u32>, usize)> {
    let mut labels: Vec<u32> = (0..p.len() as u32).collect();
    let mut order: Vec<u32> = labels.clone();
    let mut random = Random::new(seed);
    let mut weights: HashMap<u32, f64> = HashMap::new();
    let mut best_labels = Vec::new();
    let mut rounds = 0;
    while rounds < max_iterations {
        if stop() {
            return Err(stopped());
        }
        rounds += 1;
        for i in (1..order.len()).rev() {
            order.swap(i, random.below(i + 1));
        }
        let mut changed = false;
        for &node in &order {
            weights.clear();
            for step in p.neighbors(node, Direction::Both) {
                if step.node != node {
                    *weights.entry(labels[step.node as usize]).or_default() += step.weight;
                }
            }
            let current = labels[node as usize];
            let Some(best) = weights.values().copied().reduce(f64::max) else {
                continue;
            };
            if weights.get(&current).is_some_and(|w| *w >= best) {
                continue;
            }
            best_labels.clear();
            best_labels.extend(
                weights
                    .iter()
                    .filter(|(_, w)| **w >= best)
                    .map(|(label, _)| *label),
            );
            best_labels.sort_unstable();
            labels[node as usize] = best_labels[random.below(best_labels.len())];
            changed = true;
        }
        if !changed {
            break;
        }
    }
    Ok((renumber(&labels), rounds))
}

/// An undirected weighted graph, for Louvain: `adjacency[i]` holds
/// `(j, A_ij)`, with a self-loop entry holding `A_ii` (twice the loop's
/// weight, so that every row sums to the node's degree).
struct Undirected {
    adjacency: Vec<Vec<(u32, f64)>>,
}

impl Undirected {
    fn from_projection(p: &Projection) -> Self {
        let mut adjacency = Vec::with_capacity(p.len());
        let mut row: HashMap<u32, f64> = HashMap::new();
        for node in 0..p.len() as u32 {
            row.clear();
            for step in p.neighbors(node, Direction::Both) {
                let weight = step.weight.max(0.0);
                let weight = if step.node == node {
                    2.0 * weight
                } else {
                    weight
                };
                *row.entry(step.node).or_default() += weight;
            }
            let mut entries: Vec<(u32, f64)> = row.iter().map(|(n, w)| (*n, *w)).collect();
            entries.sort_unstable_by_key(|(n, _)| *n);
            adjacency.push(entries);
        }
        Self { adjacency }
    }

    fn degrees(&self) -> Vec<f64> {
        self.adjacency
            .iter()
            .map(|row| row.iter().map(|(_, w)| w).sum())
            .collect()
    }
}

/// Louvain settings.
#[derive(Clone, Debug)]
pub struct Louvain {
    /// Above 1 favors smaller communities, below 1 larger ones.
    pub resolution: f64,
    /// Rounds of aggregation.
    pub max_levels: usize,
    /// Passes over the nodes in each level.
    pub max_iterations: usize,
    /// A level ends when a pass improves modularity less than this.
    pub tolerance: f64,
}

impl Default for Louvain {
    fn default() -> Self {
        Self {
            resolution: 1.0,
            max_levels: 10,
            max_iterations: 10,
            tolerance: 1e-7,
        }
    }
}

/// Louvain communities: moves nodes to the neighbor community that raises
/// modularity most, merges each community into one node, and repeats.
/// Edges count in both directions. Returns the communities, their
/// modularity and the levels run.
pub fn louvain(p: &Projection, settings: &Louvain, stop: Stop) -> Result<(Vec<u32>, f64, usize)> {
    let (hierarchy, q, levels) = louvain_levels(p, settings, stop)?;
    let last = hierarchy
        .into_iter()
        .last()
        .unwrap_or_else(|| (0..p.len() as u32).collect());
    Ok((last, q, levels))
}

/// Louvain kept as a hierarchy: the communities after each level of
/// aggregation, finest first, each a community per node. A community of a
/// level is a union of communities of the level before (communities of
/// communities). Levels that merge nothing are left out, so the list is
/// empty when no node moved. Returns them with the modularity of the last
/// and the levels run.
pub fn louvain_levels(
    p: &Projection,
    settings: &Louvain,
    stop: Stop,
) -> Result<(Vec<Vec<u32>>, f64, usize)> {
    let original = Undirected::from_projection(p);
    let total: f64 = original.degrees().iter().sum();
    let mut membership: Vec<u32> = (0..p.len() as u32).collect();
    let mut hierarchy: Vec<Vec<u32>> = Vec::new();
    if total <= 0.0 {
        return Ok((hierarchy, 0.0, 0));
    }
    let mut graph = Undirected {
        adjacency: original.adjacency.clone(),
    };
    let mut levels = 0;
    while levels < settings.max_levels {
        if stop() {
            return Err(stopped());
        }
        let n = graph.adjacency.len();
        let degree = graph.degrees();
        let mut community: Vec<u32> = (0..n as u32).collect();
        let mut sum_total = degree.clone();
        let mut moved_any = false;
        let mut to_community: HashMap<u32, f64> = HashMap::new();
        for _ in 0..settings.max_iterations {
            let mut gain_total = 0.0;
            let mut moved = false;
            for node in 0..n {
                check(stop, node)?;
                let k = degree[node];
                let current = community[node];
                to_community.clear();
                for &(other, weight) in &graph.adjacency[node] {
                    if other as usize != node {
                        *to_community.entry(community[other as usize]).or_default() += weight;
                    }
                }
                sum_total[current as usize] -= k;
                let gain = |c: u32, links: f64| {
                    links - settings.resolution * sum_total[c as usize] * k / total
                };
                let stay = gain(current, to_community.get(&current).copied().unwrap_or(0.0));
                let mut best = (current, stay);
                let mut candidates: Vec<(u32, f64)> =
                    to_community.iter().map(|(c, w)| (*c, *w)).collect();
                candidates.sort_unstable_by_key(|(c, _)| *c);
                for (c, links) in candidates {
                    let g = gain(c, links);
                    if g > best.1 + 1e-12 {
                        best = (c, g);
                    }
                }
                sum_total[best.0 as usize] += k;
                if best.0 != current {
                    community[node] = best.0;
                    gain_total += best.1 - stay;
                    moved = true;
                    moved_any = true;
                }
            }
            if !moved || gain_total / total < settings.tolerance {
                break;
            }
        }
        levels += 1;
        if !moved_any {
            break;
        }
        // Merges each community into one node.
        let renumbered = renumber(&community);
        for member in &mut membership {
            *member = renumbered[*member as usize];
        }
        hierarchy.push(renumber(&membership));
        let count = renumbered
            .iter()
            .copied()
            .max()
            .map_or(0, |m| m as usize + 1);
        let mut rows: Vec<HashMap<u32, f64>> = vec![HashMap::new(); count];
        for (node, row) in graph.adjacency.iter().enumerate() {
            let a = renumbered[node];
            for &(other, weight) in row {
                *rows[a as usize]
                    .entry(renumbered[other as usize])
                    .or_default() += weight;
            }
        }
        graph = Undirected {
            adjacency: rows
                .into_iter()
                .map(|row| {
                    let mut entries: Vec<(u32, f64)> = row.into_iter().collect();
                    entries.sort_unstable_by_key(|(n, _)| *n);
                    entries
                })
                .collect(),
        };
        if count == n {
            break;
        }
    }
    let q = modularity_of(&original, &renumber(&membership), settings.resolution);
    Ok((hierarchy, q, levels))
}

fn modularity_of(graph: &Undirected, communities: &[u32], resolution: f64) -> f64 {
    let degree = graph.degrees();
    let total: f64 = degree.iter().sum();
    if total <= 0.0 {
        return 0.0;
    }
    let count = communities
        .iter()
        .copied()
        .max()
        .map_or(0, |m| m as usize + 1);
    let mut inside = vec![0.0; count];
    let mut sum = vec![0.0; count];
    for (node, row) in graph.adjacency.iter().enumerate() {
        let c = communities[node] as usize;
        sum[c] += degree[node];
        for &(other, weight) in row {
            if communities[other as usize] as usize == c {
                inside[c] += weight;
            }
        }
    }
    inside
        .iter()
        .zip(&sum)
        .map(|(inside, sum)| inside / total - resolution * (sum / total).powi(2))
        .sum()
}

/// The modularity of a division into communities (one per node): how much
/// more weight falls inside communities than chance would put there, from
/// -0.5 to 1. Edges count in both directions.
pub fn modularity(p: &Projection, communities: &[u32]) -> f64 {
    modularity_of(&Undirected::from_projection(p), &renumber(communities), 1.0)
}

/// Triangles each node is part of, and its local clustering coefficient
/// (the share of pairs of its neighbors that are neighbors themselves).
/// Edges count once whatever their direction or number.
#[derive(Clone, Debug, PartialEq)]
pub struct Triangles {
    pub per_node: Vec<u64>,
    pub coefficient: Vec<f64>,
    pub total: u64,
}

pub fn triangles(p: &Projection, stop: Stop) -> Result<Triangles> {
    let sets = p.neighbor_sets(Direction::Both);
    let mut per_node = vec![0u64; p.len()];
    let mut total = 0;
    for (u, neighbors) in sets.iter().enumerate() {
        check(stop, u)?;
        for &v in neighbors.iter().filter(|&&v| v as usize > u) {
            // Common neighbors w > v of u and v, by merging sorted lists.
            let other = &sets[v as usize];
            let (mut i, mut j) = (0, 0);
            while i < neighbors.len() && j < other.len() {
                match neighbors[i].cmp(&other[j]) {
                    std::cmp::Ordering::Less => i += 1,
                    std::cmp::Ordering::Greater => j += 1,
                    std::cmp::Ordering::Equal => {
                        let w = neighbors[i];
                        if w > v {
                            per_node[u] += 1;
                            per_node[v as usize] += 1;
                            per_node[w as usize] += 1;
                            total += 1;
                        }
                        i += 1;
                        j += 1;
                    }
                }
            }
        }
    }
    let coefficient = sets
        .iter()
        .zip(&per_node)
        .map(|(set, &t)| {
            let d = set.len() as f64;
            if d < 2.0 {
                0.0
            } else {
                2.0 * t as f64 / (d * (d - 1.0))
            }
        })
        .collect();
    Ok(Triangles {
        per_node,
        coefficient,
        total,
    })
}

#[cfg(test)]
mod tests {
    use super::super::never;
    use super::super::tests::graph;
    use super::*;

    /// Two triangles joined by one edge: {0, 1, 2} and {3, 4, 5}.
    fn barbell() -> Projection {
        graph(6, &[(0, 1), (1, 2), (2, 0), (3, 4), (4, 5), (5, 3), (2, 3)])
    }

    #[test]
    fn components() {
        let p = graph(5, &[(0, 1), (2, 1), (3, 4), (4, 3)]);
        assert_eq!(weakly_connected(&p), vec![0, 0, 0, 1, 1]);
        assert_eq!(strongly_connected(&p), vec![0, 1, 2, 3, 3]);
        let cycle = graph(4, &[(0, 1), (1, 2), (2, 0), (2, 3)]);
        let scc = strongly_connected(&cycle);
        assert_eq!(scc[0], scc[1]);
        assert_eq!(scc[1], scc[2]);
        assert_ne!(scc[2], scc[3]);
    }

    /// Two 5-cliques {0..5} and {5..10} joined by one edge.
    fn two_cliques() -> Projection {
        let mut edges = Vec::new();
        for base in [0u32, 5] {
            for a in 0..5 {
                for b in a + 1..5 {
                    edges.push((base + a, base + b));
                }
            }
        }
        edges.push((4, 5));
        graph(10, &edges)
    }

    #[test]
    fn label_propagation_splits_two_cliques() {
        for seed in 0..20 {
            let (labels, rounds) = label_propagation(&two_cliques(), 20, seed, &never).unwrap();
            assert!(rounds < 20, "converges");
            assert!(
                labels[..5].iter().all(|l| *l == labels[0]),
                "seed {seed}: {labels:?}"
            );
            assert!(
                labels[5..].iter().all(|l| *l == labels[5]),
                "seed {seed}: {labels:?}"
            );
        }
    }

    #[test]
    fn louvain_splits_the_barbell() {
        let p = barbell();
        let (communities, q, _) = louvain(&p, &Louvain::default(), &never).unwrap();
        assert_eq!(communities, vec![0, 0, 0, 1, 1, 1]);
        // 7 edges: each triangle has 3 inside and degree sum 7.
        let expected = 2.0 * (6.0 / 14.0 - (7.0f64 / 14.0).powi(2));
        assert!((q - expected).abs() < 1e-9, "{q} vs {expected}");
        assert!((modularity(&p, &communities) - expected).abs() < 1e-9);
        assert!(modularity(&p, &[0, 0, 0, 0, 0, 0]).abs() < 1e-9);
    }

    #[test]
    fn louvain_finds_ring_of_cliques() {
        // Eight 5-cliques in a ring.
        let mut edges = Vec::new();
        for clique in 0..8u32 {
            let base = clique * 5;
            for a in 0..5 {
                for b in a + 1..5 {
                    edges.push((base + a, base + b));
                }
            }
            edges.push((base + 4, (base + 5) % 40));
        }
        let p = graph(40, &edges);
        let (communities, q, _) = louvain(&p, &Louvain::default(), &never).unwrap();
        for clique in 0..8 {
            let first = communities[clique * 5];
            assert!(
                communities[clique * 5..clique * 5 + 5]
                    .iter()
                    .all(|c| *c == first)
            );
        }
        assert_eq!(communities.iter().max(), Some(&7));
        // 88 edges; each clique has 10 inside and degree sum 22.
        assert!((q - 8.0 * (10.0 / 88.0 - (22.0f64 / 176.0).powi(2))).abs() < 1e-9);
    }

    #[test]
    fn triangles_and_clustering() {
        let t = triangles(&barbell(), &never).unwrap();
        assert_eq!(t.total, 2);
        assert_eq!(t.per_node, vec![1, 1, 1, 1, 1, 1]);
        assert_eq!(t.coefficient[0], 1.0);
        // Node 2 has neighbors 0, 1, 3: one of three pairs is joined.
        assert!((t.coefficient[2] - 1.0 / 3.0).abs() < 1e-12);
        // A 4-clique has 4 triangles.
        let k4 = graph(4, &[(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3), (1, 0)]);
        assert_eq!(triangles(&k4, &never).unwrap().total, 4);
    }
}
