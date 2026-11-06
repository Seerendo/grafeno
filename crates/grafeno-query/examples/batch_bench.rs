//! Scans, filters and aggregates over many rows: what batched execution
//! changes. Each query runs three times; the best time is kept.
//!
//! `cargo run --release -p grafeno-query --example batch_bench [people]`

use std::time::Instant;

use grafeno::{Grafeno, IndexDef, NewEdge, NewNode};
use grafeno_query::{Limits, Params, execute_read_with};

const NS: &str = "bench";

fn build(people: usize) -> Grafeno {
    let db = Grafeno::in_memory().unwrap();
    db.write(|tx| {
        tx.create_index(NS, IndexDef::new("Person", "uid"))
            .map(|_| ())
    })
    .unwrap();
    let companies = db
        .write(|tx| {
            (0..1_000)
                .map(|i| tx.create_node(NS, NewNode::new("Company").prop("name", format!("c{i}"))))
                .collect::<grafeno::Result<Vec<_>>>()
        })
        .unwrap();
    for chunk in 0..people.div_ceil(2_000) {
        db.write(|tx| {
            for uid in chunk * 2_000..(chunk * 2_000 + 2_000).min(people) {
                let id = tx.create_node(
                    NS,
                    NewNode::new("Person")
                        .prop("uid", uid as i64)
                        .prop("status", if uid % 100 == 0 { "blocked" } else { "active" })
                        .prop("city", format!("city{}", uid % 200))
                        .prop("age", (uid % 90) as i64)
                        .prop("name", format!("person {uid}")),
                )?;
                tx.create_edge(NS, NewEdge::new(id, "works_at", companies[uid % 1_000]))?;
            }
            Ok(())
        })
        .unwrap();
    }
    db
}

fn main() {
    let people: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(200_000);
    let started = Instant::now();
    let db = build(people);
    println!(
        "{people} people, 1 000 companies ({:.1} s to build)",
        started.elapsed().as_secs_f64()
    );
    let queries = [
        ("count a label", "MATCH (p:Person) RETURN count(*)"),
        (
            "group by a property",
            "MATCH (p:Person) RETURN p.city, count(*), avg(p.age)",
        ),
        (
            "filter, then four aggregates",
            "MATCH (p:Person) WHERE p.age > 30 RETURN p.status, sum(p.age), min(p.age), max(p.age), count(p)",
        ),
        (
            "one hop, grouped by the far end",
            "MATCH (c:Company)<-[:works_at]-(p:Person) RETURN c.name, count(p)",
        ),
        (
            "collect per group",
            "MATCH (p:Person) WHERE p.uid < 20000 RETURN p.city, collect(p.uid)",
        ),
        (
            "two hops, 2 million rows counted",
            "MATCH (p:Person)-[:works_at]->(c)<-[:works_at]-(q) WHERE p.uid < 10000 RETURN count(*)",
        ),
    ];
    println!("| query | no limits | standard limits | answer |");
    for (name, query) in queries {
        let mut cells = Vec::new();
        let mut answer = String::new();
        for limits in [Limits::none(), Limits::standard()] {
            let mut best = f64::MAX;
            let mut failed = None;
            for _ in 0..3 {
                let at = Instant::now();
                match execute_read_with(
                    &db.begin_read().unwrap(),
                    NS,
                    query,
                    &Params::new(),
                    &limits,
                ) {
                    Ok(result) => {
                        best = best.min(at.elapsed().as_secs_f64() * 1000.0);
                        answer = format!(
                            "{} rows, first {:?}",
                            result.rows.len(),
                            result.rows.first().map(|r| r.len())
                        );
                        if result.rows.len() == 1 {
                            answer = format!("{:?}", result.rows[0].first());
                        }
                    }
                    Err(error) => {
                        failed = Some(error.message);
                        break;
                    }
                }
            }
            cells.push(match failed {
                Some(message) => format!("fails: {}", message.chars().take(60).collect::<String>()),
                None => format!("{best:.1} ms"),
            });
        }
        println!("| {name} | {} | {} | {answer} |", cells[0], cells[1]);
    }
}
