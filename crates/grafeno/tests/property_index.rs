//! Property indexes: lookups, upkeep on every write, uniqueness, check,
//! rebuild, export and import.

use std::ops::Bound;

use grafeno::{Error, Grafeno, IndexDef, IndexRange, NewNode, NodeId, Props, Value};
use serde_json::json;

const NS: &str = "idx";

fn def(label: &str, key: &str, unique: bool) -> IndexDef {
    IndexDef {
        name: IndexDef::default_name(label, key, unique),
        label: label.into(),
        key: key.into(),
        unique,
    }
}

fn people(db: &Grafeno) -> Vec<NodeId> {
    db.write(|tx| {
        let mut ids = Vec::new();
        for (name, age) in [
            ("Ana", 34),
            ("Bob", 28),
            ("Carla", 41),
            ("Dan", 19),
            ("Eve", 34),
        ] {
            ids.push(tx.create_node(
                NS,
                NewNode::new("Person").prop("name", name).prop("age", age),
            )?);
        }
        // Same property, other label: not in the index.
        tx.create_node(NS, NewNode::new("Robot").prop("name", "R2").prop("age", 50))?;
        Ok(ids)
    })
    .unwrap()
}

fn lookup(db: &Grafeno, name: &str, range: IndexRange) -> Vec<NodeId> {
    db.begin_read()
        .unwrap()
        .index_lookup(NS, name, &range, usize::MAX)
        .unwrap()
}

#[test]
fn lookups_by_value_range_and_prefix() {
    let db = Grafeno::in_memory().unwrap();
    let ids = people(&db);
    let filled = db
        .write(|tx| tx.create_index(NS, def("Person", "age", false)))
        .unwrap();
    assert_eq!(filled, 5);
    db.write(|tx| tx.create_index(NS, def("Person", "name", false)))
        .unwrap();

    let mut ages = lookup(&db, "Person_age", IndexRange::Equal(json!(34)));
    ages.sort();
    let mut expected = vec![ids[0], ids[4]];
    expected.sort();
    assert_eq!(ages, expected);
    // Ranges come back in value order; 34.0 equals 34.
    assert_eq!(
        lookup(
            &db,
            "Person_age",
            IndexRange::Between {
                lower: Bound::Included(json!(30)),
                upper: Bound::Unbounded
            }
        )
        .len(),
        3
    );
    assert_eq!(
        lookup(
            &db,
            "Person_age",
            IndexRange::Between {
                lower: Bound::Unbounded,
                upper: Bound::Included(json!(28.0))
            }
        ),
        vec![ids[3], ids[1]]
    );
    assert_eq!(
        lookup(&db, "Person_name", IndexRange::Prefix("Ca".into())),
        vec![ids[2]]
    );
    // A string range does not reach numbers, and a mixed range finds nothing.
    assert!(lookup(&db, "Person_age", IndexRange::Equal(json!("34"))).is_empty());
    assert!(
        lookup(
            &db,
            "Person_age",
            IndexRange::Between {
                lower: Bound::Included(json!(1)),
                upper: Bound::Included(json!("z"))
            }
        )
        .is_empty()
    );

    let read = db.begin_read().unwrap();
    let infos = read.indexes(NS).unwrap();
    assert_eq!(
        infos
            .iter()
            .map(|i| (i.def.name.as_str(), i.entries))
            .collect::<Vec<_>>(),
        [("Person_age", 5), ("Person_name", 5)]
    );
    assert!(read.check(NS).unwrap().is_ok());
}

#[test]
fn writes_keep_indexes_current() {
    let db = Grafeno::in_memory().unwrap();
    let ids = people(&db);
    db.write(|tx| tx.create_index(NS, def("Person", "age", false)))
        .unwrap();
    db.write(|tx| {
        let mut props = Props::new();
        props.insert("age".into(), Value::from(35));
        tx.set_node_props(NS, ids[0], props)?;
        tx.remove_label(NS, ids[1], "Person").err(); // the only label: refused
        tx.add_label(NS, ids[1], "Former")?;
        tx.remove_label(NS, ids[1], "Person")?;
        tx.delete_node(NS, ids[2])?;
        tx.create_node(NS, NewNode::new("Person").prop("age", 35))?;
        Ok(())
    })
    .unwrap();
    assert_eq!(
        lookup(&db, "Person_age", IndexRange::Equal(json!(34))),
        vec![ids[4]]
    );
    assert_eq!(
        lookup(&db, "Person_age", IndexRange::Equal(json!(35))).len(),
        2
    );
    assert!(
        lookup(&db, "Person_age", IndexRange::Equal(json!(28))).is_empty(),
        "no longer a Person"
    );
    assert!(db.begin_read().unwrap().check(NS).unwrap().is_ok());

    assert!(db.write(|tx| tx.drop_index(NS, "Person_age")).unwrap());
    assert!(!db.write(|tx| tx.drop_index(NS, "Person_age")).unwrap());
    assert!(lookup(&db, "Person_age", IndexRange::Equal(json!(35))).is_empty());
    assert!(db.begin_read().unwrap().check(NS).unwrap().is_ok());
}

#[test]
fn unique_indexes_refuse_duplicates() {
    let db = Grafeno::in_memory().unwrap();
    people(&db);
    // Ana and Eve are both 34.
    assert!(
        matches!(db.write(|tx| tx.create_index(NS, def("Person", "age", true))), Err(Error::Invalid(m)) if m.contains("both have 34"))
    );
    db.write(|tx| tx.create_index(NS, def("Person", "name", true)))
        .unwrap();
    let error = db
        .write(|tx| tx.create_node(NS, NewNode::new("Person").prop("name", "Ana")))
        .unwrap_err();
    assert!(error.to_string().contains("must be unique"), "{error}");
    // Another label may reuse the value, and so may the same node.
    db.write(|tx| tx.create_node(NS, NewNode::new("Pet").prop("name", "Ana")))
        .unwrap();
    let ana = lookup(&db, "Person_name_unique", IndexRange::Equal(json!("Ana")))[0];
    db.write(|tx| {
        let mut props = Props::new();
        props.insert("nick".into(), Value::from("A"));
        tx.set_node_props(NS, ana, props)
    })
    .unwrap();
    // Same definition again: kept; a clash: refused.
    assert_eq!(
        db.write(|tx| tx.create_index(NS, def("Person", "name", true)))
            .unwrap(),
        5
    );
    let clash = IndexDef {
        name: "other".into(),
        ..def("Person", "name", true)
    };
    assert!(db.write(|tx| tx.create_index(NS, clash)).is_err());
}

#[test]
fn indexes_survive_rebuild_and_export() {
    let db = Grafeno::in_memory().unwrap();
    people(&db);
    db.write(|tx| tx.create_index(NS, def("Person", "age", false)))
        .unwrap();
    db.write(|tx| tx.rebuild_indexes(NS)).unwrap();
    assert_eq!(
        lookup(&db, "Person_age", IndexRange::Equal(json!(34))).len(),
        2
    );
    assert!(db.begin_read().unwrap().check(NS).unwrap().is_ok());

    let mut dump = Vec::new();
    db.begin_read().unwrap().export(&[], &mut dump).unwrap();
    assert!(String::from_utf8_lossy(&dump).contains(r#""type":"index""#));
    let copy = Grafeno::in_memory().unwrap();
    copy.import(dump.as_slice()).unwrap();
    assert_eq!(
        lookup(&copy, "Person_age", IndexRange::Equal(json!(34))).len(),
        2
    );
    assert!(copy.begin_read().unwrap().check(NS).unwrap().is_ok());
}
