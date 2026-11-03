use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{EdgeId, NodeId, Timestamp};

/// Properties of a node or edge.
pub type Props = serde_json::Map<String, Value>;

/// A node: an entity, document, chunk, episode… whatever its labels say.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    /// Sorted, without duplicates.
    pub labels: Vec<String>,
    #[serde(default)]
    pub props: Props,
    /// Nodes this one was derived from (provenance).
    #[serde(default)]
    pub sources: Vec<NodeId>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl Node {
    pub fn has_label(&self, label: &str) -> bool {
        self.labels
            .binary_search_by(|item| item.as_str().cmp(label))
            .is_ok()
    }

    pub fn prop(&self, key: &str) -> Option<&Value> {
        self.props.get(key)
    }

    /// A short human name: the `name`, `title` or `label` property, or the
    /// first label and the start of the id.
    pub fn display_name(&self) -> String {
        ["name", "title", "label"]
            .iter()
            .find_map(|key| self.prop(key).and_then(Value::as_str))
            .map(str::to_owned)
            .unwrap_or_else(|| {
                let label = self.labels.first().map(String::as_str).unwrap_or("Node");
                let id = self.id.to_string();
                format!("{label} {}", &id[id.len() - 8..])
            })
    }
}

/// A directed edge: a fact relating two nodes.
///
/// It has two time axes. *Valid time* (`valid_from`, `valid_to`) says when the
/// fact is true in the world; `None` is open-ended. *Record time*
/// (`recorded_at`, `expired_at`) says when the engine learned it and when it
/// was retracted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub id: EdgeId,
    pub label: String,
    pub from: NodeId,
    pub to: NodeId,
    #[serde(default)]
    pub props: Props,
    #[serde(default)]
    pub sources: Vec<NodeId>,
    pub valid_from: Option<Timestamp>,
    pub valid_to: Option<Timestamp>,
    pub recorded_at: Timestamp,
    pub expired_at: Option<Timestamp>,
}

impl Edge {
    /// The fact is true at `at` (`valid_from <= at < valid_to`).
    pub fn holds_at(&self, at: Timestamp) -> bool {
        self.valid_from.is_none_or(|from| from <= at) && self.valid_to.is_none_or(|to| at < to)
    }

    /// The engine knew the fact at `at` (`recorded_at <= at < expired_at`).
    pub fn known_at(&self, at: Timestamp) -> bool {
        self.recorded_at <= at && self.expired_at.is_none_or(|expired| at < expired)
    }

    pub fn is_retracted(&self) -> bool {
        self.expired_at.is_some()
    }

    /// The other end, seen from `node`.
    pub fn other(&self, node: NodeId) -> NodeId {
        if self.from == node {
            self.to
        } else {
            self.from
        }
    }
}

/// Data to create a node.
#[derive(Clone, Debug, Default)]
pub struct NewNode {
    pub labels: Vec<String>,
    pub props: Props,
    pub sources: Vec<NodeId>,
    /// Text indexed for full-text search (BM25).
    pub text: Option<String>,
    /// Embedding indexed for vector search.
    pub embedding: Option<Vec<f32>>,
}

impl NewNode {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            labels: vec![label.into()],
            ..Self::default()
        }
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.labels.push(label.into());
        self
    }

    pub fn prop(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.props.insert(key.into(), value.into());
        self
    }

    pub fn source(mut self, source: NodeId) -> Self {
        self.sources.push(source);
        self
    }

    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    pub fn embedding(mut self, embedding: impl Into<Vec<f32>>) -> Self {
        self.embedding = Some(embedding.into());
        self
    }
}

/// Data to create an edge.
#[derive(Clone, Debug)]
pub struct NewEdge {
    pub from: NodeId,
    pub label: String,
    pub to: NodeId,
    pub props: Props,
    pub sources: Vec<NodeId>,
    pub valid_from: Option<Timestamp>,
    pub valid_to: Option<Timestamp>,
}

impl NewEdge {
    pub fn new(from: NodeId, label: impl Into<String>, to: NodeId) -> Self {
        Self {
            from,
            label: label.into(),
            to,
            props: Props::new(),
            sources: Vec::new(),
            valid_from: None,
            valid_to: None,
        }
    }

    pub fn prop(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.props.insert(key.into(), value.into());
        self
    }

    pub fn source(mut self, source: NodeId) -> Self {
        self.sources.push(source);
        self
    }

    pub fn valid_from(mut self, at: Timestamp) -> Self {
        self.valid_from = Some(at);
        self
    }

    pub fn valid_to(mut self, at: Timestamp) -> Self {
        self.valid_to = Some(at);
        self
    }
}
