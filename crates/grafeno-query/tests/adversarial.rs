//! Costly and hostile queries: each must finish within its limits, with a
//! result or a `query_limit` error, and never crash or run away.

use std::time::{Duration, Instant};

use grafeno::{Grafeno, NewEdge, NewNode};
use grafeno_query::{Limits, Params, execute_with};

const NS: &str = "hostile";

fn world() -> Grafeno {
    let db = Grafeno::in_memory().unwrap();
    db.write(|tx| {
        let mut ids = Vec::new();
        for i in 0..2_000u64 {
            ids.push(
                tx.create_node(
                    NS,
                    NewNode::new("N")
                        .prop("i", i)
                        .prop("name", format!("node {i}")),
                )?,
            );
        }
        for (i, id) in ids.iter().enumerate() {
            for step in [1, 7, 31] {
                tx.create_edge(NS, NewEdge::new(*id, "link", ids[(i + step) % ids.len()]))?;
            }
        }
        Ok(())
    })
    .unwrap();
    db
}

fn limits() -> Limits {
    Limits {
        timeout: Some(Duration::from_secs(2)),
        max_reads: Some(2_000_000),
        max_rows: Some(200_000),
        max_memory: Some(64 * 1024 * 1024),
        ..Limits::standard()
    }
}

/// Runs a query and says how it ended: "ok", "limit: …" or "error: …".
fn run(db: &Grafeno, query: &str) -> (String, Duration) {
    let started = Instant::now();
    let outcome = match execute_with(db, NS, query, &Params::new(), &limits()) {
        Ok(_) => "ok".to_owned(),
        Err(error) if error.limit => format!("limit: {}", error.message),
        Err(error) => format!(
            "error: {}",
            error.message.chars().take(80).collect::<String>()
        ),
    };
    (outcome, started.elapsed())
}

fn hostile() -> Vec<(&'static str, String)> {
    let deep_parens = format!("RETURN {}1{}", "(".repeat(5_000), ")".repeat(5_000));
    let deep_lists = format!("RETURN {}1{}", "[".repeat(5_000), "]".repeat(5_000));
    let long_and = format!("RETURN {}", vec!["1 = 1"; 50_000].join(" AND "));
    let long_plus = format!("RETURN {}", vec!["1"; 50_000].join(" + "));
    let deep_not = format!("RETURN {}true", "NOT ".repeat(20_000));
    let deep_minus = format!("RETURN {}1", "- ".repeat(20_000));
    let deep_case = format!(
        "RETURN {}1{}",
        "CASE WHEN true THEN ".repeat(3_000),
        " END".repeat(3_000)
    );
    let big_list = format!("RETURN size([{}])", vec!["1"; 300_000].join(","));
    vec![
        ("cartesian product", "MATCH (a), (b), (c) RETURN count(*)".into()),
        ("long variable paths", "MATCH (a)-[*1..10]-(b) RETURN count(*)".into()),
        ("unwind squared", "UNWIND range(1, 1000000) AS x UNWIND range(1, 1000000) AS y RETURN count(*)".into()),
        ("string built in reduce", "RETURN size(reduce(s = '', x IN range(1, 1000000) | s + 'abcdefghij'))".into()),
        ("nested comprehension", "RETURN size([x IN range(1, 1000000) | [y IN range(1, 1000) | y]])".into()),
        ("string doubled", "RETURN size(reduce(s = 'x', i IN range(1, 40) | s + s))".into()),
        ("replace amplified", "WITH reduce(s = 'a', i IN range(1, 20) | s + s) AS s RETURN size(replace(s, 'a', s))".into()),
        ("list nested in reduce", "RETURN size(reduce(l = [], x IN range(1, 100000) | [l]))".into()),
        ("map nested in reduce", "RETURN size(keys(reduce(m = {}, x IN range(1, 100000) | {inner: m})))".into()),
        ("a range per item", "RETURN size([x IN range(1, 1000) | range(1, 1000000)])".into()),
        ("list grown in reduce","RETURN size(reduce(l = [], x IN range(1, 100000) | l + range(1, 1000)))".into()),
        ("collect then square", "MATCH (n) WITH collect(n) AS all UNWIND all AS a UNWIND all AS b RETURN count(*)".into()),
        ("sort a million", "UNWIND range(1, 1000000) AS x RETURN x ORDER BY x DESC LIMIT 1".into()),
        ("distinct a million", "UNWIND range(1, 1000000) AS x RETURN count(DISTINCT x)".into()),
        ("collect a million", "UNWIND range(1, 1000000) AS x RETURN size(collect(x))".into()),
        ("group a million", "UNWIND range(1, 1000000) AS x RETURN x % 1000 AS k, count(*) ORDER BY k LIMIT 1".into()),
        ("pattern predicate paths", "MATCH (a) WHERE EXISTS { (a)-[*1..8]-() } RETURN count(a)".into()),
        ("comprehension over matches", "MATCH (a) RETURN size([x IN range(1, 100000) | a.i + x])".into()),
        ("cartesian in optional", "MATCH (a) OPTIONAL MATCH (b), (c) RETURN count(*)".into()),
        ("union of products", "MATCH (a), (b) RETURN count(*) UNION MATCH (a), (b) RETURN count(*)".into()),
        ("foreach writes", "UNWIND range(1, 1000000) AS x FOREACH (y IN range(1, 10) | CREATE (:Junk {x: x, y: y}))".into()),
        ("merge flood", "UNWIND range(1, 1000000) AS x MERGE (:Junk {x: x})".into()),
        ("deep parentheses", deep_parens),
        ("deep lists", deep_lists),
        ("long AND chain", long_and),
        ("long + chain", long_plus),
        ("deep NOT", deep_not),
        ("deep minus", deep_minus),
        ("deep CASE", deep_case),
        ("big list literal", big_list),
        ("algorithm on everything", "CALL algo.betweenness() YIELD score RETURN count(score)".into()),
        ("all shortest paths", "MATCH (a {i: 0}), (b {i: 1999}) CALL algo.kShortestPaths(a, b, 200) YIELD cost RETURN count(cost)".into()),
    ]
}

#[test]
fn hostile_queries_stay_within_their_limits() {
    let db = world();
    let mut failures = Vec::new();
    for (name, query) in hostile() {
        // A thread of its own: a stack overflow would end the process, so
        // it shows here as a crash of the whole test.
        let db = &db;
        let (outcome, took) = std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(2 * 1024 * 1024)
                .spawn_scoped(scope, move || run(db, &query))
                .unwrap()
                .join()
                .unwrap_or_else(|_| ("panic".to_owned(), Duration::ZERO))
        });
        eprintln!("{name:28} {:>8.0?} {outcome}", took);
        // Two seconds of time limit, plus room for one step to notice.
        if took > Duration::from_secs(4) || outcome == "panic" {
            failures.push(format!("{name}: {outcome} after {took:?}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Just under the nesting limit, queries run on a thread with the stack of
/// a server worker (2 MiB), in a debug build too.
#[test]
fn nesting_just_under_the_limit_runs() {
    let db = Grafeno::in_memory().unwrap();
    let queries = [
        format!("RETURN {}1{}", "(".repeat(190), ")".repeat(190)),
        format!("RETURN size({}1{})", "[".repeat(120), "]".repeat(120)),
        format!("RETURN {}", vec!["1 = 1"; 95].join(" AND ")),
        format!("RETURN {}", vec!["1"; 190].join(" + ")),
        format!("RETURN {}true", "NOT ".repeat(190)),
        format!("RETURN {}1", "- ".repeat(190)),
        format!(
            "RETURN {}1{}",
            "CASE WHEN true THEN ".repeat(90),
            " END".repeat(90)
        ),
        format!("RETURN {}1{}", "abs(".repeat(190), ")".repeat(190)),
        format!("RETURN {}1{}", "{a: ".repeat(120), "}".repeat(120)),
        format!(
            "MATCH (n) WHERE {} RETURN count(n)",
            vec!["EXISTS { (n)--() }"; 90].join(" OR ")
        ),
    ];
    for query in queries {
        let (db, text) = (&db, &query);
        let outcome = std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(2 * 1024 * 1024)
                .spawn_scoped(scope, move || {
                    execute_with(db, NS, text, &Params::new(), &Limits::standard())
                        .map(|_| ())
                        .map_err(|error| error.message)
                })
                .unwrap()
                .join()
        });
        let outcome = outcome.unwrap_or_else(|_| panic!("crashed: {}", &query[..60]));
        assert!(outcome.is_ok(), "{outcome:?}");
    }
}

/// The total estimated cost of a query: the last step of its EXPLAIN.
fn cost(db: &Grafeno, query: &str) -> f64 {
    let plan = execute_with(
        db,
        NS,
        &format!("EXPLAIN {query}"),
        &Params::new(),
        &Limits::none(),
    )
    .unwrap();
    let last = plan.rows.last().unwrap();
    last[4].to_json().as_f64().unwrap()
}

#[test]
fn explain_estimates_rank_queries_and_max_cost_refuses_the_costly() {
    let db = world();
    execute_with(
        &db,
        NS,
        "CREATE INDEX FOR (n:N) ON (n.i)",
        &Params::new(),
        &Limits::none(),
    )
    .unwrap();
    let lookup = cost(&db, "MATCH (n:N {i: 7}) RETURN n.name");
    let scan = cost(&db, "MATCH (n:N) RETURN n.name");
    let hop = cost(&db, "MATCH (n:N {i: 7})-[:link]->(m) RETURN m.name");
    let paths = cost(&db, "MATCH (a)-[*1..10]-(b) RETURN count(*)");
    let product = cost(&db, "MATCH (a), (b), (c) RETURN count(*)");
    let unwind = cost(
        &db,
        "UNWIND range(1, 1000000) AS x UNWIND range(1, 1000000) AS y RETURN count(*)",
    );
    let reduce = cost(&db, "RETURN reduce(s = 0, x IN range(1, 1000000) | s + x)");
    eprintln!(
        "lookup {lookup}, hop {hop}, scan {scan}, paths {paths}, product {product}, unwind {unwind}, reduce {reduce}"
    );
    assert!(lookup < scan && hop < scan, "an index lookup beats a scan");
    assert!(scan < 10_000.0);
    assert!(
        product > 1e9 && paths > 1e9 && unwind > 1e11,
        "products and long paths stand out"
    );
    assert!(reduce > 1e6 && reduce < 1e8);

    let limits = Limits {
        max_cost: Some(1e8),
        ..Limits::none()
    };
    let refused = execute_with(
        &db,
        NS,
        "MATCH (a), (b), (c) RETURN count(*)",
        &Params::new(),
        &limits,
    )
    .unwrap_err();
    assert!(
        refused.limit && refused.message.contains("max_cost"),
        "{}",
        refused.message
    );
    let fine = execute_with(
        &db,
        NS,
        "MATCH (n:N {i: 7})-[:link]->(m) RETURN m.name",
        &Params::new(),
        &limits,
    )
    .unwrap();
    assert_eq!(fine.rows.len(), 3);
}
