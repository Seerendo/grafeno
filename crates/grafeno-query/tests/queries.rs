use grafeno::Grafeno;
use grafeno_query::{Params, QueryResult, Value, execute, params_from_json};
use serde_json::{Value as Json, json};

const NS: &str = "q";

fn run(db: &Grafeno, query: &str) -> QueryResult {
    execute(db, NS, query, &Params::new()).unwrap_or_else(|e| panic!("{query}\n→ {e}"))
}

fn fail(db: &Grafeno, query: &str) -> String {
    match execute(db, NS, query, &Params::new()) {
        Ok(_) => panic!("{query} should fail"),
        Err(error) => error.to_string(),
    }
}

/// Rows as JSON, for compact assertions.
fn rows(db: &Grafeno, query: &str) -> Json {
    run(db, query).to_json()["rows"].clone()
}

/// A small company graph:
/// Ana -works_at(2020..2023)-> Acme, Ana -works_at(2023..)-> Initech,
/// Ana -knows-> Bob -knows-> Carla, Bob -works_at-> Acme.
fn company() -> Grafeno {
    let db = Grafeno::in_memory().unwrap();
    run(
        &db,
        "CREATE (ana:Person {name: 'Ana', age: 34, @text: 'Ana writes Rust at Initech'}),
                (bob:Person {name: 'Bob', age: 28}),
                (carla:Person:Admin {name: 'Carla', age: 41}),
                (acme:Company {name: 'Acme'}), (ini:Company {name: 'Initech'}),
                (ana)-[:works_at {@valid_from: '2020-01-01', @valid_to: '2023-01-01'}]->(acme),
                (ana)-[:works_at {@valid_from: '2023-01-01', role: 'lead'}]->(ini),
                (ana)-[:knows]->(bob), (bob)-[:knows]->(carla), (bob)-[:works_at]->(acme)",
    );
    db
}

#[test]
fn create_reports_what_changed() {
    let db = Grafeno::in_memory().unwrap();
    let result = run(
        &db,
        "CREATE (a:Note {title: 'x'})-[:links_to]->(b:Note {title: 'y'}) RETURN a.title, b.title",
    );
    assert_eq!(result.columns, ["a.title", "b.title"]);
    assert_eq!(result.to_json()["rows"], json!([["x", "y"]]));
    assert_eq!(
        (result.stats.nodes_created, result.stats.edges_created),
        (2, 1)
    );
    let stats = db.begin_read().unwrap().stats(NS).unwrap();
    assert_eq!((stats.nodes, stats.edges), (2, 1));
}

#[test]
fn patterns_follow_directions_and_labels() {
    let db = company();
    assert_eq!(
        rows(
            &db,
            "MATCH (p:Person)-[:works_at]->(c:Company {name: 'Acme'}) RETURN p.name ORDER BY p.name"
        ),
        json!([["Ana"], ["Bob"]])
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (c:Company {name: 'Acme'})<-[:works_at]-(p) RETURN p.name ORDER BY p.name"
        ),
        json!([["Ana"], ["Bob"]])
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (b {name: 'Bob'})-[:knows]-(other) RETURN other.name ORDER BY other.name"
        ),
        json!([["Ana"], ["Carla"]])
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (a {name: 'Ana'})-[r:knows|works_at]->(x) RETURN type(r), x.name ORDER BY x.name"
        ),
        json!([
            ["works_at", "Acme"],
            ["knows", "Bob"],
            ["works_at", "Initech"]
        ])
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (a:Person)-[:knows]->(b)-[:knows]->(c) RETURN a.name, b.name, c.name"
        ),
        json!([["Ana", "Bob", "Carla"]])
    );
    assert_eq!(
        rows(&db, "MATCH (n:Admin) RETURN n.name"),
        json!([["Carla"]])
    );
    assert_eq!(
        rows(&db, "MATCH (n:Admin:Person) RETURN count(*)"),
        json!([[1]])
    );
}

#[test]
fn where_uses_three_valued_logic() {
    let db = company();
    assert_eq!(
        rows(
            &db,
            "MATCH (p:Person) WHERE p.age > 30 AND NOT p:Admin RETURN p.name"
        ),
        json!([["Ana"]])
    );
    // `missing` is null for everyone: neither `= 1` nor `<> 1` holds.
    assert_eq!(
        rows(&db, "MATCH (p:Person) WHERE p.missing = 1 RETURN p"),
        json!([])
    );
    assert_eq!(
        rows(&db, "MATCH (p:Person) WHERE p.missing <> 1 RETURN p"),
        json!([])
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (p:Person) WHERE p.missing IS NULL AND p.name IN ['Bob', 'Zed'] RETURN p.name"
        ),
        json!([["Bob"]])
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (p:Person) WHERE p.name STARTS WITH 'C' OR p.name ENDS WITH 'b' OR p.name CONTAINS 'zz'
             RETURN p.name ORDER BY p.name"
        ),
        json!([["Bob"], ["Carla"]])
    );
}

#[test]
fn return_aggregates_orders_and_pages() {
    let db = company();
    assert_eq!(
        rows(
            &db,
            "MATCH (p:Person)-[:works_at]->(c) RETURN c.name AS company, count(*) AS people, collect(p.name) AS names
             ORDER BY people DESC, company"
        ),
        json!([["Acme", 2, ["Ana", "Bob"]], ["Initech", 1, ["Ana"]]])
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (p:Person) RETURN min(p.age), max(p.age), sum(p.age), avg(p.age), count(p.missing)"
        ),
        json!([[28, 41, 103, 103.0 / 3.0, 0]])
    );
    assert_eq!(rows(&db, "MATCH (n:Nothing) RETURN count(*)"), json!([[0]]));
    assert_eq!(
        rows(&db, "MATCH (n:Nothing) RETURN n.name, count(*)"),
        json!([])
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (p:Person) RETURN p.name ORDER BY p.age DESC SKIP 1 LIMIT 1"
        ),
        json!([["Ana"]])
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (p)-[:works_at]->(c) RETURN DISTINCT c.name ORDER BY c.name"
        ),
        json!([["Acme"], ["Initech"]])
    );
    // Aggregates in ORDER BY must also be returned.
    assert!(fail(&db, "MATCH (p:Person) RETURN p.name ORDER BY count(*)").contains("aggregate"));
}

#[test]
fn pseudo_properties_and_functions() {
    let db = company();
    let result = run(
        &db,
        "MATCH (a {name: 'Ana'}) RETURN a.@id, a.@text, labels(a), size(a.name), upper(a.name)",
    );
    let row = &result.to_json()["rows"][0];
    let id = row[0].as_str().unwrap().to_owned();
    assert_eq!(row[1], "Ana writes Rust at Initech");
    assert_eq!(row[2], json!(["Person"]));
    assert_eq!((row[3].clone(), row[4].clone()), (json!(3), json!("ANA")));

    // Look a node up by id.
    assert_eq!(
        rows(&db, &format!("MATCH (n {{@id: '{id}'}}) RETURN n.name")),
        json!([["Ana"]])
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (:Person {name: 'Ana'})-[w:works_at]->(c) RETURN c.name, date(w.@valid_from), w.@valid_to IS NULL ORDER BY c.name"
        ),
        json!([
            ["Acme", "2020-01-01", false],
            ["Initech", "2023-01-01", true]
        ])
    );
    assert_eq!(
        rows(
            &db,
            "RETURN 1 + 2 * 3, 7 / 2, 7.0 / 2, 'a' + 1, [1, 2] + 3, coalesce(null, 'x'), toInteger('42'), substring('grafeno', 0, 4)"
        ),
        json!([[7, 3, 3.5, "a1", [1, 2, 3], "x", 42, "graf"]])
    );
    assert_eq!(
        rows(&db, "RETURN [10, 20, 30][-1], {a: {b: 2}}.a.b, range(1, 3)"),
        json!([[30, 2, [1, 2, 3]]])
    );
}

#[test]
fn variable_length_paths() {
    let db = company();
    assert_eq!(
        rows(
            &db,
            "MATCH (a {name: 'Ana'})-[:knows*1..2]->(x) RETURN x.name ORDER BY x.name"
        ),
        json!([["Bob"], ["Carla"]])
    );
    assert_eq!(
        rows(&db, "MATCH (a {name: 'Ana'})-[:knows*2]->(x) RETURN x.name"),
        json!([["Carla"]])
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (a {name: 'Ana'})-[path:knows*]->(x {name: 'Carla'}) RETURN size(path)"
        ),
        json!([[2]])
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (a {name: 'Ana'})-[*0..1]->(x:Person) RETURN x.name ORDER BY x.name"
        ),
        json!([["Ana"], ["Bob"]])
    );
}

#[test]
fn time_travel_with_valid_and_known_at() {
    let db = company();
    let employer = |at: &str| {
        rows(
            &db,
            &format!(
                "MATCH (:Person {{name: 'Ana'}})-[:works_at]->(c) VALID AT '{at}' RETURN c.name"
            ),
        )
    };
    assert_eq!(employer("2021-06-01"), json!([["Acme"]]));
    assert_eq!(employer("2024-06-01"), json!([["Initech"]]));
    assert_eq!(employer("2019-01-01"), json!([]));

    let before = grafeno::Timestamp::now().iso();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let retracted = run(
        &db,
        "MATCH (:Person {name: 'Bob'})-[r:works_at]->() RETRACT r",
    );
    assert_eq!(retracted.stats.edges_retracted, 1);
    assert_eq!(
        rows(
            &db,
            "MATCH (:Person {name: 'Bob'})-[:works_at]->(c) RETURN c.name"
        ),
        json!([])
    );
    assert_eq!(
        rows(
            &db,
            &format!(
                "MATCH (:Person {{name: 'Bob'}})-[:works_at]->(c) KNOWN AT '{before}' RETURN c.name"
            )
        ),
        json!([["Acme"]])
    );
}

#[test]
fn set_remove_and_delete() {
    let db = company();
    let result = run(
        &db,
        "MATCH (b:Person {name: 'Bob'}) SET b.age = b.age + 1, b += {city: 'Lima'}, b:Senior, b.@text = 'Bob in Lima'
         RETURN b.age, b.city, b:Senior, b.@text",
    );
    assert_eq!(
        result.to_json()["rows"],
        json!([[29, "Lima", true, "Bob in Lima"]])
    );
    assert_eq!(result.stats.labels_added, 1);
    assert_eq!(
        db.begin_read()
            .unwrap()
            .search_text(NS, "lima", 5)
            .unwrap()
            .len(),
        1
    );

    run(
        &db,
        "MATCH (b {name: 'Bob'}) REMOVE b.city, b:Senior, b.@text",
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (b {name: 'Bob'}) RETURN b.city, b:Senior, b.@text"
        ),
        json!([[null, false, null]])
    );

    run(&db, "MATCH (b {name: 'Bob'}) SET b = {name: 'Robert'}");
    assert_eq!(
        rows(
            &db,
            "MATCH (b:Person {name: 'Robert'}) RETURN properties(b)"
        ),
        json!([[{"name": "Robert"}]])
    );

    // Invalidate a fact by setting @valid_to.
    run(
        &db,
        "MATCH (:Person {name: 'Ana'})-[w:works_at {role: 'lead'}]->() SET w.@valid_to = '2025-01-01'",
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (:Person {name: 'Ana'})-[:works_at]->(c) VALID AT '2025-06-01' RETURN c"
        ),
        json!([])
    );

    let deleted = run(&db, "MATCH (p:Person) WHERE p.age > 40 DELETE p");
    assert_eq!(deleted.stats.nodes_deleted, 1);
    assert_eq!(rows(&db, "MATCH (p:Person) RETURN count(*)"), json!([[2]]));
    assert!(db.begin_read().unwrap().check(NS).unwrap().is_ok());

    let deleted = run(&db, "MATCH ()-[k:knows]->() DELETE k RETURN count(*)");
    assert_eq!(deleted.stats.edges_deleted, 1);
}

#[test]
fn a_failing_write_changes_nothing() {
    let db = company();
    let error = fail(
        &db,
        "MATCH (p:Person) CREATE (p)-[:likes]->(x:Thing) SET x.bad = p",
    );
    assert!(error.contains("a property cannot hold a node"), "{error}");
    assert_eq!(rows(&db, "MATCH (t:Thing) RETURN count(*)"), json!([[0]]));
    assert!(
        !db.begin_read()
            .unwrap()
            .stats(NS)
            .unwrap()
            .edge_labels
            .contains_key("likes")
    );
}

#[test]
fn parameters() {
    let db = company();
    let params = params_from_json(
        json!({ "name": "Carla", "since": "2022-06-01" })
            .as_object()
            .unwrap(),
    );
    let result = execute(
        &db,
        NS,
        "MATCH (p:Person {name: $name}) RETURN p.age",
        &params,
    )
    .unwrap();
    assert_eq!(result.to_json()["rows"], json!([[41]]));
    let mut params = Params::new();
    params.insert("ids".into(), Value::List(vec![]));
    let result = execute(
        &db,
        NS,
        "MATCH (p:Person) WHERE p.@id IN $ids RETURN p",
        &params,
    )
    .unwrap();
    assert!(result.rows.is_empty());
    assert!(fail(&db, "RETURN $missing").contains("missing parameter $missing"));
}

#[test]
fn return_star_and_edge_uniqueness() {
    let db = Grafeno::in_memory().unwrap();
    run(&db, "CREATE (a:N {v: 1})-[:e]->(b:N {v: 2})");
    let result = run(&db, "MATCH (x)-[r]->(y) RETURN *");
    assert_eq!(result.columns, ["x", "r", "y"]);
    // One edge cannot be used twice in a pattern, so there is no x->y<-x match.
    assert_eq!(
        rows(&db, "MATCH (x)-[]->(y)<-[]-(z) RETURN x, z"),
        json!([])
    );
    assert_eq!(
        rows(&db, "MATCH (x:N), (y:N) RETURN count(*)"),
        json!([[4]])
    );
}

#[test]
fn helpful_errors() {
    let db = company();
    for (query, expected) in [
        ("MATCH (n) RETURN m", "unknown variable `m`"),
        ("MATCH (n) WHERE count(*) > 1 RETURN n", "aggregate"),
        ("MATCH (n)", "needs RETURN"),
        ("CREATE (n:X) MATCH (m) RETURN m", "MATCH after a write"),
        ("CREATE (n)", "at least one label"),
        ("MATCH (a), (b) CREATE (a)-[:x]-(b)", "direction"),
        ("MATCH (n) RETURN n.@nope", "no pseudo-property @nope"),
        ("RETURN nosuch(1)", "unknown function nosuch()"),
        ("RETURN 1 / 0", "division by zero"),
        ("RETURN 'a' AND true", "expected a boolean"),
        ("MATCH (n {name: 'Ana'}) SET n.@id = 'x'", "not @id"),
        ("MATCH (n:Person) REMOVE n:Person", "at least one label"),
    ] {
        let error = fail(&db, query);
        assert!(
            error.contains(expected),
            "{query}\n→ {error}\n(expected {expected:?})"
        );
    }
}
