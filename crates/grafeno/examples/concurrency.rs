//! Concurrent small writes on a database file: one transaction per write,
//! or group commit through a `Writer`, with immediate or eventual
//! durability; and how many reads run meanwhile.
//!
//! `cargo run --release -p grafeno --example concurrency`

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use grafeno::{Durability, Grafeno, NewNode, Writer, WriterOptions};

const THREADS: usize = 16;
const WRITES: usize = 200;

fn open(name: &str) -> Arc<Grafeno> {
    let path =
        std::env::temp_dir().join(format!("grafeno-bench-{name}-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&path);
    Arc::new(Grafeno::open(&path).unwrap())
}

/// Writes per second, and reads per second by four readers meanwhile.
fn measure(
    db: Arc<Grafeno>,
    write: impl Fn(&Grafeno, usize) + Send + Sync + 'static,
) -> (f64, f64) {
    let write = Arc::new(write);
    let stop = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicU64::new(0));
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let (db, stop, reads) = (db.clone(), stop.clone(), reads.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let read = db.begin_read().unwrap();
                    let _ = read.nodes_page("bench", Some("Note"), None, 20).unwrap();
                    reads.fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect();
    let started = Instant::now();
    let writers: Vec<_> = (0..THREADS)
        .map(|thread| {
            let (db, write) = (db.clone(), write.clone());
            std::thread::spawn(move || {
                for i in 0..WRITES {
                    write(&db, thread * WRITES + i);
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    let seconds = started.elapsed().as_secs_f64();
    stop.store(true, Ordering::Relaxed);
    for reader in readers {
        reader.join().unwrap();
    }
    (
        (THREADS * WRITES) as f64 / seconds,
        reads.load(Ordering::Relaxed) as f64 / seconds,
    )
}

fn note(i: usize) -> NewNode {
    NewNode::new("Note").prop("n", i as i64)
}

fn main() {
    println!(
        "{THREADS} threads × {WRITES} writes of one node each, on a file; 4 readers meanwhile.\n"
    );
    println!("| Writes | Durability | Writes/s | Reads/s meanwhile |");
    println!("| --- | --- | --- | --- |");
    for durability in [Durability::Immediate, Durability::Eventual] {
        let db = open("single");
        db.set_durability(durability);
        let (writes, reads) = measure(db, |db, i| {
            db.write(|tx| tx.create_node("bench", note(i))).unwrap();
        });
        println!("| one transaction each | {durability:?} | {writes:.0} | {reads:.0} |");

        let db = open("group");
        db.set_durability(durability);
        let writer = Arc::new(Writer::new(db.clone(), WriterOptions::default()));
        let (writes, reads) = measure(db, move |_, i| {
            writer
                .write(move |tx| tx.create_node("bench", note(i)))
                .unwrap();
        });
        println!("| group commit | {durability:?} | {writes:.0} | {reads:.0} |");
    }
}
