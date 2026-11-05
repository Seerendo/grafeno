//! CSV and GraphML: writing a namespace out and reading it back.
//!
//! `cargo run --release -p grafeno --example interchange_bench [nodes]`

use std::time::Instant;

use grafeno::{Grafeno, InterchangeOptions, NewEdge, NewNode, NodeId};

fn main() {
    let nodes: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(100_000);
    let db = Grafeno::in_memory().unwrap();
    let mut previous: Option<NodeId> = None;
    for chunk in 0..nodes.div_ceil(1_000) {
        db.write(|tx| {
            for k in 0..1_000.min(nodes - chunk * 1_000) {
                let id = tx.create_node(
                    "a",
                    NewNode::new("Note")
                        .prop("n", (chunk * 1_000 + k) as i64)
                        .prop("name", format!("note {k}"))
                        .text(format!("note {k} about topic {}", k % 50)),
                )?;
                if let Some(previous) = previous {
                    tx.create_edge("a", NewEdge::new(previous, "next", id).prop("w", 1.5))?;
                }
                previous = Some(id);
            }
            Ok(())
        })
        .unwrap();
    }
    println!("{nodes} nodes with text and {} edges", nodes - 1);
    let records = (nodes * 2 - 1) as f64;
    let options = InterchangeOptions::default();
    let read = db.begin_read().unwrap();

    let started = Instant::now();
    let (mut csv_nodes, mut csv_edges) = (Vec::new(), Vec::new());
    read.export_nodes_csv("a", &mut csv_nodes).unwrap();
    read.export_edges_csv("a", &mut csv_edges).unwrap();
    let seconds = started.elapsed().as_secs_f64();
    println!(
        "| CSV | export | {seconds:.2} s | {:.0} records/s | {:.1} MiB |",
        records / seconds,
        (csv_nodes.len() + csv_edges.len()) as f64 / 1_048_576.0
    );
    let copy = Grafeno::in_memory().unwrap();
    let started = Instant::now();
    copy.import_nodes_csv("b", csv_nodes.as_slice(), &options)
        .unwrap();
    let stats = copy
        .import_edges_csv("b", csv_edges.as_slice(), &options)
        .unwrap();
    let seconds = started.elapsed().as_secs_f64();
    assert_eq!(stats.failed, 0);
    println!(
        "| CSV | import | {seconds:.2} s | {:.0} records/s | |",
        records / seconds
    );

    let started = Instant::now();
    let mut graphml = Vec::new();
    read.export_graphml("a", &mut graphml).unwrap();
    let seconds = started.elapsed().as_secs_f64();
    println!(
        "| GraphML | export | {seconds:.2} s | {:.0} records/s | {:.1} MiB |",
        records / seconds,
        graphml.len() as f64 / 1_048_576.0
    );
    let copy = Grafeno::in_memory().unwrap();
    let started = Instant::now();
    let stats = copy
        .import_graphml("b", graphml.as_slice(), &options)
        .unwrap();
    let seconds = started.elapsed().as_secs_f64();
    assert_eq!(stats.failed, 0);
    println!(
        "| GraphML | import | {seconds:.2} s | {:.0} records/s | |",
        records / seconds
    );
    assert!(copy.begin_read().unwrap().check("b").unwrap().is_ok());
}
