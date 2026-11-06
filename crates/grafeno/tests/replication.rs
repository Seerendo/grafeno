//! Phase 13.1: the replication log. A database with the log on is written
//! in every way it can be; its entries, applied to an empty database, give
//! the same data, and a member restored from a backup catches up from the
//! entries after it.

use std::sync::Arc;

use grafeno::textsim::Measure;
use grafeno::{
    Constraint, Direction, EdgeFilter, FieldRule, Grafeno, Grant, IndexDef, MatchRule, NewEdge,
    NewNode, NodeId, OnConflict, Principal, Role, Scope, Timestamp, Writer, WriterOptions,
};

const NS: &str = "r";

/// Everything a reader can see, for comparing two databases.
fn snapshot(db: &Grafeno) -> String {
    let read = db.begin_read().unwrap();
    let mut out = Vec::new();
    let namespaces: Vec<String> = read
        .namespaces()
        .unwrap()
        .into_iter()
        .map(|n| n.name)
        .collect();
    read.export(&namespaces, &mut out).unwrap();
    let mut text = String::from_utf8(out).unwrap();
    // The export's header carries the time it was taken.
    text = text.lines().skip(1).collect::<Vec<_>>().join("\n");
    let mut lines = vec![text];
    lines.push(format!("{:?}", read.namespaces().unwrap()));
    for ns in &namespaces {
        lines.push(format!("{:?}", read.stats(ns).unwrap()));
        lines.push(format!("{:?}", read.constraints(ns).unwrap()));
        lines.push(format!("{:?}", read.match_rules(ns).unwrap()));
        lines.push(format!("{:?}", read.merges(ns, None).unwrap()));
        lines.push(format!("{:?}", read.indexes(ns).unwrap()));
        lines.push(format!(
            "{:?}",
            read.state_entries(ns, "", "", usize::MAX).unwrap()
        ));
        for kind in ["history", "events", "import.id", "app"] {
            lines.push(format!(
                "{kind}: {:?}",
                read.state_entries(ns, kind, "", usize::MAX).unwrap()
            ));
        }
        lines.push(format!("{:?}", read.changes(ns, 0, usize::MAX).unwrap()));
    }
    lines.push(format!("{:?}", read.principals().unwrap()));
    lines.join("\n")
}

fn replay(from: &Grafeno, onto: &Grafeno, after: u64) -> usize {
    let entries = from
        .begin_read()
        .unwrap()
        .log_entries(after, usize::MAX)
        .unwrap();
    onto.apply_entries(&entries).unwrap()
}

fn rule() -> MatchRule {
    MatchRule {
        label: "Person".into(),
        strong_keys: vec!["email".into()],
        fields: vec![FieldRule {
            key: "name".into(),
            measure: Measure::JaroWinkler,
            weight: 1.0,
        }],
        neighbors: 0.0,
        same: 0.9,
        review: 0.7,
    }
}

/// Writes of every kind, over several transactions.
fn write_everything(db: &Grafeno) -> (NodeId, NodeId) {
    let (ana, acme) = db
        .write(|tx| {
            tx.create_namespace(NS, Some("replicated"))?;
            tx.create_index(NS, IndexDef::new("Person", "email"))?;
            tx.create_constraint(
                NS,
                Constraint::Functional {
                    name: "Person_city".into(),
                    label: "Person".into(),
                    key: "city".into(),
                },
            )?;
            tx.create_constraint(
                NS,
                Constraint::Edge {
                    name: "one_employer".into(),
                    label: "works_at".into(),
                    scope: Scope::Source,
                    on_conflict: OnConflict::Invalidate,
                },
            )?;
            let ana = tx.create_node(
                NS,
                NewNode::new("Person")
                    .prop("name", "Ana")
                    .prop("email", "ana@x.org")
                    .prop("city", "Lima")
                    .text("Ana works on graph engines")
                    .embedding(vec![1.0, 0.0, 0.5]),
            )?;
            let acme = tx.create_node(NS, NewNode::new("Company").prop("name", "Acme"))?;
            tx.create_edge(
                NS,
                NewEdge::new(ana, "works_at", acme).valid_from(Timestamp::from_micros(1_000_000)),
            )?;
            Ok((ana, acme))
        })
        .unwrap();
    db.write(|tx| {
        // A functional property changes: its history is kept.
        let mut props = grafeno::Props::new();
        props.insert("city".into(), "Quito".into());
        tx.set_node_props(NS, ana, props)?;
        tx.add_label(NS, ana, "Engineer")?;
        tx.set_importance(NS, ana, 0.8)?;
        // A second employer: the constraint ends the first fact.
        let globex = tx.create_node(NS, NewNode::new("Company").prop("name", "Globex"))?;
        tx.create_edge(NS, NewEdge::new(ana, "works_at", globex))?;
        tx.set_match_rule(NS, &rule())?;
        tx.put_state(NS, "app", "cursor", b"42")?;
        tx.create_principal(&Principal {
            name: "agent".into(),
            secret_hash: "ab".repeat(32),
            grants: vec![Grant {
                namespace: NS.into(),
                role: Role::Write,
            }],
            owner: false,
            own_only: true,
            description: None,
            created_at: Timestamp::from_micros(5),
        })?;
        Ok(())
    })
    .unwrap();
    db.write(|tx| {
        // Duplicates merged; an edge retracted; texts and embeddings changed.
        let dup = tx.create_node(
            NS,
            NewNode::new("Person")
                .prop("name", "Ana P.")
                .text("duplicate of Ana")
                .embedding(vec![0.9, 0.1, 0.5]),
        )?;
        let note = tx.create_node(NS, NewNode::new("Note").text("to be deleted"))?;
        let link = tx.create_edge(NS, NewEdge::new(note, "about", acme))?;
        tx.merge_nodes(NS, ana, dup)?;
        tx.retract_edge(NS, link)?;
        tx.delete_node(NS, note)?;
        tx.set_text(NS, acme, "Acme builds rockets")?;
        tx.set_embedding(NS, acme, &[0.0, 1.0, 0.2])?;
        tx.delete_state(NS, "app", "cursor")?;
        tx.create_namespace("scratch", None)?;
        tx.create_node("scratch", NewNode::new("Tmp"))?;
        Ok(())
    })
    .unwrap();
    db.write(|tx| {
        tx.drop_namespace("scratch")?;
        tx.create_node(
            "implicit",
            NewNode::new("Thing").text("a namespace made by its first write"),
        )?;
        tx.delete_principal("agent")?;
        tx.drop_index(NS, "Person_email")?;
        tx.create_index(NS, IndexDef::new("Company", "name"))?;
        Ok(())
    })
    .unwrap();
    // A failing transaction leaves nothing in the log.
    let _ = db.write(|tx| {
        tx.create_node(NS, NewNode::new("Ghost"))?;
        Err::<(), _>(grafeno::Error::Invalid("on purpose".into()))
    });
    db.write(|tx| tx.compact_vectors(NS).map(|_| ())).unwrap();
    (ana, acme)
}

#[test]
fn replaying_the_log_gives_the_same_data() {
    let leader = Grafeno::in_memory().unwrap();
    leader.enable_replication_log().unwrap();
    let (ana, acme) = write_everything(&leader);
    let position = leader.begin_read().unwrap().log_position().unwrap();
    assert!(position.enabled);
    assert_eq!((position.first, position.last, position.applied), (1, 5, 5));

    let follower = Grafeno::in_memory().unwrap();
    assert_eq!(replay(&leader, &follower, 0), 5);
    assert_eq!(snapshot(&follower), snapshot(&leader));
    let read = follower.begin_read().unwrap();
    assert_eq!(read.log_position().unwrap(), position);
    for ns in [NS, "implicit"] {
        assert!(read.check(ns).unwrap().is_ok(), "{ns}");
    }
    // Derived structures work: traversals, text and vector search, history.
    assert_eq!(
        read.neighbors(NS, ana, Direction::Out, &EdgeFilter::any())
            .unwrap()
            .len(),
        leader
            .begin_read()
            .unwrap()
            .neighbors(NS, ana, Direction::Out, &EdgeFilter::any())
            .unwrap()
            .len()
    );
    assert_eq!(read.search_text(NS, "rockets", 1).unwrap()[0].node, acme);
    assert_eq!(
        read.search_vector(NS, &[0.0, 1.0, 0.2], 1).unwrap()[0].node,
        acme
    );
    assert_eq!(read.property_history(NS, ana, "city").unwrap().len(), 1);
    drop(read);

    // Applying again changes nothing; a gap is refused.
    assert_eq!(replay(&leader, &follower, 0), 0);
    let later = leader.begin_read().unwrap().log_entries(4, 1).unwrap();
    let fresh = Grafeno::in_memory().unwrap();
    assert!(fresh.apply_entries(&later).is_err());
}

#[test]
fn a_member_catches_up_from_a_backup_and_the_entries_after_it() {
    let leader = Grafeno::in_memory().unwrap();
    leader.enable_replication_log().unwrap();
    write_everything(&leader);
    let path = std::env::temp_dir().join(format!("grafeno-repl-{}.redb", NodeId::new()));
    leader.begin_read().unwrap().backup(&path).unwrap();
    // The leader goes on, and drops the entries the backup holds.
    leader
        .write(|tx| {
            tx.create_node(
                NS,
                NewNode::new("Person")
                    .prop("name", "Bea")
                    .text("after the backup"),
            )?;
            Ok(())
        })
        .unwrap();
    leader.write(|tx| tx.truncate_log(5).map(|_| ())).unwrap();
    let position = leader.begin_read().unwrap().log_position().unwrap();
    assert_eq!((position.first, position.last), (6, 6));
    assert!(leader.begin_read().unwrap().log_entries(0, 10).unwrap()[0].0 == 6);

    {
        let member = Grafeno::open(&path).unwrap();
        let at = member.begin_read().unwrap().log_position().unwrap().applied;
        assert_eq!(at, 5);
        assert_eq!(replay(&leader, &member, at), 1);
        assert_eq!(snapshot(&member), snapshot(&leader));
        // The member keeps logging its own writes after the leader's entries.
        member
            .write(|tx| tx.create_node(NS, NewNode::new("Local")).map(|_| ()))
            .unwrap();
        assert_eq!(member.begin_read().unwrap().log_position().unwrap().last, 7);
    }
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn group_commits_log_one_entry_with_the_writes_that_stayed() {
    let leader = Arc::new(Grafeno::in_memory().unwrap());
    leader.enable_replication_log().unwrap();
    let writer = Writer::new(leader.clone(), WriterOptions::default());
    let handles: Vec<_> = (0..20)
        .map(|i| {
            let writer = &writer;
            std::thread::scope(|_| {
                writer.write(move |tx| {
                    if i % 7 == 3 {
                        return Err(grafeno::Error::Invalid("refused".into()));
                    }
                    tx.create_node(NS, NewNode::new("N").prop("i", i))
                        .map(|_| ())
                })
            })
        })
        .collect();
    let failed = handles.iter().filter(|outcome| outcome.is_err()).count();
    assert_eq!(failed, 3);
    drop(writer);
    let follower = Grafeno::in_memory().unwrap();
    replay(&leader, &follower, 0);
    assert_eq!(snapshot(&follower), snapshot(&leader));
    assert_eq!(follower.begin_read().unwrap().stats(NS).unwrap().nodes, 17);
}

#[test]
fn without_the_log_nothing_is_written() {
    let db = Grafeno::in_memory().unwrap();
    db.write(|tx| tx.create_node(NS, NewNode::new("N")).map(|_| ()))
        .unwrap();
    let position = db.begin_read().unwrap().log_position().unwrap();
    assert_eq!(position, grafeno::LogPosition::default());
    assert!(
        db.begin_read()
            .unwrap()
            .log_entries(0, 10)
            .unwrap()
            .is_empty()
    );
}
