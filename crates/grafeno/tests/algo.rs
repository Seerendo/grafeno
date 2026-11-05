//! Projections for graph algorithms: what they hold, and the cache that
//! read transactions share.

use std::sync::Arc;

use grafeno::algo::{self, ProjectionSpec};
use grafeno::{Direction, Grafeno, NewEdge, NewNode, NodeId, Timestamp};

const NS: &str = "g";

fn sample() -> (Grafeno, Vec<NodeId>) {
    let db = Grafeno::in_memory().unwrap();
    let ids = db
        .write(|tx| {
            let a = tx.create_node(NS, NewNode::new("Person").prop("name", "a"))?;
            let b = tx.create_node(NS, NewNode::new("Person").prop("name", "b"))?;
            let c = tx.create_node(NS, NewNode::new("Place").prop("name", "c"))?;
            tx.create_edge(NS, NewEdge::new(a, "knows", b).prop("w", 2.5))?;
            tx.create_edge(NS, NewEdge::new(b, "visits", c))?;
            // A fact that held only in 2020.
            tx.create_edge(
                NS,
                NewEdge::new(a, "visits", c)
                    .valid_from(Timestamp::parse("2020-01-01").unwrap())
                    .valid_to(Timestamp::parse("2021-01-01").unwrap()),
            )?;
            // Another namespace stays out.
            let x = tx.create_node("other", NewNode::new("Person"))?;
            tx.create_edge("other", NewEdge::new(x, "knows", x))?;
            Ok(vec![a, b, c])
        })
        .unwrap();
    (db, ids)
}

#[test]
fn projections_follow_the_spec() {
    let (db, ids) = sample();
    let read = db.begin_read().unwrap();
    let all = read.projection(NS, &ProjectionSpec::default()).unwrap();
    assert_eq!((all.len(), all.edge_count()), (3, 3));
    assert!(!all.is_weighted());
    let a = all.position(ids[0]).unwrap();
    assert_eq!(all.id(a), ids[0]);

    let people = read
        .projection(
            NS,
            &ProjectionSpec {
                labels: vec!["Person".into()],
                weight: Some("w".into()),
                ..ProjectionSpec::default()
            },
        )
        .unwrap();
    assert_eq!((people.len(), people.edge_count()), (2, 1));
    let step = people.neighbors(0, Direction::Out).next().unwrap();
    assert_eq!(step.weight, 2.5);
    assert!(people.position(ids[2]).is_none());

    let visits = ProjectionSpec {
        edge_labels: vec!["visits".into()],
        ..ProjectionSpec::default()
    };
    assert_eq!(read.projection(NS, &visits).unwrap().edge_count(), 2);
    let in_2020 = ProjectionSpec {
        valid_at: Some(Timestamp::parse("2020-06-01").unwrap()),
        ..visits.clone()
    };
    assert_eq!(read.projection(NS, &in_2020).unwrap().edge_count(), 2);
    let in_2024 = ProjectionSpec {
        valid_at: Some(Timestamp::parse("2024-06-01").unwrap()),
        ..visits
    };
    assert_eq!(read.projection(NS, &in_2024).unwrap().edge_count(), 1);

    let path = algo::shortest_path(
        &all,
        a,
        all.position(ids[2]).unwrap(),
        Direction::Out,
        &algo::never,
    )
    .unwrap()
    .unwrap();
    assert_eq!(path.nodes.len(), 2, "the direct visit");
}

#[test]
fn the_cache_lasts_until_a_commit() {
    let (db, ids) = sample();
    let spec = ProjectionSpec::default();
    let first = db.begin_read().unwrap().projection(NS, &spec).unwrap();
    let again = db.begin_read().unwrap().projection(NS, &spec).unwrap();
    assert!(Arc::ptr_eq(&first, &again), "shared by later readers");

    // A reader opened before a write keeps its snapshot's projection.
    let before = db.begin_read().unwrap();
    db.write(|tx| tx.create_edge(NS, NewEdge::new(ids[2], "near", ids[0])))
        .unwrap();
    assert!(Arc::ptr_eq(&before.projection(NS, &spec).unwrap(), &first));
    let after = db.begin_read().unwrap().projection(NS, &spec).unwrap();
    assert!(!Arc::ptr_eq(&after, &first));
    assert_eq!(after.edge_count(), 4);

    // An aborted write changes nothing, and a write transaction sees its
    // own changes.
    let tx = db.begin_write().unwrap();
    tx.abort().unwrap();
    let mut tx = db.begin_write().unwrap();
    tx.create_edge(NS, NewEdge::new(ids[1], "near", ids[0]))
        .unwrap();
    assert_eq!(tx.projection(NS, &spec).unwrap().edge_count(), 5);
    drop(tx);
    assert_eq!(
        db.begin_read()
            .unwrap()
            .projection(NS, &spec)
            .unwrap()
            .edge_count(),
        4
    );
}

#[test]
fn edge_ids_only_when_asked_and_the_cache_is_bounded_by_bytes() {
    let (db, ids) = sample();
    let read = db.begin_read().unwrap();
    let plain = read.projection(NS, &ProjectionSpec::default()).unwrap();
    assert!(!plain.has_edge_ids());
    assert_eq!(plain.edge_id(0), None);
    let with_ids = read
        .projection(
            NS,
            &ProjectionSpec {
                edge_ids: true,
                ..ProjectionSpec::default()
            },
        )
        .unwrap();
    assert!(with_ids.has_edge_ids());
    assert!(with_ids.bytes() > plain.bytes());
    // Every out-edge of a has its id, in both directions.
    let a = with_ids.position(ids[0]).unwrap();
    let b = with_ids.position(ids[1]).unwrap();
    let out: Vec<_> = with_ids.neighbors(a, Direction::Out).collect();
    let back = with_ids
        .neighbors(b, Direction::In)
        .find(|step| step.node == a)
        .unwrap();
    assert!(out.iter().any(|step| step.edge == back.edge));
    assert!(with_ids.edge_id(back.edge).is_some());
    // Cached: the same projection comes back.
    assert!(Arc::ptr_eq(
        &plain,
        &read.projection(NS, &ProjectionSpec::default()).unwrap()
    ));
    assert_eq!(
        db.projection_cache_bytes(),
        plain.bytes() + with_ids.bytes()
    );
    drop(read);

    // With no room for projections, none is kept.
    let path = std::env::temp_dir().join(format!("grafeno-budget-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let small = Grafeno::open_with(
        &path,
        &grafeno::OpenOptions {
            projection_cache_bytes: 0,
            ..grafeno::OpenOptions::default()
        },
    )
    .unwrap();
    small
        .write(|tx| tx.create_node(NS, NewNode::new("Person")).map(|_| ()))
        .unwrap();
    let read = small.begin_read().unwrap();
    let first = read.projection(NS, &ProjectionSpec::default()).unwrap();
    let second = read.projection(NS, &ProjectionSpec::default()).unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(small.projection_cache_bytes(), 0);
    drop((read, small));
    let _ = std::fs::remove_file(&path);
}
