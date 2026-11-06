//! Lecturas por IDs: orden, ausentes y una instantánea compartida.
use grafeno::{EdgeId, Grafeno, NewEdge, NewNode, NodeId};

#[test]
fn ordered_reads_preserve_duplicates_and_namespace_isolation() {
    let db = Grafeno::in_memory().unwrap();
    let (a, b, edge, foreign) = db
        .write(|tx| {
            let a = tx.create_node("lab", NewNode::new("Person").prop("name", "Ana"))?;
            let b = tx.create_node("lab", NewNode::new("Person").prop("name", "Bob"))?;
            let edge = tx.create_edge("lab", NewEdge::new(a, "knows", b))?;
            let foreign = tx.create_node("otro", NewNode::new("Secret"))?;
            tx.retract_edge("lab", edge)?;
            Ok((a, b, edge, foreign))
        })
        .unwrap();
    let read = db.begin_read().unwrap();
    let nodes = read
        .nodes_by_id("lab", &[b, foreign, a, a, NodeId::new()])
        .unwrap();
    assert_eq!(
        nodes
            .iter()
            .map(|node| node.as_ref().map(|node| node.id))
            .collect::<Vec<_>>(),
        vec![Some(b), None, Some(a), Some(a), None]
    );
    let edges = read
        .edges_by_id("lab", &[edge, EdgeId::new(), edge])
        .unwrap();
    assert!(edges[0].as_ref().unwrap().expired_at.is_some());
    assert!(edges[1].is_none());
    assert_eq!(edges[2].as_ref().unwrap().id, edge);
    assert!(read.edges_by_id("otro", &[edge]).unwrap()[0].is_none());
    assert!(read.nodes_by_id("lab", &[]).unwrap().is_empty());
}

#[test]
fn nodes_and_edges_share_the_snapshot_despite_later_writes() {
    let db = Grafeno::in_memory().unwrap();
    let (a, b, edge) = db
        .write(|tx| {
            let a = tx.create_node("lab", NewNode::new("Person"))?;
            let b = tx.create_node("lab", NewNode::new("Person"))?;
            Ok((a, b, tx.create_edge("lab", NewEdge::new(a, "knows", b))?))
        })
        .unwrap();
    let snapshot = db.begin_read().unwrap();
    db.write(|tx| tx.delete_node("lab", a)).unwrap();
    assert!(
        snapshot
            .nodes_by_id("lab", &[a, b])
            .unwrap()
            .iter()
            .all(Option::is_some)
    );
    assert!(snapshot.edges_by_id("lab", &[edge]).unwrap()[0].is_some());
    let latest = db.begin_read().unwrap();
    assert!(latest.nodes_by_id("lab", &[a]).unwrap()[0].is_none());
    assert!(latest.edges_by_id("lab", &[edge]).unwrap()[0].is_none());
}
