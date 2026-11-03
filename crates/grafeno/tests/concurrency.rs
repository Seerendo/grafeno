//! Concurrency: readers working while a writer commits must always see a
//! complete snapshot, and successive snapshots only move forward.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use grafeno::{Direction, EdgeFilter, Grafeno, NewEdge, NewNode};

const NS: &str = "busy";
const TRANSACTIONS: u64 = 300;

#[test]
fn readers_see_whole_transactions() {
    let db = Arc::new(Grafeno::in_memory().unwrap());
    let done = Arc::new(AtomicBool::new(false));

    let readers: Vec<_> = (0..4)
        .map(|reader| {
            let (db, done) = (db.clone(), done.clone());
            thread::spawn(move || {
                let (mut snapshots, mut last_nodes) = (0u64, 0u64);
                while !done.load(Ordering::Acquire) {
                    let read = db.begin_read().unwrap();
                    let stats = read.stats(NS).unwrap();
                    // Each transaction adds two linked nodes with text.
                    assert_eq!(stats.nodes, stats.edges * 2, "reader {reader}");
                    assert_eq!(stats.texts, stats.nodes);
                    assert!(stats.nodes >= last_nodes, "snapshots never go back");
                    last_nodes = stats.nodes;

                    let hits = read.search_text(NS, "pair", usize::MAX).unwrap();
                    assert_eq!(hits.len() as u64, stats.nodes);
                    for node in read.nodes_page(NS, Some("Left"), None, usize::MAX).unwrap() {
                        let out = read
                            .neighbors(NS, node.id, Direction::Out, &EdgeFilter::any())
                            .unwrap();
                        assert_eq!(out.len(), 1);
                        assert!(read.node(NS, out[0].node).unwrap().is_some());
                    }
                    if snapshots % 25 == 0 {
                        let report = read.check(NS).unwrap();
                        assert!(report.is_ok(), "{:?}", report.problems);
                    }
                    snapshots += 1;
                }
                snapshots
            })
        })
        .collect();

    for index in 0..TRANSACTIONS {
        db.write(|tx| {
            let left =
                tx.create_node(NS, NewNode::new("Left").text(format!("pair {index} left")))?;
            let right = tx.create_node(
                NS,
                NewNode::new("Right").text(format!("pair {index} right")),
            )?;
            tx.create_edge(NS, NewEdge::new(left, "pairs", right))?;
            Ok(())
        })
        .unwrap();
    }
    done.store(true, Ordering::Release);

    let snapshots: u64 = readers
        .into_iter()
        .map(|reader| reader.join().unwrap())
        .sum();
    assert!(snapshots > 0);
    let stats = db.begin_read().unwrap().stats(NS).unwrap();
    assert_eq!((stats.nodes, stats.edges), (TRANSACTIONS * 2, TRANSACTIONS));
}
