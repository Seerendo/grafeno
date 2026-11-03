//! Property test: random sequences of writes, applied both to Grafeno and to
//! a plain in-memory model, must leave them in the same state, with every
//! index consistent and every search complete. Export/import and rebuild
//! must preserve that state.

use std::collections::{BTreeMap, BTreeSet};

use grafeno::{Direction, EdgeFilter, EdgeId, Error, Grafeno, NewEdge, NewNode, NodeId, tokenize};
use proptest::prelude::*;

const NS: &str = "model";
const WORDS: &[&str] = &[
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel",
];
const LABELS: &[&str] = &["A", "B", "C"];
const EDGE_LABELS: &[&str] = &["x", "y"];

#[derive(Clone, Debug)]
enum Op {
    Create {
        label: usize,
        text: Option<Vec<usize>>,
        vector: Option<[i8; 3]>,
    },
    Upsert {
        slot: usize,
        label: usize,
        text: Option<Vec<usize>>,
    },
    Delete {
        slot: usize,
    },
    SetText {
        slot: usize,
        text: Vec<usize>,
    },
    RemoveText {
        slot: usize,
    },
    SetVector {
        slot: usize,
        vector: [i8; 3],
    },
    RemoveVector {
        slot: usize,
    },
    AddLabel {
        slot: usize,
        label: usize,
    },
    RemoveLabel {
        slot: usize,
        label: usize,
    },
    Link {
        from: usize,
        to: usize,
        label: usize,
    },
    Unlink {
        slot: usize,
    },
    Retract {
        slot: usize,
    },
    UnlinkFrom {
        slot: usize,
        label: Option<usize>,
    },
    /// Applies the operation, then aborts the transaction.
    Aborted(Box<Op>),
}

fn words() -> impl Strategy<Value = Vec<usize>> {
    prop::collection::vec(0..WORDS.len(), 0..5)
}

fn vector() -> impl Strategy<Value = [i8; 3]> {
    prop::array::uniform3(-3i8..=3)
}

fn op() -> impl Strategy<Value = Op> {
    let slot = 0..64usize;
    let simple = prop_oneof![
        4 => (0..LABELS.len(), prop::option::of(words()), prop::option::of(vector()))
            .prop_map(|(label, text, vector)| Op::Create { label, text, vector }),
        1 => (slot.clone(), 0..LABELS.len(), prop::option::of(words()))
            .prop_map(|(slot, label, text)| Op::Upsert { slot, label, text }),
        1 => slot.clone().prop_map(|slot| Op::Delete { slot }),
        1 => (slot.clone(), words()).prop_map(|(slot, text)| Op::SetText { slot, text }),
        1 => slot.clone().prop_map(|slot| Op::RemoveText { slot }),
        2 => (slot.clone(), vector()).prop_map(|(slot, vector)| Op::SetVector { slot, vector }),
        1 => slot.clone().prop_map(|slot| Op::RemoveVector { slot }),
        1 => (slot.clone(), 0..LABELS.len()).prop_map(|(slot, label)| Op::AddLabel { slot, label }),
        1 => (slot.clone(), 0..LABELS.len()).prop_map(|(slot, label)| Op::RemoveLabel { slot, label }),
        3 => (slot.clone(), slot.clone(), 0..EDGE_LABELS.len())
            .prop_map(|(from, to, label)| Op::Link { from, to, label }),
        1 => slot.clone().prop_map(|slot| Op::Unlink { slot }),
        1 => slot.clone().prop_map(|slot| Op::Retract { slot }),
        1 => (slot, prop::option::of(0..EDGE_LABELS.len()))
            .prop_map(|(slot, label)| Op::UnlinkFrom { slot, label }),
    ];
    prop_oneof![9 => simple.clone(), 1 => simple.prop_map(|op| Op::Aborted(Box::new(op)))]
}

#[derive(Clone, Debug, PartialEq)]
struct ModelNode {
    labels: BTreeSet<String>,
    text: Option<String>,
    has_vector: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct ModelEdge {
    from: NodeId,
    to: NodeId,
    label: String,
    retracted: bool,
}

#[derive(Clone, Debug, Default)]
struct Model {
    nodes: BTreeMap<NodeId, ModelNode>,
    edges: BTreeMap<EdgeId, ModelEdge>,
}

fn text_of(words: &[usize]) -> String {
    words
        .iter()
        .map(|&w| WORDS[w])
        .collect::<Vec<_>>()
        .join(" ")
}

fn embedding(vector: [i8; 3]) -> Vec<f32> {
    let vector = if vector == [0, 0, 0] {
        [1, 0, 0]
    } else {
        vector
    };
    vector.iter().map(|&v| f32::from(v)).collect()
}

fn pick<T: Copy>(items: impl Iterator<Item = T>, slot: usize) -> Option<T> {
    let items: Vec<T> = items.collect();
    (!items.is_empty()).then(|| items[slot % items.len()])
}

/// Applies `op` to both. Operations that do not apply (no nodes yet, a
/// label that would leave a node without labels) are skipped.
fn apply(db: &Grafeno, model: &mut Model, op: &Op) {
    if let Op::Aborted(inner) = op {
        let mut scratch = model.clone();
        let mut tx = db.begin_write().unwrap();
        apply_in(&mut tx, &mut scratch, inner);
        tx.abort().unwrap();
        return;
    }
    let mut tx = db.begin_write().unwrap();
    apply_in(&mut tx, model, op);
    tx.commit().unwrap();
}

fn apply_in(tx: &mut grafeno::WriteTx, model: &mut Model, op: &Op) {
    let node = |slot: usize, model: &Model| pick(model.nodes.keys().copied(), slot);
    match op {
        Op::Create {
            label,
            text,
            vector,
        } => {
            let mut new = NewNode::new(LABELS[*label]);
            new.text = text.as_deref().map(text_of);
            new.embedding = vector.map(embedding);
            let id = tx.create_node(NS, new.clone()).unwrap();
            model.nodes.insert(
                id,
                ModelNode {
                    labels: BTreeSet::from([LABELS[*label].to_owned()]),
                    text: new.text,
                    has_vector: new.embedding.is_some(),
                },
            );
        }
        Op::Upsert { slot, label, text } => {
            let Some(id) = node(*slot, model) else { return };
            let mut new = NewNode::new(LABELS[*label]);
            new.text = text.as_deref().map(text_of);
            assert!(!tx.upsert_node(NS, id, new.clone()).unwrap());
            let entry = model.nodes.get_mut(&id).unwrap();
            entry.labels = BTreeSet::from([LABELS[*label].to_owned()]);
            entry.text = new.text;
            entry.has_vector = false;
        }
        Op::Delete { slot } => {
            let Some(id) = node(*slot, model) else { return };
            tx.delete_node(NS, id).unwrap();
            model.nodes.remove(&id);
            model
                .edges
                .retain(|_, edge| edge.from != id && edge.to != id);
        }
        Op::SetText { slot, text } => {
            let Some(id) = node(*slot, model) else { return };
            tx.set_text(NS, id, &text_of(text)).unwrap();
            model.nodes.get_mut(&id).unwrap().text = Some(text_of(text));
        }
        Op::RemoveText { slot } => {
            let Some(id) = node(*slot, model) else { return };
            let had = tx.remove_text(NS, id).unwrap();
            let entry = model.nodes.get_mut(&id).unwrap();
            assert_eq!(had, entry.text.is_some());
            entry.text = None;
        }
        Op::SetVector { slot, vector } => {
            let Some(id) = node(*slot, model) else { return };
            tx.set_embedding(NS, id, &embedding(*vector)).unwrap();
            model.nodes.get_mut(&id).unwrap().has_vector = true;
        }
        Op::RemoveVector { slot } => {
            let Some(id) = node(*slot, model) else { return };
            let had = tx.remove_embedding(NS, id).unwrap();
            let entry = model.nodes.get_mut(&id).unwrap();
            assert_eq!(had, entry.has_vector);
            entry.has_vector = false;
        }
        Op::AddLabel { slot, label } => {
            let Some(id) = node(*slot, model) else { return };
            tx.add_label(NS, id, LABELS[*label]).unwrap();
            model
                .nodes
                .get_mut(&id)
                .unwrap()
                .labels
                .insert(LABELS[*label].to_owned());
        }
        Op::RemoveLabel { slot, label } => {
            let Some(id) = node(*slot, model) else { return };
            let entry = model.nodes.get_mut(&id).unwrap();
            let label = LABELS[*label];
            match tx.remove_label(NS, id, label) {
                Ok(()) => {
                    entry.labels.remove(label);
                }
                Err(Error::Invalid(_)) => assert_eq!(entry.labels.len(), 1),
                Err(error) => panic!("{error}"),
            }
        }
        Op::Link { from, to, label } => {
            let (Some(from), Some(to)) = (node(*from, model), node(*to, model)) else {
                return;
            };
            let id = tx
                .create_edge(NS, NewEdge::new(from, EDGE_LABELS[*label], to))
                .unwrap();
            model.edges.insert(
                id,
                ModelEdge {
                    from,
                    to,
                    label: EDGE_LABELS[*label].to_owned(),
                    retracted: false,
                },
            );
        }
        Op::Unlink { slot } => {
            let Some(id) = pick(model.edges.keys().copied(), *slot) else {
                return;
            };
            tx.delete_edge(NS, id).unwrap();
            model.edges.remove(&id);
        }
        Op::Retract { slot } => {
            let Some(id) = pick(model.edges.keys().copied(), *slot) else {
                return;
            };
            tx.retract_edge(NS, id).unwrap();
            model.edges.get_mut(&id).unwrap().retracted = true;
        }
        Op::UnlinkFrom { slot, label } => {
            let Some(from) = node(*slot, model) else {
                return;
            };
            let label = label.map(|label| EDGE_LABELS[label]);
            let deleted = tx.delete_edges_from(NS, from, label).unwrap();
            let before = model.edges.len();
            model
                .edges
                .retain(|_, edge| !(edge.from == from && label.is_none_or(|l| edge.label == l)));
            assert_eq!(deleted, before - model.edges.len());
        }
        Op::Aborted(_) => unreachable!("nested aborts are not generated"),
    }
}

/// Checks that `db` holds exactly what `model` says, and that it is consistent.
fn verify(db: &Grafeno, model: &Model) {
    let read = db.begin_read().unwrap();
    let report = read.check(NS).unwrap();
    assert!(report.is_ok(), "{:#?}", report.problems);

    let nodes = read.nodes(NS).unwrap();
    assert_eq!(nodes.len(), model.nodes.len());
    for node in &nodes {
        let expected = &model.nodes[&node.id];
        let labels: BTreeSet<String> = node.labels.iter().cloned().collect();
        assert_eq!(labels, expected.labels);
        assert_eq!(read.text(NS, node.id).unwrap(), expected.text);
        assert_eq!(
            read.embedding(NS, node.id).unwrap().is_some(),
            expected.has_vector
        );
    }
    let edges: BTreeMap<EdgeId, ModelEdge> = read
        .edges(NS)
        .unwrap()
        .into_iter()
        .map(|edge| {
            let retracted = edge.is_retracted();
            (
                edge.id,
                ModelEdge {
                    from: edge.from,
                    to: edge.to,
                    label: edge.label,
                    retracted,
                },
            )
        })
        .collect();
    assert_eq!(edges, model.edges);

    let stats = read.stats(NS).unwrap();
    assert_eq!(
        (stats.nodes, stats.edges),
        (model.nodes.len() as u64, model.edges.len() as u64)
    );
    for label in LABELS {
        let expected = model
            .nodes
            .values()
            .filter(|n| n.labels.contains(*label))
            .count() as u64;
        assert_eq!(
            stats.node_labels.get(*label).copied().unwrap_or(0),
            expected,
            "{label}"
        );
        let page = read.nodes_page(NS, Some(label), None, 1000).unwrap();
        assert_eq!(page.len() as u64, expected);
    }

    // Current neighbors leave out retracted edges.
    for &id in model.nodes.keys() {
        let out = read
            .neighbors(NS, id, Direction::Out, &EdgeFilter::any())
            .unwrap();
        let expected = model
            .edges
            .values()
            .filter(|e| e.from == id && !e.retracted)
            .count();
        assert_eq!(out.len(), expected);
    }

    // Text search finds exactly the nodes whose text contains the word.
    for word in WORDS {
        let found: BTreeSet<NodeId> = read
            .search_text(NS, word, 10_000)
            .unwrap()
            .into_iter()
            .map(|hit| hit.node)
            .collect();
        let expected: BTreeSet<NodeId> = model
            .nodes
            .iter()
            .filter(|(_, node)| {
                node.text
                    .as_deref()
                    .is_some_and(|text| tokenize(text).iter().any(|t| t == word))
            })
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(found, expected, "search for {word:?}");
    }

    // A wide vector search returns every vector: the graph stays connected.
    let with_vectors: BTreeSet<NodeId> = model
        .nodes
        .iter()
        .filter(|(_, node)| node.has_vector)
        .map(|(id, _)| *id)
        .collect();
    if !with_vectors.is_empty() {
        for query in [[1.0, 0.0, 0.0], [-1.0, 0.5, 0.25], [0.0, 0.0, -1.0]] {
            let found: BTreeSet<NodeId> = read
                .search_vector_with(NS, &query, with_vectors.len(), 10_000)
                .unwrap()
                .into_iter()
                .map(|hit| hit.node)
                .collect();
            assert_eq!(found, with_vectors);
        }
    }
}

proptest! {
    #[test]
    fn engine_matches_the_model(ops in prop::collection::vec(op(), 1..80)) {
        let db = Grafeno::in_memory().unwrap();
        let mut model = Model::default();
        for op in &ops {
            apply(&db, &mut model, op);
        }
        verify(&db, &model);

        // Export and import into a new database: same state.
        let mut dump = Vec::new();
        db.begin_read().unwrap().export(&[], &mut dump).unwrap();
        let copy = Grafeno::in_memory().unwrap();
        copy.import(dump.as_slice()).unwrap();
        verify(&copy, &model);

        // Rebuilding the indexes: same state.
        db.write(|tx| tx.rebuild_indexes(NS)).unwrap();
        verify(&db, &model);
    }
}
