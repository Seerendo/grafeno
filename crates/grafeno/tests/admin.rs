//! Namespaces, statistics, paging, integrity checks and export/import.

use grafeno::{Grafeno, NewEdge, NewNode, NodeId, Timestamp};

fn sample(db: &Grafeno) -> (NodeId, NodeId, NodeId) {
    db.write(|tx| {
        let ana = tx.create_node(
            "vault",
            NewNode::new("Person")
                .prop("name", "Ana")
                .text("Ana writes Rust")
                .embedding(vec![1.0, 0.0, 0.0]),
        )?;
        let note = tx.create_node(
            "vault",
            NewNode::new("Note")
                .label("Pinned")
                .prop("title", "Ownership")
                .text("Each value has one owner")
                .embedding(vec![0.0, 1.0, 0.0]),
        )?;
        let tag = tx.create_node("vault", NewNode::new("Tag").prop("name", "rust"))?;
        tx.create_edge(
            "vault",
            NewEdge::new(ana, "wrote", note).valid_from(Timestamp::from_secs(100)),
        )?;
        tx.create_edge("vault", NewEdge::new(note, "tagged", tag))?;
        tx.create_node(
            "agent",
            NewNode::new("Memory").text("The user likes short answers"),
        )?;
        Ok((ana, note, tag))
    })
    .unwrap()
}

#[test]
fn namespaces_and_stats_follow_every_write() {
    let db = Grafeno::in_memory().unwrap();
    let (ana, note, _) = sample(&db);
    {
        let read = db.begin_read().unwrap();
        let names: Vec<(String, u64, u64)> = read
            .namespaces()
            .unwrap()
            .into_iter()
            .map(|info| (info.name, info.nodes, info.edges))
            .collect();
        assert_eq!(names, vec![("agent".into(), 1, 0), ("vault".into(), 3, 2)]);
        let stats = read.stats("vault").unwrap();
        assert_eq!(stats.node_labels.get("Note"), Some(&1));
        assert_eq!(stats.node_labels.get("Pinned"), Some(&1));
        assert_eq!(stats.edge_labels.get("wrote"), Some(&1));
        assert_eq!(
            (stats.texts, stats.vectors, stats.vector_dimension),
            (2, 2, Some(3))
        );
        assert_eq!(read.stats("missing").unwrap().nodes, 0);
    }

    db.write(|tx| {
        tx.remove_label("vault", note, "Pinned")?;
        tx.add_label("vault", ana, "Admin")?;
        tx.upsert_node("vault", note, NewNode::new("Draft"))?;
        tx.delete_node("vault", ana)
    })
    .unwrap();
    let read = db.begin_read().unwrap();
    let stats = read.stats("vault").unwrap();
    assert_eq!((stats.nodes, stats.edges), (2, 1));
    assert_eq!(
        stats.node_labels.keys().collect::<Vec<_>>(),
        ["Draft", "Tag"]
    );
    assert_eq!(stats.edge_labels.keys().collect::<Vec<_>>(), ["tagged"]);
    assert_eq!(
        (stats.texts, stats.vectors, stats.vector_dimension),
        (0, 0, None)
    );
    assert!(read.check("vault").unwrap().is_ok());

    db.write(|tx| tx.delete_node("agent", read.nodes("agent")?[0].id))
        .unwrap();
    let names: Vec<String> = db
        .begin_read()
        .unwrap()
        .namespaces()
        .unwrap()
        .into_iter()
        .map(|info| info.name)
        .collect();
    assert_eq!(
        names,
        ["agent", "vault"],
        "empty namespaces stay until dropped"
    );
}

#[test]
fn pages_cover_everything_once() {
    let db = Grafeno::in_memory().unwrap();
    let ids = db
        .write(|tx| {
            (0..25)
                .map(|i| {
                    let label = if i % 3 == 0 { "Even" } else { "Odd" };
                    tx.create_node("ns", NewNode::new(label).prop("i", i))
                })
                .collect::<grafeno::Result<Vec<_>>>()
        })
        .unwrap();
    db.write(|tx| {
        for pair in ids.windows(2) {
            let label = if pair[0] < pair[1] { "next" } else { "never" };
            tx.create_edge("ns", NewEdge::new(pair[0], label, pair[1]))?;
        }
        Ok(())
    })
    .unwrap();

    let read = db.begin_read().unwrap();
    let mut seen = Vec::new();
    let mut after = None;
    loop {
        let page = read.nodes_page("ns", None, after, 10).unwrap();
        if page.is_empty() {
            break;
        }
        after = page.last().map(|node| node.id);
        seen.extend(page.into_iter().map(|node| node.id));
    }
    assert_eq!(seen, ids);

    let evens = read.nodes_page("ns", Some("Even"), None, 100).unwrap();
    assert_eq!(evens.len(), 9);
    let rest = read
        .nodes_page("ns", Some("Even"), Some(evens[4].id), 100)
        .unwrap();
    assert_eq!(rest.len(), 4);

    let edges = read.edges_page("ns", Some("next"), None, 30).unwrap();
    assert_eq!(edges.len(), 24);
    let page = read.edges_page("ns", None, Some(edges[19].id), 30).unwrap();
    assert_eq!(page.len(), 4);
    assert!(read.nodes_page("ns", None, None, 0).unwrap().is_empty());
}

#[test]
fn check_passes_after_many_kinds_of_writes() {
    let db = Grafeno::in_memory().unwrap();
    let (ana, note, tag) = sample(&db);
    db.write(|tx| {
        tx.set_text("vault", tag, "tag for Rust things")?;
        tx.set_embedding("vault", tag, &[0.0, 0.0, 2.0])?;
        tx.set_embedding("vault", ana, &[0.5, 0.5, 0.0])?;
        tx.remove_text("vault", note)?;
        let edge = tx.create_edge("vault", NewEdge::new(tag, "about", ana))?;
        tx.retract_edge("vault", edge)?;
        tx.delete_edges_from("vault", note, Some("tagged"))?;
        Ok(())
    })
    .unwrap();
    let read = db.begin_read().unwrap();
    for ns in ["vault", "agent", "empty"] {
        let report = read.check(ns).unwrap();
        assert!(report.is_ok(), "{ns}: {:?}", report.problems);
    }
    let report = read.check("vault").unwrap();
    assert_eq!(
        (report.nodes, report.edges, report.texts, report.vectors),
        (3, 2, 2, 3)
    );
}

#[test]
fn rebuild_keeps_data_and_searches() {
    let db = Grafeno::in_memory().unwrap();
    let (ana, note, _) = sample(&db);
    let report = db.write(|tx| tx.rebuild_indexes("vault")).unwrap();
    assert_eq!(
        (report.nodes, report.edges, report.texts, report.vectors),
        (3, 2, 2, 2)
    );
    assert_eq!(
        report.dropped_edges + report.dropped_texts + report.dropped_vectors,
        0
    );

    let read = db.begin_read().unwrap();
    assert!(read.check("vault").unwrap().is_ok());
    assert!(
        read.check("agent").unwrap().is_ok(),
        "other namespaces are untouched"
    );
    assert_eq!(read.search_text("vault", "owner", 1).unwrap()[0].node, note);
    assert_eq!(
        read.search_vector("vault", &[1.0, 0.1, 0.0], 1).unwrap()[0].node,
        ana
    );
    assert_eq!(read.search_text("agent", "answers", 1).unwrap().len(), 1);
    assert_eq!(read.stats("vault").unwrap().edges, 2);
}

#[test]
fn export_and_import_round_trip() {
    let db = Grafeno::in_memory().unwrap();
    let (ana, _, _) = sample(&db);
    db.write(|tx| {
        let edge = tx.create_edge("vault", NewEdge::new(ana, "likes", ana))?;
        tx.invalidate_edge("vault", edge, Timestamp::from_secs(500))?;
        tx.retract_edge("vault", edge)
    })
    .unwrap();

    let mut dump = Vec::new();
    let stats = db.begin_read().unwrap().export(&[], &mut dump).unwrap();
    assert_eq!((stats.namespaces, stats.nodes, stats.edges), (2, 4, 3));

    let copy = Grafeno::in_memory().unwrap();
    let imported = copy.import(dump.as_slice()).unwrap();
    assert_eq!(imported, stats);

    let (original, restored) = (db.begin_read().unwrap(), copy.begin_read().unwrap());
    for ns in ["vault", "agent"] {
        assert_eq!(original.nodes(ns).unwrap(), restored.nodes(ns).unwrap());
        assert_eq!(original.edges(ns).unwrap(), restored.edges(ns).unwrap());
        assert_eq!(original.stats(ns).unwrap(), restored.stats(ns).unwrap());
        assert!(restored.check(ns).unwrap().is_ok());
        for node in original.nodes(ns).unwrap() {
            assert_eq!(
                original.text(ns, node.id).unwrap(),
                restored.text(ns, node.id).unwrap()
            );
            assert_eq!(
                original.embedding(ns, node.id).unwrap(),
                restored.embedding(ns, node.id).unwrap(),
                "embeddings survive bit for bit"
            );
        }
    }
    // Exporting the copy gives the same records (only the header's time differs).
    let mut again = Vec::new();
    restored.export(&[], &mut again).unwrap();
    let body = |dump: &[u8]| {
        String::from_utf8(dump.to_vec())
            .unwrap()
            .lines()
            .skip(1)
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(body(&dump), body(&again));

    // Importing twice replaces instead of duplicating.
    copy.import(dump.as_slice()).unwrap();
    assert_eq!(copy.begin_read().unwrap().stats("vault").unwrap().nodes, 3);

    let mut one = Vec::new();
    original.export(&["agent".into()], &mut one).unwrap();
    // The header, the namespace and its one node.
    assert_eq!(String::from_utf8(one).unwrap().lines().count(), 3);
}

#[test]
fn import_is_all_or_nothing() {
    let db = Grafeno::in_memory().unwrap();
    let header = r#"{"type":"header","format":"grafeno-export","version":1,"exported_at":0}"#;
    let node = r#"{"type":"node","ns":"x","node":{"id":"0190f5c2-7a3e-7cde-8f01-000000000001","labels":["N"],"created_at":0,"updated_at":0}}"#;
    let bad_edge = r#"{"type":"edge","ns":"x","edge":{"id":"0190f5c2-7a3e-7cde-8f01-0000000000e1","label":"l","from":"0190f5c2-7a3e-7cde-8f01-000000000001","to":"0190f5c2-7a3e-7cde-8f01-000000000099","valid_from":null,"valid_to":null,"recorded_at":0,"expired_at":null}}"#;

    let error = db
        .import(format!("{header}\n{node}\n{bad_edge}\n").as_bytes())
        .unwrap_err();
    assert!(error.to_string().contains("line 3:"), "{error}");
    assert!(db.begin_read().unwrap().namespaces().unwrap().is_empty());

    for (input, expected) in [
        (String::new(), "empty"),
        (
            node.to_owned(),
            "line 1: the export must start with a header",
        ),
        (format!("{header}\n{header}"), "line 2: a second header"),
        (
            r#"{"type":"header","format":"other","version":1}"#.to_owned(),
            "not a Grafeno export",
        ),
        (
            r#"{"type":"header","format":"grafeno-export","version":99}"#.to_owned(),
            "newer",
        ),
        (
            format!("{header}\n{{\"type\":\"bogus\"}}"),
            "unknown record type",
        ),
    ] {
        let error = db.import(input.as_bytes()).unwrap_err().to_string();
        assert!(
            error.contains(expected),
            "{error:?} should mention {expected:?}"
        );
    }
    let imported = db
        .import(format!("{header}\n\n{node}\n").as_bytes())
        .unwrap();
    assert_eq!(imported.nodes, 1);
}
