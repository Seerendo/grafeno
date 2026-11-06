//! Phase 12.4: batched and parallel traversal. Patterns grow from batches
//! of anchors one step at a time, reading each node once; large frontiers
//! go on in slices, across threads. The rows and their order must equal
//! growing one anchor at a time, which a row cap (`LIMIT`) still does.

mod common;

use grafeno::Grafeno;
use grafeno_query::{Limits, Params, QueryResult, execute, execute_read_with, set_parallelism};
use serde_json::{Value as Json, json};

const NS: &str = "t";

fn run(db: &Grafeno, query: &str) -> QueryResult {
    let result =
        execute(db, NS, query, &Params::new()).unwrap_or_else(|e| panic!("{query}\n→ {e}"));
    // Split across members, a read gives the same rows (13.4).
    common::check_split(db, NS, query, &Params::new());
    result
}

fn rows(db: &Grafeno, query: &str) -> Json {
    run(db, query).to_json()["rows"].clone()
}

/// The rows of `query`, and the same with a row cap so large it changes
/// nothing but makes the match grow one anchor at a time.
fn both_ways(db: &Grafeno, query: &str) -> (Json, Json) {
    (
        rows(db, query),
        rows(db, &format!("{query} LIMIT 100000000")),
    )
}

/// 600 people at 30 companies, each knowing four others (one a colleague).
fn town() -> Grafeno {
    let db = Grafeno::in_memory().unwrap();
    run(
        &db,
        "UNWIND range(0, 29) AS i CREATE (:Company {name: 'c' + toString(i), size: i % 4})",
    );
    run(
        &db,
        "UNWIND range(0, 599) AS i
         MATCH (c:Company {name: 'c' + toString(i % 30)})
         CREATE (:Person {i: i, team: i % 5})-[:works_at {since: 2000 + i % 20}]->(c)",
    );
    run(
        &db,
        "MATCH (p:Person), (q:Person)
         WHERE q.i = (p.i * 7 + 1) % 600 OR q.i = (p.i * 13 + 5) % 600 OR q.i = (p.i + 1) % 600
            OR q.i = (p.i + 30) % 600
         CREATE (p)-[:knows]->(q)",
    );
    db
}

#[test]
fn batched_rows_equal_one_anchor_at_a_time() {
    let db = town();
    for query in [
        // Two hops through shared nodes.
        "MATCH (p:Person)-[:works_at]->(c)<-[:works_at]-(q) WHERE p.i < 40 RETURN p.i, q.i",
        // Anchored in the middle: steps both ways.
        "MATCH (p:Person)-[:works_at]->(c:Company {name: 'c3'})<-[:works_at]-(q)-[:knows]->(f) RETURN p.i, q.i, f.i",
        // Edge variables and properties, and a target filtered by an earlier one.
        "MATCH (p:Person)-[w:works_at]->(c)<-[v:works_at]-(q:Person {team: p.team}) WHERE w.since > 2015 RETURN p.i, q.i, v.since",
        // Edge uniqueness inside a pattern: two people who know the same one.
        "MATCH (a:Person)-[:knows]->(b)<-[:knows]-(c) WHERE a.i < 50 RETURN a.i, b.i, c.i",
        // Variable length with a path and its edges.
        "MATCH p = (a:Person)-[r:knows*1..3]->(b) WHERE a.i < 15 RETURN a.i, b.i, length(p), size(r)",
        // Leftwards variable length.
        "MATCH (b:Person)<-[:knows*2]-(a:Person {i: 7}) RETURN b.i",
        // A node bound by an earlier clause.
        "MATCH (c:Company {name: 'c5'}) MATCH (c)<-[:works_at]-(p)-[:knows]->(q)-[:works_at]->(c) RETURN p.i, q.i",
        // A filter on whole rows.
        "MATCH (p:Person)-[:knows]->(q)-[:knows]->(r) WHERE p.i + r.i = 600 RETURN p.i, q.i, r.i",
    ] {
        let (batched, single) = both_ways(&db, query);
        assert_eq!(batched, single, "{query}");
        assert!(!batched.as_array().unwrap().is_empty(), "{query}");
    }
    // Writes see their own changes while batching.
    assert_eq!(
        rows(
            &db,
            "MATCH (c:Company {name: 'c0'})<-[:works_at]-(p) SET p.seen = true
             WITH count(p) AS n MATCH (q:Person {seen: true})-[:works_at]->(c) RETURN n, count(q), collect(DISTINCT c.name)"
        ),
        json!([[20, 20, ["c0"]]])
    );
}

#[test]
fn shared_nodes_are_read_once() {
    let db = town();
    // 600 people × 19 colleagues each (an edge is used once per match, so
    // not themselves), from shared reads, not one read per row.
    let profile = run(
        &db,
        "PROFILE MATCH (p:Person)-[:works_at]->(c)<-[:works_at]-(q) WHERE p.i < 600 RETURN count(*)",
    );
    assert_eq!(profile.to_json()["rows"], json!([[11400]]));
    let reads: u64 = profile
        .profile
        .unwrap()
        .iter()
        .filter_map(|step| step.reads)
        .sum();
    // 600 people and their edges, 30 companies' edges, each person decoded
    // once more as a colleague: a few thousand reads for 11 400 rows.
    assert!(reads < 4_000, "{reads} reads");
}

#[test]
fn large_frontiers_go_on_in_slices_and_threads() {
    let db = Grafeno::in_memory().unwrap();
    // One hub reaching 6 000 nodes, each reaching two more.
    run(&db, "CREATE (:Hub)");
    run(
        &db,
        "MATCH (h:Hub) UNWIND range(0, 5999) AS i
         CREATE (h)-[:to]->(x:X {i: i})-[:to]->(:Y {i: i * 2}), (x)-[:to]->(:Y {i: i * 2 + 1})",
    );
    let query = "MATCH (h:Hub)-[:to]->(x)-[:to]->(y) RETURN x.i, y.i";
    let single = rows(&db, &format!("{query} LIMIT 100000000"));
    assert_eq!(single.as_array().unwrap().len(), 12_000);
    let read = db.begin_read().unwrap();
    for threads in [1, 4] {
        set_parallelism(threads);
        let result = execute_read_with(&read, NS, query, &Params::new(), &Limits::none()).unwrap();
        assert_eq!(result.to_json()["rows"], single, "{threads} thread(s)");
        let counted = execute_read_with(
            &read,
            NS,
            "MATCH (h:Hub)-[:to]->(x)-[:to]->(y) RETURN count(*), sum(y.i)",
            &Params::new(),
            &Limits::none(),
        )
        .unwrap();
        assert_eq!(counted.to_json()["rows"], json!([[12000, 71994000]]));
    }
    set_parallelism(0);
    // Rows held are still limited across slices.
    let error = execute_read_with(
        &read,
        NS,
        query,
        &Params::new(),
        &Limits {
            max_rows: Some(5_000),
            ..Limits::none()
        },
    )
    .unwrap_err();
    assert!(error.limit, "{error}");
}
