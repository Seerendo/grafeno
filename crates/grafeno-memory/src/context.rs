use std::collections::{HashMap, HashSet};

use grafeno::{Direction, Edge, EdgeFilter, EdgeId, Node, NodeId, ReadTx, Result};

use crate::{RecallQuery, recall};

const MEMORY_HEADER: &str = "## Memory\n";
const FACTS_HEADER: &str = "\n## Facts\n";
/// Below this many tokens, a truncated item is not worth including.
const MIN_TRUNCATED_TOKENS: usize = 24;
const ELLIPSIS: &str = "…";

/// A rough token count: about four characters per token in English prose.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

#[derive(Clone, Debug)]
pub struct ContextOptions {
    /// Maximum tokens of the rendered context.
    pub budget: usize,
    /// Share of the budget kept for facts, from 0 to 1. Facts also get
    /// whatever the items leave unused.
    pub fact_share: f32,
    /// Maximum facts considered.
    pub max_facts: usize,
    /// Facts (edges) must pass this filter.
    pub facts: EdgeFilter,
    /// Counts the tokens of a piece of text. Defaults to [`estimate_tokens`];
    /// pass the model's tokenizer for exact budgets.
    pub count_tokens: fn(&str) -> usize,
}

impl Default for ContextOptions {
    fn default() -> Self {
        Self::with_budget(2_000)
    }
}

impl ContextOptions {
    pub fn with_budget(budget: usize) -> Self {
        Self {
            budget,
            fact_share: 0.25,
            max_facts: 50,
            facts: EdgeFilter::any(),
            count_tokens: estimate_tokens,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContextItem {
    pub node: NodeId,
    pub name: String,
    pub labels: Vec<String>,
    /// The indexed text, or the other properties as JSON.
    pub text: String,
    pub score: f32,
    pub sources: Vec<NodeId>,
    /// The text was cut to fit the budget.
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContextFact {
    pub edge: EdgeId,
    pub from: NodeId,
    pub label: String,
    pub to: NodeId,
    /// The rendered line, such as `Ana works_at Acme (from 2024-01-01)`.
    pub text: String,
}

/// Recalled knowledge packed for a prompt.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Context {
    pub items: Vec<ContextItem>,
    pub facts: Vec<ContextFact>,
    /// Tokens of `rendered`, as counted by `ContextOptions::count_tokens`
    /// (the sum of its pieces).
    pub tokens: usize,
    pub rendered: String,
}

/// Recalls nodes for `query` and packs them, best first, with the facts
/// that connect them, into at most `options.budget` tokens.
pub fn context_for(
    read: &ReadTx,
    ns: &str,
    query: &RecallQuery,
    options: &ContextOptions,
) -> Result<Context> {
    let count = options.count_tokens;
    let fact_reserve = (options.budget as f32 * options.fact_share.clamp(0.0, 1.0)) as usize;
    let item_budget = options.budget.saturating_sub(fact_reserve);
    let mut context = Context::default();

    for recalled in recall(read, ns, query)? {
        let node = &recalled.node;
        let name = node.display_name();
        let number = context.items.len() + 1;
        let header_cost = if context.items.is_empty() {
            count(MEMORY_HEADER)
        } else {
            0
        };
        let available = item_budget.saturating_sub(context.tokens + header_cost);
        let mut text = item_body(read, ns, node)?;

        let mut block = render_item(number, &name, &node.labels, &text);
        let mut cost = count(&block);
        let mut truncated = false;
        if cost > available {
            let frame = count(&render_item(number, &name, &node.labels, ""));
            if text.is_empty() || available < frame + MIN_TRUNCATED_TOKENS {
                continue;
            }
            text = truncate(&text, available - frame, count);
            block = render_item(number, &name, &node.labels, &text);
            cost = count(&block);
            if cost > available {
                continue;
            }
            truncated = true;
        }
        if header_cost > 0 {
            context.rendered.push_str(MEMORY_HEADER);
        }
        context.rendered.push_str(&block);
        context.tokens += header_cost + cost;
        context.items.push(ContextItem {
            node: node.id,
            name,
            labels: node.labels.clone(),
            text,
            score: recalled.score,
            sources: node.sources.clone(),
            truncated,
        });
    }

    add_facts(read, ns, options, &mut context)?;
    Ok(context)
}

fn add_facts(
    read: &ReadTx,
    ns: &str,
    options: &ContextOptions,
    context: &mut Context,
) -> Result<()> {
    let count = options.count_tokens;
    let names: HashMap<NodeId, String> = context
        .items
        .iter()
        .map(|item| (item.node, item.name.clone()))
        .collect();
    let scores: HashMap<NodeId, f32> = context
        .items
        .iter()
        .map(|item| (item.node, item.score))
        .collect();

    // Facts between two recalled nodes first, then facts touching one.
    let mut seen = HashSet::new();
    let mut candidates: Vec<(bool, f32, Edge)> = Vec::new();
    for item in &context.items {
        for neighbor in read.neighbors(ns, item.node, Direction::Both, &options.facts)? {
            if !seen.insert(neighbor.edge.id) {
                continue;
            }
            let both = names.contains_key(&neighbor.node);
            let score = item.score + scores.get(&neighbor.node).copied().unwrap_or(0.0);
            candidates.push((both, score, neighbor.edge));
        }
    }
    candidates.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.1.total_cmp(&a.1))
            .then(a.2.id.cmp(&b.2.id))
    });
    candidates.truncate(options.max_facts);

    let mut other_names: HashMap<NodeId, String> = HashMap::new();
    for (_, _, edge) in candidates {
        let from = name_of(read, ns, edge.from, &names, &mut other_names)?;
        let to = name_of(read, ns, edge.to, &names, &mut other_names)?;
        let line = format!("- {from} {} {to}{}\n", edge.label, validity(&edge));
        let header_cost = if context.facts.is_empty() {
            count(FACTS_HEADER)
        } else {
            0
        };
        let cost = count(&line);
        if context.tokens + header_cost + cost > options.budget {
            continue;
        }
        if header_cost > 0 {
            context.rendered.push_str(FACTS_HEADER);
        }
        context.rendered.push_str(&line);
        context.tokens += header_cost + cost;
        context.facts.push(ContextFact {
            edge: edge.id,
            from: edge.from,
            label: edge.label,
            to: edge.to,
            text: line.trim_start_matches("- ").trim_end().to_owned(),
        });
    }
    Ok(())
}

fn name_of(
    read: &ReadTx,
    ns: &str,
    id: NodeId,
    names: &HashMap<NodeId, String>,
    cache: &mut HashMap<NodeId, String>,
) -> Result<String> {
    if let Some(name) = names.get(&id).or_else(|| cache.get(&id)) {
        return Ok(name.clone());
    }
    let name = read
        .node(ns, id)?
        .map(|node| node.display_name())
        .unwrap_or_else(|| id.to_string());
    cache.insert(id, name.clone());
    Ok(name)
}

fn validity(edge: &Edge) -> String {
    match (edge.valid_from, edge.valid_to) {
        (None, None) => String::new(),
        (Some(from), None) => format!(" (from {})", from.date()),
        (None, Some(to)) => format!(" (until {})", to.date()),
        (Some(from), Some(to)) => format!(" ({} to {})", from.date(), to.date()),
    }
}

/// The indexed text of a node, or its properties other than the name as JSON.
fn item_body(read: &ReadTx, ns: &str, node: &Node) -> Result<String> {
    if let Some(text) = read.text(ns, node.id)? {
        return Ok(text);
    }
    let mut props = node.props.clone();
    for key in ["name", "title", "label"] {
        props.remove(key);
    }
    Ok(if props.is_empty() {
        String::new()
    } else {
        grafeno::Value::Object(props).to_string()
    })
}

fn render_item(number: usize, name: &str, labels: &[String], body: &str) -> String {
    let labels = labels.join(", ");
    if body.is_empty() {
        format!("[{number}] {name} ({labels})\n")
    } else {
        format!("[{number}] {name} ({labels})\n{body}\n\n")
    }
}

/// The longest prefix of `text`, plus an ellipsis, within `max_tokens`.
fn truncate(text: &str, max_tokens: usize, count: fn(&str) -> usize) -> String {
    let ends: Vec<usize> = text
        .char_indices()
        .map(|(at, c)| at + c.len_utf8())
        .collect();
    let fits = |chars: usize| {
        let end = if chars == 0 { 0 } else { ends[chars - 1] };
        count(&format!("{}{ELLIPSIS}", &text[..end])) <= max_tokens
    };
    let (mut low, mut high) = (0, ends.len());
    while low < high {
        let middle = (low + high).div_ceil(2);
        if fits(middle) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    let end = if low == 0 { 0 } else { ends[low - 1] };
    format!("{}{ELLIPSIS}", text[..end].trim_end())
}
