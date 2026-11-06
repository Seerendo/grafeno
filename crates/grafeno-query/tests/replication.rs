//! Phase 13.1 through GQ: writing statements on a database with the
//! replication log on, replayed on an empty one, give the same data.

use grafeno::Grafeno;
use grafeno_query::{Params, execute};

const NS: &str = "g";

fn run(db: &Grafeno, query: &str) {
    execute(db, NS, query, &Params::new()).unwrap_or_else(|e| panic!("{query}\n→ {e}"));
}

fn dump(db: &Grafeno) -> String {
    let mut out = Vec::new();
    db.begin_read().unwrap().export(&[], &mut out).unwrap();
    String::from_utf8(out)
        .unwrap()
        .lines()
        .skip(1)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn gq_writes_replay_to_the_same_data() {
    let leader = Grafeno::in_memory().unwrap();
    leader.enable_replication_log().unwrap();
    for query in [
        "CREATE INDEX FOR (p:Person) ON (p.email)",
        "CREATE CONSTRAINT FOR ()-[r:works_at]->() REQUIRE ONE PER SOURCE",
        "UNWIND range(1, 30) AS i CREATE (:Person {name: 'P' + toString(i), email: 'p' + toString(i) + '@x', age: i})",
        "UNWIND range(1, 3) AS i CREATE (:Company {name: 'C' + toString(i)})",
        "MATCH (p:Person), (c:Company {name: 'C1'}) WHERE p.age <= 10 CREATE (p)-[:works_at]->(c)",
        // The constraint ends the old facts when people move.
        "MATCH (p:Person), (c:Company {name: 'C2'}) WHERE p.age <= 5 CREATE (p)-[:works_at]->(c)",
        "MERGE (p:Person {email: 'p1@x'}) ON MATCH SET p.seen = true",
        "MERGE (p:Person {email: 'new@x'}) ON CREATE SET p.name = 'New', p.@text = 'made by MERGE'",
        "MATCH (p:Person) WHERE p.age > 25 SET p:Senior, p.level = p.age / 10",
        "MATCH (p:Person) WHERE p.age = 30 DETACH DELETE p",
        "MATCH (p:Person {age: 1})-[r:works_at]->() RETRACT r",
        "FOREACH (i IN range(1, 3) | CREATE (:Tag {n: i}))",
        "MATCH (t:Tag) WHERE t.n = 2 DELETE t",
        "MATCH (p:Person {age: 2}) SET p.@embedding = [0.1, 0.2, 0.3]",
    ] {
        run(&leader, query);
    }
    let entries = leader
        .begin_read()
        .unwrap()
        .log_entries(0, usize::MAX)
        .unwrap();
    assert!(entries.len() >= 14);
    let follower = Grafeno::in_memory().unwrap();
    follower.apply_entries(&entries).unwrap();
    assert_eq!(dump(&follower), dump(&leader));
    let (a, b) = (leader.begin_read().unwrap(), follower.begin_read().unwrap());
    assert_eq!(
        a.changes(NS, 0, usize::MAX).unwrap(),
        b.changes(NS, 0, usize::MAX).unwrap()
    );
    assert_eq!(
        a.constraint_events(NS, 100).unwrap(),
        b.constraint_events(NS, 100).unwrap()
    );
    assert!(b.check(NS).unwrap().is_ok());
    // Queries agree on the follower.
    let count = |db: &Grafeno, query: &str| {
        execute(db, NS, query, &Params::new()).unwrap().to_json()["rows"].clone()
    };
    for query in [
        "MATCH (p:Person)-[r:works_at]->(c) RETURN c.name, count(*) ORDER BY c.name",
        "MATCH (p:Person {email: 'p3@x'}) RETURN p.name",
        "MATCH (p:Senior) RETURN count(p)",
    ] {
        assert_eq!(count(&follower, query), count(&leader, query), "{query}");
    }
}
