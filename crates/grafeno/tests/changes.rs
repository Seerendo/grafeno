//! The change log: one entry per record changed in a commit, numbered per
//! namespace, nothing from aborted transactions, a window that prunes old
//! entries (and says so to late followers), and commit listeners.

use std::sync::{Arc, Mutex};

use grafeno::{
    ChangeOp, Error, Grafeno, NewEdge, NewNode, Part, Props, Retention, Value, Writer,
    WriterOptions,
};

#[test]
fn commits_append_folded_entries_per_namespace() {
    let db = Grafeno::in_memory().unwrap();
    let (ana, acme, edge) = db
        .write(|tx| {
            let ana = tx.create_node("a", NewNode::new("Person").text("Ana from Lima"))?;
            let acme = tx.create_node("a", NewNode::new("Company"))?;
            // Changed again in the same transaction: still one entry.
            let mut props = Props::new();
            props.insert("name".into(), Value::from("Acme"));
            tx.set_node_props("a", acme, props)?;
            let edge = tx.create_edge("a", NewEdge::new(ana, "works_at", acme))?;
            tx.create_node("b", NewNode::new("Note"))?;
            Ok((ana, acme, edge))
        })
        .unwrap();
    let read = db.begin_read().unwrap();
    let page = read.changes("a", 0, 100).unwrap();
    assert_eq!(page.last, 3);
    assert!(!page.gap);
    let seqs: Vec<u64> = page.changes.iter().map(|c| c.seq).collect();
    assert_eq!(seqs, [1, 2, 3]);
    assert_eq!(page.changes[0].node, Some(ana));
    assert_eq!(page.changes[0].op, ChangeOp::Created);
    assert_eq!(page.changes[0].parts, [Part::Record, Part::Text]);
    assert_eq!(page.changes[1].node, Some(acme));
    assert_eq!(
        page.changes[1].op,
        ChangeOp::Created,
        "created, then changed: created"
    );
    assert_eq!(page.changes[2].edge, Some(edge));
    assert_eq!(page.changes[2].labels, ["works_at"]);
    assert_eq!(
        (page.changes[2].from, page.changes[2].to),
        (Some(ana), Some(acme))
    );
    assert_eq!(
        read.changes("b", 0, 100).unwrap().last,
        1,
        "numbered per namespace"
    );
    drop(read);

    // Later commits continue the numbers; `since` skips what was read.
    db.write(|tx| {
        tx.set_text("a", ana, "Ana from Quito")?;
        tx.delete_node("a", acme)
    })
    .unwrap();
    let page = db.begin_read().unwrap().changes("a", 3, 100).unwrap();
    let got: Vec<(u64, ChangeOp)> = page.changes.iter().map(|c| (c.seq, c.op)).collect();
    assert_eq!(
        got,
        [
            (4, ChangeOp::Updated),
            (5, ChangeOp::Deleted),
            (6, ChangeOp::Deleted)
        ],
        "{page:?}"
    );
    assert_eq!(page.changes[0].parts, [Part::Text]);
    assert_eq!(page.next(3), 6);
    assert!(
        db.begin_read()
            .unwrap()
            .changes("a", 6, 10)
            .unwrap()
            .changes
            .is_empty()
    );
}

#[test]
fn aborted_and_failed_writes_leave_nothing() {
    let db = Arc::new(Grafeno::in_memory().unwrap());
    let failed: grafeno::Result<()> = db.write(|tx| {
        tx.create_node("a", NewNode::new("Person"))?;
        Err(Error::Invalid("no".into()))
    });
    assert!(failed.is_err());
    assert_eq!(
        db.begin_read().unwrap().changes("a", 0, 10).unwrap().last,
        0
    );

    // In a group commit, a failing write is dropped with its changes.
    let writer = Writer::new(db.clone(), WriterOptions::default());
    std::thread::scope(|scope| {
        for k in 0..20 {
            let writer = &writer;
            scope.spawn(move || {
                let _ = writer.write(move |tx| {
                    tx.create_node("a", NewNode::new("Person"))?;
                    if k % 5 == 0 {
                        return Err(Error::Invalid("refused".into()));
                    }
                    Ok(())
                });
            });
        }
    });
    let page = db.begin_read().unwrap().changes("a", 0, 100).unwrap();
    assert_eq!(page.changes.len(), 16);
    assert_eq!(
        db.begin_read().unwrap().stats("a").unwrap().nodes,
        16,
        "entries match the graph"
    );
}

#[test]
fn the_window_prunes_and_late_followers_see_the_gap() {
    let db = Grafeno::in_memory().unwrap();
    db.set_change_retention(Retention {
        max_entries: 10,
        ..Retention::default()
    });
    for _ in 0..25 {
        db.write(|tx| tx.create_node("a", NewNode::new("Note")).map(|_| ()))
            .unwrap();
    }
    let read = db.begin_read().unwrap();
    let page = read.changes("a", 0, 100).unwrap();
    assert_eq!(page.last, 25);
    assert_eq!(page.oldest, 16);
    assert_eq!(page.changes.len(), 10);
    assert!(page.gap, "entries 1 to 15 are gone");
    assert!(!read.changes("a", 15, 100).unwrap().gap);
    drop(read);

    // Off: commits append nothing.
    db.set_change_retention(Retention {
        enabled: false,
        ..Retention::default()
    });
    db.write(|tx| tx.create_node("a", NewNode::new("Note")).map(|_| ()))
        .unwrap();
    assert_eq!(
        db.begin_read().unwrap().changes("a", 0, 100).unwrap().last,
        25
    );
    assert!(db.begin_read().unwrap().check("a").unwrap().is_ok());
}

#[test]
fn listeners_hear_each_commit() {
    let db = Grafeno::in_memory().unwrap();
    let heard: Arc<Mutex<Vec<(String, u64)>>> = Arc::default();
    let sink = heard.clone();
    db.on_commit(move |touched| sink.lock().unwrap().extend(touched.iter().cloned()));
    db.write(|tx| {
        tx.create_node("a", NewNode::new("Note"))?;
        tx.create_node("b", NewNode::new("Note"))?;
        tx.create_node("b", NewNode::new("Note"))
    })
    .unwrap();
    // A commit that changes nothing is not heard.
    db.write(|_| Ok(())).unwrap();
    assert_eq!(
        *heard.lock().unwrap(),
        [("a".to_owned(), 1), ("b".to_owned(), 2)]
    );
}
