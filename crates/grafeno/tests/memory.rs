//! Memory primitives: names, importance and accesses, merging.

use grafeno::{
    DEFAULT_IMPORTANCE, Direction, EdgeFilter, Error, Grafeno, MergeOptions, NewEdge, NewNode,
    NodeId, Props, Timestamp, Value,
};

const NS: &str = "mem";

#[test]
fn nodes_are_found_by_normalized_names_and_aliases() {
    let db = Grafeno::in_memory().unwrap();
    let ana = db
        .write(|tx| {
            tx.create_node(
                NS,
                NewNode::new("Person")
                    .prop("name", "Ana García")
                    .prop("aliases", serde_json::json!(["Anita", "A. García"])),
            )
        })
        .unwrap();
    let read = db.begin_read().unwrap();
    for name in ["ana garcia", "  ANA   GARCÍA ", "Anita", "a garcia"] {
        assert_eq!(read.find_by_name(NS, name).unwrap(), vec![ana], "{name}");
    }
    assert!(read.find_by_name(NS, "Ana").unwrap().is_empty());
    assert!(read.find_by_name("other", "Anita").unwrap().is_empty());
    drop(read);

    // Renaming updates the index.
    db.write(|tx| {
        let mut props = Props::new();
        props.insert("name".into(), Value::from("Ana G. López"));
        props.insert("aliases".into(), Value::Null);
        tx.set_node_props(NS, ana, props)
    })
    .unwrap();
    let read = db.begin_read().unwrap();
    assert!(read.find_by_name(NS, "Anita").unwrap().is_empty());
    assert_eq!(read.find_by_name(NS, "ana g lopez").unwrap(), vec![ana]);
    assert!(read.check(NS).unwrap().is_ok());
    drop(read);

    db.write(|tx| tx.delete_node(NS, ana)).unwrap();
    assert!(
        db.begin_read()
            .unwrap()
            .find_by_name(NS, "ana g lopez")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn importance_and_accesses() {
    let db = Grafeno::in_memory().unwrap();
    let id = db
        .write(|tx| tx.create_node(NS, NewNode::new("Memory").text("likes tea")))
        .unwrap();
    let read = db.begin_read().unwrap();
    let node = read.node(NS, id).unwrap().unwrap();
    let state = read.memory(NS, &node).unwrap();
    assert_eq!((state.importance, state.accesses), (DEFAULT_IMPORTANCE, 0));
    assert_eq!(state.last_access, node.updated_at);
    drop(read);

    let at = Timestamp::from_secs(4_000_000_000);
    db.write(|tx| {
        tx.set_importance(NS, id, 0.9)?;
        tx.touch(NS, &[id, NodeId::new()], at)?;
        tx.touch(NS, &[id], Timestamp::from_secs(1))
    })
    .unwrap();
    let read = db.begin_read().unwrap();
    let state = read
        .memory(NS, &read.node(NS, id).unwrap().unwrap())
        .unwrap();
    assert_eq!(
        (state.importance, state.accesses, state.last_access),
        (0.9, 2, at),
        "an older access does not go back"
    );
    drop(read);
    assert!(matches!(
        db.write(|tx| tx.set_importance(NS, id, 1.5)),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn merging_moves_edges_and_joins_everything() {
    let db = Grafeno::in_memory().unwrap();
    let (keep, dup, acme, note) = db
        .write(|tx| {
            let keep = tx.create_node(
                NS,
                NewNode::new("Person")
                    .prop("name", "Ana García")
                    .prop("city", "Lima")
                    .text("Ana writes Rust")
                    .embedding(vec![1.0, 0.0]),
            )?;
            let dup = tx.create_node(
                NS,
                NewNode::new("Person")
                    .label("Engineer")
                    .prop("name", "ana garcia lopez")
                    .prop("city", "Cusco")
                    .prop("age", 34)
                    .text("Ana lives in Lima"),
            )?;
            let acme = tx.create_node(NS, NewNode::new("Company").prop("name", "Acme"))?;
            let note = tx.create_node(NS, NewNode::new("Note").prop("title", "Meeting"))?;
            tx.create_edge(NS, NewEdge::new(keep, "works_at", acme))?;
            tx.create_edge(NS, NewEdge::new(dup, "works_at", acme))?; // the same fact: dropped
            tx.create_edge(NS, NewEdge::new(note, "mentions", dup))?; // moves to keep
            tx.create_edge(NS, NewEdge::new(dup, "same_as", keep))?; // between them: dropped
            tx.set_importance(NS, dup, 0.8)?;
            tx.touch(NS, &[dup], Timestamp::from_secs(4_000_000_000))?;
            Ok((keep, dup, acme, note))
        })
        .unwrap();

    // They disagree on the city: a plain merge refuses and says why.
    let refused = db.write(|tx| tx.merge_nodes(NS, keep, dup)).unwrap_err();
    assert!(
        matches!(&refused, Error::Conflict(message) if message.contains("conflicting city")),
        "{refused}"
    );
    let force = MergeOptions {
        force: true,
        ..MergeOptions::default()
    };
    let report = db
        .write(|tx| tx.merge_nodes_with(NS, keep, dup, &force))
        .unwrap();
    assert_eq!((report.edges_moved, report.edges_dropped), (1, 2));
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(report.conflicts[0].key, "city");

    let read = db.begin_read().unwrap();
    assert!(read.node(NS, dup).unwrap().is_none());
    let node = read.node(NS, keep).unwrap().unwrap();
    assert_eq!(node.labels, ["Engineer", "Person"]);
    assert_eq!(
        node.prop("city"),
        Some(&Value::from("Lima")),
        "the kept node's properties win"
    );
    assert_eq!(node.prop("age"), Some(&Value::from(34)));
    assert_eq!(
        node.prop("aliases"),
        Some(&serde_json::json!(["ana garcia lopez"]))
    );
    assert_eq!(
        read.find_by_name(NS, "Ana Garcia Lopez").unwrap(),
        vec![keep]
    );
    assert_eq!(
        read.text(NS, keep).unwrap().as_deref(),
        Some("Ana writes Rust\n\nAna lives in Lima")
    );
    assert_eq!(read.search_text(NS, "lives", 5).unwrap()[0].node, keep);
    assert!(read.embedding(NS, keep).unwrap().is_some());
    let state = read.memory(NS, &node).unwrap();
    assert_eq!((state.importance, state.accesses), (0.8, 1));

    let out: Vec<_> = read
        .neighbor_ids(NS, keep, Direction::Out, &EdgeFilter::any())
        .unwrap();
    assert_eq!(
        out.iter().map(|(_, other)| *other).collect::<Vec<_>>(),
        vec![acme]
    );
    let incoming = read
        .neighbor_ids(NS, keep, Direction::In, &EdgeFilter::any())
        .unwrap();
    assert_eq!(
        incoming.iter().map(|(_, other)| *other).collect::<Vec<_>>(),
        vec![note]
    );
    let report = read.check(NS).unwrap();
    assert!(report.is_ok(), "{:?}", report.problems);
    drop(read);

    assert!(db.write(|tx| tx.merge_nodes(NS, keep, keep)).is_err());
    assert!(matches!(
        db.write(|tx| tx.merge_nodes(NS, keep, dup)),
        Err(Error::NodeNotFound(_))
    ));
}

#[test]
fn memory_survives_export_import_and_rebuild() {
    let db = Grafeno::in_memory().unwrap();
    let id = db
        .write(|tx| {
            let id = tx.create_node(
                NS,
                NewNode::new("Person")
                    .prop("name", "Bob")
                    .prop("aliases", serde_json::json!(["Bobby"])),
            )?;
            tx.set_importance(NS, id, 0.25)?;
            tx.touch(NS, &[id], Timestamp::from_secs(4_000_000_000))?;
            tx.create_node(NS, NewNode::new("Note"))?;
            Ok(id)
        })
        .unwrap();

    let mut dump = Vec::new();
    db.begin_read().unwrap().export(&[], &mut dump).unwrap();
    let text = String::from_utf8(dump.clone()).unwrap();
    assert_eq!(
        text.matches("\"memory\"").count(),
        1,
        "only states that differ from the default"
    );

    let copy = Grafeno::in_memory().unwrap();
    copy.import(dump.as_slice()).unwrap();
    let read = copy.begin_read().unwrap();
    let node = read.node(NS, id).unwrap().unwrap();
    let state = read.memory(NS, &node).unwrap();
    assert_eq!((state.importance, state.accesses), (0.25, 1));
    assert_eq!(read.find_by_name(NS, "bobby").unwrap(), vec![id]);
    drop(read);

    copy.write(|tx| tx.rebuild_indexes(NS)).unwrap();
    let read = copy.begin_read().unwrap();
    assert!(read.check(NS).unwrap().is_ok());
    assert_eq!(
        read.memory(NS, &node).unwrap().importance,
        0.25,
        "rebuild keeps memory states"
    );
    assert_eq!(read.find_by_name(NS, "Bob").unwrap(), vec![id]);
}
