//! The one writer under load: write latency against offered load, with
//! immediate and eventual durability, on a database file; then an agent's
//! writes while a large import runs, with and without the bulk lane.
//!
//! `cargo run --release -p grafeno --example saturation [seconds per step]`
//! (`MAX_BATCH=512` changes the group size, `ONLY=saturation` skips the
//! import part)
//!
//! Load is open: writes are scheduled at a fixed rate whatever the answers,
//! and latency runs from the scheduled time, so a writer that falls behind
//! shows it (no coordinated omission).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use grafeno::{Durability, Error, Grafeno, NewEdge, NewNode, NodeId, Writer, WriterOptions};

const NS: &str = "load";
const CLIENTS: usize = 512;

fn temp(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("grafeno-{name}-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

/// Waits until `at`, sleeping most of the way.
fn until(at: Instant) {
    loop {
        let now = Instant::now();
        if now >= at {
            return;
        }
        let left = at - now;
        if left > Duration::from_millis(2) {
            std::thread::sleep(left - Duration::from_millis(1));
        } else {
            std::hint::spin_loop();
        }
    }
}

fn percentile(sorted: &[Duration], q: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    sorted[((q * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len()) - 1]
}

fn ms(duration: Duration) -> String {
    let ms = duration.as_secs_f64() * 1e3;
    if ms < 10.0 {
        format!("{ms:.2}")
    } else {
        format!("{ms:.0}")
    }
}

struct Outcome {
    latencies: Vec<Duration>,
    rejected: usize,
    elapsed: Duration,
}

/// `rate` writes per second for `seconds`, each a node and an edge to an
/// earlier node.
fn offer(writer: &Arc<Writer>, ids: &Arc<Mutex<Vec<NodeId>>>, rate: f64, seconds: f64) -> Outcome {
    let total = (rate * seconds) as usize;
    let start = Instant::now() + Duration::from_millis(50);
    let next = Arc::new(AtomicUsize::new(0));
    let results = Arc::new(Mutex::new((Vec::with_capacity(total), 0usize)));
    std::thread::scope(|scope| {
        for _ in 0..CLIENTS.min(total.max(1)) {
            let (writer, ids, next, results) =
                (writer.clone(), ids.clone(), next.clone(), results.clone());
            scope.spawn(move || {
                loop {
                    let ticket = next.fetch_add(1, Ordering::Relaxed);
                    if ticket >= total {
                        return;
                    }
                    let scheduled = start + Duration::from_secs_f64(ticket as f64 / rate);
                    until(scheduled);
                    let target = {
                        let ids = ids.lock().unwrap();
                        ids.get(ticket.wrapping_mul(2_654_435_761) % ids.len().max(1))
                            .copied()
                    };
                    let outcome = writer.write(move |tx| {
                        let id =
                            tx.create_node(NS, NewNode::new("Event").prop("n", ticket as i64))?;
                        if let Some(target) = target {
                            tx.create_edge(NS, NewEdge::new(id, "after", target))?;
                        }
                        Ok(id)
                    });
                    let latency = scheduled.elapsed();
                    let mut results = results.lock().unwrap();
                    match outcome {
                        Ok(id) => {
                            results.0.push(latency);
                            drop(results);
                            ids.lock().unwrap().push(id);
                        }
                        Err(Error::Busy(_)) => results.1 += 1,
                        Err(error) => panic!("{error}"),
                    }
                }
            });
        }
    });
    let (mut latencies, rejected) = std::mem::take(&mut *results.lock().unwrap());
    latencies.sort();
    Outcome {
        latencies,
        rejected,
        elapsed: start.elapsed(),
    }
}

fn saturation(seconds: f64) {
    for durability in [Durability::Immediate, Durability::Eventual] {
        let path = temp("saturation");
        let db = Arc::new(Grafeno::open(&path).unwrap());
        db.set_durability(durability);
        let writer = Arc::new(Writer::new(db.clone(), options()));
        let ids = Arc::new(Mutex::new(Vec::new()));
        println!(
            "\n{durability:?} durability, max_batch {}, {seconds} s per step, {CLIENTS} clients\n",
            options().max_batch
        );
        println!(
            "| offered/s | done/s | p50 ms | p99 ms | p999 ms | max ms | writes/commit | max queue | rejected |"
        );
        println!("| --- | --- | --- | --- | --- | --- | --- | --- | --- |");
        for rate in [
            250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0, 32_000.0,
        ] {
            let before = writer.metrics();
            let peak = Arc::new(AtomicUsize::new(0));
            let watching = Arc::new(std::sync::atomic::AtomicBool::new(true));
            let watcher = {
                let (writer, peak, watching) = (writer.clone(), peak.clone(), watching.clone());
                std::thread::spawn(move || {
                    while watching.load(Ordering::Relaxed) {
                        peak.fetch_max(writer.metrics().queued, Ordering::Relaxed);
                        std::thread::sleep(Duration::from_millis(5));
                    }
                })
            };
            let outcome = offer(&writer, &ids, rate, seconds);
            watching.store(false, Ordering::Relaxed);
            watcher.join().unwrap();
            let after = writer.metrics();
            let groups = after.groups - before.groups;
            let writes = after.writes - before.writes;
            let l = &outcome.latencies;
            println!(
                "| {rate:.0} | {:.0} | {} | {} | {} | {} | {:.1} | {} | {} |",
                l.len() as f64 / outcome.elapsed.as_secs_f64(),
                ms(percentile(l, 0.5)),
                ms(percentile(l, 0.99)),
                ms(percentile(l, 0.999)),
                ms(l.last().copied().unwrap_or_default()),
                writes as f64 / groups.max(1) as f64,
                peak.load(Ordering::Relaxed),
                outcome.rejected,
            );
            // A step that rejected or fell far behind is past saturation.
            if outcome.rejected > 0 && percentile(l, 0.5) > Duration::from_secs(1) {
                break;
            }
        }
        drop(writer);
        drop(db);
        let _ = std::fs::remove_file(&path);
    }
}

/// An agent writing 200 times a second while a 300 000-node export is
/// imported, through the bulk lane or around the writer (as the server did
/// before 10.7).
fn interference(seconds: f64) {
    let source = Grafeno::in_memory().unwrap();
    for chunk in 0..30 {
        source
            .write(|tx| {
                for i in 0..10_000 {
                    let n = chunk * 10_000 + i;
                    tx.create_node(
                        NS,
                        NewNode::new("Doc")
                            .prop("n", n as i64)
                            .text(format!("document {n} about topic{} and words", n % 97)),
                    )?;
                }
                Ok(())
            })
            .unwrap();
    }
    let mut dump = Vec::new();
    source.begin_read().unwrap().export(&[], &mut dump).unwrap();
    drop(source);
    let dump = Arc::new(dump);
    println!("\nAn agent writing 200/s for {seconds} s while 300 000 nodes are imported\n");
    println!("| import | import s | agent p50 ms | p99 ms | max ms | rejected |");
    println!("| --- | --- | --- | --- | --- | --- |");
    for (name, lane, chunk) in [
        ("none (agent alone)", None, 0),
        (
            "around the writer, one transaction",
            Some(false),
            usize::MAX,
        ),
        ("around the writer, chunks of 20 000", Some(false), 20_000),
        ("bulk lane, one transaction", Some(true), usize::MAX),
        ("bulk lane, chunks of 20 000", Some(true), 20_000),
        ("bulk lane, chunks of 2 000", Some(true), 2_000),
    ] {
        let path = temp("interference");
        let db = Arc::new(Grafeno::open(&path).unwrap());
        let writer = Arc::new(Writer::new(db.clone(), WriterOptions::default()));
        let ids = Arc::new(Mutex::new(Vec::new()));
        let import = lane.map(|bulk| {
            let (db, writer, dump) = (db.clone(), writer.clone(), dump.clone());
            std::thread::spawn(move || {
                let started = Instant::now();
                if bulk {
                    writer.import(dump.as_slice(), chunk).unwrap();
                } else {
                    let options = grafeno::ImportOptions {
                        chunk: (chunk != usize::MAX).then_some(chunk),
                    };
                    db.import_with(dump.as_slice(), &options).unwrap();
                }
                started.elapsed()
            })
        });
        let outcome = offer(&writer, &ids, 200.0, seconds);
        let took = import.map(|import| import.join().unwrap());
        let l = &outcome.latencies;
        println!(
            "| {name} | {} | {} | {} | {} | {} |",
            took.map_or("-".into(), |took| format!("{:.1}", took.as_secs_f64())),
            ms(percentile(l, 0.5)),
            ms(percentile(l, 0.99)),
            ms(l.last().copied().unwrap_or_default()),
            outcome.rejected,
        );
        drop(writer);
        drop(db);
        let _ = std::fs::remove_file(&path);
    }
}

fn options() -> WriterOptions {
    let base = WriterOptions::default();
    WriterOptions {
        max_batch: std::env::var("MAX_BATCH")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(base.max_batch),
        ..base
    }
}

fn main() {
    let seconds: f64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3.0);
    saturation(seconds);
    if std::env::var("ONLY").as_deref() != Ok("saturation") {
        interference(seconds * 4.0);
    }
}
