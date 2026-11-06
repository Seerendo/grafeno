//! Phase 12.2: planner statistics (value distributions per index, edge
//! degrees by direction), cost-based starts, the order of the patterns of
//! a MATCH, the statement cache and parallel scans.

mod common;

use grafeno::{Grafeno, IndexRange};
use grafeno_query::{
    Limits, Params, QueryResult, Value, execute, execute_read_with, execute_with, set_parallelism,
    statement_cache_stats,
};
use serde_json::{Value as Json, json};

const NS: &str = "p";

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

/// The details of the first step of a plan.
fn plan(db: &Grafeno, query: &str) -> String {
    run(db, &format!("EXPLAIN {query}")).to_json()["rows"][0][2]
        .as_str()
        .unwrap()
        .to_owned()
}

/// 6,000 people: 99% active, half of them in one big city; 100
/// companies, everyone working at one; one owner of three companies.
fn skewed() -> Grafeno {
    let db = Grafeno::in_memory().unwrap();
    for key in ["status", "city", "age"] {
        run(&db, &format!("CREATE INDEX FOR (p:Person) ON (p.{key})"));
    }
    run(
        &db,
        "UNWIND range(0, 99) AS i CREATE (:Company {name: 'c' + toString(i)})",
    );
    run(
        &db,
        "CREATE (o:Owner {name: 'x'}) WITH o
         MATCH (c:Company) WHERE c.name IN ['c1', 'c2', 'c3'] CREATE (o)-[:owns]->(c)",
    );
    run(
        &db,
        "UNWIND range(0, 5999) AS i
         MATCH (c:Company {name: 'c' + toString(i % 100)})
         CREATE (:Person {
             uid: i,
             status: CASE WHEN i % 100 = 0 THEN 'blocked' ELSE 'active' END,
             city: CASE WHEN i % 2 = 0 THEN 'big' ELSE 'city' + toString(i % 50) END,
             age: i % 90,
             name: 'person ' + toString(i)
         })-[:works_at]->(c)",
    );
    db
}

#[test]
fn statistics_describe_values_and_degrees() {
    let db = skewed();
    let stats = db.begin_read().unwrap().analyze(NS).unwrap();
    let status = stats.index("Person_status").unwrap();
    assert_eq!((status.entries, status.distinct), (6000, 2));
    assert_eq!(status.common[0], (json!("active"), 5940));
    assert_eq!(status.common[1], (json!("blocked"), 60));
    assert_eq!(status.estimate(&IndexRange::Equal(json!("blocked"))), 60.0);
    // Every value is listed, so one that is not there finds nothing.
    assert_eq!(status.estimate(&IndexRange::Equal(json!("gone"))), 0.0);
    let city = stats.index("Person_city").unwrap();
    assert_eq!(city.common[0], (json!("big"), 3000));
    // A range from the buckets: ages 0 to 89, evenly.
    let age = stats.index("Person_age").unwrap();
    let everyone = age.estimate(&IndexRange::Between {
        lower: std::ops::Bound::Included(json!(0)),
        upper: std::ops::Bound::Unbounded,
    });
    assert!(everyone > 5500.0, "{everyone}");
    let few = age.estimate(&IndexRange::Between {
        lower: std::ops::Bound::Included(json!(85)),
        upper: std::ops::Bound::Unbounded,
    });
    assert!((200.0..800.0).contains(&few), "{few}");
    // Degrees by direction: one employer per person, sixty people per company.
    let works = stats.edge_label("works_at").unwrap();
    assert_eq!(
        (works.edges, works.sources, works.targets),
        (6000, 6000, 100)
    );
    assert_eq!(works.out_degree(), 1.0);
    assert_eq!(works.in_degree(), 60.0);

    // The same through the procedure.
    let analyzed = rows(
        &db,
        "CALL db.analyze() YIELD kind, name, count, out_degree, in_degree
         WHERE name IN ['Person_status', 'works_at'] RETURN kind, name, count, out_degree, in_degree",
    );
    assert_eq!(
        analyzed,
        json!([
            ["index", "Person_status", 6000, null, null],
            ["edge", "works_at", 6000, 1.0, 60.0]
        ])
    );
}

#[test]
fn starts_follow_the_value_distribution() {
    let db = skewed();
    // Written order does not matter: the rare value starts the match.
    for query in [
        "MATCH (p:Person) WHERE p.city = 'big' AND p.status = 'blocked' RETURN count(*)",
        "MATCH (p:Person) WHERE p.status = 'blocked' AND p.city = 'big' RETURN count(*)",
    ] {
        assert!(
            plan(&db, query).contains("index Person_status"),
            "{query}: {}",
            plan(&db, query)
        );
        assert_eq!(rows(&db, query), json!([[60]]));
    }
    // A small city beats the common status.
    let query = "MATCH (p:Person) WHERE p.status = 'active' AND p.city = 'city7' RETURN count(*)";
    assert!(
        plan(&db, query).contains("index Person_city"),
        "{}",
        plan(&db, query)
    );
    assert_eq!(rows(&db, query), json!([[120]]));
    // Parameters are looked up in the statistics too.
    let mut params = Params::new();
    params.insert("s".into(), Value::Str("blocked".into()));
    params.insert("c".into(), Value::Str("big".into()));
    let explained = execute(
        &db,
        NS,
        "EXPLAIN MATCH (p:Person) WHERE p.city = $c AND p.status = $s RETURN p",
        &params,
    )
    .unwrap()
    .to_json();
    assert!(
        explained["rows"][0][2]
            .as_str()
            .unwrap()
            .contains("index Person_status"),
        "{explained}"
    );
    // A range that holds everyone loses to a label of a hundred companies.
    let query =
        "MATCH (p:Person)-[:works_at]->(c:Company {name: 'c7'}) WHERE p.age >= 0 RETURN count(*)";
    assert!(
        plan(&db, query).starts_with("start at (c:Company"),
        "{}",
        plan(&db, query)
    );
    assert_eq!(rows(&db, query), json!([[60]]));
    // A narrow range wins over the label.
    let query = "MATCH (p:Person) WHERE p.age >= 89 RETURN count(*)";
    assert!(
        plan(&db, query).contains("index Person_age"),
        "{}",
        plan(&db, query)
    );
    assert_eq!(rows(&db, query), json!([[66]]));
}

#[test]
fn patterns_run_cheapest_first() {
    let db = skewed();
    let query =
        "MATCH (p:Person)-[:works_at]->(c), (o:Owner {name: 'x'})-[:owns]->(c) RETURN count(*)";
    let explained = plan(&db, query);
    assert!(
        explained.starts_with("start at (o:Owner {name})"),
        "{explained}"
    );
    assert!(
        explained.contains("start at (c) (the bound node)"),
        "{explained}"
    );
    assert_eq!(rows(&db, query), json!([[180]]));
    // The same rows as the order written by hand.
    assert_eq!(
        rows(
            &db,
            "MATCH (o:Owner {name: 'x'})-[:owns]->(c) MATCH (p:Person)-[:works_at]->(c) RETURN count(*)"
        ),
        json!([[180]])
    );
    // Patterns that share nothing still give every combination.
    assert_eq!(
        rows(
            &db,
            "MATCH (p:Person {uid: 5}), (o:Owner) RETURN p.uid, o.name"
        ),
        json!([[5, "x"]])
    );
}

#[test]
fn statistics_follow_the_data() {
    let db = Grafeno::in_memory().unwrap();
    run(&db, "CREATE INDEX FOR (t:Task) ON (t.state)");
    run(
        &db,
        "UNWIND range(1, 200) AS i CREATE (:Task {state: 'open'})",
    );
    let stats = db.begin_read().unwrap().planner_stats(NS).unwrap().unwrap();
    assert_eq!(stats.index("Task_state").unwrap().common[0].1, 200);
    // Far more data: the next plan rebuilds them.
    run(
        &db,
        "UNWIND range(1, 2000) AS i CREATE (:Task {state: 'done'})",
    );
    let stats = db.begin_read().unwrap().planner_stats(NS).unwrap().unwrap();
    let state = stats.index("Task_state").unwrap();
    assert_eq!(state.entries, 2200);
    assert_eq!(state.common[0], (json!("done"), 2000));
    // A new index rebuilds them too.
    run(&db, "CREATE INDEX FOR (t:Task) ON (t.owner)");
    let stats = db.begin_read().unwrap().planner_stats(NS).unwrap().unwrap();
    assert!(stats.index("Task_owner").is_some());
    // A dropped index is not planned with, even for a statement parsed before.
    let query = "MATCH (t:Task) WHERE t.state = 'open' RETURN count(*)";
    assert!(plan(&db, query).contains("index Task_state"));
    assert_eq!(rows(&db, query), json!([[200]]));
    run(&db, "DROP INDEX Task_state");
    assert!(
        plan(&db, query).contains("scan of :Task"),
        "{}",
        plan(&db, query)
    );
    assert_eq!(rows(&db, query), json!([[200]]));
}

#[test]
fn repeated_statements_are_parsed_once() {
    let db = skewed();
    let query = "MATCH (p:Person {uid: $u})-[:works_at]->(c) RETURN p.name AS cached_name, c.name";
    let before = statement_cache_stats();
    for uid in [3, 1234, 5999] {
        let mut params = Params::new();
        params.insert("u".into(), Value::Int(uid));
        let result = execute(&db, NS, query, &params).unwrap().to_json();
        assert_eq!(
            result["rows"],
            json!([[format!("person {uid}"), format!("c{}", uid % 100)]])
        );
    }
    let after = statement_cache_stats();
    // Other tests share the cache: at least two hits came from here.
    assert!(after.hits >= before.hits + 2, "{before:?} → {after:?}");
    // Syntax errors are reported every time, never cached as statements.
    for _ in 0..2 {
        assert!(execute(&db, NS, "MATCH (p RETURN p", &Params::new()).is_err());
    }
}

#[test]
fn parallel_scans_return_what_one_thread_does() {
    let db = skewed();
    let queries = [
        // A label scan.
        "MATCH (p:Person)-[:works_at]->(c) WHERE p.name CONTAINS '7' RETURN p.uid, c.name",
        // An index range.
        "MATCH (p:Person)-[:works_at]->(c) WHERE p.age >= 5 AND p.name ENDS WITH '3' RETURN p.uid, c.name",
        // Aggregates over it, and a pattern predicate inside it.
        "MATCH (p:Person) WHERE (p)-[:works_at]->(:Company {name: 'c3'}) RETURN count(*), sum(p.age)",
    ];
    let read = db.begin_read().unwrap();
    let mut serial = Vec::new();
    set_parallelism(1);
    for query in queries {
        serial.push(execute_read_with(&read, NS, query, &Params::new(), &Limits::none()).unwrap());
    }
    set_parallelism(4);
    for (query, one) in queries.iter().zip(&serial) {
        assert!(
            common::check_split(&db, NS, query, &Params::new()),
            "{query}"
        );
        let many = execute_read_with(&read, NS, query, &Params::new(), &Limits::none()).unwrap();
        assert_eq!(many.rows, one.rows, "{query}");
        assert!(!many.rows.is_empty(), "{query}");
    }
    // Limits count every thread's reads.
    let limited = Limits {
        max_reads: Some(2_000),
        ..Limits::none()
    };
    let error = execute_with(&db, NS, queries[0], &Params::new(), &limited).unwrap_err();
    assert!(error.limit, "{error}");
    set_parallelism(0);
}

#[test]
fn split_queries_return_what_one_server_does() {
    // Every planner query above, and these, split in 2, 3 and 5 parts.
    let db = skewed();
    let queries = [
        "MATCH (p:Person) RETURN p.city, count(*), sum(p.age), avg(p.age), min(p.uid), max(p.uid)",
        "MATCH (p:Person)-[:works_at]->(c) RETURN c.name, collect(p.uid)[0..3], count(DISTINCT p.city)",
        "MATCH (p:Person) WHERE p.age > 40 RETURN p.uid ORDER BY p.age DESC, p.uid LIMIT 7",
        "MATCH (p:Person) RETURN DISTINCT p.city ORDER BY p.city",
        "MATCH (p:Person) RETURN p.uid SKIP 10 LIMIT 5",
        "MATCH (p:Person) WITH p, p.age * 2 AS twice WHERE twice > 50 UNWIND [1, 2] AS k RETURN k, count(*), sum(twice)",
        "MATCH (p:Person) OPTIONAL MATCH (p)-[:owns]->(c) RETURN count(c), count(*)",
        "MATCH (p:Person) RETURN stdev(p.age), percentileCont(p.age, 0.9), sum(DISTINCT p.age), sum(p.age * 0.1)",
        "MATCH (n) RETURN labels(n)[0] AS label, count(*) ORDER BY label",
        "MATCH (p:Person {status: 'blocked'})-[:works_at]->(c) RETURN p.uid, c.name ORDER BY p.uid",
    ];
    for query in queries {
        assert!(
            common::check_split(&db, NS, query, &Params::new()),
            "{query}"
        );
    }
    // Parts large enough to scan on several threads themselves.
    let big = Grafeno::in_memory().unwrap();
    run(
        &big,
        "UNWIND range(1, 10000) AS i CREATE (:N {i: i, g: i % 7, f: i * 0.1})",
    );
    set_parallelism(4);
    for query in [
        "MATCH (n:N) RETURN n.g, count(*), sum(n.f), collect(n.i)[0..5] ORDER BY n.g",
        "MATCH (n:N) WHERE n.i % 3 = 0 RETURN n.i, n.f",
        "MATCH (n:N) RETURN n.i ORDER BY n.g DESC LIMIT 20",
    ] {
        assert!(
            common::check_split(&big, NS, query, &Params::new()),
            "{query}"
        );
    }
    set_parallelism(0);
    // Not split: writes, unions, aggregating WITHs, randomness.
    for query in [
        "MATCH (p:Person) SET p.seen = true",
        "MATCH (p:Person) RETURN p.uid UNION MATCH (c:Company) RETURN c.name AS uid",
        "MATCH (p:Person) WITH count(*) AS n RETURN n",
        "MATCH (p:Person) RETURN rand() < 2",
        "RETURN 1",
    ] {
        assert!(
            !common::check_split(&db, NS, query, &Params::new()),
            "{query}"
        );
    }
}
