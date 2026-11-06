//! The cost of enforcing rights on reads (phase 14.4), and the recall of
//! vector search when some vectors are hidden.
//!
//! `people` Person nodes (a tenth also `Salary`, half of them in team red,
//! each with an email and a 32-dimension embedding) and about three
//! `knows` edges each. The same queries run without a view, with a view
//! that reads the whole namespace (decided once), and with one decided per
//! record: `Salary` denied, `email` hidden, and a policy keeping the
//! account's team. Best of three runs each.
//!
//! Vector search: `search_vector` under the per-record view (which widens
//! the walk until enough visible hits are found) against the exact top 10
//! among the visible vectors.
//!
//! `cargo run --release -p grafeno-query --example visibility_bench [people]`

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use grafeno::{
    Account, AccountKind, Act, Applies, Condition, Grafeno, NewNode, Object, Policy, PolicyValue,
    Privilege, Rights, RoleDef, Timestamp, View,
};
use grafeno_query::{Limits, Params, execute_read_with};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn unit(&mut self) -> f32 {
        (self.next() % 10_000) as f32 / 10_000.0 - 0.5
    }
}

fn view(privileges: Vec<Privilege>, policies: Vec<Policy>) -> Arc<View> {
    let role = RoleDef {
        name: "r".into(),
        privileges,
        grants: Vec::new(),
        roles: Vec::new(),
        policies,
        description: None,
        created_at: Timestamp::now(),
    };
    let mut attributes = serde_json::Map::new();
    attributes.insert("team".into(), serde_json::json!("red"));
    let account = Account {
        name: "me".into(),
        kind: AccountKind::Service,
        password_hash: None,
        grants: Vec::new(),
        roles: vec!["r".into()],
        superuser: false,
        own_only: false,
        disabled: false,
        expires_at: None,
        attributes,
        description: None,
        created_at: Timestamp::now(),
    };
    let rights = Rights::compute(&account, &HashMap::from([("r".to_owned(), role)]));
    Arc::new(View::new(Arc::new(rights), Some(&account)))
}

fn main() {
    let people: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(50_000);
    let ns = "b";
    let db = Grafeno::in_memory().unwrap();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut ids = Vec::with_capacity(people);
    let mut vectors = Vec::with_capacity(people);
    db.write(|tx| {
        for i in 0..people {
            let mut labels = vec!["Person".to_owned()];
            if i % 10 == 0 {
                labels.push("Salary".into());
            }
            let embedding: Vec<f32> = (0..32).map(|_| rng.unit()).collect();
            let mut node = NewNode::new("Person");
            node.labels = labels;
            node.props.insert("i".into(), serde_json::json!(i));
            node.props.insert(
                "email".into(),
                serde_json::json!(format!("p{i}@example.com")),
            );
            node.props.insert(
                "team".into(),
                serde_json::json!(if i % 2 == 0 { "red" } else { "blue" }),
            );
            node.text = Some(format!("person {i} works on project {}", i % 100));
            node.embedding = Some(embedding.clone());
            ids.push(tx.create_node(ns, node)?);
            vectors.push((i, embedding));
        }
        Ok(())
    })
    .unwrap();
    db.write(|tx| {
        for (i, &from) in ids.iter().enumerate() {
            for _ in 0..3 {
                let to = ids[(rng.next() as usize) % people];
                if to != from {
                    tx.create_edge(ns, grafeno::NewEdge::new(from, "knows", to))?;
                }
            }
            let _ = i;
        }
        Ok(())
    })
    .unwrap();

    let namespace = || Object::Namespace {
        namespace: ns.into(),
    };
    let whole = view(vec![Privilege::grant(Act::Read, namespace())], Vec::new());
    let per_record = view(
        vec![
            Privilege::grant(Act::Read, namespace()),
            Privilege::deny(
                Act::Read,
                Object::Label {
                    namespace: ns.into(),
                    label: "Salary".into(),
                    edges: false,
                },
            ),
            Privilege::deny(
                Act::Read,
                Object::Property {
                    namespace: ns.into(),
                    label: "Person".into(),
                    property: "email".into(),
                },
            ),
        ],
        vec![Policy {
            namespace: ns.into(),
            label: Some("Person".into()),
            applies: Applies::Read,
            condition: Condition::Property {
                key: "team".into(),
                equals: PolicyValue::Attribute {
                    name: "team".into(),
                },
            },
        }],
    );

    let queries = [
        ("count every node", "MATCH (n) RETURN count(n)"),
        (
            "filter on a property",
            "MATCH (n:Person) WHERE n.i % 97 = 0 RETURN n.i",
        ),
        (
            "one hop from 1 000 people",
            "MATCH (a:Person)-[:knows]->(b) WHERE a.i < 1000 RETURN count(b)",
        ),
        (
            "two hops from 100 people",
            "MATCH (a:Person)-[:knows*2]->(b) WHERE a.i < 100 RETURN count(DISTINCT b)",
        ),
        (
            "full-text search",
            "CALL db.search('project 42', 20) YIELD node RETURN node.i",
        ),
    ];
    println!("query,no view (ms),whole namespace (ms),per record (ms),overhead per record");
    for (name, query) in queries {
        let mut best = [f64::MAX; 3];
        for _ in 0..3 {
            for (slot, v) in [None, Some(whole.clone()), Some(per_record.clone())]
                .into_iter()
                .enumerate()
            {
                let limits = Limits {
                    view: v,
                    ..Limits::none()
                };
                let read = db.begin_read().unwrap();
                let started = Instant::now();
                execute_read_with(&read, ns, query, &Params::new(), &limits).unwrap();
                best[slot] = best[slot].min(started.elapsed().as_secs_f64() * 1000.0);
            }
        }
        println!(
            "{name},{:.1},{:.1},{:.1},{:+.0}%",
            best[0],
            best[1],
            best[2],
            (best[2] / best[0] - 1.0) * 100.0
        );
    }

    // Vector search among the visible: red people without Salary (45%).
    let visible = |i: usize| i.is_multiple_of(2) && !i.is_multiple_of(10);
    let probes = 200;
    let mut recall = 0.0;
    let mut plain_recall = 0.0;
    let (mut filtered_ms, mut plain_ms) = (0.0, 0.0);
    for p in 0..probes {
        let query: Vec<f32> = (0..32).map(|_| rng.unit()).collect();
        let mut exact: Vec<(f32, usize)> = vectors
            .iter()
            .filter(|(i, _)| visible(*i))
            .map(|(i, v)| {
                let (dot, a, b) = v
                    .iter()
                    .zip(&query)
                    .fold((0.0, 0.0, 0.0), |(d, a, b), (x, y)| {
                        (d + x * y, a + x * x, b + y * y)
                    });
                (-(dot / (a * b).sqrt()), *i)
            })
            .collect();
        exact.sort_by(|a, b| a.0.total_cmp(&b.0));
        let truth: std::collections::HashSet<usize> =
            exact.iter().take(10).map(|(_, i)| *i).collect();
        let read = db.begin_read().unwrap().with_view(Some(per_record.clone()));
        let started = Instant::now();
        let hits = read.search_vector(ns, &query, 10).unwrap();
        filtered_ms += started.elapsed().as_secs_f64() * 1000.0;
        let plain = db.begin_read().unwrap();
        let started = Instant::now();
        let all = plain.search_vector(ns, &query, 10).unwrap();
        plain_ms += started.elapsed().as_secs_f64() * 1000.0;
        // The recall of the same search without a view, for comparison.
        let mut every: Vec<(f32, usize)> = vectors
            .iter()
            .map(|(i, v)| {
                let (dot, a, b) = v
                    .iter()
                    .zip(&query)
                    .fold((0.0, 0.0, 0.0), |(d, a, b), (x, y)| {
                        (d + x * y, a + x * x, b + y * y)
                    });
                (-(dot / (a * b).sqrt()), *i)
            })
            .collect();
        every.sort_by(|a, b| a.0.total_cmp(&b.0));
        let all_truth: std::collections::HashSet<usize> =
            every.iter().take(10).map(|(_, i)| *i).collect();
        plain_recall += all
            .iter()
            .filter(|hit| {
                let node = plain.node(ns, hit.node).unwrap().unwrap();
                all_truth.contains(&(node.props["i"].as_u64().unwrap() as usize))
            })
            .count() as f64
            / 10.0;
        let found = hits
            .iter()
            .filter(|hit| {
                let node = read.node(ns, hit.node).unwrap().unwrap();
                truth.contains(&(node.props["i"].as_u64().unwrap() as usize))
            })
            .count();
        recall += found as f64 / 10.0;
        let _ = p;
    }
    println!(
        "vector search top 10 (ms; recall without a view {:.3}; with 45% visible {:.3}),{:.2},,{:.2},",
        plain_recall / probes as f64,
        recall / probes as f64,
        plain_ms / probes as f64,
        filtered_ms / probes as f64,
    );
}
