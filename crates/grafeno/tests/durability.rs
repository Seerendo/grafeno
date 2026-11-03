//! Durability: a child process writes transactions in a loop and is killed
//! at an arbitrary moment. Reopening must show every committed transaction,
//! nothing half-written, and consistent indexes.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use grafeno::{Direction, EdgeFilter, Grafeno, NewEdge, NewNode, NodeId, Value};

const NS: &str = "durable";
const DB_VAR: &str = "GRAFENO_DURABILITY_DB";

/// Each transaction adds a node (with text and an embedding) linked to the
/// previous one, and prints its number once committed.
fn write_forever(path: &Path) -> ! {
    let db = Grafeno::open(path).unwrap();
    let mut next = db.begin_read().unwrap().stats(NS).unwrap().nodes;
    let mut previous: Option<NodeId> = db
        .begin_read()
        .unwrap()
        .nodes_page(NS, None, None, usize::MAX)
        .unwrap()
        .last()
        .map(|node| node.id);
    loop {
        let index = next;
        let id = db
            .write(|tx| {
                let angle = index as f32;
                let id = tx.create_node(
                    NS,
                    NewNode::new("Step")
                        .prop("index", index)
                        .text(format!("step number {index} of the durability test"))
                        .embedding(vec![angle.cos(), angle.sin(), 1.0]),
                )?;
                if let Some(previous) = previous {
                    tx.create_edge(NS, NewEdge::new(previous, "next", id))?;
                }
                Ok(id)
            })
            .unwrap();
        println!("committed {index}");
        previous = Some(id);
        next += 1;
    }
}

/// Not a test on its own: the child process of `survives_being_killed`.
#[test]
#[ignore = "run by survives_being_killed in a child process"]
fn durability_writer() {
    if let Ok(path) = std::env::var(DB_VAR) {
        write_forever(Path::new(&path));
    }
}

/// Runs the writer until it reports `commits` more transactions, then kills
/// it. Returns the last transaction it reported.
fn run_and_kill(path: &Path, commits: u64) -> u64 {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "durability_writer",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(DB_VAR, path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut seen = 0;
    let mut last = None;
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let Some(index) = line
            .unwrap()
            .strip_prefix("committed ")
            .map(|n| n.parse().unwrap())
        else {
            continue;
        };
        last = Some(index);
        seen += 1;
        if seen == commits {
            // The writer is already inside its next transaction.
            child.kill().unwrap();
            break;
        }
    }
    child.wait().unwrap();
    last.expect("the writer committed nothing")
}

fn verify(path: &Path, last_reported: u64) -> u64 {
    let db = Grafeno::open(path).unwrap();
    let read = db.begin_read().unwrap();
    let report = read.check(NS).unwrap();
    assert!(report.is_ok(), "{:#?}", report.problems);

    let nodes = read.nodes_page(NS, None, None, usize::MAX).unwrap();
    let count = nodes.len() as u64;
    // Every reported commit is there; at most one more finished unreported.
    assert!(
        count == last_reported + 1 || count == last_reported + 2,
        "{count} nodes after reporting commit {last_reported}"
    );
    for (expected, node) in nodes.iter().enumerate() {
        assert_eq!(node.prop("index"), Some(&Value::from(expected as u64)));
    }
    // The chain of edges is complete: one per node after the first.
    assert_eq!(read.stats(NS).unwrap().edges, count - 1);
    for pair in nodes.windows(2) {
        let next = read
            .neighbors(NS, pair[0].id, Direction::Out, &EdgeFilter::any())
            .unwrap();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].node, pair[1].id);
    }
    let hits = read.search_text(NS, "durability", usize::MAX).unwrap();
    assert_eq!(hits.len() as u64, count);
    count
}

#[test]
fn survives_being_killed() {
    let path: PathBuf =
        std::env::temp_dir().join(format!("grafeno-durability-{}.redb", NodeId::new()));
    let mut total = 0;
    for round in 0..3 {
        let last = run_and_kill(&path, 40 + round * 25);
        total = verify(&path, last);
    }
    assert!(total >= 40 + 65 + 90);
    std::fs::remove_file(&path).unwrap();
}
