//! The cost of the change log: writes with it on and off, reading it back,
//! and pruning a small window.
//!
//! `cargo run --release -p grafeno --example changes_bench [nodes]`

use std::time::Instant;

use grafeno::{Grafeno, NewEdge, NewNode, NodeId, Retention};

fn load(db: &Grafeno, nodes: usize) -> f64 {
    let started = Instant::now();
    let mut previous: Option<NodeId> = None;
    for chunk in 0..nodes.div_ceil(1_000) {
        db.write(|tx| {
            for k in 0..1_000.min(nodes - chunk * 1_000) {
                let id = tx.create_node(
                    "bench",
                    NewNode::new("Note")
                        .prop("n", (chunk * 1_000 + k) as i64)
                        .text(format!("note {k} about topic {}", k % 50)),
                )?;
                if let Some(previous) = previous {
                    tx.create_edge("bench", NewEdge::new(previous, "next", id))?;
                }
                previous = Some(id);
            }
            Ok(())
        })
        .unwrap();
    }
    started.elapsed().as_secs_f64()
}

fn main() {
    let nodes: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(100_000);
    let records = nodes * 2 - 1;
    println!(
        "{nodes} nodes with text and {} edges, transactions of 1 000 nodes",
        nodes - 1
    );

    let off = Grafeno::in_memory().unwrap();
    off.set_change_retention(Retention {
        enabled: false,
        ..Retention::default()
    });
    let plain = load(&off, nodes);
    let on = Grafeno::in_memory().unwrap();
    let logged = load(&on, nodes);
    println!(
        "| change log off | {plain:.2} s | {:.0} records/s |",
        records as f64 / plain
    );
    println!(
        "| change log on | {logged:.2} s | {:.0} records/s | {:+.1}% |",
        records as f64 / logged,
        (logged / plain - 1.0) * 100.0
    );

    // Reading it all back, a page at a time.
    let read = on.begin_read().unwrap();
    let started = Instant::now();
    let (mut since, mut count) = (0, 0usize);
    loop {
        let page = read.changes("bench", since, 1_000).unwrap();
        if page.changes.is_empty() {
            break;
        }
        count += page.changes.len();
        since = page.next(since);
    }
    let seconds = started.elapsed().as_secs_f64();
    println!(
        "| reading the log | {count} entries in {seconds:.2} s | {:.0} entries/s |",
        count as f64 / seconds
    );
    drop(read);

    // A window of 10 000 entries: each commit prunes what falls out.
    let small = Grafeno::in_memory().unwrap();
    small.set_change_retention(Retention {
        max_entries: 10_000,
        ..Retention::default()
    });
    let windowed = load(&small, nodes);
    let page = small.begin_read().unwrap().changes("bench", 0, 1).unwrap();
    println!(
        "| window of 10 000 entries | {windowed:.2} s | {:.0} records/s | kept {} to {} |",
        records as f64 / windowed,
        page.oldest,
        page.last
    );
    assert!(small.begin_read().unwrap().check("bench").unwrap().is_ok());
}
