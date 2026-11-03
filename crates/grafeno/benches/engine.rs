//! Engine benchmarks: `cargo bench -p grafeno`.
//!
//! Every database is in memory, so the numbers measure the engine (redb
//! included) without disk syncs. Durable commits to a file add the cost of
//! an fsync per transaction.

use std::hint::black_box;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use grafeno::{Direction, EdgeFilter, Grafeno, NewEdge, NewNode, NodeId};

const NS: &str = "bench";
const VOCABULARY: usize = 2_000;

/// Deterministic pseudo-random numbers (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    }

    /// A sentence of `words` words with a skewed (Zipf-like) vocabulary.
    fn sentence(&mut self, words: usize) -> String {
        (0..words)
            .map(|_| {
                let rank = self.below(VOCABULARY).min(self.below(VOCABULARY));
                format!("w{rank}")
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn vector(&mut self, dimension: usize) -> Vec<f32> {
        (0..dimension).map(|_| self.unit()).collect()
    }
}

/// A database with `nodes` notes (text of 60 words, optionally a vector) and
/// about four random links per note.
fn corpus(nodes: usize, dimension: Option<usize>) -> (Grafeno, Vec<NodeId>) {
    let db = Grafeno::in_memory().unwrap();
    let mut rng = Rng(42);
    let mut ids = Vec::with_capacity(nodes);
    for chunk in (0..nodes).collect::<Vec<_>>().chunks(500) {
        let new: Vec<NewNode> = chunk
            .iter()
            .map(|_| {
                let mut node = NewNode::new("Note").text(rng.sentence(60));
                node.embedding = dimension.map(|d| rng.vector(d));
                node
            })
            .collect();
        ids.extend(
            db.write(|tx| {
                new.into_iter()
                    .map(|n| tx.create_node(NS, n))
                    .collect::<grafeno::Result<Vec<_>>>()
            })
            .unwrap(),
        );
    }
    let links: Vec<(NodeId, NodeId)> = (0..nodes * 4)
        .map(|_| (ids[rng.below(nodes)], ids[rng.below(nodes)]))
        .collect();
    for chunk in links.chunks(2_000) {
        db.write(|tx| {
            for &(from, to) in chunk {
                tx.create_edge(NS, NewEdge::new(from, "links_to", to))?;
            }
            Ok(())
        })
        .unwrap();
    }
    (db, ids)
}

fn writes(c: &mut Criterion) {
    let mut group = c.benchmark_group("write");
    group.sample_size(20);
    let mut rng = Rng(7);
    group.bench_function("100 nodes with text, one transaction", |b| {
        b.iter_batched(
            || {
                let db = Grafeno::in_memory().unwrap();
                let texts: Vec<String> = (0..100).map(|_| rng.sentence(60)).collect();
                (db, texts)
            },
            |(db, texts)| {
                db.write(|tx| {
                    for text in texts {
                        tx.create_node(NS, NewNode::new("Note").text(text))?;
                    }
                    Ok(())
                })
                .unwrap();
            },
            BatchSize::SmallInput,
        )
    });
    let (db, ids) = corpus(2_000, Some(64));
    group.bench_function("1 node with text and a 64-d vector into 2k", |b| {
        b.iter(|| {
            let node = NewNode::new("Note")
                .text(rng.sentence(60))
                .embedding(rng.vector(64));
            db.write(|tx| tx.create_node(NS, node)).unwrap()
        })
    });
    group.bench_function("1 edge", |b| {
        b.iter(|| {
            let (from, to) = (ids[rng.below(ids.len())], ids[rng.below(ids.len())]);
            db.write(|tx| tx.create_edge(NS, NewEdge::new(from, "links_to", to)))
                .unwrap()
        })
    });
    group.finish();
}

fn reads(c: &mut Criterion) {
    let (db, ids) = corpus(10_000, None);
    let read = db.begin_read().unwrap();
    let mut rng = Rng(9);
    let any = EdgeFilter::any();
    let mut group = c.benchmark_group("read 10k nodes, 40k edges");
    group.bench_function("get node", |b| {
        b.iter(|| read.node(NS, ids[rng.below(ids.len())]).unwrap())
    });
    group.bench_function("neighbors (both directions)", |b| {
        b.iter(|| {
            read.neighbors(NS, ids[rng.below(ids.len())], Direction::Both, &any)
                .unwrap()
        })
    });
    group.bench_function("k-hop depth 2", |b| {
        b.iter(|| {
            read.k_hop(NS, ids[rng.below(ids.len())], 2, Direction::Out, &any)
                .unwrap()
        })
    });
    group.bench_function("shortest path (max 6)", |b| {
        b.iter(|| {
            let (from, to) = (ids[rng.below(ids.len())], ids[rng.below(ids.len())]);
            read.shortest_path(NS, from, to, 6, Direction::Out, &any)
                .unwrap()
        })
    });
    group.bench_function("BM25, 3-word query, top 10", |b| {
        b.iter(|| {
            read.search_text(NS, black_box(&rng.sentence(3)), 10)
                .unwrap()
        })
    });
    group.finish();
}

fn vectors(c: &mut Criterion) {
    let (db, _) = corpus(10_000, Some(128));
    let read = db.begin_read().unwrap();
    let mut rng = Rng(11);
    let mut group = c.benchmark_group("vectors 10k x 128-d");
    for ef in [32, 64, 128] {
        group.bench_function(format!("HNSW top 10, ef {ef}"), |b| {
            b.iter(|| {
                read.search_vector_with(NS, &rng.vector(128), 10, ef)
                    .unwrap()
            })
        });
    }
    group.finish();
}

criterion_group!(benches, writes, reads, vectors);
criterion_main!(benches);
