use grafeno::{Grafeno, NewEdge, NewNode, NodeId, Timestamp};
use grafeno_memory::{
    ContextOptions, Expansion, RecallQuery, Signal, context_for, estimate_tokens, recall,
};

const NS: &str = "agent";

/// A tiny knowledge base. Embeddings are hand-made 3-dimensional vectors:
/// x = programming, y = cooking, z = travel.
struct Fixture {
    db: Grafeno,
    rust_note: NodeId,
    borrow_note: NodeId,
    pasta_note: NodeId,
    ana: NodeId,
    acme: NodeId,
}

fn fixture() -> Fixture {
    let db = Grafeno::in_memory().unwrap();
    let (rust_note, borrow_note, pasta_note, ana, acme) = db
        .write(|tx| {
            let rust_note = tx.create_node(
                NS,
                NewNode::new("Note")
                    .prop("title", "Rust notes")
                    .text("Rust ownership rules and the borrow checker")
                    .embedding(vec![0.9, 0.1, 0.0]),
            )?;
            // Semantically close to programming, but shares no words with the query.
            let borrow_note = tx.create_node(
                NS,
                NewNode::new("Note")
                    .prop("title", "Lifetimes")
                    .text("References must not outlive the data they point to")
                    .embedding(vec![0.95, 0.0, 0.05]),
            )?;
            let pasta_note = tx.create_node(
                NS,
                NewNode::new("Note")
                    .prop("title", "Pasta")
                    .text("Boil the pasta in salted water")
                    .embedding(vec![0.0, 1.0, 0.0]),
            )?;
            // No text and no embedding: only reachable through the graph.
            let ana = tx.create_node(NS, NewNode::new("Person").prop("name", "Ana"))?;
            let acme = tx.create_node(NS, NewNode::new("Company").prop("name", "Acme"))?;
            tx.create_edge(NS, NewEdge::new(ana, "wrote", rust_note))?;
            tx.create_edge(
                NS,
                NewEdge::new(ana, "works_at", acme).valid_from(Timestamp::from_secs(1_704_067_200)),
            )?;
            Ok((rust_note, borrow_note, pasta_note, ana, acme))
        })
        .unwrap();
    Fixture {
        db,
        rust_note,
        borrow_note,
        pasta_note,
        ana,
        acme,
    }
}

fn ids(results: &[grafeno_memory::Recalled]) -> Vec<NodeId> {
    results.iter().map(|r| r.node.id).collect()
}

#[test]
fn hybrid_recall_fuses_text_and_vectors() {
    let f = fixture();
    let read = f.db.begin_read().unwrap();
    let query = RecallQuery::new()
        .text("borrow checker")
        .embedding(vec![1.0, 0.0, 0.0])
        .expand(None);
    let results = recall(&read, NS, &query).unwrap();
    let found = ids(&results);

    assert_eq!(found[0], f.rust_note, "matches both text and vector");
    assert!(
        found.contains(&f.borrow_note),
        "vector-only match is recalled"
    );
    let top = &results[0];
    assert!(
        top.signals
            .iter()
            .any(|s| matches!(s, Signal::Text { rank: 1, .. }))
    );
    assert!(
        top.signals
            .iter()
            .any(|s| matches!(s, Signal::Vector { .. }))
    );
    let pasta = found.iter().position(|id| *id == f.pasta_note);
    assert!(
        pasta.is_none_or(|rank| rank > 1),
        "unrelated note ranks last"
    );
}

#[test]
fn graph_expansion_reaches_nodes_without_text() {
    let f = fixture();
    let read = f.db.begin_read().unwrap();

    let without = recall(
        &read,
        NS,
        &RecallQuery::new().text("ownership").expand(None),
    )
    .unwrap();
    assert!(!ids(&without).contains(&f.ana));

    let with = recall(&read, NS, &RecallQuery::new().text("ownership")).unwrap();
    let ana = with
        .iter()
        .find(|r| r.node.id == f.ana)
        .expect("Ana is recalled");
    assert!(ana.signals.contains(&Signal::Graph {
        seed: f.rust_note,
        hops: 1
    }));
    assert!(ana.score < with[0].score);

    let two_hops = RecallQuery::new().text("ownership").expand(Some(Expansion {
        depth: 2,
        ..Expansion::default()
    }));
    assert!(ids(&recall(&read, NS, &two_hops).unwrap()).contains(&f.acme));
}

#[test]
fn recall_filters_by_label() {
    let f = fixture();
    let read = f.db.begin_read().unwrap();
    let results = recall(
        &read,
        NS,
        &RecallQuery::new().text("ownership").label("Person"),
    )
    .unwrap();
    assert_eq!(ids(&results), vec![f.ana]);
}

#[test]
fn context_includes_items_and_facts() {
    let f = fixture();
    let read = f.db.begin_read().unwrap();
    let query = RecallQuery::new().text("ownership").expand(Some(Expansion {
        depth: 2,
        ..Expansion::default()
    }));
    let context = context_for(&read, NS, &query, &ContextOptions::with_budget(1_000)).unwrap();

    assert_eq!(context.items[0].node, f.rust_note);
    assert_eq!(context.items[0].name, "Rust notes");
    assert!(
        context
            .rendered
            .starts_with("## Memory\n[1] Rust notes (Note)\nRust ownership")
    );
    let facts: Vec<&str> = context
        .facts
        .iter()
        .map(|fact| fact.text.as_str())
        .collect();
    assert!(facts.contains(&"Ana wrote Rust notes"), "{facts:?}");
    assert!(
        facts.contains(&"Ana works_at Acme (from 2024-01-01)"),
        "{facts:?}"
    );
    assert!(context.rendered.contains("\n## Facts\n- "));
    assert!(context.tokens <= 1_000);
}

#[test]
fn context_respects_the_token_budget() {
    let db = Grafeno::in_memory().unwrap();
    let long = "memory ".repeat(400);
    db.write(|tx| {
        for index in 0..10 {
            tx.create_node(
                NS,
                NewNode::new("Note")
                    .prop("title", format!("Note {index}"))
                    .text(format!("{long} {index}")),
            )?;
        }
        Ok(())
    })
    .unwrap();

    let read = db.begin_read().unwrap();
    for budget in [60, 300, 1_200] {
        let options = ContextOptions::with_budget(budget);
        let context = context_for(&read, NS, &RecallQuery::new().text("memory"), &options).unwrap();
        assert!(!context.items.is_empty(), "budget {budget}");
        assert!(context.tokens <= budget, "{} > {budget}", context.tokens);
        assert!(
            estimate_tokens(&context.rendered)
                <= budget + context.items.len() + context.facts.len() + 2,
            "rendered text roughly matches the counted tokens"
        );
        let last = context.items.last().unwrap();
        assert!(last.truncated && last.text.ends_with('…'));
    }
}

#[test]
fn retracted_facts_are_left_out() {
    let f = fixture();
    let edge =
        f.db.begin_read()
            .unwrap()
            .neighbors(
                NS,
                f.ana,
                grafeno::Direction::Out,
                &grafeno::EdgeFilter::any().label("works_at"),
            )
            .unwrap()[0]
            .edge
            .id;
    f.db.write(|tx| tx.retract_edge(NS, edge)).unwrap();

    let read = f.db.begin_read().unwrap();
    let query = RecallQuery::new().text("ownership");
    let context = context_for(&read, NS, &query, &ContextOptions::default()).unwrap();
    assert!(context.facts.iter().all(|fact| fact.label != "works_at"));
}
