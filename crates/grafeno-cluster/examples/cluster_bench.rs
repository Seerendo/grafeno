//! What a cluster costs: sequential writes on one server against writes
//! through a three-member cluster (in one process, files on disk, Raft
//! state fsynced), and how long a failover takes.
//!
//! `cargo run --release -p grafeno-cluster --example cluster_bench -- [writes]`

use std::sync::Arc;
use std::time::{Duration, Instant};

use grafeno::{Grafeno, NewNode, NodeId};
use grafeno_cluster::memory::Hub;
use grafeno_cluster::{Node, NodeOptions, Storage};

fn percentile(sorted: &[f64], q: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * q) as usize]
}

fn report(name: &str, mut latencies: Vec<f64>, seconds: f64) {
    latencies.sort_by(f64::total_cmp);
    println!(
        "| {name} | {:.0} writes/s | p50 {:.2} ms | p99 {:.2} ms |",
        latencies.len() as f64 / seconds,
        percentile(&latencies, 0.5),
        percentile(&latencies, 0.99)
    );
}

fn main() {
    let writes: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(500);
    let dir = std::env::temp_dir().join(format!("grafeno-cluster-bench-{}", NodeId::new()));
    std::fs::create_dir_all(&dir).unwrap();
    println!("{writes} sequential writes of one node each, files on disk, immediate durability");

    // One server.
    {
        let db = Grafeno::open(dir.join("single.redb")).unwrap();
        let started = Instant::now();
        let mut latencies = Vec::new();
        for i in 0..writes {
            let at = Instant::now();
            db.write(|tx| {
                tx.create_node("b", NewNode::new("N").prop("i", i as i64))
                    .map(|_| ())
            })
            .unwrap();
            latencies.push(at.elapsed().as_secs_f64() * 1000.0);
        }
        report("one server", latencies, started.elapsed().as_secs_f64());
    }

    // Three members.
    let hub = Hub::new();
    let mut nodes = Vec::new();
    for id in 1..=3u64 {
        let db = Arc::new(Grafeno::open(dir.join(format!("member-{id}.redb"))).unwrap());
        let storage = Storage::open(&dir.join(format!("member-{id}.raft"))).unwrap();
        let node = Arc::new(
            Node::start(
                db,
                storage,
                hub.transport(),
                &[1, 2, 3],
                NodeOptions::new(id, dir.join(format!("snapshots-{id}"))),
            )
            .unwrap(),
        );
        hub.register(&node);
        nodes.push(node);
    }
    let leader = || loop {
        if let Some(node) = nodes.iter().find(|n| n.status().role == "leader") {
            break node.clone();
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let first = leader();
    let started = Instant::now();
    let mut latencies = Vec::new();
    for i in 0..writes {
        let at = Instant::now();
        first
            .write(|tx| {
                tx.create_node("b", NewNode::new("N").prop("i", i as i64))
                    .map(|_| ())
            })
            .unwrap();
        latencies.push(at.elapsed().as_secs_f64() * 1000.0);
    }
    report(
        "three-member cluster",
        latencies,
        started.elapsed().as_secs_f64(),
    );

    // Failover: the leader is cut off; time until another commits a write.
    let mut failovers = Vec::new();
    for _ in 0..5 {
        let old = leader();
        hub.isolate(old.id(), true);
        let cut = Instant::now();
        loop {
            let candidate = nodes
                .iter()
                .find(|n| n.id() != old.id() && n.status().role == "leader")
                .cloned();
            if let Some(node) = candidate
                && node
                    .write(|tx| tx.create_node("b", NewNode::new("F")).map(|_| ()))
                    .is_ok()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        failovers.push(cut.elapsed().as_secs_f64() * 1000.0);
        hub.isolate(old.id(), false);
        std::thread::sleep(Duration::from_millis(1500));
    }
    failovers.sort_by(f64::total_cmp);
    println!(
        "| failover (leader cut off, until another commits), 50 ms ticks | min {:.0} ms | median {:.0} ms | max {:.0} ms |",
        failovers[0],
        failovers[failovers.len() / 2],
        failovers[failovers.len() - 1]
    );
    for node in &nodes {
        node.stop();
    }
    drop(nodes);
    let _ = std::fs::remove_dir_all(&dir);
}
