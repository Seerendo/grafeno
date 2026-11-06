//! Online backups: a consistent copy while writes go on.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use grafeno::{Grafeno, IndexDef, NewEdge, NewNode, NodeId, Principal, Timestamp};

fn temp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("grafeno-{name}-{}.redb", NodeId::new()))
}

/// The export of a database without its header line (which has a time).
fn content(db: &Grafeno) -> String {
    let mut out = Vec::new();
    db.begin_read().unwrap().export(&[], &mut out).unwrap();
    String::from_utf8(out)
        .unwrap()
        .lines()
        .skip(1)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_backup_is_the_snapshot_while_writes_go_on() {
    let source = temp("backup-source");
    let db = Arc::new(Grafeno::open(&source).unwrap());
    db.write(|tx| {
        let mut previous = None;
        for i in 0..300 {
            let id = tx.create_node(
                "vault",
                NewNode::new("Note")
                    .prop("i", i)
                    .text(format!("note {i} about graphs"))
                    .embedding(vec![(i as f32).cos(), (i as f32).sin(), 1.0]),
            )?;
            if let Some(previous) = previous {
                tx.create_edge("vault", NewEdge::new(previous, "next", id))?;
            }
            previous = Some(id);
        }
        tx.create_index(
            "vault",
            IndexDef {
                name: "Note_i".into(),
                label: "Note".into(),
                key: "i".into(),
                unique: false,
                fuzzy: false,
            },
        )?;
        tx.create_namespace("empty", Some("still here"))?;
        let (account, key) = Principal {
            name: "agent".into(),
            secret_hash: "00".repeat(32),
            grants: Vec::new(),
            owner: true,
            own_only: false,
            description: None,
            created_at: Timestamp::now(),
        }
        .split();
        tx.create_account(&account)?;
        tx.create_api_key(&key)
    })
    .unwrap();

    let snapshot = db.begin_read().unwrap();
    let expected = content(&db);
    // Writes keep coming while the backup runs.
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (db, stop) = (db.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut n = 0;
            while !stop.load(Ordering::Relaxed) {
                db.write(|tx| tx.create_node("vault", NewNode::new("Late").prop("n", n)))
                    .unwrap();
                n += 1;
            }
            n
        })
    };
    let target = temp("backup-copy");
    let stats = snapshot.backup(&target).unwrap();
    stop.store(true, Ordering::Relaxed);
    let late = writer.join().unwrap();
    assert!(stats.entries > 1_000 && stats.bytes > 0, "{stats:?}");
    assert!(!target.with_extension("partial").exists());

    let copy = Grafeno::open(&target).unwrap();
    assert_eq!(
        content(&copy),
        expected,
        "the snapshot, without the {late} later writes"
    );
    let read = copy.begin_read().unwrap();
    for ns in ["vault", "empty"] {
        assert!(read.check(ns).unwrap().is_ok(), "{ns}");
    }
    assert_eq!(read.search_text("vault", "graphs", 5).unwrap().len(), 5);
    assert_eq!(
        read.search_vector("vault", &[1.0, 0.0, 1.0], 3)
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        (
            read.accounts().unwrap().len(),
            read.api_keys().unwrap().len()
        ),
        (1, 1),
        "accounts and keys are backed up too"
    );
    assert_eq!(
        read.namespace("empty")
            .unwrap()
            .unwrap()
            .description
            .as_deref(),
        Some("still here")
    );

    // A backup never overwrites a file.
    assert!(db.begin_read().unwrap().backup(&target).is_err());
    drop((copy, read, snapshot, db));
    let _ = std::fs::remove_file(&source);
    let _ = std::fs::remove_file(&target);
}
