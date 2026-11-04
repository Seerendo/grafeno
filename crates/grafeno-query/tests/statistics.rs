//! Phase 8.2: statistical aggregates, durations, date parts, truncation,
//! per-row VALID AT and temporal.series.

use grafeno::Grafeno;
use grafeno_query::{Params, QueryResult, execute};
use serde_json::{Value as Json, json};

const NS: &str = "s";

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

fn close(value: &Json, expected: f64) -> bool {
    (value.as_f64().unwrap() - expected).abs() < 1e-9
}

#[test]
fn statistical_aggregates() {
    let db = Grafeno::in_memory().unwrap();
    run(
        &db,
        "UNWIND [2, 4, 4, 4, 5, 5, 7, 9] AS x CREATE (:Sample {x: x, group: CASE WHEN x < 5 THEN 'low' ELSE 'high' END})",
    );
    let result = rows(
        &db,
        "MATCH (s:Sample) RETURN stdevp(s.x), stdev(s.x), variancep(s.x), variance(s.x),
                median(s.x), percentileCont(s.x, 0.25), percentileDisc(s.x, 0.5), mode(s.x),
                avg(s.x), min(s.x), max(s.x), count(DISTINCT s.x)",
    );
    let row = &result[0];
    assert!(close(&row[0], 2.0), "{row}");
    assert!(close(&row[1], (32.0f64 / 7.0).sqrt()));
    assert!(close(&row[2], 4.0));
    assert!(close(&row[3], 32.0 / 7.0));
    assert!(close(&row[4], 4.5));
    assert!(close(&row[5], 4.0));
    assert_eq!(row[6], 4);
    assert_eq!(row[7], 4);
    assert!(close(&row[8], 5.0));
    assert_eq!(
        (&row[9], &row[10], &row[11]),
        (&json!(2), &json!(9), &json!(5))
    );

    // Grouped, and filtered on an aggregate with WITH.
    assert_eq!(
        rows(
            &db,
            "MATCH (s:Sample) WITH s.group AS g, median(s.x) AS m, stdevp(s.x) AS sd
             WHERE m > 4 RETURN g, m, round(sd, 3)"
        ),
        json!([["high", 6.0, 1.658]])
    );
    // Nothing to aggregate gives null; one value has no spread.
    assert_eq!(
        rows(
            &db,
            "MATCH (s:Missing) RETURN stdev(s.x), median(s.x), mode(s.x)"
        ),
        json!([[null, null, null]])
    );
    assert_eq!(
        rows(&db, "RETURN stdev(1), variance(1)"),
        json!([[0.0, 0.0]])
    );
    assert!(
        fail(&db, "MATCH (s:Sample) RETURN percentileCont(s.x, 2)").contains("between 0 and 1")
    );
    assert!(fail(&db, "RETURN median('a')").contains("needs numbers"));
}

#[test]
fn durations_and_calendar_arithmetic() {
    let db = Grafeno::in_memory().unwrap();
    assert_eq!(
        rows(
            &db,
            "RETURN duration('P1Y2M3DT4H') AS d, duration({days: 1, hours: 12}) AS e,
                    duration('PT90M').minutes, duration('P1Y2M').monthsOfYear,
                    datetime('2024-01-31') + duration('P1M') AS feb,
                    '2024-03-01' - duration('P1D') AS last_feb,
                    duration('P1D') * 3, duration('PT1H') / 4, -duration('P1D')"
        ),
        json!([[
            "P1Y2M3DT4H",
            "P1DT12H",
            90,
            2,
            "2024-02-29T00:00:00Z",
            "2024-02-29T00:00:00Z",
            "P3D",
            "PT15M",
            "P-1D"
        ]])
    );
    assert_eq!(
        rows(
            &db,
            "WITH datetime('2023-01-15') AS a, datetime('2024-03-20T06:00:00Z') AS b
             RETURN duration.between(a, b), duration.inDays(a, b).days,
                    duration.inMonths(a, b).months, duration.inSeconds(a, b).hours, b - a > duration('P400D')"
        ),
        json!([["P1Y2M5DT6H", 430, 14, 10326, true]])
    );
    assert!(fail(&db, "RETURN duration('soon')").contains("invalid duration"));
    assert!(fail(&db, "RETURN duration('P1D').fortnights").contains("no component"));
}

#[test]
fn date_parts_and_grouping_by_period() {
    let db = Grafeno::in_memory().unwrap();
    assert_eq!(
        rows(
            &db,
            "WITH datetime('2024-12-30T13:45:30Z') AS t
             RETURN t.year, t.quarter, t.month, t.day, t.week, t.weekYear, t.dayOfWeek, t.hour,
                    date.truncate('month', t), datetime.truncate('week', t), date(t)"
        ),
        json!([[
            2024,
            4,
            12,
            30,
            1,
            2025,
            1,
            13,
            "2024-12-01T00:00:00Z",
            "2024-12-30T00:00:00Z",
            "2024-12-30"
        ]])
    );
    run(
        &db,
        "UNWIND ['2024-01-05', '2024-01-20', '2024-02-11', '2024-04-02'] AS day
         CREATE (:Sale {at: day, amount: 10})",
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (s:Sale) WITH date.truncate('month', s.at) AS month, sum(s.amount) AS total
             RETURN date(month), total ORDER BY month"
        ),
        json!([["2024-01-01", 20], ["2024-02-01", 10], ["2024-04-01", 10]])
    );
    assert!(fail(&db, "RETURN datetime('2024-01-01').era").contains("no part"));
}

#[test]
fn facts_over_time() {
    let db = Grafeno::in_memory().unwrap();
    run(
        &db,
        "CREATE (ana:Person {name: 'Ana'}), (bob:Person {name: 'Bob'}),
                (acme:Company {name: 'Acme'}),
                (ana)-[:works_at {@valid_from: '2024-01-01', @valid_to: '2024-07-01'}]->(acme),
                (bob)-[:works_at {@valid_from: '2024-03-01'}]->(acme)",
    );
    // How long each fact held (open facts until a fixed date).
    assert_eq!(
        rows(
            &db,
            "MATCH (p)-[r:works_at]->() RETURN p.name,
                duration.inDays(r.@valid_from, coalesce(r.@valid_to, datetime('2024-12-31'))).days AS days
             ORDER BY p.name"
        ),
        json!([["Ana", 182], ["Bob", 305]])
    );
    // People at Acme on the first day of each quarter: VALID AT per row.
    assert_eq!(
        rows(
            &db,
            "CALL temporal.series('2024-01-01', '2024-12-31', 'P3M') YIELD at
             OPTIONAL MATCH (p)-[:works_at]->(:Company {name: 'Acme'}) VALID AT at
             RETURN date(at), count(p) ORDER BY at"
        ),
        json!([
            ["2024-01-01", 1],
            ["2024-04-01", 2],
            ["2024-07-01", 1],
            ["2024-10-01", 1]
        ])
    );
    // Month steps stay on the calendar.
    assert_eq!(
        rows(
            &db,
            "CALL temporal.series('2024-01-31', '2024-04-30', 'P1M') YIELD at RETURN collect(date(at))"
        ),
        json!([[["2024-01-31", "2024-02-29", "2024-03-31", "2024-04-30"]]])
    );
    assert!(
        fail(
            &db,
            "CALL temporal.series('2024-01-01', '2025-01-01', 'PT1S')"
        )
        .contains("more than 10000")
    );
    assert!(
        fail(
            &db,
            "CALL temporal.series('2024-01-01', '2025-01-01', 'P-1D')"
        )
        .contains("positive")
    );
}

#[test]
fn math_functions() {
    let db = Grafeno::in_memory().unwrap();
    let row = rows(
        &db,
        "RETURN round(2.71828, 2), log(e()), log10(1000), exp(0), sign(-4), sign(0), round(sqrt(2), 3), log(0)",
    );
    assert_eq!(row, json!([[2.72, 1.0, 3.0, 1.0, -1, 0, 1.414, null]]));
    assert!(rows(&db, "RETURN timestamp()")[0][0].as_i64().unwrap() > 1_700_000_000_000);
}
