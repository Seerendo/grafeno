//! Centrality: degree, PageRank, betweenness and closeness.

use std::collections::{BinaryHeap, VecDeque};
use std::ops::Range;

use grafeno_core::Result;

use super::{Projection, Random, Stop, check, stopped};
use crate::traverse::Direction;

/// Splits `0..count` among the cores and runs `work` on each part.
pub(crate) fn parallel<T: Send>(
    count: usize,
    work: impl Fn(Range<usize>) -> Result<T> + Sync,
) -> Result<Vec<T>> {
    let threads = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(count.div_ceil(64))
        .max(1);
    if threads == 1 {
        return Ok(vec![work(0..count)?]);
    }
    let chunk = count.div_ceil(threads);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let range = (t * chunk).min(count)..((t + 1) * chunk).min(count);
                let work = &work;
                scope.spawn(move || work(range))
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or_else(|_| Err(stopped())))
            .collect()
    })
}

/// How many edges each node has, or the sum of their weights.
pub fn degree(p: &Projection, direction: Direction, weighted: bool) -> Vec<f64> {
    (0..p.len() as u32)
        .map(|node| {
            if weighted {
                p.neighbors(node, direction).map(|step| step.weight).sum()
            } else {
                p.degree(node, direction) as f64
            }
        })
        .collect()
}

/// PageRank settings.
#[derive(Clone, Debug)]
pub struct PageRank {
    /// The chance of following an edge rather than jumping anywhere.
    pub damping: f64,
    pub max_iterations: usize,
    /// Stops when the scores change less than this in total.
    pub tolerance: f64,
    pub direction: Direction,
    /// Jumps land only on these nodes (personalized PageRank); on any node
    /// when empty.
    pub sources: Vec<u32>,
}

impl Default for PageRank {
    fn default() -> Self {
        Self {
            damping: 0.85,
            max_iterations: 20,
            tolerance: 1e-7,
            direction: Direction::Out,
            sources: Vec::new(),
        }
    }
}

/// PageRank: the chance of being at each node after a long random walk
/// that follows edges (in proportion to their weight) and sometimes jumps.
/// Scores add up to 1. Returns them with the iterations run.
pub fn page_rank(p: &Projection, settings: &PageRank, stop: Stop) -> Result<(Vec<f64>, usize)> {
    let n = p.len();
    if n == 0 {
        return Ok((Vec::new(), 0));
    }
    let mut jump = vec![0.0; n];
    if settings.sources.is_empty() {
        jump.fill(1.0 / n as f64);
    } else {
        let share = 1.0 / settings.sources.len() as f64;
        for &source in &settings.sources {
            jump[source as usize] += share;
        }
    }
    let total: Vec<f64> = (0..n as u32)
        .map(|node| {
            p.neighbors(node, settings.direction)
                .map(|step| step.weight.max(0.0))
                .sum()
        })
        .collect();
    let d = settings.damping;
    let mut rank = jump.clone();
    let mut next = vec![0.0; n];
    let mut iterations = 0;
    while iterations < settings.max_iterations {
        if stop() {
            return Err(stopped());
        }
        iterations += 1;
        // Nodes without edges out give their rank to the jump targets.
        let mut dangling = 0.0;
        for (node, value) in next.iter_mut().enumerate() {
            *value = 0.0;
            if total[node] <= 0.0 {
                dangling += rank[node];
            }
        }
        for node in 0..n {
            if total[node] > 0.0 {
                let share = d * rank[node] / total[node];
                for step in p.neighbors(node as u32, settings.direction) {
                    next[step.node as usize] += share * step.weight.max(0.0);
                }
            }
        }
        let mut change = 0.0;
        for node in 0..n {
            next[node] += ((1.0 - d) + d * dangling) * jump[node];
            change += (next[node] - rank[node]).abs();
        }
        std::mem::swap(&mut rank, &mut next);
        if change < settings.tolerance {
            break;
        }
    }
    Ok((rank, iterations))
}

/// Shortest-path state for one source of Brandes' algorithm, reused.
struct Brandes {
    order: Vec<u32>,
    parents: Vec<Vec<u32>>,
    paths: Vec<f64>,
    cost: Vec<f64>,
    delta: Vec<f64>,
}

impl Brandes {
    fn new(n: usize) -> Self {
        Self {
            order: Vec::with_capacity(n),
            parents: vec![Vec::new(); n],
            paths: vec![0.0; n],
            cost: vec![f64::INFINITY; n],
            delta: vec![0.0; n],
        }
    }

    /// Settles every node reachable from `source` in order of cost,
    /// counting the shortest paths to each and their last steps.
    fn explore(&mut self, p: &Projection, source: u32, direction: Direction) {
        for node in self.order.drain(..) {
            self.parents[node as usize].clear();
            self.paths[node as usize] = 0.0;
            self.cost[node as usize] = f64::INFINITY;
            self.delta[node as usize] = 0.0;
        }
        let s = source as usize;
        self.paths[s] = 1.0;
        self.cost[s] = 0.0;
        if !p.is_weighted() {
            let mut queue = VecDeque::from([source]);
            while let Some(node) = queue.pop_front() {
                self.order.push(node);
                let next = self.cost[node as usize] + 1.0;
                for step in p.neighbors(node, direction) {
                    let other = step.node as usize;
                    if self.cost[other].is_infinite() {
                        self.cost[other] = next;
                        queue.push_back(step.node);
                    }
                    if self.cost[other] == next {
                        self.paths[other] += self.paths[node as usize];
                        self.parents[other].push(node);
                    }
                }
            }
            return;
        }
        let mut queue = BinaryHeap::new();
        queue.push(Cheapest(0.0, source));
        let mut done = vec![false; p.len()];
        while let Some(Cheapest(cost, node)) = queue.pop() {
            if done[node as usize] || cost > self.cost[node as usize] {
                continue;
            }
            done[node as usize] = true;
            self.order.push(node);
            for step in p.neighbors(node, direction) {
                let other = step.node as usize;
                let next = cost + step.weight.max(0.0);
                let known = self.cost[other];
                let slack = 1e-12 * next.abs().max(1.0);
                if known.is_infinite() || next < known - slack {
                    self.cost[other] = next;
                    self.paths[other] = self.paths[node as usize];
                    self.parents[other].clear();
                    self.parents[other].push(node);
                    queue.push(Cheapest(next, step.node));
                } else if (next - known).abs() <= slack && !done[other] {
                    self.paths[other] += self.paths[node as usize];
                    self.parents[other].push(node);
                }
            }
        }
    }

    /// Adds the dependencies of `source` to `scores`.
    fn accumulate(&mut self, source: u32, scores: &mut [f64]) {
        for &node in self.order.iter().rev() {
            let w = node as usize;
            let coefficient = (1.0 + self.delta[w]) / self.paths[w];
            for &parent in &self.parents[w] {
                self.delta[parent as usize] += self.paths[parent as usize] * coefficient;
            }
            if node != source {
                scores[w] += self.delta[w];
            }
        }
    }
}

#[derive(PartialEq)]
struct Cheapest(f64, u32);

impl Eq for Cheapest {}

impl Ord for Cheapest {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .0
            .total_cmp(&self.0)
            .then_with(|| other.1.cmp(&self.1))
    }
}

impl PartialOrd for Cheapest {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Betweenness: how many shortest paths between other nodes pass through
/// each node (Brandes). Weighted projections use the weights as costs.
/// With `sample`, only that many sources (chosen with `seed`) are used and
/// scores are scaled up: an estimate for large graphs. With `Both`, each
/// path is counted once.
pub fn betweenness(
    p: &Projection,
    direction: Direction,
    sample: Option<usize>,
    seed: u64,
    stop: Stop,
) -> Result<Vec<f64>> {
    let n = p.len();
    let sources: Vec<u32> = match sample {
        Some(k) if k < n => {
            let mut all: Vec<u32> = (0..n as u32).collect();
            let mut random = Random::new(seed);
            for i in 0..k {
                let j = i + random.below(n - i);
                all.swap(i, j);
            }
            all.truncate(k);
            all
        }
        _ => (0..n as u32).collect(),
    };
    let parts = parallel(sources.len(), |range| {
        let mut state = Brandes::new(n);
        let mut scores = vec![0.0; n];
        for (i, &source) in sources[range].iter().enumerate() {
            check(stop, i)?;
            state.explore(p, source, direction);
            state.accumulate(source, &mut scores);
        }
        Ok(scores)
    })?;
    let scale = if sources.is_empty() {
        1.0
    } else {
        n as f64 / sources.len() as f64
    } / if direction == Direction::Both {
        2.0
    } else {
        1.0
    };
    let mut total = vec![0.0; n];
    for part in parts {
        for (sum, value) in total.iter_mut().zip(part) {
            *sum += value;
        }
    }
    for value in &mut total {
        *value *= scale;
    }
    Ok(total)
}

/// Closeness: how near each node is to the nodes it reaches. The classic
/// form is `reached / sum of costs`, scaled by the share of the graph
/// reached (Wasserman and Faust), so that small components do not score
/// high; the harmonic form is the mean of `1 / cost` over every other node.
pub fn closeness(
    p: &Projection,
    direction: Direction,
    harmonic: bool,
    stop: Stop,
) -> Result<Vec<f64>> {
    let n = p.len();
    let parts = parallel(n, |range| {
        let mut state = Brandes::new(n);
        let mut scores = Vec::with_capacity(range.len());
        for (i, source) in range.enumerate() {
            check(stop, i)?;
            state.explore(p, source as u32, direction);
            let (mut reached, mut sum, mut inverse) = (0usize, 0.0, 0.0);
            for &node in &state.order {
                let cost = state.cost[node as usize];
                if node as usize != source && cost > 0.0 {
                    reached += 1;
                    sum += cost;
                    inverse += 1.0 / cost;
                }
            }
            let score = if n <= 1 {
                0.0
            } else if harmonic {
                inverse / (n - 1) as f64
            } else if sum > 0.0 {
                (reached as f64 / (n - 1) as f64) * (reached as f64 / sum)
            } else {
                0.0
            };
            scores.push(score);
        }
        Ok(scores)
    })?;
    Ok(parts.into_iter().flatten().collect())
}

#[cfg(test)]
mod tests {
    use super::super::never;
    use super::super::tests::{graph, weighted};
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn degrees() {
        let p = weighted(3, &[(0, 1, 2.0), (0, 2, 3.0)], true);
        assert_eq!(degree(&p, Direction::Out, false), vec![2.0, 0.0, 0.0]);
        assert_eq!(degree(&p, Direction::Both, true), vec![5.0, 2.0, 3.0]);
    }

    #[test]
    fn page_rank_of_a_star_and_a_cycle() {
        // Everyone points at 0, which points back at 1.
        let p = graph(4, &[(1, 0), (2, 0), (3, 0), (0, 1)]);
        let settings = PageRank {
            max_iterations: 100,
            ..PageRank::default()
        };
        let (rank, _) = page_rank(&p, &settings, &never).unwrap();
        assert!(close(rank.iter().sum::<f64>(), 1.0));
        assert!(rank[0] > rank[1] && rank[1] > rank[2]);
        assert!(close(rank[2], rank[3]));
        // On a cycle every node is equal.
        let cycle = graph(3, &[(0, 1), (1, 2), (2, 0)]);
        let (rank, _) = page_rank(&cycle, &settings, &never).unwrap();
        assert!(rank.iter().all(|r| close(*r, 1.0 / 3.0)));
        // Personalized: jumps only to 2, so 2 gains.
        let personal = PageRank {
            sources: vec![2],
            ..settings
        };
        let (rank, _) = page_rank(&p, &personal, &never).unwrap();
        assert!(rank[2] > rank[3]);
    }

    #[test]
    fn betweenness_of_a_path_and_a_star() {
        // 0 - 1 - 2 - 3: the middle nodes carry 2 paths each.
        let p = graph(4, &[(0, 1), (1, 2), (2, 3)]);
        let scores = betweenness(&p, Direction::Both, None, 0, &never).unwrap();
        assert_eq!(scores, vec![0.0, 2.0, 2.0, 0.0]);
        // A star with center 0 and 4 leaves: C(4, 2) = 6 paths.
        let star = graph(5, &[(0, 1), (0, 2), (0, 3), (0, 4)]);
        let scores = betweenness(&star, Direction::Both, None, 0, &never).unwrap();
        assert_eq!(scores[0], 6.0);
        // Two equal paths split the credit: 0 -> {1, 2} -> 3.
        let diamond = graph(4, &[(0, 1), (0, 2), (1, 3), (2, 3)]);
        let scores = betweenness(&diamond, Direction::Out, None, 0, &never).unwrap();
        assert_eq!(scores, vec![0.0, 0.5, 0.5, 0.0]);
        // Weighted: the cheap side carries the path.
        let w = weighted(
            4,
            &[(0, 1, 1.0), (0, 2, 5.0), (1, 3, 1.0), (2, 3, 1.0)],
            true,
        );
        let scores = betweenness(&w, Direction::Out, None, 0, &never).unwrap();
        assert_eq!(scores, vec![0.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn closeness_of_a_path() {
        let p = graph(3, &[(0, 1), (1, 2)]);
        let scores = closeness(&p, Direction::Both, false, &never).unwrap();
        assert!(close(scores[1], 1.0));
        assert!(close(scores[0], 2.0 / 3.0));
        let harmonic = closeness(&p, Direction::Both, true, &never).unwrap();
        assert!(close(harmonic[0], (1.0 + 0.5) / 2.0));
        // Out only: the last node reaches nothing.
        let out = closeness(&p, Direction::Out, false, &never).unwrap();
        assert_eq!(out[2], 0.0);
    }
}
