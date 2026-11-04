//! Planning a pattern match: where each pattern starts.
//!
//! The start (anchor) is the most selective node of the pattern: a variable
//! bound by an earlier clause, then an `@id`, then a property index (from
//! the pattern's own properties or the `WHERE` conditions), then the label
//! with the fewest nodes. Everything else is reached by expanding edges.

use std::collections::{HashMap, HashSet};
use std::ops::Bound;

use grafeno::{IndexDef, IndexRange};
use serde_json::Value as Json;

use crate::QueryError;
use crate::ast::{BinOp, Expr, Key, NodePattern};
use crate::eval::{Context, Row, Scope, eval};
use crate::value::Value;

/// What the planner knows about the namespace, read once per statement.
#[derive(Clone, Debug, Default)]
pub(crate) struct PlanInfo {
    pub(crate) indexes: Vec<IndexDef>,
    pub(crate) label_counts: HashMap<String, u64>,
    pub(crate) edge_labels: HashMap<String, u64>,
    pub(crate) nodes: u64,
    pub(crate) edges: u64,
}

/// The parts of a condition joined by `AND`.
pub(crate) fn conjuncts(expr: &Expr) -> Vec<&Expr> {
    match expr {
        Expr::Binary(BinOp::And, left, right) => {
            let mut parts = conjuncts(left);
            parts.extend(conjuncts(right));
            parts
        }
        other => vec![other],
    }
}

/// The variables an expression reads, or `None` when it holds a pattern
/// predicate or binds variables of its own (comprehensions and the like),
/// which only the full row can evaluate.
pub(crate) fn free_vars(expr: &Expr) -> Option<HashSet<String>> {
    let mut vars = HashSet::new();
    let mut local = true;
    expr.visit(&mut |inner| match inner {
        Expr::Var(name) => {
            vars.insert(name.clone());
        }
        Expr::Exists { .. }
        | Expr::Comprehension { .. }
        | Expr::Quantified { .. }
        | Expr::Reduce { .. } => local = false,
        _ => {}
    });
    local.then_some(vars)
}

fn evaluable(expr: &Expr, is_bound: &dyn Fn(&str) -> bool) -> bool {
    free_vars(expr).is_some_and(|vars| vars.iter().all(|var| is_bound(var)))
}

/// Which values of an index to read.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Seek<'q> {
    Equal(&'q Expr),
    In(&'q Expr),
    Prefix(&'q Expr),
    /// Bounds, each with whether it is inclusive.
    Range {
        lower: Option<(&'q Expr, bool)>,
        upper: Option<(&'q Expr, bool)>,
    },
}

/// How the start of a pattern is found.
#[derive(Clone, Debug)]
pub(crate) enum Source<'q> {
    Bound,
    Id(&'q Expr),
    Index {
        name: String,
        key: String,
        seek: Seek<'q>,
    },
    Label {
        label: String,
        count: u64,
    },
    Scan,
}

impl Source<'_> {
    /// Higher is more selective; ties between labels go to the smaller one.
    fn score(&self) -> (u8, i64) {
        match self {
            Source::Bound => (7, 0),
            Source::Id(_) => (6, 0),
            Source::Index {
                seek: Seek::Equal(_),
                ..
            } => (5, 0),
            Source::Index {
                seek: Seek::In(_), ..
            } => (4, 0),
            Source::Index { .. } => (3, 0),
            Source::Label { count, .. } => (1, -(*count as i64)),
            Source::Scan => (0, 0),
        }
    }

    pub(crate) fn describe(&self, node: &str) -> String {
        match self {
            Source::Bound => format!("start at {node} (the bound node)"),
            Source::Id(_) => format!("start at {node} (lookup by @id)"),
            Source::Index { name, key, seek } => {
                let how = match seek {
                    Seek::Equal(_) => format!("{key} = …"),
                    Seek::In(_) => format!("{key} IN […]"),
                    Seek::Prefix(_) => format!("{key} STARTS WITH …"),
                    Seek::Range { lower, upper } => {
                        let low = lower.map_or(
                            "",
                            |(_, inclusive)| if inclusive { "… <= " } else { "… < " },
                        );
                        let high = upper.map_or(
                            "",
                            |(_, inclusive)| if inclusive { " <= …" } else { " < …" },
                        );
                        format!("{low}{key}{high}")
                    }
                };
                format!("start at {node} (index {name}: {how})")
            }
            Source::Label { label, count } => {
                format!("start at {node} (scan of :{label}, {count} nodes)")
            }
            Source::Scan => format!("start at {node} (scan of every node)"),
        }
    }
}

/// The best way to start from one node pattern.
fn source_for<'q>(
    node: &'q NodePattern,
    is_bound: &dyn Fn(&str) -> bool,
    conditions: &[&'q Expr],
    info: &PlanInfo,
) -> Source<'q> {
    if node.var.as_deref().is_some_and(is_bound) {
        return Source::Bound;
    }
    if let Some((_, expr)) = node
        .props
        .iter()
        .find(|(key, expr)| *key == Key::Meta("id".into()) && evaluable(expr, is_bound))
    {
        return Source::Id(expr);
    }
    let mut best: Option<Source<'q>> = None;
    let mut consider = |candidate: Source<'q>| {
        if best
            .as_ref()
            .is_none_or(|current| candidate.score() > current.score())
        {
            best = Some(candidate);
        }
    };
    for def in info
        .indexes
        .iter()
        .filter(|def| node.labels.contains(&def.label))
    {
        if let Some(seek) = seek_for(node, &def.key, is_bound, conditions) {
            consider(Source::Index {
                name: def.name.clone(),
                key: def.key.clone(),
                seek,
            });
        }
    }
    for label in &node.labels {
        consider(Source::Label {
            label: label.clone(),
            count: info.label_counts.get(label).copied().unwrap_or(0),
        });
    }
    best.unwrap_or(Source::Scan)
}

/// A way to read the index on `key`: the pattern's own `{key: value}`, or
/// conditions such as `n.key = …`, `n.key IN …`, `n.key STARTS WITH …` and
/// ranges, whose other side can be computed before matching.
fn seek_for<'q>(
    node: &'q NodePattern,
    key: &str,
    is_bound: &dyn Fn(&str) -> bool,
    conditions: &[&'q Expr],
) -> Option<Seek<'q>> {
    if let Some((_, expr)) = node
        .props
        .iter()
        .find(|(k, expr)| *k == Key::Prop(key.to_owned()) && evaluable(expr, is_bound))
    {
        return Some(Seek::Equal(expr));
    }
    let var = node.var.as_deref()?;
    let is_property = |expr: &Expr| {
        matches!(expr, Expr::Property(target, Key::Prop(name))
            if name == key && matches!(target.as_ref(), Expr::Var(v) if v == var))
    };
    let (mut lower, mut upper, mut other) = (None, None, None);
    for condition in conditions {
        let Expr::Binary(op, left, right) = condition else {
            continue;
        };
        // `n.key op value`, or `value op n.key` turned around.
        let (op, value) = if is_property(left) && evaluable(right, is_bound) {
            (*op, right.as_ref())
        } else if is_property(right) && evaluable(left, is_bound) {
            let flipped = match op {
                BinOp::Lt => BinOp::Gt,
                BinOp::Le => BinOp::Ge,
                BinOp::Gt => BinOp::Lt,
                BinOp::Ge => BinOp::Le,
                BinOp::Eq => BinOp::Eq,
                _ => continue,
            };
            (flipped, left.as_ref())
        } else {
            continue;
        };
        match op {
            BinOp::Eq => return Some(Seek::Equal(value)),
            BinOp::In => other = Some(Seek::In(value)),
            BinOp::StartsWith if other.is_none() => other = Some(Seek::Prefix(value)),
            BinOp::Gt => lower = Some((value, false)),
            BinOp::Ge => lower = Some((value, true)),
            BinOp::Lt => upper = Some((value, false)),
            BinOp::Le => upper = Some((value, true)),
            _ => {}
        }
    }
    other.or_else(|| (lower.is_some() || upper.is_some()).then_some(Seek::Range { lower, upper }))
}

/// Where to start a pattern: the position of its anchor node and how to
/// find it.
pub(crate) fn plan_anchor<'q>(
    nodes: &[&'q NodePattern],
    is_bound: &dyn Fn(&str) -> bool,
    conditions: &[&'q Expr],
    info: &PlanInfo,
) -> (usize, Source<'q>) {
    let mut best = (0, source_for(nodes[0], is_bound, conditions, info));
    for (index, node) in nodes.iter().enumerate().skip(1) {
        let source = source_for(node, is_bound, conditions, info);
        if source.score() > best.1.score() {
            best = (index, source);
        }
    }
    best
}

/// A value the index can look up: numbers, strings and booleans.
fn indexable(value: &Value) -> Option<Json> {
    match value {
        Value::Int(i) => Some(Json::from(*i)),
        Value::Float(f) if f.is_finite() => serde_json::Number::from_f64(*f).map(Json::Number),
        Value::Str(s) => Some(Json::String(s.clone())),
        Value::Bool(b) => Some(Json::Bool(*b)),
        _ => None,
    }
}

/// The index ranges to read for `seek` in this row. `None`: the values
/// cannot be looked up (a time, a list…), so scan the label instead. An
/// empty list: nothing can match (`= null`).
pub(crate) fn seek_ranges(
    ctx: &Context,
    seek: Seek,
    row: &Row,
) -> Result<Option<Vec<IndexRange>>, QueryError> {
    let value = |expr: &Expr| eval(ctx, expr, Scope::Row(row));
    Ok(match seek {
        Seek::Equal(expr) => match value(expr)? {
            Value::Null => Some(Vec::new()),
            other => indexable(&other).map(|json| vec![IndexRange::Equal(json)]),
        },
        Seek::In(expr) => match value(expr)? {
            Value::Null => Some(Vec::new()),
            Value::List(items) => {
                let mut ranges = Vec::new();
                for item in items.iter().filter(|item| !item.is_null()) {
                    match indexable(item) {
                        Some(json) => ranges.push(IndexRange::Equal(json)),
                        None => return Ok(None),
                    }
                }
                Some(ranges)
            }
            _ => None,
        },
        Seek::Prefix(expr) => match value(expr)? {
            Value::Null => Some(Vec::new()),
            Value::Str(prefix) => Some(vec![IndexRange::Prefix(prefix)]),
            _ => None,
        },
        Seek::Range { lower, upper } => {
            // Inclusive on both ends: the condition itself still runs on
            // every node found.
            let bound = |side: Option<(&Expr, bool)>| -> Result<Option<Bound<Json>>, QueryError> {
                Ok(match side {
                    None => Some(Bound::Unbounded),
                    Some((expr, _)) => match value(expr)? {
                        Value::Null => None,
                        other => indexable(&other).map(Bound::Included),
                    },
                })
            };
            match (bound(lower)?, bound(upper)?) {
                (Some(lower), Some(upper)) => Some(vec![IndexRange::Between { lower, upper }]),
                // A null bound: the comparison is never true.
                _ if [lower, upper]
                    .iter()
                    .flatten()
                    .any(|(expr, _)| value(expr).is_ok_and(|value| value.is_null())) =>
                {
                    Some(Vec::new())
                }
                _ => None,
            }
        }
    })
}
