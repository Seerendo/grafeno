//! How much a property index saves: one query on 100,000 nodes, with and
//! without an index. Run with `cargo run --release -p grafeno-query --example index_bench`.

use std::time::Instant;

use grafeno::Grafeno;
use grafeno_query::{Params, execute};

fn time(db: &Grafeno, query: &str) -> (f64, u64) {
    let runs = 20;
    let started = Instant::now();
    for _ in 0..runs {
        execute(db, "bench", query, &Params::new()).unwrap();
    }
    let ms = started.elapsed().as_secs_f64() * 1000.0 / f64::from(runs);
    let profile = execute(db, "bench", &format!("PROFILE {query}"), &Params::new()).unwrap();
    let reads = profile
        .profile
        .unwrap()
        .iter()
        .filter_map(|step| step.reads)
        .sum();
    (ms, reads)
}

fn main() {
    let db = Grafeno::in_memory().unwrap();
    for chunk in 0..10 {
        execute(
            &db,
            "bench",
            &format!(
                "UNWIND range({}, {}) AS i CREATE (:Person {{name: 'P' + toString(i), age: i % 100}})",
                chunk * 10_000,
                chunk * 10_000 + 9_999
            ),
            &Params::new(),
        )
        .unwrap();
    }
    let queries = [
        (
            "range, 2% of the nodes",
            "MATCH (p:Person) WHERE p.age > 97 RETURN count(p)",
        ),
        ("equality, 1%", "MATCH (p:Person {age: 42}) RETURN count(p)"),
        (
            "first 10 rows",
            "MATCH (p:Person) WHERE p.age = 42 RETURN p.name LIMIT 10",
        ),
    ];
    let before: Vec<_> = queries.iter().map(|(_, query)| time(&db, query)).collect();
    execute(
        &db,
        "bench",
        "CREATE INDEX FOR (p:Person) ON (p.age)",
        &Params::new(),
    )
    .unwrap();
    println!("| Query (100,000 :Person nodes) | Without index | With index |");
    println!("| --- | --- | --- |");
    for ((name, query), (ms, reads)) in queries.iter().zip(before) {
        let (indexed_ms, indexed_reads) = time(&db, query);
        println!(
            "| {name} | {ms:.1} ms, {reads} reads | {indexed_ms:.2} ms, {indexed_reads} reads |"
        );
    }
}
