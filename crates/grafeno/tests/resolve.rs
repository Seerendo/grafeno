//! Entity resolution by declared rules: strong keys decide, fields score
//! with their measures and weights, neighbors add evidence, every decision
//! comes with its fields, and indexes keep the comparisons few.

use grafeno::textsim::Measure;
use grafeno::{
    Decision, FieldRule, Grafeno, IndexDef, MatchRule, NewEdge, NewNode, NodeId, Probe, Props,
};
use serde_json::json;

const NS: &str = "people";

fn rule() -> MatchRule {
    MatchRule {
        label: "Person".into(),
        strong_keys: vec!["email".into()],
        fields: vec![
            FieldRule {
                key: "name".into(),
                measure: Measure::JaroWinkler,
                weight: 2.0,
            },
            FieldRule {
                key: "born_on".into(),
                measure: Measure::Date { days: 365.0 },
                weight: 1.0,
            },
            FieldRule {
                key: "city".into(),
                measure: Measure::Exact,
                weight: 0.5,
            },
        ],
        neighbors: 0.0,
        same: 0.9,
        review: 0.75,
    }
}

fn props(value: serde_json::Value) -> Props {
    value.as_object().unwrap().clone().into_iter().collect()
}

fn people(db: &Grafeno) -> Vec<NodeId> {
    db.write(|tx| {
        let mut ids = Vec::new();
        for (name, born, city, email) in [
            ("John Smith", "1980-03-12", "Lima", Some("john@a.com")),
            ("Jane Smith", "1982-07-01", "Lima", None),
            ("María José García", "1975-11-30", "Quito", None),
            ("Pedro López", "1990-01-05", "Cusco", None),
            ("Johan Smit", "1961-09-09", "Oslo", None),
        ] {
            let mut node = NewNode::new("Person")
                .prop("name", name)
                .prop("born_on", born)
                .prop("city", city);
            if let Some(email) = email {
                node = node.prop("email", email);
            }
            ids.push(tx.create_node(NS, node)?);
        }
        Ok(ids)
    })
    .unwrap()
}

#[test]
fn records_match_by_rule_with_reasons() {
    let db = Grafeno::in_memory().unwrap();
    let ids = people(&db);
    db.write(|tx| {
        tx.create_index(NS, IndexDef::fuzzy("Person", "name"))?;
        tx.set_match_rule(NS, &rule())
    })
    .unwrap();
    let read = db.begin_read().unwrap();
    assert_eq!(read.match_rules(NS).unwrap(), vec![rule()]);

    // A typo in the name, the same birth date and city: the same person.
    let probe = Probe {
        label: "Person".into(),
        props: props(json!({ "name": "Jon Smyth", "born_on": "1980-03-12", "city": "lima" })),
        neighbors: Vec::new(),
    };
    let found = read.find_matches(NS, &probe, None, 3, None).unwrap();
    assert_eq!(found[0].node, ids[0]);
    assert_eq!(found[0].decision, Decision::Same, "{:#?}", found[0]);
    let keys: Vec<&str> = found[0].fields.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(keys, ["name", "born_on", "city"], "every field explained");
    assert!(found[0].fields[0].similarity > 0.85 && found[0].fields[0].similarity < 1.0);
    assert_eq!(found[0].fields[1].similarity, 1.0);
    // A namesake born elsewhere and decades before is not.
    let johan = found.iter().find(|m| m.node == ids[4]);
    assert!(
        johan.is_none_or(|m| m.decision == Decision::Different),
        "{johan:#?}"
    );

    // Day and month swapped, the name alone: to review, not the same.
    let probe = Probe {
        props: props(json!({ "name": "John Smith", "born_on": "1980-12-03", "city": "Arequipa" })),
        ..probe
    };
    let found = read.find_matches(NS, &probe, None, 1, None).unwrap();
    assert_eq!(
        (found[0].node, found[0].decision),
        (ids[0], Decision::Review),
        "{:#?}",
        found[0]
    );

    // A strong key decides either way.
    let by_email = Probe {
        props: props(json!({ "name": "J. S.", "email": " JOHN@a.com " })),
        ..probe.clone()
    };
    let found = read.find_matches(NS, &by_email, None, 5, None).unwrap();
    let john = found.iter().find(|m| m.node == ids[0]).unwrap();
    assert_eq!(
        (john.decision, john.decided_by.as_deref()),
        (Decision::Same, Some("email"))
    );
    let other_email = Probe {
        props: props(json!({ "name": "John Smith", "born_on": "1980-03-12", "email": "js@b.org" })),
        ..probe
    };
    let found = read.find_matches(NS, &other_email, None, 5, None).unwrap();
    let john = found.iter().find(|m| m.node == ids[0]).unwrap();
    assert_eq!(
        john.decision,
        Decision::Different,
        "a namesake with another email"
    );
}

#[test]
fn without_indexes_the_label_is_scanned_and_neighbors_add_evidence() {
    let db = Grafeno::in_memory().unwrap();
    let (a, b, acme) = db
        .write(|tx| {
            let acme = tx.create_node(NS, NewNode::new("Company").prop("name", "Acme"))?;
            let a = tx.create_node(NS, NewNode::new("Person").prop("name", "J. Pérez"))?;
            let b = tx.create_node(NS, NewNode::new("Person").prop("name", "J. Pérez"))?;
            let globex = tx.create_node(NS, NewNode::new("Company").prop("name", "Globex"))?;
            tx.create_edge(NS, NewEdge::new(a, "works_at", acme))?;
            tx.create_edge(NS, NewEdge::new(b, "works_at", globex))?;
            // Nothing known about this one's links: the name alone.
            tx.create_node(NS, NewNode::new("Person").prop("name", "J. Perez"))?;
            Ok((a, b, acme))
        })
        .unwrap();
    let rule = MatchRule {
        label: "Person".into(),
        strong_keys: Vec::new(),
        fields: vec![FieldRule {
            key: "name".into(),
            measure: Measure::Exact,
            weight: 1.0,
        }],
        neighbors: 1.0,
        same: 0.9,
        review: 0.5,
    };
    let read = db.begin_read().unwrap();
    let probe = Probe {
        label: "Person".into(),
        props: props(json!({ "name": "j perez" })),
        neighbors: vec![acme],
    };
    let found = read.find_matches(NS, &probe, Some(&rule), 5, None).unwrap();
    assert_eq!(found.len(), 3, "no index: the label is compared whole");
    let of = |id| found.iter().find(|m| m.node == id).unwrap();
    assert_eq!(
        (of(a).score, of(a).neighbors),
        (1.0, Some(1.0)),
        "the same company"
    );
    assert_eq!(of(a).decision, Decision::Same);
    // Another company: the name agrees, the graph does not.
    assert_eq!((of(b).score, of(b).neighbors), (0.5, Some(0.0)));
    assert_eq!(of(b).decision, Decision::Review);
    // No links known: the graph says nothing.
    let unknown = found.iter().find(|m| m.node != a && m.node != b).unwrap();
    assert_eq!((unknown.score, unknown.neighbors), (1.0, None));
}

#[test]
fn pairs_for_deduplication_and_rules_are_checked() {
    let db = Grafeno::in_memory().unwrap();
    let ids = people(&db);
    db.write(|tx| {
        tx.create_node(
            NS,
            NewNode::new("Person")
                .prop("name", "Jhon Smith")
                .prop("born_on", "1980-03-12")
                .prop("city", "Lima"),
        )?;
        tx.create_index(NS, IndexDef::fuzzy("Person", "name"))?;
        tx.set_match_rule(NS, &rule())
    })
    .unwrap();
    let read = db.begin_read().unwrap();
    let pairs = read.match_pairs(NS, "Person", 10).unwrap();
    assert_eq!(pairs.len(), 1, "{pairs:#?}");
    let (other, found) = &pairs[0];
    assert!(*other == ids[0] || found.node == ids[0]);
    assert_eq!(found.decision, Decision::Same);
    drop(read);

    // Marked distinct: no longer proposed.
    let jhon = db
        .begin_read()
        .unwrap()
        .find_by_name(NS, "Jhon Smith")
        .unwrap()[0];
    db.write(|tx| {
        tx.create_edge(NS, NewEdge::new(ids[0], "distinct_from", jhon))
            .map(|_| ())
    })
    .unwrap();
    assert!(
        db.begin_read()
            .unwrap()
            .match_pairs(NS, "Person", 10)
            .unwrap()
            .is_empty()
    );

    // Bad rules are refused; rules go with their namespace.
    let bad = MatchRule {
        review: 0.95,
        ..rule()
    };
    assert!(db.write(|tx| tx.set_match_rule(NS, &bad)).is_err());
    let empty = MatchRule {
        fields: Vec::new(),
        strong_keys: Vec::new(),
        ..rule()
    };
    assert!(db.write(|tx| tx.set_match_rule(NS, &empty)).is_err());
    assert!(db.write(|tx| tx.delete_match_rule(NS, "Person")).unwrap());
    db.write(|tx| tx.set_match_rule(NS, &rule())).unwrap();
    db.write(|tx| tx.drop_namespace(NS).map(|_| ())).unwrap();
    assert!(db.begin_read().unwrap().match_rules(NS).unwrap().is_empty());
}
