//! Loading many nodes: an import in one transaction, then the same import in
//! bulk mode, on a database file.
//!
//! `cargo run --release -p grafeno --example bulk_load -- 200000`
//! (`NO_TEXT=1` leaves the texts out, `BULK_ONLY=1` measures only bulk mode)

use std::io::Write;
use std::time::Instant;

use grafeno::{Grafeno, ImportOptions, NewEdge, NewNode, NodeId};

fn main() {
    let nodes: usize = std::env::args()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(100_000);
    let words: Vec<String> = (0..2_000).map(|i| format!("w{i}")).collect();
    let source = Grafeno::in_memory().unwrap();
    let started = Instant::now();
    for chunk in 0..nodes.div_ceil(10_000) {
        source
            .write(|tx| {
                let mut previous: Option<NodeId> = None;
                for i in chunk * 10_000..((chunk + 1) * 10_000).min(nodes) {
                    // 30 words from a skewed vocabulary.
                    let text: Vec<&str> = (0..30)
                        .map(|k| {
                            words[((i * 31 + k * k * 7) % 2_000).min((i + k) % 2_000)].as_str()
                        })
                        .collect();
                    let mut new = NewNode::new("Note").prop("n", i as i64);
                    if std::env::var_os("NO_TEXT").is_none() {
                        new = new.text(text.join(" "));
                    }
                    let id = tx.create_node("bulk", new)?;
                    if let Some(previous) = previous {
                        tx.create_edge("bulk", NewEdge::new(previous, "next", id))?;
                    }
                    previous = Some(id);
                }
                Ok(())
            })
            .unwrap();
    }
    let mut dump = Vec::new();
    source.begin_read().unwrap().export(&[], &mut dump).unwrap();
    println!(
        "{nodes} nodes with 30-word texts and {} edges: built in {:.1} s, export of {} MB",
        nodes - nodes.div_ceil(10_000),
        started.elapsed().as_secs_f64(),
        dump.len() / 1_000_000
    );
    std::io::stdout().flush().unwrap();

    let mut modes = vec![
        ("one transaction", ImportOptions::default()),
        ("bulk", ImportOptions::bulk()),
    ];
    if std::env::var_os("BULK_ONLY").is_some() {
        modes.remove(0);
    }
    for (name, options) in modes {
        let path =
            std::env::temp_dir().join(format!("grafeno-bulk-{}-{name}.redb", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let db = Grafeno::open(&path).unwrap();
        let started = Instant::now();
        let stats = db.import_with(dump.as_slice(), &options).unwrap();
        let seconds = started.elapsed().as_secs_f64();
        assert_eq!(stats.nodes as usize, nodes);
        assert!(db.begin_read().unwrap().check("bulk").unwrap().is_ok());
        println!(
            "| {name} | {seconds:.1} s | {:.0} nodes/s |",
            nodes as f64 / seconds
        );
        drop(db);
        let _ = std::fs::remove_file(&path);
    }
}
