//! Integrity check of a namespace, and rebuilding its indexes from the records.
//!
//! Node, edge, text and vector records are the source of truth. Everything
//! else (label index, adjacency, postings, HNSW links, counters) is derived
//! and can be rebuilt from them.

use std::collections::{BTreeMap, HashMap, HashSet};

use grafeno_core::{Edge, Node, NodeId, Result};
use redb::ReadableTable;
use serde::Serialize;

use crate::db::{ReadTx, WriteTx};
use crate::memory::names_of;
use crate::stats::{bump_edge_label, bump_namespace, label_counts};
use crate::store::{
    ADJ_IN, ADJ_OUT, EDGE_LABEL_COUNTS, EDGES, HNSW_LINKS, LABEL_COUNTS, MEMORY, NAMES,
    NODE_LABELS, NODES, NS_COUNTS, POSTINGS, TEXT_LENGTHS, TEXT_STATS, TEXTS, VECTOR_META, VECTORS,
    adjacency_value, scan_records, storage,
};
use crate::{text, vector};

const MAX_PROBLEMS: usize = 100;

/// The result of [`ReadTx::check`].
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CheckReport {
    pub namespace: String,
    pub nodes: u64,
    pub edges: u64,
    pub texts: u64,
    pub vectors: u64,
    /// Vector-graph links that point to removed nodes. They are expected
    /// after deletes, and searches skip them.
    pub dangling_links: u64,
    /// Inconsistencies found, at most 100 (the last line counts the rest).
    pub problems: Vec<String>,
}

impl CheckReport {
    pub fn is_ok(&self) -> bool {
        self.problems.is_empty()
    }
}

/// The result of [`WriteTx::rebuild_indexes`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RebuildReport {
    pub namespace: String,
    pub nodes: u64,
    pub edges: u64,
    pub texts: u64,
    pub vectors: u64,
    /// Records dropped because their node no longer exists.
    pub dropped_edges: u64,
    pub dropped_texts: u64,
    pub dropped_vectors: u64,
}

impl ReadTx {
    /// Checks that every index and counter of the namespace agrees with its
    /// records, and that every vector can be reached by searches.
    pub fn check(&self, ns: &str) -> Result<CheckReport> {
        let mut problems = Vec::new();
        let nodes: Vec<Node> = scan_records(&self.nodes, ns)?;
        let edges: Vec<Edge> = scan_records(&self.edges, ns)?;
        let ids: HashSet<u128> = nodes.iter().map(|node| node.id.as_u128()).collect();
        let by_id: HashMap<u128, &Node> =
            nodes.iter().map(|node| (node.id.as_u128(), node)).collect();

        // Nodes and the label index.
        let mut node_labels: BTreeMap<String, u64> = BTreeMap::new();
        for node in &nodes {
            let id = node.id;
            if node.labels.is_empty() {
                problems.push(format!("node {id} has no labels"));
            }
            if node.labels.windows(2).any(|pair| pair[0] >= pair[1]) {
                problems.push(format!("labels of node {id} are not sorted and unique"));
            }
            for label in &node.labels {
                *node_labels.entry(label.clone()).or_default() += 1;
                if self
                    .labels
                    .get((ns, label.as_str(), id.as_u128()))
                    .map_err(storage)?
                    .is_none()
                {
                    problems.push(format!("label index misses node {id} under {label:?}"));
                }
            }
        }
        for item in self.labels.range((ns, "", 0u128)..).map_err(storage)? {
            let (key, _) = item.map_err(storage)?;
            let (key_ns, label, id) = key.value();
            if key_ns != ns {
                break;
            }
            if !by_id.get(&id).is_some_and(|node| node.has_label(label)) {
                problems.push(format!(
                    "label index lists {} under {label:?}, but the node does not have it",
                    NodeId::from_u128(id)
                ));
            }
        }

        // Edges and the adjacency lists.
        let mut edge_labels: BTreeMap<String, u64> = BTreeMap::new();
        for edge in &edges {
            *edge_labels.entry(edge.label.clone()).or_default() += 1;
            for end in [edge.from, edge.to] {
                if !ids.contains(&end.as_u128()) {
                    problems.push(format!("edge {} points to missing node {end}", edge.id));
                }
            }
            let key = |node: NodeId| (ns, node.as_u128(), edge.label.as_str(), edge.id.as_u128());
            let out = self
                .out
                .get(key(edge.from))
                .map_err(storage)?
                .map(|g| g.value());
            if out != Some(adjacency_value(edge, edge.to)) {
                problems.push(format!("outgoing adjacency of edge {} is wrong", edge.id));
            }
            let inn = self
                .inn
                .get(key(edge.to))
                .map_err(storage)?
                .map(|g| g.value());
            if inn != Some(adjacency_value(edge, edge.from)) {
                problems.push(format!("incoming adjacency of edge {} is wrong", edge.id));
            }
        }
        for (table, name) in [(&self.out, "outgoing"), (&self.inn, "incoming")] {
            let mut entries = 0;
            for item in table.range((ns, 0u128, "", 0u128)..).map_err(storage)? {
                let (key, _) = item.map_err(storage)?;
                if key.value().0 != ns {
                    break;
                }
                entries += 1;
            }
            if entries != edges.len() {
                problems.push(format!(
                    "{entries} {name} adjacency entries for {} edges",
                    edges.len()
                ));
            }
        }

        // Counters.
        let counted = self
            .ns_counts
            .get(ns)
            .map_err(storage)?
            .map(|guard| guard.value())
            .unwrap_or((0, 0));
        let actual = (nodes.len() as u64, edges.len() as u64);
        if counted != actual {
            problems.push(format!(
                "namespace counters say {counted:?} (nodes, edges), records say {actual:?}"
            ));
        }
        if label_counts(&self.label_counts, ns)? != node_labels {
            problems.push("node label counters do not match the nodes".into());
        }
        if label_counts(&self.edge_label_counts, ns)? != edge_labels {
            problems.push("edge label counters do not match the edges".into());
        }

        // The names index and memory states.
        let mut expected_names = 0usize;
        for node in &nodes {
            for name in names_of(node) {
                expected_names += 1;
                if self
                    .names
                    .get((ns, name.as_str(), node.id.as_u128()))
                    .map_err(storage)?
                    .is_none()
                {
                    problems.push(format!("names index misses {name:?} of node {}", node.id));
                }
            }
        }
        let mut indexed_names = 0usize;
        for item in self.names.range((ns, "", 0u128)..).map_err(storage)? {
            let (key, _) = item.map_err(storage)?;
            if key.value().0 != ns {
                break;
            }
            indexed_names += 1;
        }
        if indexed_names != expected_names {
            problems.push(format!(
                "{indexed_names} entries in the names index, expected {expected_names}"
            ));
        }
        for item in self.memory.range((ns, 0u128)..).map_err(storage)? {
            let (key, value) = item.map_err(storage)?;
            let (key_ns, id) = key.value();
            if key_ns != ns {
                break;
            }
            if !ids.contains(&id) {
                problems.push(format!(
                    "memory state of missing node {}",
                    NodeId::from_u128(id)
                ));
            }
            let (importance, _, _) = value.value();
            if !(0.0..=1.0).contains(&importance) {
                problems.push(format!(
                    "importance {importance} of node {} is out of range",
                    NodeId::from_u128(id)
                ));
            }
        }

        let texts = text::check(
            &self.texts,
            &self.text_lengths,
            &self.postings,
            &self.text_stats,
            ns,
            &ids,
            &mut problems,
        )?;
        let (vectors, dangling_links) = vector::check(
            &self.vectors,
            &self.hnsw_links,
            &self.vector_meta,
            ns,
            &ids,
            &mut problems,
        )?;

        if problems.len() > MAX_PROBLEMS {
            let rest = problems.len() - MAX_PROBLEMS;
            problems.truncate(MAX_PROBLEMS);
            problems.push(format!("… and {rest} more"));
        }
        Ok(CheckReport {
            namespace: ns.to_owned(),
            nodes: actual.0,
            edges: actual.1,
            texts,
            vectors,
            dangling_links,
            problems,
        })
    }
}

impl WriteTx {
    /// Rebuilds every index and counter of the namespace from its node, edge,
    /// text and vector records. Edges, texts and vectors of missing nodes are
    /// dropped. The vector graph is built anew, which also removes dangling
    /// links.
    pub fn rebuild_indexes(&mut self, ns: &str) -> Result<RebuildReport> {
        let tx = &self.tx;
        let nodes: Vec<Node> = scan_records(&tx.open_table(NODES).map_err(storage)?, ns)?;
        let edges: Vec<Edge> = scan_records(&tx.open_table(EDGES).map_err(storage)?, ns)?;
        let mut texts = Vec::new();
        for item in tx
            .open_table(TEXTS)
            .map_err(storage)?
            .range((ns, 0u128)..)
            .map_err(storage)?
        {
            let (key, value) = item.map_err(storage)?;
            if key.value().0 != ns {
                break;
            }
            texts.push((key.value().1, value.value().to_owned()));
        }
        let mut vectors = Vec::new();
        {
            let table = tx.open_table(VECTORS).map_err(storage)?;
            for item in table.range((ns, 0u128)..).map_err(storage)? {
                let (key, _) = item.map_err(storage)?;
                if key.value().0 != ns {
                    break;
                }
                let id = NodeId::from_u128(key.value().1);
                if let Some(vector) = vector::embedding(&table, ns, id)? {
                    vectors.push((id, vector));
                }
            }
        }

        // Clear everything derived, plus the texts and vectors that will be
        // indexed again. Each range starts at the namespace and keeps the
        // entries of the namespaces after it.
        let other = |key_ns: &str| key_ns != ns;
        let tx = &self.tx;
        for definition in [NODE_LABELS, NAMES] {
            tx.open_table(definition)
                .map_err(storage)?
                .retain_in((ns, "", 0u128).., |key, _| other(key.0))
                .map_err(storage)?;
        }
        // Memory states are records too: only those of missing nodes go.
        let ids: HashSet<u128> = nodes.iter().map(|node| node.id.as_u128()).collect();
        tx.open_table(MEMORY)
            .map_err(storage)?
            .retain_in((ns, 0u128).., |key, _| other(key.0) || ids.contains(&key.1))
            .map_err(storage)?;
        for definition in [ADJ_OUT, ADJ_IN] {
            tx.open_table(definition)
                .map_err(storage)?
                .retain_in((ns, 0u128, "", 0u128).., |key, _| other(key.0))
                .map_err(storage)?;
        }
        tx.open_table(POSTINGS)
            .map_err(storage)?
            .retain_in((ns, "", 0u128).., |key, _| other(key.0))
            .map_err(storage)?;
        tx.open_table(TEXT_LENGTHS)
            .map_err(storage)?
            .retain_in((ns, 0u128).., |key, _| other(key.0))
            .map_err(storage)?;
        tx.open_table(TEXTS)
            .map_err(storage)?
            .retain_in((ns, 0u128).., |key, _| other(key.0))
            .map_err(storage)?;
        tx.open_table(VECTORS)
            .map_err(storage)?
            .retain_in((ns, 0u128).., |key, _| other(key.0))
            .map_err(storage)?;
        tx.open_table(HNSW_LINKS)
            .map_err(storage)?
            .retain_in((ns, 0u128, 0u8).., |key, _| other(key.0))
            .map_err(storage)?;
        for definition in [LABEL_COUNTS, EDGE_LABEL_COUNTS] {
            tx.open_table(definition)
                .map_err(storage)?
                .retain_in((ns, "").., |key, _| other(key.0))
                .map_err(storage)?;
        }
        tx.open_table(TEXT_STATS)
            .map_err(storage)?
            .remove(ns)
            .map_err(storage)?;
        tx.open_table(VECTOR_META)
            .map_err(storage)?
            .remove(ns)
            .map_err(storage)?;
        tx.open_table(NS_COUNTS)
            .map_err(storage)?
            .remove(ns)
            .map_err(storage)?;

        // The tables were changed underneath the vector cache.
        self.vectors.forget(ns);

        let mut report = RebuildReport {
            namespace: ns.to_owned(),
            ..RebuildReport::default()
        };
        let ids: HashSet<u128> = nodes.iter().map(|node| node.id.as_u128()).collect();
        for node in &nodes {
            self.put_node(ns, node, None)?;
            report.nodes += 1;
        }
        for edge in &edges {
            if ids.contains(&edge.from.as_u128()) && ids.contains(&edge.to.as_u128()) {
                self.put_edge(ns, edge)?;
                bump_namespace(&self.tx, ns, 0, 1)?;
                bump_edge_label(&self.tx, ns, &edge.label, 1)?;
                report.edges += 1;
            } else {
                self.tx
                    .open_table(EDGES)
                    .map_err(storage)?
                    .remove((ns, edge.id.as_u128()))
                    .map_err(storage)?;
                report.dropped_edges += 1;
            }
        }
        for (id, content) in &texts {
            if ids.contains(id) {
                text::index(&self.tx, ns, NodeId::from_u128(*id), content)?;
                report.texts += 1;
            } else {
                report.dropped_texts += 1;
            }
        }
        for (id, embedding) in &vectors {
            if ids.contains(&id.as_u128()) {
                vector::insert(&self.tx, &mut self.vectors, ns, *id, embedding)?;
                report.vectors += 1;
            } else {
                report.dropped_vectors += 1;
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use grafeno_core::{NewEdge, NewNode};

    use crate::Grafeno;
    use crate::store::{
        ADJ_IN, HNSW_LINKS, LABEL_COUNTS, NODE_LABELS, NS_COUNTS, POSTINGS, TEXT_LENGTHS,
    };

    #[test]
    fn check_finds_corruption_and_rebuild_repairs_it() {
        let db = Grafeno::in_memory().unwrap();
        let (a, b) = db
            .write(|tx| {
                let a = tx.create_node(
                    "ns",
                    NewNode::new("Person")
                        .text("alpha beta")
                        .embedding(vec![1.0, 0.0]),
                )?;
                let b = tx.create_node(
                    "ns",
                    NewNode::new("Person")
                        .text("beta gamma")
                        .embedding(vec![0.0, 1.0]),
                )?;
                tx.create_edge("ns", NewEdge::new(a, "knows", b))?;
                tx.create_node("other", NewNode::new("Note").text("untouched"))?;
                Ok((a, b))
            })
            .unwrap();
        assert!(db.begin_read().unwrap().check("ns").unwrap().is_ok());

        // Damage every derived structure.
        db.write(|tx| {
            let t = &tx.tx;
            t.open_table(NODE_LABELS)
                .unwrap()
                .remove(("ns", "Person", a.as_u128()))
                .unwrap();
            t.open_table(ADJ_IN)
                .unwrap()
                .retain(|key, _| key.0 != "ns")
                .unwrap();
            t.open_table(POSTINGS)
                .unwrap()
                .insert(("ns", "ghost", b.as_u128()), (7, 1))
                .unwrap();
            t.open_table(TEXT_LENGTHS)
                .unwrap()
                .insert(("ns", a.as_u128()), 99)
                .unwrap();
            t.open_table(NS_COUNTS)
                .unwrap()
                .insert("ns", (5, 5))
                .unwrap();
            t.open_table(LABEL_COUNTS)
                .unwrap()
                .insert(("ns", "Person"), 1)
                .unwrap();
            t.open_table(HNSW_LINKS)
                .unwrap()
                .retain(|key, _| key.0 != "ns")
                .unwrap();
            Ok(())
        })
        .unwrap();

        let report = db.begin_read().unwrap().check("ns").unwrap();
        let all = report.problems.join("\n");
        for expected in [
            "label index misses node",
            "incoming adjacency of edge",
            "postings in the text index",
            "text length of node",
            "namespace counters say",
            "node label counters",
            "cannot be reached from the entry point",
        ] {
            assert!(all.contains(expected), "missing {expected:?} in:\n{all}");
        }

        db.write(|tx| tx.rebuild_indexes("ns")).unwrap();
        let read = db.begin_read().unwrap();
        let report = read.check("ns").unwrap();
        assert!(report.is_ok(), "{:?}", report.problems);
        assert!(read.check("other").unwrap().is_ok());
        assert_eq!(read.search_text("ns", "gamma", 5).unwrap()[0].node, b);
        assert!(read.search_text("ns", "ghost", 5).unwrap().is_empty());
        assert_eq!(read.search_vector("ns", &[1.0, 0.1], 1).unwrap()[0].node, a);
    }
}
