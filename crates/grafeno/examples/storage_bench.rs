//! What compact storage changes: the bytes of the adjacency and vector
//! tables, reading every node's edges, and vector search (latency and
//! recall@10 against exact answers). The database is a file, measured
//! after it is closed by reopening it with redb.
//!
//! `cargo run --release -p grafeno --example storage_bench -- [nodes] [vectors]`

use std::collections::HashSet;
use std::time::Instant;

use grafeno::{Direction, EdgeFilter, Grafeno, NewEdge, NewNode, NodeId, Timestamp};
use redb::{ReadableDatabase, ReadableTableMetadata, TableHandle};

const NS: &str = "bench";
const DIM: usize = 128;
const CLUSTERS: usize = 64;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn unit(&mut self) -> f32 {
        (self.next() % 1_000_000) as f32 / 1_000_000.0
    }

    fn normal(&mut self) -> f32 {
        let (a, b) = (self.unit().max(1e-9), self.unit());
        (-2.0 * a.ln()).sqrt() * (std::f32::consts::TAU * b).cos()
    }
}

fn normalized(mut vector: Vec<f32>) -> Vec<f32> {
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    vector.iter_mut().for_each(|v| *v /= norm);
    vector
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn main() {
    let args: Vec<usize> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    let nodes = args.first().copied().unwrap_or(100_000);
    let count = args.get(1).copied().unwrap_or(50_000);
    let path = std::env::temp_dir().join(format!("grafeno-storage-{}.redb", NodeId::new()));
    let mut rng = Rng(88172645463325252);
    let started = Instant::now();
    let ids = {
        let db = Grafeno::open(&path).unwrap();
        let mut ids = Vec::with_capacity(nodes);
        for chunk in 0..nodes.div_ceil(5_000) {
            let made = db
                .write(|tx| {
                    (chunk * 5_000..(chunk * 5_000 + 5_000).min(nodes))
                        .map(|i| tx.create_node(NS, NewNode::new("Item").prop("i", i as i64)))
                        .collect::<grafeno::Result<Vec<_>>>()
                })
                .unwrap();
            ids.extend(made);
        }
        // Three edges per node; one in three with a valid-time range.
        for chunk in ids.chunks(5_000) {
            let edges: Vec<_> = chunk
                .iter()
                .flat_map(|from| {
                    (0..3)
                        .map(|k| (*from, ids[(rng.next() % nodes as u64) as usize], k))
                        .collect::<Vec<_>>()
                })
                .collect();
            db.write(|tx| {
                for (from, to, k) in &edges {
                    let mut edge = NewEdge::new(*from, "links", *to);
                    if *k == 0 {
                        edge = edge
                            .valid_from(Timestamp::from_micros(1_577_836_800_000_000))
                            .valid_to(Timestamp::from_micros(1_640_995_200_000_000));
                    }
                    tx.create_edge(NS, edge)?;
                }
                Ok(())
            })
            .unwrap();
        }
        // Clustered vectors on the first `count` nodes.
        let centers: Vec<Vec<f32>> = (0..CLUSTERS)
            // Centers far apart compared with the spread around them.
            .map(|_| (0..DIM).map(|_| rng.normal()).collect())
            .collect();
        let vectors: Vec<Vec<f32>> = (0..count)
            .map(|i| {
                let center = &centers[i % CLUSTERS];
                normalized(center.iter().map(|c| c + 0.35 * rng.normal()).collect())
            })
            .collect();
        for (chunk, part) in vectors.chunks(1_000).enumerate() {
            db.write(|tx| {
                for (k, vector) in part.iter().enumerate() {
                    tx.set_embedding(NS, ids[chunk * 1_000 + k], vector)?;
                }
                Ok(())
            })
            .unwrap();
        }
        println!(
            "{nodes} nodes, {} edges, {count} vectors of {DIM} dimensions ({:.1} s to build)",
            nodes * 3,
            started.elapsed().as_secs_f64()
        );

        // Reading every node's edges.
        let read = db.begin_read().unwrap();
        let mut best = f64::MAX;
        for _ in 0..3 {
            let at = Instant::now();
            let mut seen = 0usize;
            for id in &ids {
                seen += read
                    .neighbors(NS, *id, Direction::Both, &EdgeFilter::any())
                    .unwrap()
                    .len();
            }
            // A self loop is listed once both ways.
            assert!(seen > nodes * 6 - 100);
            best = best.min(at.elapsed().as_secs_f64() * 1000.0);
        }
        println!("| every node's edges, both ways | {best:.0} ms |");

        // Vector search against exact answers.
        let queries: Vec<Vec<f32>> = (0..300)
            .map(|q| {
                let center = &centers[(q * 7) % CLUSTERS];
                normalized(center.iter().map(|c| c + 0.35 * rng.normal()).collect())
            })
            .collect();
        let mut latencies = Vec::new();
        let mut found = 0usize;
        for query in &queries {
            let mut exact: Vec<(f32, usize)> = vectors
                .iter()
                .enumerate()
                .map(|(i, v)| (dot(query, v), i))
                .collect();
            exact.sort_by(|a, b| b.0.total_cmp(&a.0));
            let truth: HashSet<NodeId> = exact[..10].iter().map(|(_, i)| ids[*i]).collect();
            let at = Instant::now();
            let hits = read.search_vector_with(NS, query, 10, 64).unwrap();
            latencies.push(at.elapsed().as_secs_f64() * 1000.0);
            found += hits.iter().filter(|hit| truth.contains(&hit.node)).count();
        }
        latencies.sort_by(f64::total_cmp);
        println!(
            "| vector search, ef 64 | recall@10 {:.3} | p50 {:.3} ms | p99 {:.3} ms |",
            found as f64 / (queries.len() * 10) as f64,
            latencies[latencies.len() / 2],
            latencies[latencies.len() * 99 / 100]
        );
        ids
    };
    let _ = ids;

    // Table sizes, read with redb from the closed file.
    let file = std::fs::metadata(&path).unwrap().len();
    let db = redb::Database::open(&path).unwrap();
    let tx = db.begin_read().unwrap();
    println!("| table | entries | stored MiB | with metadata MiB |");
    let mut handles: Vec<_> = tx.list_tables().unwrap().collect();
    handles.sort_by(|a, b| a.name().cmp(b.name()));
    for handle in handles {
        let name = handle.name().to_owned();
        if ![
            "adjacency_out",
            "adjacency_in",
            "vectors",
            "vectors_q8",
            "hnsw_links",
            "nodes",
            "edges",
        ]
        .contains(&name.as_str())
        {
            continue;
        }
        let table = tx.open_untyped_table(handle).unwrap();
        let stats = table.stats().unwrap();
        let mib = |bytes: u64| bytes as f64 / (1024.0 * 1024.0);
        println!(
            "| {name} | {} | {:.1} | {:.1} |",
            table.len().unwrap(),
            mib(stats.stored_bytes()),
            mib(stats.stored_bytes() + stats.metadata_bytes())
        );
    }
    println!(
        "| whole file | | {:.1} | |",
        file as f64 / (1024.0 * 1024.0)
    );
    drop((tx, db));
    std::fs::remove_file(&path).unwrap();
}
