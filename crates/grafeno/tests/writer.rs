//! Group commit, durability and bulk loading.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use grafeno::{Durability, Error, Grafeno, ImportOptions, NewEdge, NewNode, Writer, WriterOptions};

const NS: &str = "w";

fn count(db: &Grafeno, label: &str) -> u64 {
    db.begin_read()
        .unwrap()
        .stats(NS)
        .unwrap()
        .node_labels
        .get(label)
        .copied()
        .unwrap_or(0)
}

#[test]
fn concurrent_writes_all_commit() {
    let db = Arc::new(Grafeno::in_memory().unwrap());
    let writer = Arc::new(Writer::new(db.clone(), WriterOptions::default()));
    let threads: Vec<_> = (0..16)
        .map(|thread| {
            let writer = writer.clone();
            std::thread::spawn(move || {
                for i in 0..50 {
                    let id = writer
                        .write(move |tx| {
                            tx.create_node(NS, NewNode::new("Note").prop("n", thread * 100 + i))
                        })
                        .unwrap();
                    assert!(!id.to_string().is_empty());
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(count(&db, "Note"), 800);
    assert!(db.begin_read().unwrap().check(NS).unwrap().is_ok());
}

#[test]
fn a_failing_write_leaves_its_group_alone() {
    let db = Arc::new(Grafeno::in_memory().unwrap());
    let writer = Arc::new(Writer::new(db.clone(), WriterOptions::default()));
    let runs = Arc::new(AtomicUsize::new(0));
    let threads: Vec<_> = (0..32)
        .map(|i| {
            let (writer, runs) = (writer.clone(), runs.clone());
            std::thread::spawn(move || {
                writer.write(move |tx| {
                    runs.fetch_add(1, Ordering::Relaxed);
                    tx.create_node(NS, NewNode::new("Note").prop("i", i))?;
                    match i % 8 {
                        // Writes part of itself, then fails: none of it stays.
                        0 => Err(Error::Invalid(format!("write {i} fails"))),
                        1 => panic!("write {i} panics"),
                        _ => Ok(i),
                    }
                })
            })
        })
        .collect();
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    let failed = results.iter().filter(|result| result.is_err()).count();
    assert_eq!(failed, 8);
    assert!(
        results
            .iter()
            .any(|r| matches!(r, Err(Error::Invalid(m)) if m.contains("fails")))
    );
    assert!(
        results
            .iter()
            .any(|r| matches!(r, Err(Error::Storage(m)) if m.contains("panicked")))
    );
    assert_eq!(count(&db, "Note"), 24, "only the writes that succeeded");
    assert!(runs.load(Ordering::Relaxed) >= 32);
    assert!(db.begin_read().unwrap().check(NS).unwrap().is_ok());
}

#[test]
fn writes_in_a_group_see_each_other() {
    let db = Arc::new(Grafeno::in_memory().unwrap());
    let writer = Writer::new(db.clone(), WriterOptions::default());
    let a = writer
        .write(|tx| tx.create_node(NS, NewNode::new("Person").prop("name", "Ana")))
        .unwrap();
    let b = writer
        .write(|tx| tx.create_node(NS, NewNode::new("Person").prop("name", "Bob")))
        .unwrap();
    writer
        .write(move |tx| tx.create_edge(NS, NewEdge::new(a, "knows", b)))
        .unwrap();
    assert_eq!(db.begin_read().unwrap().stats(NS).unwrap().edges, 1);
}

#[test]
fn a_full_queue_answers_busy() {
    let db = Arc::new(Grafeno::in_memory().unwrap());
    let writer = Arc::new(Writer::new(
        db,
        WriterOptions {
            queue: 1,
            max_batch: 1,
            ..WriterOptions::default()
        },
    ));
    let started = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Mutex::new(()));
    let gate = release.lock().unwrap();
    // The first write holds the writer until the gate opens.
    let first = {
        let (writer, started, release) = (writer.clone(), started.clone(), release.clone());
        std::thread::spawn(move || {
            writer.write(move |_| {
                started.wait();
                drop(release.lock().unwrap());
                Ok(())
            })
        })
    };
    started.wait();
    // Five more writes: one fits in the queue, the others are turned away
    // at once. Busy answers come back while the gate is still closed.
    let (results, answers) = std::sync::mpsc::channel();
    let others: Vec<_> = (0..5)
        .map(|_| {
            let (writer, results) = (writer.clone(), results.clone());
            std::thread::spawn(move || results.send(writer.write(|_| Ok(()))).unwrap())
        })
        .collect();
    let mut busy = 0;
    while busy < 4 {
        match answers.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(Err(Error::Busy(_))) => busy += 1,
            Ok(other) => panic!("only busy answers before the gate opens, got {other:?}"),
            Err(_) => panic!("{busy} busy answers, expected 4"),
        }
    }
    drop(gate);
    assert!(matches!(
        answers.recv_timeout(std::time::Duration::from_secs(10)),
        Ok(Ok(()))
    ));
    assert!(first.join().unwrap().is_ok());
    for other in others {
        other.join().unwrap();
    }
}

#[test]
fn eventual_durability_reaches_the_disk() {
    let path = std::env::temp_dir().join(format!("grafeno-durability-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&path);
    {
        let db = Arc::new(Grafeno::open(&path).unwrap());
        db.set_durability(Durability::Eventual);
        assert_eq!(db.durability(), Durability::Eventual);
        let writer = Writer::new(db.clone(), WriterOptions::default());
        for i in 0..20 {
            writer
                .write(move |tx| tx.create_node(NS, NewNode::new("Note").prop("i", i)))
                .unwrap();
        }
        // Dropping the writer and the database flushes.
    }
    let db = Grafeno::open(&path).unwrap();
    assert_eq!(count(&db, "Note"), 20);
    drop(db);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn bulk_import_matches_a_normal_import() {
    let source = Grafeno::in_memory().unwrap();
    source
        .write(|tx| {
            let mut previous = None;
            for i in 0..2_500 {
                let id = tx.create_node(
                    NS,
                    NewNode::new("Note")
                        .prop("i", i)
                        .text(format!("note number {i} about graphs and word{}", i % 17)),
                )?;
                if let Some(previous) = previous {
                    tx.create_edge(NS, NewEdge::new(previous, "next", id))?;
                }
                previous = Some(id);
            }
            Ok(())
        })
        .unwrap();
    let mut dump = Vec::new();
    source.begin_read().unwrap().export(&[], &mut dump).unwrap();

    let normal = Grafeno::in_memory().unwrap();
    normal.import(dump.as_slice()).unwrap();
    let bulk = Grafeno::in_memory().unwrap();
    let stats = bulk
        .import_with(dump.as_slice(), &ImportOptions { chunk: Some(700) })
        .unwrap();
    assert_eq!((stats.nodes, stats.edges), (2_500, 2_499));
    assert_eq!(
        bulk.durability(),
        Durability::Immediate,
        "restored after the load"
    );
    for db in [&normal, &bulk] {
        let read = db.begin_read().unwrap();
        assert!(read.check(NS).unwrap().is_ok());
        assert_eq!(read.stats(NS).unwrap().texts, 2_500);
    }
    let search = |db: &Grafeno| {
        db.begin_read()
            .unwrap()
            .search_text(NS, "word3 graphs", 5)
            .unwrap()
            .iter()
            .map(|hit| hit.node)
            .collect::<Vec<_>>()
    };
    assert_eq!(search(&normal), search(&bulk));
    // A broken line stops a bulk load; the chunks before it stay.
    let mut broken = dump.clone();
    broken.extend_from_slice(b"not json\n");
    let partial = Grafeno::in_memory().unwrap();
    assert!(
        partial
            .import_with(broken.as_slice(), &ImportOptions::bulk())
            .is_err()
    );
    assert!(normal.import(broken.as_slice()).is_err());
}
