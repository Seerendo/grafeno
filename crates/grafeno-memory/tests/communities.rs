//! Community summaries: Louvain levels kept as a hierarchy, a summary per
//! community from the graph alone, refreshes that touch only what changed,
//! agents' own texts kept, and global search with drill-down.

use std::collections::HashMap;

use grafeno::{Direction, EdgeFilter, Grafeno, NewEdge, NewNode, NodeId, Value};
use grafeno_memory::{
    COMMUNITY, CommunityOptions, ContextOptions, MEMBER_OF, OverviewOptions, RecallQuery,
    community_members, context_for, overview, recall, refresh_communities, set_community_text,
};

const NS: &str = "c";

/// Two teams, each people who all work together and notes on their own
/// topic, joined by one acquaintance.
fn teams(db: &Grafeno) -> (Vec<NodeId>, Vec<NodeId>) {
    db.write(|tx| {
        let mut team = |people: &[&str], topic: &str| -> grafeno::Result<Vec<NodeId>> {
            let mut ids = Vec::new();
            for person in people {
                ids.push(tx.create_node(NS, NewNode::new("Person").prop("name", *person))?);
            }
            for (i, x) in ids.clone().iter().enumerate() {
                for y in &ids.clone()[i + 1..] {
                    // The first is the lead: everyone works with them twice.
                    tx.create_edge(NS, NewEdge::new(*y, "works_with", *x))?;
                }
            }
            for k in 1..people.len() {
                let note = tx.create_node(
                    NS,
                    NewNode::new("Memory").text(format!(
                        "{} and {} discussed the {topic} plan",
                        people[0], people[k]
                    )),
                )?;
                tx.create_edge(NS, NewEdge::new(note, "about", ids[0]))?;
                tx.create_edge(NS, NewEdge::new(note, "about", ids[k]))?;
                ids.push(note);
            }
            Ok(ids)
        };
        let a = team(&["Ana", "Bruno", "Carla", "Dario"], "compiler")?;
        let b = team(&["Eva", "Fabio", "Gala", "Hugo"], "garden")?;
        tx.create_edge(NS, NewEdge::new(a[1], "knows", b[1]))?;
        Ok((a, b))
    })
    .unwrap()
}

fn summary_of(db: &Grafeno, node: NodeId) -> Option<NodeId> {
    let found = db
        .begin_read()
        .unwrap()
        .neighbors(
            NS,
            node,
            Direction::Out,
            &EdgeFilter::any().label(MEMBER_OF),
        )
        .unwrap();
    assert!(found.len() <= 1, "one lowest summary per node");
    found.first().map(|n| n.node)
}

/// The summaries that hold a node: its lowest and their parents.
fn within(db: &Grafeno, node: NodeId) -> Vec<NodeId> {
    let read = db.begin_read().unwrap();
    let mut out = Vec::new();
    let mut at = summary_of(db, node);
    while let Some(id) = at {
        out.push(id);
        at = read
            .neighbors(
                NS,
                id,
                Direction::Out,
                &EdgeFilter::any().label(grafeno_memory::PART_OF),
            )
            .unwrap()
            .first()
            .map(|n| n.node);
    }
    out
}

#[test]
fn summaries_describe_each_community_and_refreshes_are_incremental() {
    let db = Grafeno::in_memory().unwrap();
    let (a, b) = teams(&db);
    let report = refresh_communities(&db, NS, &CommunityOptions::default()).unwrap();
    assert!(report.created >= 2, "{report:?}");
    assert_eq!(report.nodes, a.len() + b.len());

    // Each team in its own summary, which names its lead and its topic.
    let first = summary_of(&db, a[0]).unwrap();
    let second = summary_of(&db, b[0]).unwrap();
    assert_ne!(first, second);
    for member in &a {
        assert_eq!(summary_of(&db, *member), Some(first));
    }
    let read = db.begin_read().unwrap();
    let node = read.node(NS, first).unwrap().unwrap();
    assert!(node.has_label(COMMUNITY));
    let text = read.text(NS, first).unwrap().unwrap();
    assert!(text.contains("Central: Ana"), "{text}");
    assert!(text.contains("compiler"), "{text}");
    assert!(!text.contains("garden"), "{text}");
    assert_eq!(node.props["size"], Value::from(a.len()));
    assert_eq!(node.props["labels"]["Person"], Value::from(4));
    assert!(!node.props["facts"].as_array().unwrap().is_empty());
    drop(read);

    // Nothing changed: nothing rewritten.
    let again = refresh_communities(&db, NS, &CommunityOptions::default()).unwrap();
    assert_eq!(
        (again.created, again.updated, again.removed),
        (0, 0, 0),
        "{again:?}"
    );
    assert_eq!(again.unchanged, report.communities);

    // A new colleague joins the first team: its summary is updated, in place.
    let ivo = db
        .write(|tx| {
            let ivo = tx.create_node(NS, NewNode::new("Person").prop("name", "Ivo"))?;
            tx.create_edge(NS, NewEdge::new(ivo, "works_with", a[0]))?;
            tx.create_edge(NS, NewEdge::new(ivo, "works_with", a[2]))?;
            Ok(ivo)
        })
        .unwrap();
    let changed = refresh_communities(&db, NS, &CommunityOptions::default()).unwrap();
    assert!(changed.updated >= 1, "{changed:?}");
    // The same summaries, rewritten in place (a coarser level may appear).
    assert_eq!(changed.removed, 0, "{changed:?}");
    assert!(
        within(&db, ivo).contains(&first),
        "Ivo is in the first team"
    );
    assert!(within(&db, b[0]).contains(&second));
    assert!(!within(&db, b[0]).contains(&first));
    assert!(db.begin_read().unwrap().check(NS).unwrap().is_ok());
}

#[test]
fn agents_text_is_kept_and_marked_stale() {
    let db = Grafeno::in_memory().unwrap();
    let (a, _) = teams(&db);
    refresh_communities(&db, NS, &CommunityOptions::default()).unwrap();
    let first = summary_of(&db, a[0]).unwrap();
    db.write(|tx| set_community_text(tx, NS, first, Some("Ana's compiler team.")))
        .unwrap();
    db.write(|tx| {
        let ivo = tx.create_node(NS, NewNode::new("Person").prop("name", "Ivo"))?;
        tx.create_edge(NS, NewEdge::new(ivo, "works_with", a[0]))
            .map(|_| ())
    })
    .unwrap();
    let report = refresh_communities(&db, NS, &CommunityOptions::default()).unwrap();
    assert_eq!(report.stale, 1, "{report:?}");
    let read = db.begin_read().unwrap();
    assert_eq!(
        read.text(NS, first).unwrap().as_deref(),
        Some("Ana's compiler team.")
    );
    let node = read.node(NS, first).unwrap().unwrap();
    assert_eq!(node.props["stale"], Value::from(true));
    assert_eq!(
        node.props["size"],
        Value::from(a.len() + 1),
        "the rest is fresh"
    );
    drop(read);

    // Back to the generated text.
    db.write(|tx| set_community_text(tx, NS, first, None))
        .unwrap();
    refresh_communities(&db, NS, &CommunityOptions::default()).unwrap();
    let text = db.begin_read().unwrap().text(NS, first).unwrap().unwrap();
    assert!(text.starts_with("A group of"), "{text}");
}

#[test]
fn global_search_drill_down_recall_and_context() {
    let db = Grafeno::in_memory().unwrap();
    let (a, _) = teams(&db);
    refresh_communities(&db, NS, &CommunityOptions::default()).unwrap();
    let first = summary_of(&db, a[0]).unwrap();
    let read = db.begin_read().unwrap();

    let hits = overview(&read, NS, Some("compiler"), &OverviewOptions::default()).unwrap();
    assert_eq!(hits[0].id, first);
    assert!(
        hits.iter()
            .all(|hit| hit.text.contains("compiler") || hit.score < hits[0].score)
    );
    let top = overview(&read, NS, None, &OverviewOptions::default()).unwrap();
    assert!(top.iter().all(|hit| hit.parent.is_none()));
    assert!(!top.is_empty());

    let opened = community_members(&read, NS, first, 3).unwrap();
    assert_eq!(opened.members.len(), 3);
    assert_eq!(opened.members[0].id, a[0], "the most central first");

    // Recall leaves summaries to the overview; context puts them first.
    let recalled = recall(&read, NS, &RecallQuery::new().text("compiler plan")).unwrap();
    assert!(!recalled.is_empty());
    assert!(recalled.iter().all(|r| !r.node.has_label(COMMUNITY)));
    let mut options = ContextOptions::with_budget(400);
    options.communities = 2;
    let context = context_for(
        &read,
        NS,
        &RecallQuery::new().text("compiler plan"),
        &options,
    )
    .unwrap();
    assert_eq!(context.overview.first(), Some(&first));
    assert!(
        context.rendered.starts_with("## Overview\n- A group of"),
        "{}",
        context.rendered
    );
    assert!(context.rendered.contains("## Memory"));
}

#[test]
fn levels_form_a_hierarchy() {
    // Four teams of five in two departments: each pair of teams is closely
    // linked, the departments barely. Louvain finds the teams first and the
    // departments above them.
    let db = Grafeno::in_memory().unwrap();
    db.write(|tx| {
        let mut teams: Vec<Vec<NodeId>> = Vec::new();
        for t in 0..4 {
            let ids: Vec<NodeId> = (0..5)
                .map(|k| {
                    tx.create_node(NS, NewNode::new("Person").prop("name", format!("t{t}p{k}")))
                })
                .collect::<grafeno::Result<_>>()?;
            for i in 0..5 {
                for j in i + 1..5 {
                    tx.create_edge(NS, NewEdge::new(ids[i], "near", ids[j]).prop("w", 3.0))?;
                }
            }
            teams.push(ids);
        }
        for (x, y) in [(0, 1), (2, 3)] {
            for a in &teams[x] {
                for b in &teams[y] {
                    tx.create_edge(NS, NewEdge::new(*a, "near", *b).prop("w", 1.0))?;
                }
            }
        }
        tx.create_edge(
            NS,
            NewEdge::new(teams[0][0], "near", teams[2][0]).prop("w", 0.1),
        )?;
        Ok(())
    })
    .unwrap();
    let options = CommunityOptions {
        weight: Some("w".into()),
        ..CommunityOptions::default()
    };
    let report = refresh_communities(&db, NS, &options).unwrap();
    assert_eq!(report.levels, 2, "{report:?}");
    assert_eq!(
        report.communities, 6,
        "four teams, two departments: {report:?}"
    );
    let read = db.begin_read().unwrap();
    let mut levels: HashMap<NodeId, i64> = HashMap::new();
    for id in read.nodes_with_label(NS, COMMUNITY).unwrap() {
        let node = read.node(NS, id).unwrap().unwrap();
        levels.insert(id, node.props["level"].as_i64().unwrap());
    }
    for (&id, &level) in &levels {
        let opened = community_members(&read, NS, id, 100).unwrap();
        if level == 0 {
            assert_eq!(opened.members.len(), 5);
            let parent = opened.community.parent.expect("a team is in a department");
            assert_eq!(levels[&parent], 1);
        } else {
            assert_eq!(opened.children.len(), 2);
            assert!(opened.members.is_empty());
            assert_eq!(opened.community.size, 10);
        }
    }
    let top = overview(&read, NS, None, &OverviewOptions::default()).unwrap();
    assert_eq!(top.len(), 2, "the departments");
}
