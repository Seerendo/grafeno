//! Memory under a large graph: builds one (or reopens it), runs the usual
//! operations and prints, for each, the time, the resident memory after it
//! (steady) and the most it reached while it ran (peak).
//!
//! Peak and resident memory come from `/proc/self/status` (Linux; elsewhere
//! they print as `-`), and the peak is reset before each operation. Run it
//! in a container to give it a memory limit (see `docs/benchmarks.md`):
//!
//! ```text
//! ram_bench build DB NODES EDGES     # the graph, in transactions of 20 000
//! ram_bench vectors DB COUNT DIM     # COUNT vectors on as many nodes
//! ram_bench ops DB                   # reads, queries, algorithms, export
//! ram_bench search DB DIM            # vector searches
//! ```
//!
//! `CACHE_MB`, `PROJECTION_CACHE_MB` and `VECTOR_CACHE_MB` size the caches.

use std::time::Instant;

use grafeno::algo::{PageRank, ProjectionSpec, page_rank};
use grafeno::{Direction, EdgeFilter, Grafeno, NewEdge, NewNode, NodeId, OpenOptions};
use grafeno_query::{Limits, Params, execute_with};

const NS: &str = "big";
const LABELS: [&str; 4] = ["knows", "likes", "works_with", "mentions"];

/// `(VmRSS, VmHWM)` in MiB.
fn memory() -> Option<(f64, f64)> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let field = |name: &str| -> Option<f64> {
        let line = status.lines().find(|line| line.starts_with(name))?;
        let kb: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb / 1024.0)
    };
    Some((field("VmRSS:")?, field("VmHWM:")?))
}

/// Starts a new peak at the current resident memory.
fn reset_peak() {
    let _ = std::fs::write("/proc/self/clear_refs", "5");
}

fn show(value: Option<f64>) -> String {
    value.map_or("-".into(), |mb| format!("{mb:.0}"))
}

/// Runs `work` and prints a row: operation, seconds, steady MiB, peak MiB.
fn measure<T>(name: &str, work: impl FnOnce() -> T) -> T {
    reset_peak();
    let started = Instant::now();
    let value = work();
    let took = started.elapsed().as_secs_f64();
    let memory = memory();
    println!(
        "| {name} | {took:.1} | {} | {} |",
        show(memory.map(|m| m.0)),
        show(memory.map(|m| m.1))
    );
    value
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn unit(&mut self) -> f32 {
        (self.next() >> 11) as f32 / (1u64 << 53) as f32 * 2.0 - 1.0
    }
}

fn mb(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .map_or(default, |mb| mb * 1024 * 1024)
}

fn open(path: &str) -> Grafeno {
    let base = OpenOptions::default();
    let options = OpenOptions {
        cache_bytes: mb("CACHE_MB", base.cache_bytes),
        projection_cache_bytes: mb("PROJECTION_CACHE_MB", base.projection_cache_bytes),
        vector_cache_bytes: mb("VECTOR_CACHE_MB", base.vector_cache_bytes),
    };
    Grafeno::open_with(path, &options).unwrap()
}

fn node_ids(db: &Grafeno) -> Vec<NodeId> {
    let read = db.begin_read().unwrap();
    let mut ids = Vec::new();
    let mut after = None;
    loop {
        let page = read.nodes_page(NS, None, after, 50_000).unwrap();
        if page.is_empty() {
            break;
        }
        after = page.last().map(|node| node.id);
        ids.extend(page.into_iter().map(|node| node.id));
    }
    ids
}

fn build(path: &str, nodes: usize, edges: usize) {
    let db = open(path);
    let mut rng = Rng(0x5EED);
    let ids: Vec<NodeId> = measure(&format!("create {nodes} nodes"), || {
        let mut ids = Vec::with_capacity(nodes);
        for chunk in 0..nodes.div_ceil(20_000) {
            let range = chunk * 20_000..((chunk + 1) * 20_000).min(nodes);
            ids.extend(
                db.write(|tx| {
                    range
                        .map(|i| {
                            tx.create_node(
                                NS,
                                NewNode::new(if i % 10 == 0 { "Company" } else { "Person" })
                                    .prop("name", format!("entity {i}"))
                                    .prop("i", i as i64),
                            )
                        })
                        .collect::<grafeno::Result<Vec<_>>>()
                })
                .unwrap(),
            );
        }
        ids
    });
    measure(&format!("create {edges} edges"), || {
        for chunk in 0..edges.div_ceil(20_000) {
            let count = 20_000.min(edges - chunk * 20_000);
            db.write(|tx| {
                for k in 0..count {
                    // A skewed graph: low positions get more edges.
                    let from = ids[rng.below(nodes)];
                    let spread = rng.below(nodes) + 1;
                    let to = ids[rng.below(spread)];
                    tx.create_edge(NS, NewEdge::new(from, LABELS[k % 4], to))?;
                }
                Ok(())
            })
            .unwrap();
        }
    });
}

fn vectors(path: &str, count: usize, dimension: usize) {
    let db = open(path);
    let ids = node_ids(&db);
    let mut rng = Rng(0xFEED);
    let centers: Vec<Vec<f32>> = (0..256)
        .map(|_| (0..dimension).map(|_| rng.unit()).collect())
        .collect();
    let started = Instant::now();
    measure(&format!("set {count} vectors of {dimension}"), || {
        for chunk in 0..count.div_ceil(5_000) {
            let range = chunk * 5_000..((chunk + 1) * 5_000).min(count);
            let batch: Vec<(NodeId, Vec<f32>)> = range
                .map(|i| {
                    let center = &centers[i % centers.len()];
                    let vector = center.iter().map(|c| c + 0.3 * rng.unit()).collect();
                    (ids[i % ids.len()], vector)
                })
                .collect();
            db.write(|tx| {
                for (id, vector) in &batch {
                    tx.set_embedding(NS, *id, vector)?;
                }
                Ok(())
            })
            .unwrap();
            if chunk % 20 == 19 {
                eprintln!(
                    "{} vectors in {:.0} s",
                    (chunk + 1) * 5_000,
                    started.elapsed().as_secs_f64()
                );
            }
        }
    });
    drop(db);
    search(path, dimension);
}

/// Vector searches on a reopened database.
fn search(path: &str, dimension: usize) {
    let db = measure("open", || open(path));
    let mut rng = Rng(0xFEED);
    let centers: Vec<Vec<f32>> = (0..256)
        .map(|_| (0..dimension).map(|_| rng.unit()).collect())
        .collect();
    let read = db.begin_read().unwrap();
    for round in ["first", "second"] {
        measure(
            &format!("1 000 vector searches (k 10), {round} round"),
            || {
                for i in 0..1_000 {
                    let query: Vec<f32> = centers[i % centers.len()]
                        .iter()
                        .map(|c| c + 0.3 * rng.unit())
                        .collect();
                    read.search_vector(NS, &query, 10).unwrap();
                }
            },
        );
    }
}

fn ops(path: &str) {
    let db = measure("open", || open(path));
    let ids = measure("list every node id", || node_ids(&db));
    let mut rng = Rng(0xABCD);
    let read = db.begin_read().unwrap();
    measure("10 000 neighbor lookups", || {
        for _ in 0..10_000 {
            let id = ids[rng.below(ids.len())];
            read.neighbors(NS, id, Direction::Both, &EdgeFilter::any())
                .unwrap();
        }
    });
    measure("100 two-hop neighborhoods", || {
        for _ in 0..100 {
            let id = ids[rng.below(ids.len())];
            for hop in read
                .neighbor_ids(NS, id, Direction::Out, &EdgeFilter::any())
                .unwrap()
            {
                read.neighbor_ids(NS, hop.1, Direction::Out, &EdgeFilter::any())
                    .unwrap();
            }
        }
    });
    let gq = |query: &str| {
        execute_with(&db, NS, query, &Params::new(), &Limits::standard())
            .map(|result| format!("{:?}", result.rows.first()))
            .unwrap_or_else(|error| format!("error: {}", error.message))
    };
    let answer = measure("GQ: count every edge", || {
        gq("MATCH ()-[r]->() RETURN count(r)")
    });
    eprintln!("  {answer}");
    let answer = measure("GQ: top 10 by degree", || {
        gq("MATCH (n:Company)-[r]-() RETURN n.name, count(r) AS d ORDER BY d DESC LIMIT 10")
    });
    eprintln!("  {}", &answer[..answer.len().min(120)]);
    measure("GQ: lookup by property (no index)", || {
        gq("MATCH (n:Person {i: 777}) RETURN n.name")
    });
    measure("export (to nowhere)", || {
        read.export(&[NS.to_owned()], &mut std::io::sink()).unwrap();
    });
    measure("check", || {
        let report = read.check(NS).unwrap();
        assert!(report.is_ok(), "{:?}", report.problems);
    });
    let plain = measure("projection (no edge ids)", || {
        read.projection(NS, &ProjectionSpec::default()).unwrap()
    });
    eprintln!(
        "  {} nodes, {} edges, {:.0} MiB",
        plain.len(),
        plain.edge_count(),
        plain.bytes() as f64 / 1048576.0
    );
    // Dropped first, as a query would: only the cache keeps it.
    drop(plain);
    measure(
        "PageRank (projection from the cache, or built again)",
        || {
            let p = read.projection(NS, &ProjectionSpec::default()).unwrap();
            page_rank(&p, &PageRank::default(), &grafeno::algo::never).unwrap();
        },
    );
    let with_ids = measure("projection with edge ids", || {
        read.projection(
            NS,
            &ProjectionSpec {
                edge_ids: true,
                ..ProjectionSpec::default()
            },
        )
        .unwrap()
    });
    eprintln!(
        "  {:.0} MiB; cache holds {:.0} MiB",
        with_ids.bytes() as f64 / 1048576.0,
        db.projection_cache_bytes() as f64 / 1048576.0
    );
    drop(with_ids);
    let answer = measure("GQ: algo.pageRank({top: 5})", || {
        gq("CALL algo.pageRank({top: 5}) YIELD node, score RETURN node.name, score")
    });
    eprintln!("  {}", &answer[..answer.len().min(120)]);
    // Every node as a row, then the best five: the held rows meet max_memory.
    let answer = measure("GQ: algo.pageRank, every row, then LIMIT 5", || {
        gq(
            "CALL algo.pageRank() YIELD node, score RETURN node.name, score ORDER BY score DESC LIMIT 5",
        )
    });
    eprintln!("  {}", &answer[..answer.len().min(160)]);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |at: usize| -> usize { args[at].replace('_', "").parse().unwrap() };
    println!("| operation | s | steady MiB | peak MiB |");
    println!("| --- | --- | --- | --- |");
    match args.get(1).map(String::as_str) {
        Some("build") => build(&args[2], arg(3), arg(4)),
        Some("vectors") => vectors(&args[2], arg(3), arg(4)),
        Some("ops") => ops(&args[2]),
        Some("search") => search(&args[2], arg(3)),
        _ => eprintln!("usage: ram_bench build|vectors|ops DB …"),
    }
}
