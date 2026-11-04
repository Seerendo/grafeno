//! Similarity by shared neighbors, and link prediction.

use std::collections::HashMap;

use grafeno_core::{Error, Result};

use super::{Projection, Stop, check};
use crate::traverse::Direction;

/// How alike two neighbor sets are, from 0 to 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimilarityMetric {
    /// Shared / all.
    Jaccard,
    /// Shared / the smaller set.
    Overlap,
    /// Shared / the geometric mean of the sizes.
    Cosine,
}

impl SimilarityMetric {
    pub fn parse(name: &str) -> Result<Self> {
        Ok(match name.to_ascii_lowercase().as_str() {
            "jaccard" => Self::Jaccard,
            "overlap" => Self::Overlap,
            "cosine" => Self::Cosine,
            other => {
                return Err(Error::Invalid(format!(
                    "unknown similarity metric {other:?} (jaccard, overlap, cosine)"
                )));
            }
        })
    }

    fn score(self, shared: usize, a: usize, b: usize) -> f64 {
        if shared == 0 {
            return 0.0;
        }
        let shared = shared as f64;
        match self {
            Self::Jaccard => shared / ((a + b) as f64 - shared),
            Self::Overlap => shared / a.min(b) as f64,
            Self::Cosine => shared / ((a * b) as f64).sqrt(),
        }
        .min(1.0)
    }
}

/// Elements in both sorted, duplicate-free lists.
fn shared<'a, T: Ord>(a: &'a [T], b: &'a [T]) -> impl Iterator<Item = &'a T> {
    let (mut i, mut j) = (0, 0);
    std::iter::from_fn(move || {
        while i < a.len() && j < b.len() {
            match a[i].cmp(&b[j]) {
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
                std::cmp::Ordering::Equal => {
                    i += 1;
                    j += 1;
                    return Some(&a[i - 1]);
                }
            }
        }
        None
    })
}

/// The similarity of two sorted, duplicate-free neighbor sets.
pub fn neighbor_similarity<T: Ord>(a: &[T], b: &[T], metric: SimilarityMetric) -> f64 {
    metric.score(shared(a, b).count(), a.len(), b.len())
}

/// For each node (or only `source`), the `top_k` nodes whose neighbors in
/// `direction` are most like its own, with a score of at least `cutoff`,
/// best first, as `(node, other, score)`. Only nodes with a neighbor in
/// common are compared, so the cost follows the two-hop neighborhoods.
pub fn similar_neighbors(
    p: &Projection,
    metric: SimilarityMetric,
    direction: Direction,
    top_k: usize,
    cutoff: f64,
    source: Option<u32>,
    stop: Stop,
) -> Result<Vec<(u32, u32, f64)>> {
    let sets = p.neighbor_sets(direction);
    let reverse = match direction {
        Direction::Out => p.neighbor_sets(Direction::In),
        Direction::In => p.neighbor_sets(Direction::Out),
        Direction::Both => Vec::new(),
    };
    let back = if direction == Direction::Both {
        &sets
    } else {
        &reverse
    };
    let mut out = Vec::new();
    let mut counts: HashMap<u32, usize> = HashMap::new();
    let nodes: Box<dyn Iterator<Item = u32>> = match source {
        Some(node) => Box::new(std::iter::once(node)),
        None => Box::new(0..p.len() as u32),
    };
    for (i, node) in nodes.enumerate() {
        check(stop, i)?;
        counts.clear();
        for &middle in &sets[node as usize] {
            for &other in &back[middle as usize] {
                if other != node {
                    *counts.entry(other).or_default() += 1;
                }
            }
        }
        let a = sets[node as usize].len();
        let mut scored: Vec<(u32, f64)> = counts
            .iter()
            .map(|(&other, &shared)| (other, metric.score(shared, a, sets[other as usize].len())))
            .filter(|(_, score)| *score >= cutoff && *score > 0.0)
            .collect();
        scored.sort_unstable_by(|x, y| y.1.total_cmp(&x.1).then(x.0.cmp(&y.0)));
        scored.truncate(top_k);
        out.extend(
            scored
                .into_iter()
                .map(|(other, score)| (node, other, score)),
        );
    }
    Ok(out)
}

/// How likely an edge between two nodes is, from their neighbors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkMetric {
    /// Neighbors in common.
    CommonNeighbors,
    /// Common neighbors, each worth `1 / ln(its degree)`: rare
    /// connections say more.
    AdamicAdar,
    /// Common neighbors, each worth `1 / its degree`.
    ResourceAllocation,
    /// The product of the two degrees: well connected nodes attract.
    PreferentialAttachment,
    /// Neighbors of either.
    TotalNeighbors,
    /// Shared / all neighbors.
    Jaccard,
}

impl LinkMetric {
    pub fn parse(name: &str) -> Result<Self> {
        Ok(
            match name.to_ascii_lowercase().replace(['_', '-'], "").as_str() {
                "commonneighbors" => Self::CommonNeighbors,
                "adamicadar" => Self::AdamicAdar,
                "resourceallocation" => Self::ResourceAllocation,
                "preferentialattachment" => Self::PreferentialAttachment,
                "totalneighbors" => Self::TotalNeighbors,
                "jaccard" => Self::Jaccard,
                other => {
                    return Err(Error::Invalid(format!(
                        "unknown link metric {other:?} (commonNeighbors, adamicAdar, resourceAllocation, preferentialAttachment, totalNeighbors, jaccard)"
                    )));
                }
            },
        )
    }
}

/// The score of a link between nodes with the sorted, duplicate-free
/// neighbor sets `a` and `b`; `degree` gives the neighbor count of a
/// common neighbor.
pub fn link_score<T: Ord + Copy>(
    a: &[T],
    b: &[T],
    metric: LinkMetric,
    degree: impl Fn(T) -> usize,
) -> f64 {
    let common = || shared(a, b).copied();
    match metric {
        LinkMetric::CommonNeighbors => common().count() as f64,
        LinkMetric::AdamicAdar => common()
            .map(|w| degree(w) as f64)
            .filter(|d| *d > 1.0)
            .map(|d| 1.0 / d.ln())
            .sum(),
        LinkMetric::ResourceAllocation => common()
            .map(|w| degree(w) as f64)
            .filter(|d| *d > 0.0)
            .map(|d| 1.0 / d)
            .sum(),
        LinkMetric::PreferentialAttachment => (a.len() * b.len()) as f64,
        LinkMetric::TotalNeighbors => (a.len() + b.len() - common().count()) as f64,
        LinkMetric::Jaccard => neighbor_similarity(a, b, SimilarityMetric::Jaccard),
    }
}

/// The `limit` most likely new edges of `node`: nodes two hops away that
/// it is not joined to yet, best first. Edges count in both directions.
pub fn predict_links(
    p: &Projection,
    node: u32,
    metric: LinkMetric,
    limit: usize,
    stop: Stop,
) -> Result<Vec<(u32, f64)>> {
    let sets = p.neighbor_sets(Direction::Both);
    let mine = &sets[node as usize];
    let mut candidates: Vec<u32> = Vec::new();
    for (i, &middle) in mine.iter().enumerate() {
        check(stop, i)?;
        candidates.extend(
            sets[middle as usize]
                .iter()
                .copied()
                .filter(|&other| other != node && mine.binary_search(&other).is_err()),
        );
    }
    candidates.sort_unstable();
    candidates.dedup();
    let mut scored: Vec<(u32, f64)> = candidates
        .into_iter()
        .map(|other| {
            let score = link_score(mine, &sets[other as usize], metric, |w: u32| {
                sets[w as usize].len()
            });
            (other, score)
        })
        .collect();
    scored.sort_unstable_by(|x, y| y.1.total_cmp(&x.1).then(x.0.cmp(&y.0)));
    scored.truncate(limit);
    Ok(scored)
}

#[cfg(test)]
mod tests {
    use super::super::never;
    use super::super::tests::graph;
    use super::*;

    #[test]
    fn set_metrics() {
        let (a, b) = ([1, 2, 3], [2, 3, 4, 5]);
        assert_eq!(
            neighbor_similarity(&a, &b, SimilarityMetric::Jaccard),
            2.0 / 5.0
        );
        assert_eq!(
            neighbor_similarity(&a, &b, SimilarityMetric::Overlap),
            2.0 / 3.0
        );
        let cosine = neighbor_similarity(&a, &b, SimilarityMetric::Cosine);
        assert!((cosine - 2.0 / 12f64.sqrt()).abs() < 1e-12);
        assert_eq!(
            neighbor_similarity::<u32>(&[], &[], SimilarityMetric::Jaccard),
            0.0
        );
    }

    #[test]
    fn people_who_like_the_same_things() {
        // People 0, 1, 2 like items 3, 4, 5.
        let p = graph(6, &[(0, 3), (0, 4), (1, 3), (1, 4), (1, 5), (2, 5)]);
        let pairs = similar_neighbors(
            &p,
            SimilarityMetric::Jaccard,
            Direction::Out,
            10,
            0.0,
            None,
            &never,
        )
        .unwrap();
        assert_eq!(pairs[0], (0, 1, 2.0 / 3.0));
        assert!(pairs.contains(&(1, 0, 2.0 / 3.0)));
        assert!(pairs.contains(&(2, 1, 1.0 / 3.0)));
        assert!(
            !pairs.iter().any(|(a, b, _)| (*a, *b) == (0, 2)),
            "nothing shared"
        );
        let top = similar_neighbors(
            &p,
            SimilarityMetric::Jaccard,
            Direction::Out,
            1,
            0.5,
            Some(1),
            &never,
        )
        .unwrap();
        assert_eq!(top, vec![(1, 0, 2.0 / 3.0)]);
    }

    #[test]
    fn link_prediction() {
        // 0 and 3 share neighbors 1 and 2; 2 also knows 4.
        let p = graph(5, &[(0, 1), (0, 2), (3, 1), (3, 2), (2, 4)]);
        let predicted = predict_links(&p, 0, LinkMetric::CommonNeighbors, 10, &never).unwrap();
        assert_eq!(predicted, vec![(3, 2.0), (4, 1.0)]);
        let sets = p.neighbor_sets(Direction::Both);
        let degree = |w: u32| sets[w as usize].len();
        let aa = link_score(&sets[0], &sets[3], LinkMetric::AdamicAdar, degree);
        assert!((aa - (1.0 / 2f64.ln() + 1.0 / 3f64.ln())).abs() < 1e-12);
        let ra = link_score(&sets[0], &sets[3], LinkMetric::ResourceAllocation, degree);
        assert!((ra - (0.5 + 1.0 / 3.0)).abs() < 1e-12);
        assert_eq!(
            link_score(
                &sets[0],
                &sets[4],
                LinkMetric::PreferentialAttachment,
                degree
            ),
            2.0
        );
        assert_eq!(
            link_score(&sets[0], &sets[3], LinkMetric::TotalNeighbors, degree),
            2.0
        );
        assert!(LinkMetric::parse("adamic_adar").is_ok());
        assert!(LinkMetric::parse("nope").is_err());
    }
}
