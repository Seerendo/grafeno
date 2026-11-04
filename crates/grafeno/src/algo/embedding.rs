//! Node embeddings from the graph's structure: FastRP.

use grafeno_core::{Error, Result};

use super::centrality::parallel;
use super::{Projection, Random, Stop, stopped};
use crate::traverse::Direction;

/// FastRP settings.
#[derive(Clone, Debug)]
pub struct FastRp {
    pub dimension: usize,
    /// The weight of each round of averaging over neighbors: the first is
    /// one hop, the second two hops, and so on.
    pub iteration_weights: Vec<f64>,
    /// The weight of a node's own random vector in the result.
    pub self_influence: f64,
    /// Scales starting vectors by `degree ^ strength`: negative values
    /// play down hubs, positive values play them up.
    pub normalization_strength: f64,
    /// The same seed gives the same embeddings.
    pub seed: u64,
    pub direction: Direction,
}

impl Default for FastRp {
    fn default() -> Self {
        Self {
            dimension: 128,
            iteration_weights: vec![0.0, 1.0, 1.0],
            self_influence: 0.0,
            normalization_strength: 0.0,
            seed: 42,
            direction: Direction::Both,
        }
    }
}

fn normalize(vector: &mut [f32]) {
    let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in vector {
            *x /= norm;
        }
    }
}

/// FastRP (fast random projection): every node starts with a sparse random
/// vector, and each round replaces it with the weighted mean of its
/// neighbors' vectors. The weighted sum of the rounds is the embedding,
/// with length 1: nodes with similar neighborhoods get similar vectors.
/// Isolated nodes get a zero vector unless `self_influence` is set.
pub fn fast_rp(p: &Projection, settings: &FastRp, stop: Stop) -> Result<Vec<Vec<f32>>> {
    let d = settings.dimension;
    if d == 0 || d > 4096 {
        return Err(Error::Invalid(format!(
            "the embedding dimension must be 1 to 4096, not {d}"
        )));
    }
    let n = p.len();
    // Sparse random vectors: each entry is +s or -s with chance 1 / (2s²)
    // each (s = √3), else 0; seeded by node id so a node keeps its vector
    // across projections.
    let scale = 3f32.sqrt();
    let mut start = vec![0f32; n * d];
    for node in 0..n {
        let id = p.id(node as u32).as_u128();
        let mut random = Random::new(settings.seed ^ (id as u64) ^ ((id >> 64) as u64));
        let degree = p.degree(node as u32, settings.direction).max(1) as f64;
        let factor = degree.powf(settings.normalization_strength) as f32;
        for x in &mut start[node * d..(node + 1) * d] {
            *x = match random.below(6) {
                0 => scale * factor,
                1 => -scale * factor,
                _ => 0.0,
            };
        }
    }
    let mut result: Vec<f32> = start
        .iter()
        .map(|x| x * settings.self_influence as f32)
        .collect();
    let mut current = start;
    for &weight in &settings.iteration_weights {
        if stop() {
            return Err(stopped());
        }
        let parts = parallel(n, |range| {
            let mut next = vec![0f32; range.len() * d];
            for (row, node) in range.enumerate() {
                let target = &mut next[row * d..(row + 1) * d];
                let mut total = 0.0f32;
                for step in p.neighbors(node as u32, settings.direction) {
                    let w = step.weight as f32;
                    total += w;
                    let source = &current[step.node as usize * d..(step.node as usize + 1) * d];
                    for (t, s) in target.iter_mut().zip(source) {
                        *t += w * s;
                    }
                }
                if total > 0.0 {
                    for t in target.iter_mut() {
                        *t /= total;
                    }
                }
                normalize(target);
            }
            Ok(next)
        })?;
        current = parts.concat();
        for (r, c) in result.iter_mut().zip(&current) {
            *r += weight as f32 * c;
        }
    }
    Ok(result
        .chunks(d)
        .map(|chunk| {
            let mut vector = chunk.to_vec();
            normalize(&mut vector);
            vector
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::super::never;
    use super::super::tests::graph;
    use super::*;

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    #[test]
    fn neighbors_in_the_same_cluster_are_closer() {
        // Two 5-cliques joined by one edge.
        let mut edges = Vec::new();
        for base in [0u32, 5] {
            for a in 0..5 {
                for b in a + 1..5 {
                    edges.push((base + a, base + b));
                }
            }
        }
        edges.push((4, 5));
        let p = graph(10, &edges);
        let settings = FastRp {
            dimension: 64,
            ..FastRp::default()
        };
        let vectors = fast_rp(&p, &settings, &never).unwrap();
        assert_eq!(vectors.len(), 10);
        assert!(vectors.iter().all(|v| (cosine(v, v) - 1.0).abs() < 1e-4));
        let same = cosine(&vectors[0], &vectors[1]);
        let apart = cosine(&vectors[0], &vectors[9]);
        assert!(same > apart, "{same} vs {apart}");
        assert_eq!(
            vectors,
            fast_rp(&p, &settings, &never).unwrap(),
            "repeatable"
        );
        assert!(
            fast_rp(
                &p,
                &FastRp {
                    dimension: 0,
                    ..FastRp::default()
                },
                &never
            )
            .is_err()
        );
    }
}
