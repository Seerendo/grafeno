//! What the planner and parallel scans change: queries a planner without
//! statistics gets wrong, a repeated query, a large scan, and read
//! throughput with many readers and a busy writer.
//!
//! `cargo run --release -p grafeno-query --example planner_bench [people] [readers]`
//! (`readers`: only the readers and the writer).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use grafeno::{Grafeno, IndexDef, NewEdge, NewNode};
use grafeno_query::{Limits, Params, Value, execute_read_with, set_parallelism};

const NS: &str = "bench";

fn build(people: usize) -> Grafeno {
    let db = Grafeno::in_memory().unwrap();
    db.write(|tx| {
        for key in ["status", "city", "age", "uid"] {
            tx.create_index(NS, IndexDef::new("Person", key))?;
        }
        Ok(())
    })
    .unwrap();
    let companies = db
        .write(|tx| {
            (0..1_000)
                .map(|i| tx.create_node(NS, NewNode::new("Company").prop("name", format!("c{i}"))))
                .collect::<grafeno::Result<Vec<_>>>()
        })
        .unwrap();
    let owner = db
        .write(|tx| {
            let owner = tx.create_node(NS, NewNode::new("Owner").prop("name", "x"))?;
            for company in &companies[..3] {
                tx.create_edge(NS, NewEdge::new(owner, "owns", *company))?;
            }
            Ok(owner)
        })
        .unwrap();
    let _ = owner;
    let mut state = 88172645463325252u64;
    let mut next = move |n: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % n
    };
    for chunk in 0..people.div_ceil(2_000) {
        let batch: Vec<(usize, u64, u64, u64)> = (0..2_000.min(people - chunk * 2_000))
            .map(|k| (chunk * 2_000 + k, next(1000), next(100), next(1000)))
            .collect();
        db.write(|tx| {
            for (uid, skew, age, company) in &batch {
                // 99% active; half of everyone in one big city.
                let status = if *skew < 990 { "active" } else { "blocked" };
                let city = if *skew % 2 == 0 {
                    "big".to_owned()
                } else {
                    format!("city{}", skew % 200)
                };
                let id = tx.create_node(
                    NS,
                    NewNode::new("Person")
                        .prop("uid", *uid as i64)
                        .prop("status", status)
                        .prop("city", city)
                        .prop("age", *age as i64)
                        .prop("name", format!("person {uid}")),
                )?;
                tx.create_edge(
                    NS,
                    NewEdge::new(id, "works_at", companies[*company as usize]),
                )?;
            }
            Ok(())
        })
        .unwrap();
    }
    db
}

fn run(db: &Grafeno, query: &str, params: &Params, limits: &Limits) -> (f64, Vec<Vec<Value>>) {
    let started = Instant::now();
    let result = execute_read_with(&db.begin_read().unwrap(), NS, query, params, limits).unwrap();
    (started.elapsed().as_secs_f64() * 1000.0, result.rows)
}

fn best_of(db: &Grafeno, query: &str, limits: &Limits, times: usize) -> (f64, String) {
    let mut best = f64::MAX;
    let mut answer = String::new();
    for _ in 0..times {
        let (ms, rows) = run(db, query, &Params::new(), limits);
        best = best.min(ms);
        answer = format!("{:?}", rows.first().and_then(|r| r.first()));
    }
    (best, answer)
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    sorted
        .get(((sorted.len().max(1) - 1) as f64 * q) as usize)
        .copied()
        .unwrap_or(0.0)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let people: usize = args.iter().find_map(|a| a.parse().ok()).unwrap_or(200_000);
    let started = Instant::now();
    let db = Arc::new(build(people));
    println!(
        "{people} people, 1 000 companies ({:.1} s to build), {} logical CPUs",
        started.elapsed().as_secs_f64(),
        std::thread::available_parallelism().map_or(1, |n| n.get())
    );
    let limits = Limits::none();
    let started = Instant::now();
    let stats = db.begin_read().unwrap().analyze(NS).unwrap();
    println!(
        "planner statistics: {} indexes, {} edge labels, built in {:.0} ms",
        stats.indexes.len(),
        stats.edge_labels.len(),
        started.elapsed().as_secs_f64() * 1000.0
    );

    let readers_only = args.iter().any(|a| a == "readers");
    if !readers_only {
        println!("## plans");
        for (name, query) in [
            (
                "two equalities, the rare one second",
                "MATCH (p:Person) WHERE p.city = 'big' AND p.status = 'blocked' RETURN count(*)",
            ),
            (
                "two equalities, the rare one first",
                "MATCH (p:Person) WHERE p.status = 'blocked' AND p.city = 'city1' RETURN count(*)",
            ),
            (
                "a range that holds everyone, and a small label",
                "MATCH (p:Person)-[:works_at]->(c:Company {name: 'c7'}) WHERE p.age >= 0 RETURN count(*)",
            ),
            (
                "the big pattern written first",
                "MATCH (p:Person)-[:works_at]->(c), (o:Owner {name: 'x'})-[:owns]->(c) RETURN count(*)",
            ),
        ] {
            let (ms, answer) = best_of(&db, query, &limits, 3);
            println!("| {name} | {ms:.2} ms | {answer} |");
            let (_, plan) = run(&db, &format!("EXPLAIN {query}"), &Params::new(), &limits);
            if let Some(Value::Str(details)) = plan.first().and_then(|row| row.get(2)) {
                println!("  plan: {details}");
            }
        }

        println!("## a repeated query");
        let mut params = Params::new();
        let count = 50_000;
        let started = Instant::now();
        for k in 0..count {
            params.insert("u".into(), Value::Int((k % people) as i64));
            execute_read_with(
                &db.begin_read().unwrap(),
                NS,
                "MATCH (p:Person {uid: $u})-[:works_at]->(c) RETURN p.name, c.name",
                &params,
                &limits,
            )
            .unwrap();
        }
        let seconds = started.elapsed().as_secs_f64();
        println!(
            "| {count} point queries with a parameter | {seconds:.2} s | {:.0} queries/s |",
            count as f64 / seconds
        );

        println!("## a large scan");
        let scan = "MATCH (p:Person)-[:works_at]->(c) WHERE p.age > 50 AND p.name CONTAINS '7' RETURN count(*)";
        set_parallelism(1);
        let (ms, answer) = best_of(&db, scan, &limits, 3);
        println!("| one thread | {ms:.0} ms | {answer} |");
        set_parallelism(0);
        let (ms, answer) = best_of(&db, scan, &limits, 3);
        println!("| every thread | {ms:.0} ms | {answer} |");
        let (ms, answer) = best_of(&db, scan, &Limits::standard(), 3);
        println!("| every thread, standard limits | {ms:.0} ms | {answer} |");
    }
    println!("## readers and a writer");
    println!("| readers | writer | queries/s | p50 ms | p99 ms | writes/s |");
    for readers in [1usize, 2, 4, 8, 16] {
        for busy in [false, true] {
            let stop = Arc::new(AtomicBool::new(false));
            let writes = Arc::new(AtomicU64::new(0));
            let writer = busy.then(|| {
                let (db, stop, writes) = (db.clone(), stop.clone(), writes.clone());
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        db.write(|tx| {
                            for _ in 0..10 {
                                tx.create_node(NS, NewNode::new("Note").prop("x", 1))?;
                            }
                            Ok(())
                        })
                        .unwrap();
                        writes.fetch_add(10, Ordering::Relaxed);
                    }
                })
            });
            let run_for = Duration::from_secs(3);
            let handles: Vec<_> = (0..readers)
                .map(|r| {
                    let db = db.clone();
                    std::thread::spawn(move || {
                        let mut latencies = Vec::new();
                        let started = Instant::now();
                        let mut k = r * 7919;
                        let limits = Limits::standard();
                        let mut params = Params::new();
                        while started.elapsed() < run_for {
                            k += 1;
                            params.insert("u".into(), Value::Int((k % people) as i64));
                            let at = Instant::now();
                            execute_read_with(
                                &db.begin_read().unwrap(),
                                NS,
                                "MATCH (p:Person {uid: $u})-[:works_at]->(c)<-[:works_at]-(q) RETURN count(q)",
                                &params,
                                &limits,
                            )
                            .unwrap();
                            latencies.push(at.elapsed().as_secs_f64() * 1000.0);
                        }
                        latencies
                    })
                })
                .collect();
            let mut all: Vec<f64> = handles
                .into_iter()
                .flat_map(|h| h.join().unwrap())
                .collect();
            stop.store(true, Ordering::Relaxed);
            if let Some(writer) = writer {
                writer.join().unwrap();
            }
            all.sort_by(f64::total_cmp);
            println!(
                "| {readers} | {} | {:.0} | {:.3} | {:.3} | {:.0} |",
                if busy { "busy" } else { "none" },
                all.len() as f64 / run_for.as_secs_f64(),
                percentile(&all, 0.5),
                percentile(&all, 0.99),
                writes.load(Ordering::Relaxed) as f64 / run_for.as_secs_f64()
            );
        }
    }
}
