//! Extraction and reconciliation: texts become `mentions` edges to entities
//! that were known, made, or queued; runs read only what changed; decisions
//! on the queue last; an agent's mentions go through the same rules.

use grafeno::{
    Direction, EdgeFilter, FieldRule, Grafeno, IndexDef, MatchRule, NewEdge, NewNode, NodeId,
    textsim::Measure,
};
use grafeno_memory::extract::IdentifierPattern;
use grafeno_memory::{
    AgentMention, ExtractOptions, MENTIONS, Resolution, extract, ingest, resolve, review_queue,
};
use serde_json::json;

const NS: &str = "x";

fn mentioned(db: &Grafeno, source: NodeId) -> Vec<(String, String)> {
    let read = db.begin_read().unwrap();
    let mut out: Vec<(String, String)> = read
        .neighbors(
            NS,
            source,
            Direction::Out,
            &EdgeFilter::any().label(MENTIONS),
        )
        .unwrap()
        .into_iter()
        .map(|n| {
            let node = read.node(NS, n.node).unwrap().unwrap();
            (
                n.edge.props["kind"].as_str().unwrap().to_owned(),
                node.display_name(),
            )
        })
        .collect();
    out.sort();
    out
}

fn memory(db: &Grafeno, text: &str) -> NodeId {
    db.write(|tx| tx.create_node(NS, NewNode::new("Memory").text(text)))
        .unwrap()
}

#[test]
fn texts_link_to_known_entities_patterns_and_new_ones() {
    let db = Grafeno::in_memory().unwrap();
    let (ana, _acme, globex) = db
        .write(|tx| {
            let ana = tx.create_node(
                NS,
                NewNode::new("Person")
                    .prop("name", "Ana García")
                    .prop("email", "ana@acme.com")
                    .prop("aliases", vec!["Anita"]),
            )?;
            let acme = tx.create_node(NS, NewNode::new("Company").prop("name", "Acme"))?;
            let globex = tx.create_node(NS, NewNode::new("Company").prop("name", "Globex"))?;
            tx.create_index(NS, IndexDef::new("Person", "email"))?;
            Ok((ana, acme, globex))
        })
        .unwrap();
    let m1 = memory(
        &db,
        "Anita escribió desde ana@acme.com: la reunión con Globex es el 03/04/2026 \
         y el presupuesto es USD 1,250.50. Ver https://globex.com/plan #presupuesto. \
         También vino Rodrigo Paz Pereira. Factura INV-2026-0042.",
    );
    let m2 = memory(
        &db,
        "Rodrigo Paz Pereira llamó otra vez; quiere hablar con Ana García.",
    );
    let options = ExtractOptions {
        identifiers: vec![IdentifierPattern {
            label: "Invoice".into(),
            pattern: r"\bINV-\d{4}-\d{4}\b".into(),
        }],
        ..ExtractOptions::default()
    };
    let report = extract(&db, NS, &options).unwrap();
    assert_eq!(report.read, 2);
    // A url, a hashtag, an invoice and Rodrigo; the email is Ana's.
    assert_eq!(report.created, 4, "{report:#?}");
    let got = mentioned(&db, m1);
    for expected in [
        ("email", "Ana García"),
        ("name", "Ana García"),
        ("name", "Globex"),
        ("hashtag", "#presupuesto"),
        ("url", "https://globex.com/plan"),
        ("identifier:Invoice", "INV-2026-0042"),
        ("phrase", "Rodrigo Paz Pereira"),
    ] {
        assert!(
            got.contains(&(expected.0.to_owned(), expected.1.to_owned())),
            "missing {expected:?} in {got:?}"
        );
    }
    // The phrase appeared in two texts: it is an entity linked from both.
    assert!(mentioned(&db, m2).contains(&("name".into(), "Ana García".into())));
    let rodrigo = db
        .begin_read()
        .unwrap()
        .find_by_name(NS, "Rodrigo Paz Pereira")
        .unwrap();
    assert_eq!(rodrigo.len(), 1);
    let read = db.begin_read().unwrap();
    let node = read.node(NS, rodrigo[0]).unwrap().unwrap();
    assert!(node.has_label("Entity"));
    assert_eq!(node.sources.len(), 2, "made from both texts");
    let m1_node = read.node(NS, m1).unwrap().unwrap();
    assert_eq!(m1_node.props["mentioned_dates"], json!(["2026-04-03"]));
    assert_eq!(m1_node.props["mentioned_amounts"], json!(["USD 1250.5"]));
    // Positions point at the words in the text.
    let edge = read
        .neighbors(NS, m1, Direction::Out, &EdgeFilter::any().label(MENTIONS))
        .unwrap()
        .into_iter()
        .find(|n| n.node == globex)
        .unwrap()
        .edge;
    let text: Vec<char> = read.text(NS, m1).unwrap().unwrap().chars().collect();
    let (start, end) = (
        edge.props["start"].as_u64().unwrap() as usize,
        edge.props["end"].as_u64().unwrap() as usize,
    );
    assert_eq!(text[start..end].iter().collect::<String>(), "Globex");
    assert!(read.check(NS).unwrap().is_ok());
    let _ = ana;
    drop(read);

    // Nothing changed: nothing read. A changed text is read again, its old
    // edges replaced.
    let again = extract(&db, NS, &options).unwrap();
    assert_eq!((again.read, again.unchanged), (0, 2));
    db.write(|tx| tx.set_text(NS, m2, "Ahora solo menciona a Globex."))
        .unwrap();
    let again = extract(&db, NS, &options).unwrap();
    assert_eq!((again.read, again.unchanged), (1, 1));
    assert_eq!(
        mentioned(&db, m2),
        [("name".to_owned(), "Globex".to_owned())]
    );
}

#[test]
fn namesakes_are_settled_by_context_or_queued_and_decisions_last() {
    let db = Grafeno::in_memory().unwrap();
    let (juan_acme, juan_globex, acme) = db
        .write(|tx| {
            let acme = tx.create_node(NS, NewNode::new("Company").prop("name", "Acme"))?;
            let globex = tx.create_node(NS, NewNode::new("Company").prop("name", "Globex"))?;
            let a = tx.create_node(NS, NewNode::new("Person").prop("name", "Juan Pérez"))?;
            let b = tx.create_node(NS, NewNode::new("Person").prop("name", "Juan Pérez"))?;
            tx.create_edge(NS, NewEdge::new(a, "works_at", acme))?;
            tx.create_edge(NS, NewEdge::new(b, "works_at", globex))?;
            Ok((a, b, acme))
        })
        .unwrap();
    let _ = acme;
    // The text also names Acme: only one Juan Pérez works there.
    let with_context = memory(&db, "Juan Pérez presentó el plan de Acme.");
    // Nothing tells which.
    let alone = memory(&db, "Juan Pérez llamó.");
    let report = extract(&db, NS, &ExtractOptions::default()).unwrap();
    assert_eq!(report.queued, 1, "{report:#?}");
    let read = db.begin_read().unwrap();
    let targets = |source| -> Vec<NodeId> {
        read.neighbors(
            NS,
            source,
            Direction::Out,
            &EdgeFilter::any().label(MENTIONS),
        )
        .unwrap()
        .into_iter()
        .map(|n| n.node)
        .collect()
    };
    assert!(targets(with_context).contains(&juan_acme));
    assert!(!targets(with_context).contains(&juan_globex));
    assert!(targets(alone).is_empty());
    let queue = review_queue(&read, NS, 10).unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].kind, "ambiguous");
    assert_eq!(queue[0].candidates.len(), 2);
    drop(read);

    // Resolved by someone: linked now, and the next time.
    db.write(|tx| {
        resolve(
            tx,
            NS,
            &queue[0].key,
            Resolution::Link { node: juan_globex },
        )
        .map(|_| ())
    })
    .unwrap();
    let later = memory(&db, "Juan Pérez volvió a llamar.");
    extract(&db, NS, &ExtractOptions::default()).unwrap();
    let read = db.begin_read().unwrap();
    assert_eq!(
        read.neighbors(
            NS,
            later,
            Direction::Out,
            &EdgeFilter::any().label(MENTIONS)
        )
        .unwrap()[0]
            .node,
        juan_globex
    );
    assert!(review_queue(&read, NS, 10).unwrap().is_empty());
}

#[test]
fn single_phrases_wait_dismissed_ones_go_and_agents_use_match_rules() {
    let db = Grafeno::in_memory().unwrap();
    let m1 = memory(&db, "Hablamos de Proyecto Atlas con calma.");
    extract(&db, NS, &ExtractOptions::default()).unwrap();
    let read = db.begin_read().unwrap();
    let queue = review_queue(&read, NS, 10).unwrap();
    assert_eq!(queue.len(), 1, "seen once: waits");
    assert_eq!(queue[0].kind, "phrase");
    drop(read);
    db.write(|tx| resolve(tx, NS, &queue[0].key, Resolution::Dismiss).map(|_| ()))
        .unwrap();
    eprintln!(
        "DEBUG {:?}",
        db.begin_read()
            .unwrap()
            .state_entries(NS, "extract.ignored", "", 10)
            .unwrap()
    );
    memory(&db, "Otra vez Proyecto Atlas.");
    let report = extract(&db, NS, &ExtractOptions::default()).unwrap();
    assert_eq!(report.created, 0, "dismissed: not proposed again");
    let left = review_queue(&db.begin_read().unwrap(), NS, 10).unwrap();
    assert!(left.is_empty(), "{left:#?}");

    // An agent's mentions, through the label's match rule.
    let ana = db
        .write(|tx| {
            let ana = tx.create_node(
                NS,
                NewNode::new("Person")
                    .prop("name", "Ana García")
                    .prop("born_on", "1980-03-12"),
            )?;
            tx.create_index(NS, IndexDef::fuzzy("Person", "name"))?;
            tx.set_match_rule(
                NS,
                &MatchRule {
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
                            measure: Measure::Date { days: 30.0 },
                            weight: 1.0,
                        },
                    ],
                    neighbors: 0.0,
                    same: 0.9,
                    review: 0.75,
                },
            )?;
            Ok(ana)
        })
        .unwrap();
    let read = db.begin_read().unwrap();
    let report = db
        .write(|tx| {
            ingest(
                tx,
                &read,
                NS,
                m1,
                &[
                    AgentMention {
                        surface: "Ana Garcia".into(),
                        label: Some("Person".into()),
                        props: json!({ "born_on": "1980-03-12" })
                            .as_object()
                            .unwrap()
                            .clone()
                            .into_iter()
                            .collect(),
                        start: None,
                        end: None,
                    },
                    AgentMention {
                        surface: "Luis Soto".into(),
                        label: Some("Person".into()),
                        props: json!({ "born_on": "1975-01-01" })
                            .as_object()
                            .unwrap()
                            .clone()
                            .into_iter()
                            .collect(),
                        start: None,
                        end: None,
                    },
                ],
                "Entity",
            )
        })
        .unwrap();
    assert_eq!(
        (report.linked, report.created, report.mentions),
        (1, 1, 2),
        "{report:#?}"
    );
    drop(read);
    let got = mentioned(&db, m1);
    assert!(got.contains(&("agent".into(), "Ana García".into())));
    assert!(got.contains(&("agent".into(), "Luis Soto".into())));
    // The agent's edges stay when the text is extracted again.
    db.write(|tx| tx.set_text(NS, m1, "Hablamos de otra cosa."))
        .unwrap();
    extract(&db, NS, &ExtractOptions::default()).unwrap();
    assert_eq!(mentioned(&db, m1).len(), 2);
    let _ = ana;
}
