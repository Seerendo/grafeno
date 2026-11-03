use std::collections::HashSet;

use grafeno::{Error, Grafeno, NewNode, NodeId};

const NS: &str = "test";

/// Deterministic pseudo-random numbers (xorshift64*) for reproducible tests.
struct Rng(u64);

impl Rng {
    fn next_f32(&mut self) -> f32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let value = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (value >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    }

    fn vector(&mut self, dimension: usize) -> Vec<f32> {
        (0..dimension).map(|_| self.next_f32()).collect()
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (norm(a) * norm(b))
}

fn exact_top(points: &[(NodeId, Vec<f32>)], query: &[f32], k: usize) -> Vec<NodeId> {
    let mut scored: Vec<(f32, NodeId)> = points
        .iter()
        .map(|(id, v)| (cosine(query, v), *id))
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored.into_iter().take(k).map(|(_, id)| id).collect()
}

fn recall(db: &Grafeno, points: &[(NodeId, Vec<f32>)], rng: &mut Rng, dimension: usize) -> f32 {
    let read = db.begin_read().unwrap();
    let (queries, k) = (50, 10);
    let mut found = 0;
    for _ in 0..queries {
        let query = rng.vector(dimension);
        let expected: HashSet<NodeId> = exact_top(points, &query, k).into_iter().collect();
        let hits = read.search_vector(NS, &query, k).unwrap();
        found += hits
            .iter()
            .filter(|hit| expected.contains(&hit.node))
            .count();
    }
    found as f32 / (queries * k) as f32
}

#[test]
fn text_search_ranks_by_bm25() {
    let db = Grafeno::in_memory().unwrap();
    let (rust, graphs, both) = db
        .write(|tx| {
            let rust = tx.create_node(
                NS,
                NewNode::new("Note").text("Rust ownership and borrowing in Rust programs"),
            )?;
            let graphs = tx.create_node(
                NS,
                NewNode::new("Note").text("Graph databases store nodes and edges"),
            )?;
            let both = tx.create_node(
                NS,
                NewNode::new("Note").text("A graph engine written in Rust"),
            )?;
            tx.create_node(NS, NewNode::new("Note").text("Cooking pasta at home"))?;
            Ok((rust, graphs, both))
        })
        .unwrap();

    let read = db.begin_read().unwrap();
    let hits = read.search_text(NS, "rust", 10).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].node, rust, "two occurrences rank first");

    let hits = read.search_text(NS, "graph rust", 10).unwrap();
    assert_eq!(hits[0].node, both, "matching both terms ranks first");
    let graphs_rank = hits.iter().position(|hit| hit.node == graphs).unwrap();
    assert!(graphs_rank > 0, "matching one term ranks lower");

    assert!(read.search_text(NS, "the and of", 10).unwrap().is_empty());
    assert!(read.search_text("other", "rust", 10).unwrap().is_empty());
}

#[test]
fn text_search_ignores_case_and_accents() {
    let db = Grafeno::in_memory().unwrap();
    let id = db
        .write(|tx| tx.create_node(NS, NewNode::new("Note").text("La Canción del Pirata")))
        .unwrap();
    let read = db.begin_read().unwrap();
    assert_eq!(read.search_text(NS, "cancion", 5).unwrap()[0].node, id);
    assert_eq!(read.search_text(NS, "PIRATA", 5).unwrap()[0].node, id);
}

#[test]
fn replacing_and_removing_text_updates_the_index() {
    let db = Grafeno::in_memory().unwrap();
    let id = db
        .write(|tx| tx.create_node(NS, NewNode::new("Note").text("old words")))
        .unwrap();
    db.write(|tx| tx.set_text(NS, id, "new words")).unwrap();
    {
        let read = db.begin_read().unwrap();
        assert!(read.search_text(NS, "old", 5).unwrap().is_empty());
        assert_eq!(read.search_text(NS, "new", 5).unwrap()[0].node, id);
        assert_eq!(read.text(NS, id).unwrap().as_deref(), Some("new words"));
    }
    assert!(db.write(|tx| tx.remove_text(NS, id)).unwrap());
    assert!(!db.write(|tx| tx.remove_text(NS, id)).unwrap());
    let read = db.begin_read().unwrap();
    assert!(read.search_text(NS, "words", 5).unwrap().is_empty());
    assert!(read.text(NS, id).unwrap().is_none());
}

#[test]
fn deleting_a_node_removes_it_from_both_indexes() {
    let db = Grafeno::in_memory().unwrap();
    let (keep, gone) = db
        .write(|tx| {
            let keep = tx.create_node(
                NS,
                NewNode::new("Note")
                    .text("shared word")
                    .embedding(vec![1.0, 0.0]),
            )?;
            let gone = tx.create_node(
                NS,
                NewNode::new("Note")
                    .text("shared word")
                    .embedding(vec![0.9, 0.1]),
            )?;
            Ok((keep, gone))
        })
        .unwrap();
    db.write(|tx| tx.delete_node(NS, gone)).unwrap();

    let read = db.begin_read().unwrap();
    let text: Vec<NodeId> = read
        .search_text(NS, "shared", 5)
        .unwrap()
        .into_iter()
        .map(|hit| hit.node)
        .collect();
    assert_eq!(text, vec![keep]);
    let vectors: Vec<NodeId> = read
        .search_vector(NS, &[1.0, 0.0], 5)
        .unwrap()
        .into_iter()
        .map(|hit| hit.node)
        .collect();
    assert_eq!(vectors, vec![keep]);
}

#[test]
fn vector_dimension_is_fixed_per_namespace() {
    let db = Grafeno::in_memory().unwrap();
    db.write(|tx| tx.create_node(NS, NewNode::new("Note").embedding(vec![1.0, 0.0, 0.0])))
        .unwrap();
    let result = db.write(|tx| tx.create_node(NS, NewNode::new("Note").embedding(vec![1.0, 0.0])));
    assert!(matches!(result, Err(Error::Invalid(_))));
    let result =
        db.write(|tx| tx.create_node(NS, NewNode::new("Note").embedding(vec![0.0, 0.0, 0.0])));
    assert!(matches!(result, Err(Error::Invalid(_))));
    // Another namespace has its own dimension.
    db.write(|tx| tx.create_node("other", NewNode::new("Note").embedding(vec![1.0, 0.0])))
        .unwrap();
    let read = db.begin_read().unwrap();
    assert!(matches!(
        read.search_vector(NS, &[1.0, 0.0], 1),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn hnsw_finds_the_true_nearest_neighbors() {
    let dimension = 24;
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let db = Grafeno::in_memory().unwrap();
    let mut points = Vec::new();
    // Batches of writes, as an application would do.
    for _ in 0..10 {
        let batch: Vec<Vec<f32>> = (0..150).map(|_| rng.vector(dimension)).collect();
        let ids = db
            .write(|tx| {
                batch
                    .iter()
                    .map(|v| tx.create_node(NS, NewNode::new("Point").embedding(v.clone())))
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap();
        points.extend(ids.into_iter().zip(batch));
    }

    let before = recall(&db, &points, &mut rng, dimension);
    assert!(before >= 0.9, "recall@10 = {before}");

    // Remove half of the points; the rest must stay reachable.
    let removed: Vec<NodeId> = points.iter().step_by(2).map(|(id, _)| *id).collect();
    db.write(|tx| removed.iter().try_for_each(|id| tx.delete_node(NS, *id)))
        .unwrap();
    let removed: HashSet<NodeId> = removed.into_iter().collect();
    points.retain(|(id, _)| !removed.contains(id));

    let read = db.begin_read().unwrap();
    for _ in 0..20 {
        let hits = read.search_vector(NS, &rng.vector(dimension), 10).unwrap();
        assert_eq!(hits.len(), 10);
        assert!(hits.iter().all(|hit| !removed.contains(&hit.node)));
    }
    drop(read);
    let after = recall(&db, &points, &mut rng, dimension);
    assert!(after >= 0.9, "recall@10 after deletes = {after}");
}
