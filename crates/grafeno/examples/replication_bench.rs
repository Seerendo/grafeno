//! What the replication log costs: writes with it off and on (alternating
//! runs of the same binary), the bytes it adds per record, and how fast a
//! replica applies it.
//!
//! `cargo run --release -p grafeno --example replication_bench -- [nodes]`

use std::time::Instant;

use grafeno::{Grafeno, NewEdge, NewNode};

const NS: &str = "bench";

/// Nodes with a text and an edge to the previous one, in transactions of
/// 1 000 nodes; returns records per second.
fn write(db: &Grafeno, nodes: usize) -> f64 {
    let started = Instant::now();
    let mut previous = None;
    for chunk in 0..nodes.div_ceil(1_000) {
        previous = db
            .write(|tx| {
                let mut last = previous;
                for i in chunk * 1_000..((chunk + 1) * 1_000).min(nodes) {
                    let id = tx.create_node(
                        NS,
                        NewNode::new("Note").prop("i", i as i64).text(format!(
                            "note {i} about graphs and memory and topic {}",
                            i % 50
                        )),
                    )?;
                    if let Some(last) = last {
                        tx.create_edge(NS, NewEdge::new(last, "next", id))?;
                    }
                    last = Some(id);
                }
                Ok(last)
            })
            .unwrap();
    }
    (nodes * 2) as f64 / started.elapsed().as_secs_f64()
}

fn main() {
    let nodes: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(100_000);
    println!(
        "{nodes} nodes with text and {} edges, transactions of 1 000 nodes",
        nodes - 1
    );
    println!("| run | log | records/s |");
    let mut best = [0.0f64; 2];
    let mut leader = None;
    for round in 0..3 {
        for (slot, on) in [false, true].into_iter().enumerate() {
            let db = Grafeno::in_memory().unwrap();
            if on {
                db.enable_replication_log().unwrap();
            }
            let rate = write(&db, nodes);
            best[slot] = best[slot].max(rate);
            println!(
                "| {} | {} | {rate:.0} |",
                round + 1,
                if on { "on" } else { "off" }
            );
            if on {
                leader = Some(db);
            }
        }
    }
    println!(
        "best: off {:.0}, on {:.0} records/s ({:.0}% of off)",
        best[0],
        best[1],
        100.0 * best[1] / best[0]
    );
    let leader = leader.unwrap();
    let read = leader.begin_read().unwrap();
    let entries = read.log_entries(0, usize::MAX).unwrap();
    let bytes: usize = entries
        .iter()
        .map(|(_, entry)| serde_json::to_vec(entry).unwrap().len())
        .sum();
    println!(
        "log: {} entries, {:.1} MiB, {:.0} bytes per record",
        entries.len(),
        bytes as f64 / (1024.0 * 1024.0),
        bytes as f64 / (nodes * 2) as f64
    );
    let replica = Grafeno::in_memory().unwrap();
    let started = Instant::now();
    replica.apply_entries(&entries).unwrap();
    let seconds = started.elapsed().as_secs_f64();
    println!(
        "applied by a replica in {seconds:.2} s: {:.0} records/s",
        (nodes * 2) as f64 / seconds
    );
    assert_eq!(
        replica.begin_read().unwrap().stats(NS).unwrap().nodes,
        nodes as u64
    );
}
