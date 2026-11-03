use grafeno::{Direction, EdgeFilter, Grafeno, NewEdge, NewNode, NodeId, Timestamp};
use grafeno_memory::{
    ARCHIVED, ConsolidationOptions, DISTINCT_FROM, DecayOptions, DuplicateOptions, RecallQuery,
    Signal, Summary, archive, consolidation_groups, find_duplicates, recall, weak_memories,
    write_summary,
};

const NS: &str = "agent";

fn person(db: &Grafeno, name: &str) -> NodeId {
    db.write(|tx| tx.create_node(NS, NewNode::new("Person").prop("name", name)))
        .unwrap()
}

#[test]
fn duplicates_by_name() {
    let db = Grafeno::in_memory().unwrap();
    let ana = person(&db, "Ana García");
    let ana2 = person(&db, "ana  garcia");
    let ana3 = person(&db, "Ana García López");
    person(&db, "Bob Smith");
    // Same name, but a company: not a duplicate of the person.
    db.write(|tx| tx.create_node(NS, NewNode::new("Company").prop("name", "Ana García")))
        .unwrap();

    let read = db.begin_read().unwrap();
    let found = find_duplicates(&read, NS, &DuplicateOptions::default()).unwrap();
    let pairs: Vec<(NodeId, NodeId, f32)> =
        found.iter().map(|d| (d.a.id, d.b.id, d.score)).collect();
    let has = |x: NodeId, y: NodeId| {
        pairs
            .iter()
            .find(|(a, b, _)| (*a == x && *b == y) || (*a == y && *b == x))
            .map(|p| p.2)
    };
    assert_eq!(has(ana, ana2), Some(1.0));
    assert!(
        has(ana, ana3).is_some_and(|s| (0.6..1.0).contains(&s)),
        "{pairs:?}"
    );
    assert_eq!(found[0].score, 1.0, "best first");
    assert!(found[0].reasons[0].starts_with("same name"));
    assert_eq!(pairs.len(), 3, "no person-company pair, no Bob: {pairs:?}");
    drop(read);

    // Marked as different: not proposed again.
    db.write(|tx| tx.create_edge(NS, NewEdge::new(ana, DISTINCT_FROM, ana3)))
        .unwrap();
    let read = db.begin_read().unwrap();
    let found = find_duplicates(&read, NS, &DuplicateOptions::default()).unwrap();
    assert!(
        found
            .iter()
            .all(|d| !(d.a.id == ana && d.b.id == ana3 || d.a.id == ana3 && d.b.id == ana))
    );
}

#[test]
fn duplicates_by_meaning() {
    let db = Grafeno::in_memory().unwrap();
    let (a, b) = db
        .write(|tx| {
            let a = tx.create_node(
                NS,
                NewNode::new("Fact")
                    .text("prefers tea")
                    .embedding(vec![1.0, 0.0, 0.01]),
            )?;
            let b = tx.create_node(
                NS,
                NewNode::new("Fact")
                    .text("likes tea")
                    .embedding(vec![1.0, 0.02, 0.0]),
            )?;
            tx.create_node(
                NS,
                NewNode::new("Fact")
                    .text("hates noise")
                    .embedding(vec![0.0, 1.0, 0.0]),
            )?;
            Ok((a, b))
        })
        .unwrap();
    let read = db.begin_read().unwrap();
    let found = find_duplicates(&read, NS, &DuplicateOptions::default()).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(
        [found[0].a.id, found[0].b.id]
            .iter()
            .filter(|id| **id == a || **id == b)
            .count(),
        2
    );
    assert!(found[0].reasons[0].starts_with("similar meaning"));
    let without = DuplicateOptions {
        vector_threshold: None,
        ..DuplicateOptions::default()
    };
    assert!(find_duplicates(&read, NS, &without).unwrap().is_empty());
}

#[test]
fn recall_weighs_importance_and_skips_archived() {
    let db = Grafeno::in_memory().unwrap();
    let (low, high, old) = db
        .write(|tx| {
            let low =
                tx.create_node(NS, NewNode::new("Memory").text("the user likes green tea"))?;
            let high =
                tx.create_node(NS, NewNode::new("Memory").text("the user likes black tea"))?;
            let old = tx.create_node(NS, NewNode::new("Memory").text("the user liked tea once"))?;
            tx.set_importance(NS, low, 0.1)?;
            tx.set_importance(NS, high, 1.0)?;
            Ok((low, high, old))
        })
        .unwrap();
    db.write(|tx| archive(tx, NS, &[old]).map(|_| ())).unwrap();

    let read = db.begin_read().unwrap();
    let query = RecallQuery::new().text("likes tea").expand(None);
    let results = recall(&read, NS, &query).unwrap();
    let ids: Vec<NodeId> = results.iter().map(|r| r.node.id).collect();
    assert_eq!(ids, vec![high, low], "important first, archived left out");
    assert!(
        results[0]
            .signals
            .iter()
            .any(|s| matches!(s, Signal::Memory { importance, .. } if *importance == 1.0))
    );

    let all = recall(
        &read,
        NS,
        &RecallQuery {
            include_archived: true,
            ..query.clone()
        },
    )
    .unwrap();
    assert_eq!(all.len(), 3);
    let plain = recall(
        &read,
        NS,
        &RecallQuery {
            ranking: None,
            ..query
        },
    )
    .unwrap();
    assert!(
        plain
            .iter()
            .all(|r| !r.signals.iter().any(|s| matches!(s, Signal::Memory { .. })))
    );
}

#[test]
fn weak_memories_and_archiving() {
    let db = Grafeno::in_memory().unwrap();
    let (keep, fade) = db
        .write(|tx| {
            let keep = tx.create_node(NS, NewNode::new("Memory").text("birthday is in May"))?;
            let fade = tx.create_node(NS, NewNode::new("Memory").text("ate pasta on a Tuesday"))?;
            tx.set_importance(NS, keep, 0.9)?;
            tx.set_importance(NS, fade, 0.2)?;
            Ok((keep, fade))
        })
        .unwrap();
    let read = db.begin_read().unwrap();
    // A year later, with a 180-day half-life, recency is about 0.25: 0.9 →
    // 0.22 stays, 0.2 → 0.05 falls below 0.1.
    let later = Timestamp::from_micros(Timestamp::now().as_micros() + 365 * 86_400 * 1_000_000);
    let options = DecayOptions {
        now: later,
        half_life_secs: 180.0 * 86_400.0,
        threshold: 0.1,
        ..DecayOptions::default()
    };
    let weak = weak_memories(&read, NS, &options).unwrap();
    assert!(weak.iter().any(|w| w.node.id == fade));
    assert!(
        weak.iter().all(|w| w.node.id != keep),
        "importance 0.9 decays slower than the threshold"
    );
    assert!(
        weak_memories(&read, NS, &DecayOptions::default())
            .unwrap()
            .is_empty(),
        "nothing is weak today"
    );
    drop(read);

    assert_eq!(db.write(|tx| archive(tx, NS, &[fade, fade])).unwrap(), 1);
    let read = db.begin_read().unwrap();
    assert!(read.node(NS, fade).unwrap().unwrap().has_label(ARCHIVED));
    assert!(
        weak_memories(&read, NS, &options)
            .unwrap()
            .iter()
            .all(|w| w.node.id != fade)
    );
}

#[test]
fn consolidation_groups_and_summaries() {
    let db = Grafeno::in_memory().unwrap();
    let (ana, memories) = db
        .write(|tx| {
            let ana = tx.create_node(NS, NewNode::new("Person").prop("name", "Ana"))?;
            let mut memories = Vec::new();
            for text in ["Ana likes tea", "Ana moved to Lima", "Ana started at Acme"] {
                let id = tx.create_node(NS, NewNode::new("Memory").text(text))?;
                tx.create_edge(NS, NewEdge::new(id, "about", ana))?;
                memories.push(id);
            }
            tx.set_importance(NS, memories[1], 0.8)?;
            // Two memories about Bob: fewer than the minimum.
            let bob = tx.create_node(NS, NewNode::new("Person").prop("name", "Bob"))?;
            for text in ["Bob likes coffee", "Bob plays chess"] {
                let id = tx.create_node(NS, NewNode::new("Memory").text(text))?;
                tx.create_edge(NS, NewEdge::new(id, "about", bob))?;
            }
            Ok((ana, memories))
        })
        .unwrap();

    let read = db.begin_read().unwrap();
    let in_a_month = Timestamp::from_micros(Timestamp::now().as_micros() + 30 * 86_400 * 1_000_000);
    let options = ConsolidationOptions {
        now: in_a_month,
        ..ConsolidationOptions::default()
    };
    let groups = consolidation_groups(&read, NS, &options).unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].entity.id, ana);
    assert_eq!(groups[0].memories.len(), 3);
    assert!(
        consolidation_groups(&read, NS, &ConsolidationOptions::default())
            .unwrap()
            .is_empty(),
        "too recent"
    );
    drop(read);

    let summary = db
        .write(|tx| {
            write_summary(
                tx,
                NS,
                Summary {
                    text: "Ana likes tea, lives in Lima and works at Acme.".into(),
                    sources: memories.clone(),
                    about: Some(ana),
                    archive_sources: true,
                    ..Summary::default()
                },
            )
        })
        .unwrap();

    let read = db.begin_read().unwrap();
    let node = read.node(NS, summary).unwrap().unwrap();
    assert_eq!(node.labels, ["Summary"]);
    assert_eq!(node.sources, memories);
    assert_eq!(read.memory(NS, &node).unwrap().importance, 0.8);
    let summarized = read
        .neighbors(
            NS,
            summary,
            Direction::Out,
            &EdgeFilter::any().label("summarizes"),
        )
        .unwrap();
    assert_eq!(summarized.len(), 3);
    assert!(
        memories
            .iter()
            .all(|id| read.node(NS, *id).unwrap().unwrap().has_label(ARCHIVED))
    );
    // Recall now finds the summary instead of the archived memories.
    let results = recall(&read, NS, &RecallQuery::new().text("Lima").expand(None)).unwrap();
    assert_eq!(
        results.iter().map(|r| r.node.id).collect::<Vec<_>>(),
        vec![summary]
    );
    assert!(
        consolidation_groups(&read, NS, &options)
            .unwrap()
            .is_empty(),
        "archived memories are not grouped again"
    );
    assert!(read.check(NS).unwrap().is_ok());
}
