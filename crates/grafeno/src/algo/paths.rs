//! Weighted paths: Dijkstra, A*, the k shortest paths (Yen) and every
//! shortest path between two nodes.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};

use grafeno_core::{EdgeId, Error, Result};

use super::{Projection, Stop, check};
use crate::traverse::Direction;

/// A path through a projection: positions of nodes, the edges between
/// them (`edges[i]` joins `nodes[i]` and `nodes[i + 1]`), and the sum of
/// the edge weights.
#[derive(Clone, Debug, PartialEq)]
pub struct WeightedPath {
    pub nodes: Vec<u32>,
    pub edges: Vec<EdgeId>,
    pub cost: f64,
}

/// A node waiting in the queue, the cheapest first.
#[derive(Clone, Copy, PartialEq)]
struct Queued {
    /// Cost so far plus the estimate to the target.
    priority: f64,
    node: u32,
}

impl Eq for Queued {}

impl Ord for Queued {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .priority
            .total_cmp(&self.priority)
            .then_with(|| other.node.cmp(&self.node))
    }
}

impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn weight_of(weight: f64) -> Result<f64> {
    if weight.is_finite() && weight >= 0.0 {
        Ok(weight)
    } else {
        Err(Error::Invalid(format!(
            "path weights must be finite and not negative, found {weight}"
        )))
    }
}

/// What a search may not use: nodes and edges taken out by Yen's
/// algorithm.
#[derive(Default)]
struct Banned {
    nodes: HashSet<u32>,
    edges: HashSet<EdgeId>,
}

/// The cost of reaching each node, and the step that reached it.
type Searched = (Vec<f64>, Vec<Option<(u32, EdgeId)>>);

/// Best-first search from `from`. With a target it stops when the target
/// is settled; `estimate` is A*'s heuristic (0 for Dijkstra), which must
/// never overestimate the remaining cost.
fn search(
    p: &Projection,
    from: u32,
    to: Option<u32>,
    direction: Direction,
    estimate: &dyn Fn(u32) -> f64,
    banned: &Banned,
    stop: Stop,
) -> Result<Searched> {
    let mut cost = vec![f64::INFINITY; p.len()];
    let mut previous: Vec<Option<(u32, EdgeId)>> = vec![None; p.len()];
    let mut done = vec![false; p.len()];
    let mut queue = BinaryHeap::new();
    cost[from as usize] = 0.0;
    queue.push(Queued {
        priority: estimate(from),
        node: from,
    });
    let mut settled = 0;
    while let Some(Queued { node, .. }) = queue.pop() {
        if done[node as usize] {
            continue;
        }
        done[node as usize] = true;
        settled += 1;
        check(stop, settled)?;
        if Some(node) == to {
            break;
        }
        for step in p.neighbors(node, direction) {
            if banned.nodes.contains(&step.node) || banned.edges.contains(&step.edge) {
                continue;
            }
            let next = cost[node as usize] + weight_of(step.weight)?;
            if next < cost[step.node as usize] {
                cost[step.node as usize] = next;
                previous[step.node as usize] = Some((node, step.edge));
                queue.push(Queued {
                    priority: next + estimate(step.node),
                    node: step.node,
                });
            }
        }
    }
    Ok((cost, previous))
}

fn rebuild(previous: &[Option<(u32, EdgeId)>], from: u32, to: u32, cost: f64) -> WeightedPath {
    let mut nodes = vec![to];
    let mut edges = Vec::new();
    let mut at = to;
    while at != from {
        let (before, edge) = previous[at as usize].expect("a settled node has a parent");
        nodes.push(before);
        edges.push(edge);
        at = before;
    }
    nodes.reverse();
    edges.reverse();
    WeightedPath { nodes, edges, cost }
}

fn path_between(
    p: &Projection,
    from: u32,
    to: u32,
    direction: Direction,
    estimate: &dyn Fn(u32) -> f64,
    banned: &Banned,
    stop: Stop,
) -> Result<Option<WeightedPath>> {
    let (cost, previous) = search(p, from, Some(to), direction, estimate, banned, stop)?;
    let total = cost[to as usize];
    Ok(total
        .is_finite()
        .then(|| rebuild(&previous, from, to, total)))
}

/// The cheapest path from `from` to `to` (Dijkstra). Without a weight
/// property every edge weighs 1, so it has the fewest hops.
pub fn shortest_path(
    p: &Projection,
    from: u32,
    to: u32,
    direction: Direction,
    stop: Stop,
) -> Result<Option<WeightedPath>> {
    path_between(p, from, to, direction, &|_| 0.0, &Banned::default(), stop)
}

/// The cheapest path with A*: `estimate(node)` guesses the cost from
/// `node` to `to` (a straight-line distance, say) and must never be more
/// than the real cost, or the path found may not be the cheapest.
pub fn astar(
    p: &Projection,
    from: u32,
    to: u32,
    direction: Direction,
    estimate: &dyn Fn(u32) -> f64,
    stop: Stop,
) -> Result<Option<WeightedPath>> {
    path_between(p, from, to, direction, estimate, &Banned::default(), stop)
}

/// The cost of the cheapest path from `from` to every node (infinite when
/// there is none).
pub fn distances(p: &Projection, from: u32, direction: Direction, stop: Stop) -> Result<Vec<f64>> {
    Ok(search(p, from, None, direction, &|_| 0.0, &Banned::default(), stop)?.0)
}

/// The weight of the edge `edge` leaving `node`.
fn edge_weight(p: &Projection, node: u32, edge: EdgeId, direction: Direction) -> f64 {
    p.neighbors(node, direction)
        .find(|step| step.edge == edge)
        .map_or(0.0, |step| step.weight)
}

/// Up to `k` loopless paths from `from` to `to`, cheapest first (Yen's
/// algorithm). Equal costs come with fewer hops first.
pub fn k_shortest_paths(
    p: &Projection,
    from: u32,
    to: u32,
    k: usize,
    direction: Direction,
    stop: Stop,
) -> Result<Vec<WeightedPath>> {
    let Some(first) = shortest_path(p, from, to, direction, stop)? else {
        return Ok(Vec::new());
    };
    let mut found = vec![first];
    let mut candidates: Vec<WeightedPath> = Vec::new();
    let same = |a: &WeightedPath, b: &WeightedPath| a.nodes == b.nodes && a.edges == b.edges;
    while found.len() < k {
        let last = found.last().expect("found is not empty").clone();
        let mut root_cost = 0.0;
        for i in 0..last.nodes.len() - 1 {
            let spur = last.nodes[i];
            let root_nodes = &last.nodes[..=i];
            let root_edges = &last.edges[..i];
            let mut banned = Banned::default();
            for path in &found {
                if path.nodes.len() > i + 1
                    && path.nodes[..=i] == *root_nodes
                    && path.edges[..i] == *root_edges
                {
                    banned.edges.insert(path.edges[i]);
                }
            }
            banned.nodes.extend(root_nodes[..i].iter().copied());
            if let Some(spur_path) = path_between(p, spur, to, direction, &|_| 0.0, &banned, stop)?
            {
                let mut nodes = root_nodes[..i].to_vec();
                nodes.extend(spur_path.nodes);
                let mut edges = root_edges.to_vec();
                edges.extend(spur_path.edges);
                let candidate = WeightedPath {
                    nodes,
                    edges,
                    cost: root_cost + spur_path.cost,
                };
                if !found.iter().chain(&candidates).any(|p| same(p, &candidate)) {
                    candidates.push(candidate);
                }
            }
            root_cost += edge_weight(p, spur, last.edges[i], direction);
        }
        let Some(best) = candidates
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                a.cost
                    .total_cmp(&b.cost)
                    .then(a.nodes.len().cmp(&b.nodes.len()))
            })
            .map(|(at, _)| at)
        else {
            break;
        };
        found.push(candidates.swap_remove(best));
    }
    Ok(found)
}

/// Every cheapest path from `from` to `to`, at most `limit` of them.
pub fn all_shortest_paths(
    p: &Projection,
    from: u32,
    to: u32,
    direction: Direction,
    limit: usize,
    stop: Stop,
) -> Result<Vec<WeightedPath>> {
    let same_cost = |a: f64, b: f64| (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0);
    let mut cost = vec![f64::INFINITY; p.len()];
    let mut parents: Vec<Vec<(u32, EdgeId)>> = vec![Vec::new(); p.len()];
    let mut done = vec![false; p.len()];
    let mut queue = BinaryHeap::new();
    cost[from as usize] = 0.0;
    queue.push(Queued {
        priority: 0.0,
        node: from,
    });
    let mut settled = 0;
    while let Some(Queued { node, priority }) = queue.pop() {
        if done[node as usize] {
            continue;
        }
        // Nodes beyond the target's cost cannot be on a cheapest path.
        if priority > cost[to as usize] && !same_cost(priority, cost[to as usize]) {
            break;
        }
        done[node as usize] = true;
        settled += 1;
        check(stop, settled)?;
        for step in p.neighbors(node, direction) {
            let next = cost[node as usize] + weight_of(step.weight)?;
            let known = cost[step.node as usize];
            if known.is_finite() && same_cost(next, known) {
                parents[step.node as usize].push((node, step.edge));
            } else if next < known {
                cost[step.node as usize] = next;
                parents[step.node as usize] = vec![(node, step.edge)];
                queue.push(Queued {
                    priority: next,
                    node: step.node,
                });
            }
        }
    }
    let total = cost[to as usize];
    if !total.is_finite() {
        return Ok(Vec::new());
    }
    if from == to {
        return Ok(vec![WeightedPath {
            nodes: vec![from],
            edges: Vec::new(),
            cost: 0.0,
        }]);
    }
    // Walks the parents back from the target, depth first.
    let mut out = Vec::new();
    let mut nodes = vec![to];
    let mut edges = Vec::new();
    let mut choice = vec![0usize];
    while let Some(&at) = nodes.last() {
        if out.len() == limit {
            break;
        }
        let depth = nodes.len() - 1;
        if at == from {
            let mut path = WeightedPath {
                nodes: nodes.clone(),
                edges: edges.clone(),
                cost: total,
            };
            path.nodes.reverse();
            path.edges.reverse();
            out.push(path);
        } else if let Some(&(parent, edge)) = parents[at as usize].get(choice[depth]) {
            choice[depth] += 1;
            // Zero-weight cycles can make parents loop.
            if !nodes.contains(&parent) {
                nodes.push(parent);
                edges.push(edge);
                choice.push(0);
            }
            continue;
        }
        nodes.pop();
        edges.pop();
        choice.pop();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::never;
    use super::super::tests::{graph, weighted};
    use super::*;

    #[test]
    fn dijkstra_takes_the_cheaper_detour() {
        // 0 -5-> 1, 0 -1-> 2 -1-> 1
        let p = weighted(3, &[(0, 1, 5.0), (0, 2, 1.0), (2, 1, 1.0)], true);
        let path = shortest_path(&p, 0, 1, Direction::Out, &never)
            .unwrap()
            .unwrap();
        assert_eq!(path.nodes, vec![0, 2, 1]);
        assert_eq!(path.cost, 2.0);
        assert!(
            shortest_path(&p, 1, 0, Direction::Out, &never)
                .unwrap()
                .is_none()
        );
        let back = shortest_path(&p, 1, 0, Direction::Both, &never)
            .unwrap()
            .unwrap();
        assert_eq!(back.cost, 2.0);
        let all = distances(&p, 0, Direction::Out, &never).unwrap();
        assert_eq!(all, vec![0.0, 2.0, 1.0]);
    }

    #[test]
    fn astar_agrees_with_dijkstra() {
        // A grid 5 x 5, positions x + 5y; the estimate is the Manhattan distance.
        let mut edges = Vec::new();
        for y in 0..5u32 {
            for x in 0..5u32 {
                if x < 4 {
                    edges.push((x + 5 * y, x + 1 + 5 * y));
                }
                if y < 4 {
                    edges.push((x + 5 * y, x + 5 * (y + 1)));
                }
            }
        }
        let p = graph(25, &edges);
        let estimate = |n: u32| f64::from((4 - n % 5) + (4 - n / 5));
        let a = astar(&p, 0, 24, Direction::Both, &estimate, &never)
            .unwrap()
            .unwrap();
        let d = shortest_path(&p, 0, 24, Direction::Both, &never)
            .unwrap()
            .unwrap();
        assert_eq!(a.cost, 8.0);
        assert_eq!(a.cost, d.cost);
    }

    #[test]
    fn yen_finds_the_next_cheapest_paths() {
        // The classic example: C=0 D=1 E=2 F=3 G=4 H=5.
        let p = weighted(
            6,
            &[
                (0, 1, 3.0),
                (0, 2, 2.0),
                (1, 3, 4.0),
                (2, 1, 1.0),
                (2, 3, 2.0),
                (2, 4, 3.0),
                (3, 4, 2.0),
                (3, 5, 1.0),
                (4, 5, 2.0),
            ],
            true,
        );
        let paths = k_shortest_paths(&p, 0, 5, 3, Direction::Out, &never).unwrap();
        let costs: Vec<f64> = paths.iter().map(|p| p.cost).collect();
        assert_eq!(costs, vec![5.0, 7.0, 8.0]);
        assert_eq!(paths[0].nodes, vec![0, 2, 3, 5]);
        assert_eq!(paths[1].nodes, vec![0, 2, 4, 5]);
        for path in &paths {
            let mut seen = path.nodes.clone();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), path.nodes.len(), "loopless");
        }
        assert_eq!(
            k_shortest_paths(&p, 0, 5, 50, Direction::Out, &never)
                .unwrap()
                .len(),
            7,
            "every loopless path, then it stops"
        );
    }

    #[test]
    fn every_shortest_path_of_a_diamond() {
        // 0 -> 1 -> 3, 0 -> 2 -> 3, 0 -> 3 through 4 longer.
        let p = graph(5, &[(0, 1), (0, 2), (1, 3), (2, 3), (0, 4), (4, 1)]);
        let paths = all_shortest_paths(&p, 0, 3, Direction::Out, 10, &never).unwrap();
        let mut found: Vec<Vec<u32>> = paths.iter().map(|p| p.nodes.clone()).collect();
        found.sort();
        assert_eq!(found, vec![vec![0, 1, 3], vec![0, 2, 3]]);
        assert_eq!(
            all_shortest_paths(&p, 0, 3, Direction::Out, 1, &never)
                .unwrap()
                .len(),
            1
        );
        assert!(
            all_shortest_paths(&p, 3, 0, Direction::Out, 10, &never)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn negative_weights_are_refused() {
        let p = weighted(2, &[(0, 1, -1.0)], true);
        assert!(shortest_path(&p, 0, 1, Direction::Out, &never).is_err());
    }
}
