//! Phase 12.3: batched aggregation. A MATCH followed by an aggregating
//! RETURN or WITH folds its rows into groups as it makes them, in batches
//! turned into columns; the results equal row-at-a-time aggregation and no
//! rows are held.

use grafeno::Grafeno;
use grafeno_query::{
    Limits, Params, QueryResult, execute, execute_read_with, execute_with, set_parallelism,
};
use serde_json::{Value as Json, json};

const NS: &str = "b";

fn run(db: &Grafeno, query: &str) -> QueryResult {
    execute(db, NS, query, &Params::new()).unwrap_or_else(|e| panic!("{query}\n→ {e}"))
}

fn rows(db: &Grafeno, query: &str) -> Json {
    run(db, query).to_json()["rows"].clone()
}

fn fail(db: &Grafeno, query: &str) -> String {
    execute(db, NS, query, &Params::new())
        .err()
        .unwrap_or_else(|| panic!("{query} should fail"))
        .to_string()
}

/// Ten people in three teams, with ages, scores and a few nulls.
fn team() -> Grafeno {
    let db = Grafeno::in_memory().unwrap();
    run(
        &db,
        "UNWIND [
           {n: 'a', team: 'red', age: 30, score: 1.5},
           {n: 'b', team: 'red', age: 30, score: 2.5},
           {n: 'c', team: 'red', age: 40, score: null},
           {n: 'd', team: 'blue', age: 20, score: 1.0},
           {n: 'e', team: 'blue', age: 25, score: 1},
           {n: 'f', team: 'green', age: null, score: 3.0},
           {n: 'g', team: 'blue', age: 35, score: 4.0},
           {n: 'h', team: 'red', age: 50, score: 0.5},
           {n: 'i', team: 'green', age: 60, score: null},
           {n: 'j', team: 'blue', age: 20, score: 2.0}
         ] AS p
         CREATE (:Person {name: p.n, team: p.team, age: p.age, score: p.score})",
    );
    db
}

#[test]
fn every_aggregate_folds_to_the_same_value() {
    let db = team();
    assert_eq!(
        rows(
            &db,
            "MATCH (p:Person) RETURN p.team, count(*), count(p.age), count(DISTINCT p.age),
               sum(p.age), avg(p.age), min(p.age), max(p.age), collect(p.name)"
        ),
        json!([
            ["red", 4, 4, 3, 150, 37.5, 30, 50, ["a", "b", "c", "h"]],
            ["blue", 4, 4, 3, 100, 25.0, 20, 35, ["d", "e", "g", "j"]],
            ["green", 2, 1, 1, 60, 60.0, 60, 60, ["f", "i"]]
        ])
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (p:Person) RETURN sum(p.score), stdev(p.age), median(p.age),
               percentileDisc(p.age, 0.5), percentileCont(p.age, 0.25), mode(p.age),
               collect(DISTINCT p.team), sum(DISTINCT p.age)"
        ),
        json!([[
            15.5,
            13.565683830083088,
            30.0,
            30,
            25.0,
            20,
            ["red", "blue", "green"],
            260
        ]])
    );
    // Aggregates inside expressions, and the same aggregate twice.
    assert_eq!(
        rows(
            &db,
            "MATCH (p:Person) RETURN p.team AS team, count(*) * 10 + max(p.age) AS mix, count(*) AS n
             ORDER BY mix DESC"
        ),
        json!([["red", 90, 4], ["green", 80, 2], ["blue", 75, 4]])
    );
    // min keeps the first of equal values, max the last (1 and 1.0).
    assert_eq!(
        rows(
            &db,
            "MATCH (p:Person) WHERE p.name IN ['d', 'e'] RETURN min(p.score), max(p.score)"
        ),
        json!([[1.0, 1]])
    );
}

#[test]
fn empty_groups_and_errors_behave_as_before() {
    let db = team();
    // Nothing matched: one row without grouping keys, none with them.
    assert_eq!(
        rows(
            &db,
            "MATCH (p:Person {team: 'none'}) RETURN count(*), sum(p.age), avg(p.age), min(p.age), collect(p.name)"
        ),
        json!([[0, 0, null, null, []]])
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (p:Person {team: 'none'}) RETURN p.team, count(*)"
        ),
        json!([])
    );
    assert!(fail(&db, "MATCH (p:Person) RETURN sum(p.name)").contains("needs numbers"));
    run(
        &db,
        "CREATE (:Big {v: 9223372036854775807}), (:Big {v: 1}), (:Big {v: -5})",
    );
    assert!(fail(&db, "MATCH (b:Big) RETURN sum(b.v)").contains("overflow"));
    // Durations add up; mixing them with numbers is an error.
    assert_eq!(
        rows(
            &db,
            "UNWIND [duration('P1D'), duration('PT12H')] AS d RETURN toString(sum(d)), toString(avg(d))"
        ),
        json!([["P1DT12H", "PT18H"]])
    );
    // count(*) outside a projection is still refused.
    assert!(fail(&db, "MATCH (p:Person) WHERE count(*) > 1 RETURN p").contains("aggregate"));
}

#[test]
fn with_star_order_and_filters_after_folding() {
    let db = team();
    assert_eq!(
        rows(
            &db,
            "MATCH (p:Person) WITH p.team AS team, avg(p.age) AS age WHERE age > 30
             RETURN team, age ORDER BY team"
        ),
        json!([["green", 60.0], ["red", 37.5]])
    );
    // `*` groups by every variable.
    assert_eq!(
        rows(
            &db,
            "MATCH (p:Person {name: 'a'}) WITH p, count(*) AS n RETURN p.name, n"
        ),
        json!([["a", 1]])
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (p:Person) RETURN p.team, count(*) AS n ORDER BY n DESC, p.team LIMIT 2"
        ),
        json!([["blue", 4], ["red", 4]])
    );
    // PROFILE counts the rows the match folded.
    let profile = run(&db, "PROFILE MATCH (p:Person) RETURN count(*)")
        .profile
        .unwrap();
    assert_eq!(profile[0].rows, Some(10));
}

#[test]
fn folding_holds_no_rows() {
    let db = Grafeno::in_memory().unwrap();
    run(
        &db,
        "UNWIND range(1, 300) AS i CREATE (:N {i: i, g: i % 7})",
    );
    // A batch (up to 1 024 rows) is the most a fold holds at once.
    let limited = Limits {
        max_rows: Some(2_000),
        ..Limits::none()
    };
    // 90 000 rows counted with at most 2 000 held.
    let counted = execute_with(
        &db,
        NS,
        "MATCH (a:N), (b:N) RETURN count(*), sum(a.i)",
        &Params::new(),
        &limited,
    )
    .unwrap();
    assert_eq!(counted.to_json()["rows"], json!([[90000, 13545000]]));
    let grouped = execute_with(
        &db,
        NS,
        "MATCH (a:N), (b:N) RETURN a.g, count(*) ORDER BY a.g",
        &Params::new(),
        &limited,
    )
    .unwrap();
    assert_eq!(grouped.rows.len(), 7);
    // Returned whole, the same rows go over the limit.
    let error = execute_with(
        &db,
        NS,
        "MATCH (a:N), (b:N) RETURN a.i, b.i",
        &Params::new(),
        &limited,
    )
    .unwrap_err();
    assert!(error.limit, "{error}");
}

#[test]
fn parallel_folding_equals_one_thread() {
    let db = Grafeno::in_memory().unwrap();
    run(
        &db,
        "UNWIND range(0, 9999) AS i CREATE (:P {i: i, city: 'c' + toString(i % 37), age: i % 90, w: toFloat(i) / 7.0})",
    );
    let queries = [
        "MATCH (p:P) RETURN p.city, count(*), sum(p.w), avg(p.age), min(p.w), max(p.i)",
        "MATCH (p:P) WHERE p.age > 40 RETURN p.city, collect(p.i)",
        "MATCH (p:P) RETURN sum(p.w), stdev(p.w), percentileCont(p.age, 0.9)",
    ];
    let read = db.begin_read().unwrap();
    let limited = Limits {
        max_rows: Some(2_000),
        ..Limits::none()
    };
    set_parallelism(1);
    let serial: Vec<_> = queries
        .iter()
        .map(|query| execute_read_with(&read, NS, query, &Params::new(), &limited).unwrap())
        .collect();
    set_parallelism(4);
    for (query, one) in queries.iter().zip(&serial) {
        let many = execute_read_with(&read, NS, query, &Params::new(), &limited).unwrap();
        // Same groups, same order, same floating-point sums.
        assert_eq!(many.rows, one.rows, "{query}");
    }
    set_parallelism(0);
}
