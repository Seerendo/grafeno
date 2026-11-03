use std::collections::HashMap;

use grafeno::{Direction, EdgeFilter, Node, NodeId, ReadTx, Result};
use serde::Serialize;

/// Reciprocal rank fusion constant (Cormack, Clarke and Büttcher, 2009).
const RRF_K: f32 = 60.0;

/// How recall spreads relevance through the graph.
#[derive(Clone, Debug)]
pub struct Expansion {
    /// Hops to follow from each seed.
    pub depth: usize,
    pub direction: Direction,
    pub filter: EdgeFilter,
    /// Share of a seed's score passed on per hop, from 0 to 1.
    pub decay: f32,
    /// How many of the best direct matches are expanded.
    pub seeds: usize,
}

impl Default for Expansion {
    fn default() -> Self {
        Self {
            depth: 1,
            direction: Direction::Both,
            filter: EdgeFilter::any(),
            decay: 0.5,
            seeds: 5,
        }
    }
}

/// What to recall. At least one of `text` and `embedding` should be set;
/// without them nothing matches.
#[derive(Clone, Debug)]
pub struct RecallQuery {
    pub text: Option<String>,
    pub embedding: Option<Vec<f32>>,
    pub limit: usize,
    /// Keep only nodes with at least one of these labels (empty: any label).
    pub labels: Vec<String>,
    /// Candidates taken from each index before fusing them.
    pub candidates: usize,
    /// Graph expansion; `None` disables it.
    pub expand: Option<Expansion>,
}

impl Default for RecallQuery {
    fn default() -> Self {
        Self {
            text: None,
            embedding: None,
            limit: 10,
            labels: Vec::new(),
            candidates: 50,
            expand: Some(Expansion::default()),
        }
    }
}

impl RecallQuery {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    pub fn embedding(mut self, embedding: impl Into<Vec<f32>>) -> Self {
        self.embedding = Some(embedding.into());
        self
    }

    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.labels.push(label.into());
        self
    }

    pub fn expand(mut self, expansion: Option<Expansion>) -> Self {
        self.expand = expansion;
        self
    }
}

/// Why a node was recalled. Ranks start at 1.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Signal {
    Text { rank: usize, score: f32 },
    Vector { rank: usize, similarity: f32 },
    Graph { seed: NodeId, hops: usize },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Recalled {
    pub node: Node,
    /// Fused score: comparable within one recall, not across queries.
    pub score: f32,
    pub signals: Vec<Signal>,
}

/// Hybrid recall: BM25 and vector search fused with RRF, then graph expansion.
pub fn recall(read: &ReadTx, ns: &str, query: &RecallQuery) -> Result<Vec<Recalled>> {
    let mut scores: HashMap<NodeId, (f32, Vec<Signal>)> = HashMap::new();
    let mut add = |node: NodeId, rank: usize, signal: Signal| {
        let entry = scores.entry(node).or_default();
        entry.0 += 1.0 / (RRF_K + rank as f32);
        entry.1.push(signal);
    };
    if let Some(text) = &query.text {
        for (index, hit) in read
            .search_text(ns, text, query.candidates)?
            .into_iter()
            .enumerate()
        {
            let rank = index + 1;
            add(
                hit.node,
                rank,
                Signal::Text {
                    rank,
                    score: hit.score,
                },
            );
        }
    }
    if let Some(embedding) = &query.embedding {
        for (index, hit) in read
            .search_vector(ns, embedding, query.candidates)?
            .into_iter()
            .enumerate()
        {
            let rank = index + 1;
            add(
                hit.node,
                rank,
                Signal::Vector {
                    rank,
                    similarity: hit.score,
                },
            );
        }
    }

    if let Some(expansion) = &query.expand {
        let mut seeds: Vec<(NodeId, f32)> = scores
            .iter()
            .map(|(id, (score, _))| (*id, *score))
            .collect();
        sort_by_score(&mut seeds);
        seeds.truncate(expansion.seeds);
        for (seed, seed_score) in seeds {
            for (node, hops) in read.k_hop(
                ns,
                seed,
                expansion.depth,
                expansion.direction,
                &expansion.filter,
            )? {
                let entry = scores.entry(node).or_default();
                entry.0 += seed_score * expansion.decay.powi(hops as i32);
                entry.1.push(Signal::Graph { seed, hops });
            }
        }
    }

    let mut ranked: Vec<(NodeId, f32)> = scores
        .iter()
        .map(|(id, (score, _))| (*id, *score))
        .collect();
    sort_by_score(&mut ranked);
    let mut out = Vec::new();
    for (id, score) in ranked {
        if out.len() >= query.limit {
            break;
        }
        let Some(node) = read.node(ns, id)? else {
            continue;
        };
        if !query.labels.is_empty() && !query.labels.iter().any(|label| node.has_label(label)) {
            continue;
        }
        let signals = scores
            .remove(&id)
            .map(|(_, signals)| signals)
            .unwrap_or_default();
        out.push(Recalled {
            node,
            score,
            signals,
        });
    }
    Ok(out)
}

/// Best first; ties broken by id so results are deterministic.
fn sort_by_score(items: &mut [(NodeId, f32)]) {
    items.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
}
