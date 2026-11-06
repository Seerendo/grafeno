//! Traversals from many starts: what batched and parallel traversal
//! change. Each query runs three times; the best time is kept.
//!
//! `cargo run --release -p grafeno-query --example traversal_bench [people]`

use std::time::Instant;

use grafeno::{Grafeno, IndexDef, NewEdge, NewNode};
use grafeno_query::{Limits, Params, execute_read_with};

const NS: &str = "bench";

/// People working at 1 000 companies, and each knowing 5 others.
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
    let mut ids = Vec::with_capacity(people);
    for chunk in 0..people.div_ceil(2_000) {
        ids.extend(
            db.write(|tx| {
                let mut made = Vec::new();
                for uid in chunk * 2_000..(chunk * 2_000 + 2_000).min(people) {
                    let id = tx.create_node(
                        NS,
                        NewNode::new("Person")
                            .prop("uid", uid as i64)
                            .prop("age", (uid % 90) as i64),
                    )?;
                    tx.create_edge(NS, NewEdge::new(id, "works_at", companies[uid % 1_000]))?;
                    made.push(id);
                }
                Ok(made)
            })
            .unwrap(),
        );
    }
    let mut state = 88172645463325252u64;
    let mut next = move |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % n as u64) as usize
    };
    for chunk in ids.chunks(2_000) {
        let pairs: Vec<_> = chunk
            .iter()
            .flat_map(|from| {
                (0..5)
                    .map(|_| (*from, ids[next(ids.len())]))
                    .collect::<Vec<_>>()
            })
            .collect();
        db.write(|tx| {
            for (from, to) in &pairs {
                tx.create_edge(NS, NewEdge::new(*from, "knows", *to))?;
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
        "{people} people, 1 000 companies, 5 acquaintances each ({:.1} s to build)",
        started.elapsed().as_secs_f64()
    );
    let queries = [
        (
            "two hops: colleagues of 10 000 people, counted",
            "MATCH (p:Person)-[:works_at]->(c)<-[:works_at]-(q) WHERE p.uid < 10000 RETURN count(*)",
        ),
        (
            "two hops from every company, grouped",
            "MATCH (c:Company)<-[:works_at]-(p)-[:knows]->(f) RETURN c.name, count(f)",
        ),
        (
            "friends of friends of 20 000 people",
            "MATCH (p:Person)-[:knows]->()-[:knows]->(f) WHERE p.uid < 20000 RETURN count(DISTINCT f)",
        ),
        (
            "reach within 3 hops of 2 000 people",
            "MATCH (p:Person)-[:knows*1..3]->(f) WHERE p.uid < 2000 RETURN count(*)",
        ),
        (
            "colleagues returned, 200 000 rows",
            "MATCH (p:Person)-[:works_at]->(c)<-[:works_at]-(q) WHERE p.uid < 1000 RETURN p.uid, q.uid",
        ),
    ];
    println!("| query | time | reads | answer |");
    for (name, query) in queries {
        let mut best = f64::MAX;
        let mut answer = String::new();
        for _ in 0..3 {
            let at = Instant::now();
            let result = execute_read_with(
                &db.begin_read().unwrap(),
                NS,
                query,
                &Params::new(),
                &Limits::none(),
            )
            .unwrap();
            best = best.min(at.elapsed().as_secs_f64() * 1000.0);
            answer = if result.rows.len() == 1 {
                format!("{:?}", result.rows[0].first())
            } else {
                format!("{} rows", result.rows.len())
            };
        }
        let profile = execute_read_with(
            &db.begin_read().unwrap(),
            NS,
            &format!("PROFILE {query}"),
            &Params::new(),
            &Limits::none(),
        )
        .unwrap()
        .profile
        .unwrap_or_default();
        let reads: u64 = profile.iter().filter_map(|step| step.reads).sum();
        println!("| {name} | {best:.0} ms | {reads} | {answer} |");
    }
}
