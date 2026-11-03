//! Measures the vector index on clustered data: insert throughput, and the
//! recall@10 and latency of searches against exact (brute-force) results.
//!
//! ```text
//! cargo run --release -p grafeno --example hnsw_recall -- [vectors] [dimension] [queries]
//! ```
//!
//! Defaults: 20 000 vectors of dimension 128, 200 queries.

use std::collections::HashSet;
use std::time::Instant;

use grafeno::{Grafeno, NewNode, NodeId};

const NS: &str = "recall";
const CLUSTERS: usize = 64;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    }

    /// Approximately normal (sum of uniforms).
    fn normal(&mut self) -> f32 {
        (0..4).map(|_| self.unit()).sum::<f32>() * 0.5
    }
}

fn normalized(mut vector: Vec<f32>) -> Vec<f32> {
    let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
    vector.iter_mut().for_each(|x| *x /= norm);
    vector
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn main() {
    let args: Vec<usize> = std::env::args()
        .skip(1)
        .map(|a| a.parse().expect("numbers"))
        .collect();
    let count = args.first().copied().unwrap_or(20_000);
    let dimension = args.get(1).copied().unwrap_or(128);
    let queries = args.get(2).copied().unwrap_or(200);

    // Points around random cluster centers, like embeddings of related texts.
    let mut rng = Rng(0x5EED);
    let centers: Vec<Vec<f32>> = (0..CLUSTERS)
        .map(|_| (0..dimension).map(|_| rng.unit()).collect())
        .collect();
    let point = |rng: &mut Rng| {
        let center = &centers[(rng.next() % CLUSTERS as u64) as usize];
        normalized(center.iter().map(|c| c + 0.35 * rng.normal()).collect())
    };
    let vectors: Vec<Vec<f32>> = (0..count).map(|_| point(&mut rng)).collect();

    let db = Grafeno::in_memory().unwrap();
    let started = Instant::now();
    let mut ids: Vec<NodeId> = Vec::with_capacity(count);
    for chunk in vectors.chunks(1_000) {
        let created = db
            .write(|tx| {
                chunk
                    .iter()
                    .map(|v| tx.create_node(NS, NewNode::new("Point").embedding(v.clone())))
                    .collect::<grafeno::Result<Vec<_>>>()
            })
            .unwrap();
        ids.extend(created);
    }
    let elapsed = started.elapsed().as_secs_f64();
    println!(
        "inserted {count} vectors of dimension {dimension} in {elapsed:.1} s ({:.0} per second)",
        count as f64 / elapsed
    );

    let read = db.begin_read().unwrap();
    let report = read.check(NS).unwrap();
    println!(
        "check: {} ({} dangling links)",
        if report.is_ok() {
            "ok".to_owned()
        } else {
            report.problems.join("; ")
        },
        report.dangling_links
    );

    let tests: Vec<Vec<f32>> = (0..queries).map(|_| point(&mut rng)).collect();
    let started = Instant::now();
    let exact: Vec<HashSet<NodeId>> = tests
        .iter()
        .map(|query| {
            let mut scored: Vec<(f32, NodeId)> = vectors
                .iter()
                .zip(&ids)
                .map(|(v, id)| (dot(query, v), *id))
                .collect();
            scored.sort_by(|a, b| b.0.total_cmp(&a.0));
            scored.into_iter().take(10).map(|(_, id)| id).collect()
        })
        .collect();
    let brute = started.elapsed().as_secs_f64() * 1e3 / queries as f64;

    println!("\n  ef   recall@10   p50 ms   p99 ms   (brute force: {brute:.2} ms per query)");
    for ef in [16, 32, 64, 128, 256] {
        let mut found = 0;
        let mut latencies = Vec::with_capacity(queries);
        for (query, expected) in tests.iter().zip(&exact) {
            let started = Instant::now();
            let hits = read.search_vector_with(NS, query, 10, ef).unwrap();
            latencies.push(started.elapsed().as_secs_f64() * 1e3);
            found += hits
                .iter()
                .filter(|hit| expected.contains(&hit.node))
                .count();
        }
        latencies.sort_by(f64::total_cmp);
        println!(
            "{ef:>4}   {:>9.3}   {:>6.3}   {:>6.3}",
            found as f64 / (queries * 10) as f64,
            percentile(&latencies, 0.5),
            percentile(&latencies, 0.99)
        );
    }
}
