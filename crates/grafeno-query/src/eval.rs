//! Expression evaluation: operators, pseudo-properties, functions and
//! aggregates.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use grafeno::{Direction, Edge, EdgeFilter, EdgeId, Node, NodeId, ReadTx, Timestamp, WriteTx};

use crate::QueryError;
use crate::ast::{AGGREGATES, BinOp, Expr, Key, Quantifier, UnOp};
use crate::value::{Value, compare, equals, sort_order};

/// Variable bindings of one result row.
pub(crate) type Row = HashMap<String, Value>;

/// Query parameters (`$name`).
pub type Params = HashMap<String, Value>;

/// What expressions can read: the snapshot the query started on and, in a
/// writing query, the write transaction, which sees the query's own writes
/// and is read instead.
pub(crate) struct Context<'a> {
    pub(crate) read: &'a ReadTx,
    pub(crate) write: Option<&'a WriteTx>,
    pub(crate) ns: &'a str,
    pub(crate) params: &'a Params,
    /// The edges patterns may follow: the enclosing `MATCH`'s `VALID AT`
    /// and `KNOWN AT`, for pattern predicates too.
    pub(crate) edges: &'a EdgeFilter,
}

impl Context<'_> {
    pub(crate) fn node(&self, id: NodeId) -> Result<Option<Node>, QueryError> {
        Ok(match self.write {
            Some(tx) => tx.node(self.ns, id)?,
            None => self.read.node(self.ns, id)?,
        })
    }

    pub(crate) fn edge(&self, id: EdgeId) -> Result<Option<Edge>, QueryError> {
        Ok(match self.write {
            Some(tx) => tx.edge(self.ns, id)?,
            None => self.read.edge(self.ns, id)?,
        })
    }

    /// Every node, or every node with `label`.
    pub(crate) fn nodes(&self, label: Option<&str>) -> Result<Vec<Node>, QueryError> {
        Ok(match self.write {
            Some(tx) => tx.nodes_page(self.ns, label, None, usize::MAX)?,
            None => self.read.nodes_page(self.ns, label, None, usize::MAX)?,
        })
    }

    pub(crate) fn neighbor_ids(
        &self,
        from: NodeId,
        direction: Direction,
        filter: &EdgeFilter,
    ) -> Result<Vec<(EdgeId, NodeId)>, QueryError> {
        Ok(match self.write {
            Some(tx) => tx.neighbor_ids(self.ns, from, direction, filter)?,
            None => self.read.neighbor_ids(self.ns, from, direction, filter)?,
        })
    }

    fn text(&self, id: NodeId) -> Result<Option<String>, QueryError> {
        Ok(match self.write {
            Some(tx) => tx.text(self.ns, id)?,
            None => self.read.text(self.ns, id)?,
        })
    }

    fn memory(&self, node: &Node) -> Result<grafeno::MemoryState, QueryError> {
        Ok(match self.write {
            Some(tx) => tx.memory(self.ns, node)?,
            None => self.read.memory(self.ns, node)?,
        })
    }

    fn embedding(&self, id: NodeId) -> Result<Option<Vec<f32>>, QueryError> {
        Ok(match self.write {
            Some(tx) => tx.embedding(self.ns, id)?,
            None => self.read.embedding(self.ns, id)?,
        })
    }
}

/// Where variables come from: one row, or a group of rows (for aggregates,
/// where plain variables read the group's first row).
#[derive(Clone, Copy)]
pub(crate) enum Scope<'a> {
    Row(&'a Row),
    Group(&'a [Row]),
}

impl Scope<'_> {
    fn var(&self, name: &str) -> Option<&Value> {
        match self {
            Scope::Row(row) => row.get(name),
            Scope::Group(rows) => rows.first().and_then(|row| row.get(name)),
        }
    }

    /// The row variables come from (a group's first row).
    fn row(&self) -> Row {
        match self {
            Scope::Row(row) => (*row).clone(),
            Scope::Group(rows) => rows.first().cloned().unwrap_or_default(),
        }
    }
}

/// The items of a list (`null` gives none; anything else is an error).
fn items_of(value: Value, what: &str) -> Result<Vec<Value>, QueryError> {
    match value {
        Value::List(items) => Ok(items),
        Value::Null => Ok(Vec::new()),
        other => Err(error(format!(
            "{what} needs a list, got a {}",
            other.type_name()
        ))),
    }
}

fn error(message: impl Into<String>) -> QueryError {
    QueryError::runtime(message)
}

/// `true`, `false` or unknown (`null`); anything else is an error.
pub(crate) fn truth(value: &Value) -> Result<Option<bool>, QueryError> {
    match value {
        Value::Bool(b) => Ok(Some(*b)),
        Value::Null => Ok(None),
        other => Err(error(format!(
            "expected a boolean, got a {}",
            other.type_name()
        ))),
    }
}

fn from_truth(value: Option<bool>) -> Value {
    value.map_or(Value::Null, Value::Bool)
}

/// A point in time: a time, an ISO 8601 string or microseconds.
pub(crate) fn to_time(value: &Value) -> Result<Timestamp, QueryError> {
    match value {
        Value::Time(t) => Ok(*t),
        Value::Str(s) => Ok(Timestamp::parse(s)?),
        Value::Int(micros) => Ok(Timestamp::from_micros(*micros)),
        other => Err(error(format!(
            "expected a time, got a {}",
            other.type_name()
        ))),
    }
}

fn display(value: &Value) -> String {
    match value {
        Value::Str(s) => s.clone(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Time(t) => t.iso(),
        other => other.to_json().to_string(),
    }
}

fn ids(ids: &[NodeId]) -> Value {
    Value::List(ids.iter().map(|id| Value::Str(id.to_string())).collect())
}

fn time_or_null(time: Option<Timestamp>) -> Value {
    time.map_or(Value::Null, Value::Time)
}

const NODE_META: &str = "@id, @labels, @text, @embedding, @sources, @created_at, @updated_at, @importance, @accesses, @last_access";
const EDGE_META: &str =
    "@id, @label, @from, @to, @valid_from, @valid_to, @recorded_at, @expired_at, @sources";

pub(crate) fn node_meta(ctx: &Context, node: &Node, name: &str) -> Result<Value, QueryError> {
    Ok(match name {
        "id" => Value::Str(node.id.to_string()),
        "labels" => Value::List(node.labels.iter().cloned().map(Value::Str).collect()),
        "text" => ctx.text(node.id)?.map_or(Value::Null, Value::Str),
        "embedding" => ctx.embedding(node.id)?.map_or(Value::Null, |vector| {
            Value::List(
                vector
                    .into_iter()
                    .map(|x| Value::Float(f64::from(x)))
                    .collect(),
            )
        }),
        "sources" => ids(&node.sources),
        "created_at" => Value::Time(node.created_at),
        // Through the shortest decimal form, so 0.9 reads as 0.9 and not as the
        // f64 closest to the stored f32.
        "importance" => Value::Float(
            ctx.memory(node)?
                .importance
                .to_string()
                .parse()
                .unwrap_or_default(),
        ),
        "accesses" => Value::Int(i64::from(ctx.memory(node)?.accesses)),
        "last_access" => Value::Time(ctx.memory(node)?.last_access),
        "updated_at" => Value::Time(node.updated_at),
        other => {
            return Err(error(format!(
                "nodes have no pseudo-property @{other} (they have {NODE_META})"
            )));
        }
    })
}

fn edge_meta(edge: &Edge, name: &str) -> Result<Value, QueryError> {
    Ok(match name {
        "id" => Value::Str(edge.id.to_string()),
        "label" => Value::Str(edge.label.clone()),
        "from" => Value::Str(edge.from.to_string()),
        "to" => Value::Str(edge.to.to_string()),
        "valid_from" => time_or_null(edge.valid_from),
        "valid_to" => time_or_null(edge.valid_to),
        "recorded_at" => Value::Time(edge.recorded_at),
        "expired_at" => time_or_null(edge.expired_at),
        "sources" => ids(&edge.sources),
        other => {
            return Err(error(format!(
                "edges have no pseudo-property @{other} (they have {EDGE_META})"
            )));
        }
    })
}

pub(crate) fn property(ctx: &Context, target: &Value, key: &Key) -> Result<Value, QueryError> {
    let prop =
        |props: &grafeno::Props, name: &str| props.get(name).map_or(Value::Null, Value::from_json);
    match (target, key) {
        (Value::Null, _) => Ok(Value::Null),
        (Value::Node(node), Key::Prop(name)) => Ok(prop(&node.props, name)),
        (Value::Node(node), Key::Meta(name)) => node_meta(ctx, node, name),
        (Value::Edge(edge), Key::Prop(name)) => Ok(prop(&edge.props, name)),
        (Value::Edge(edge), Key::Meta(name)) => edge_meta(edge, name),
        (Value::Map(map), Key::Prop(name)) => Ok(map.get(name).cloned().unwrap_or(Value::Null)),
        (other, _) => Err(error(format!(
            "cannot read a property of a {}",
            other.type_name()
        ))),
    }
}

pub(crate) fn eval(ctx: &Context, expr: &Expr, scope: Scope) -> Result<Value, QueryError> {
    match expr {
        Expr::Literal(value) => Ok(value.clone()),
        Expr::Param(name) => ctx
            .params
            .get(name)
            .cloned()
            .ok_or_else(|| error(format!("missing parameter ${name}"))),
        Expr::Var(name) => match scope.var(name) {
            Some(value) => Ok(value.clone()),
            None if matches!(scope, Scope::Group(rows) if rows.is_empty()) => Ok(Value::Null),
            None => Err(error(format!("unknown variable `{name}`"))),
        },
        Expr::Property(target, key) => property(ctx, &eval(ctx, target, scope)?, key),
        Expr::Index(target, index) => {
            let (target, index) = (eval(ctx, target, scope)?, eval(ctx, index, scope)?);
            match (&target, &index) {
                (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
                (Value::List(items), Value::Int(i)) => {
                    let len = items.len() as i64;
                    let at = if *i < 0 { len + i } else { *i };
                    Ok(usize::try_from(at)
                        .ok()
                        .and_then(|at| items.get(at).cloned())
                        .unwrap_or(Value::Null))
                }
                (Value::Map(map), Value::Str(key)) => {
                    Ok(map.get(key).cloned().unwrap_or(Value::Null))
                }
                (Value::Node(_) | Value::Edge(_), Value::Str(key)) => {
                    property(ctx, &target, &Key::Prop(key.clone()))
                }
                _ => Err(error(format!(
                    "cannot index a {} with a {}",
                    target.type_name(),
                    index.type_name()
                ))),
            }
        }
        Expr::List(items) => Ok(Value::List(
            items
                .iter()
                .map(|item| eval(ctx, item, scope))
                .collect::<Result<_, _>>()?,
        )),
        Expr::Map(entries) => Ok(Value::Map(
            entries
                .iter()
                .map(|(key, value)| Ok((key.clone(), eval(ctx, value, scope)?)))
                .collect::<Result<BTreeMap<_, _>, QueryError>>()?,
        )),
        Expr::Unary(UnOp::Not, inner) => {
            Ok(from_truth(truth(&eval(ctx, inner, scope)?)?.map(|b| !b)))
        }
        Expr::Unary(UnOp::Neg, inner) => match eval(ctx, inner, scope)? {
            Value::Null => Ok(Value::Null),
            Value::Int(i) => i
                .checked_neg()
                .map(Value::Int)
                .ok_or_else(|| error("integer overflow")),
            Value::Float(f) => Ok(Value::Float(-f)),
            other => Err(error(format!("cannot negate a {}", other.type_name()))),
        },
        Expr::Binary(BinOp::And, left, right) => {
            let left = truth(&eval(ctx, left, scope)?)?;
            if left == Some(false) {
                return Ok(Value::Bool(false));
            }
            let right = truth(&eval(ctx, right, scope)?)?;
            Ok(match (left, right) {
                (_, Some(false)) => Value::Bool(false),
                (Some(true), Some(true)) => Value::Bool(true),
                _ => Value::Null,
            })
        }
        Expr::Binary(BinOp::Or, left, right) => {
            let left = truth(&eval(ctx, left, scope)?)?;
            if left == Some(true) {
                return Ok(Value::Bool(true));
            }
            let right = truth(&eval(ctx, right, scope)?)?;
            Ok(match (left, right) {
                (_, Some(true)) => Value::Bool(true),
                (Some(false), Some(false)) => Value::Bool(false),
                _ => Value::Null,
            })
        }
        Expr::Binary(BinOp::Xor, left, right) => {
            let left = truth(&eval(ctx, left, scope)?)?;
            let right = truth(&eval(ctx, right, scope)?)?;
            Ok(from_truth(left.zip(right).map(|(a, b)| a != b)))
        }
        Expr::Binary(op, left, right) => {
            binary(*op, &eval(ctx, left, scope)?, &eval(ctx, right, scope)?)
        }
        Expr::IsNull { expr, negated } => {
            Ok(Value::Bool(eval(ctx, expr, scope)?.is_null() != *negated))
        }
        Expr::HasLabels(target, labels) => match eval(ctx, target, scope)? {
            Value::Null => Ok(Value::Null),
            Value::Node(node) => Ok(Value::Bool(
                labels.iter().all(|label| node.has_label(label)),
            )),
            other => Err(error(format!(
                "only nodes have labels, not a {}",
                other.type_name()
            ))),
        },
        Expr::CountStar => match scope {
            Scope::Group(rows) => Ok(Value::Int(rows.len() as i64)),
            Scope::Row(_) => Err(error("count(*) is an aggregate: use it in RETURN or WITH")),
        },
        Expr::Slice(target, from, to) => {
            let target = eval(ctx, target, scope)?;
            let from = from
                .as_ref()
                .map(|expr| eval(ctx, expr, scope))
                .transpose()?;
            let to = to.as_ref().map(|expr| eval(ctx, expr, scope)).transpose()?;
            let items = match target {
                Value::List(items) => items,
                Value::Null => return Ok(Value::Null),
                other => return Err(error(format!("cannot slice a {}", other.type_name()))),
            };
            let len = items.len() as i64;
            let at = |value: Option<Value>, default: i64| -> Result<Option<i64>, QueryError> {
                match value {
                    None => Ok(Some(default)),
                    Some(Value::Null) => Ok(None),
                    Some(Value::Int(i)) if i < 0 => Ok(Some((len + i).max(0))),
                    Some(Value::Int(i)) => Ok(Some(i.min(len))),
                    Some(other) => Err(error(format!(
                        "a slice needs integers, got a {}",
                        other.type_name()
                    ))),
                }
            };
            Ok(match (at(from, 0)?, at(to, len)?) {
                (Some(from), Some(to)) if from < to => {
                    Value::List(items[from as usize..to as usize].to_vec())
                }
                (Some(_), Some(_)) => Value::List(Vec::new()),
                _ => Value::Null,
            })
        }
        Expr::Case {
            subject,
            branches,
            otherwise,
        } => {
            let subject = subject
                .as_ref()
                .map(|subject| eval(ctx, subject, scope))
                .transpose()?;
            for (when, then) in branches {
                let when = eval(ctx, when, scope)?;
                let hit = match &subject {
                    Some(subject) => equals(subject, &when) == Some(true),
                    None => truth(&when)? == Some(true),
                };
                if hit {
                    return eval(ctx, then, scope);
                }
            }
            match otherwise {
                Some(otherwise) => eval(ctx, otherwise, scope),
                None => Ok(Value::Null),
            }
        }
        Expr::Comprehension {
            var,
            list,
            filter,
            map,
        } => {
            let items = items_of(eval(ctx, list, scope)?, "a list comprehension")?;
            let mut row = scope.row();
            let mut out = Vec::new();
            for item in items {
                row.insert(var.clone(), item.clone());
                if let Some(filter) = filter
                    && truth(&eval(ctx, filter, Scope::Row(&row))?)? != Some(true)
                {
                    continue;
                }
                out.push(match map {
                    Some(map) => eval(ctx, map, Scope::Row(&row))?,
                    None => item,
                });
            }
            Ok(Value::List(out))
        }
        Expr::Quantified {
            quantifier,
            var,
            list,
            predicate,
        } => {
            let value = eval(ctx, list, scope)?;
            if value.is_null() {
                return Ok(Value::Null);
            }
            let items = items_of(value, "a quantifier")?;
            let mut row = scope.row();
            let (mut yes, mut no, mut unknown) = (0usize, 0usize, 0usize);
            for item in items {
                row.insert(var.clone(), item);
                match truth(&eval(ctx, predicate, Scope::Row(&row))?)? {
                    Some(true) => yes += 1,
                    Some(false) => no += 1,
                    None => unknown += 1,
                }
            }
            // Three-valued, as in Cypher: unknown items make the answer
            // unknown unless the known ones already decide it.
            Ok(match quantifier {
                Quantifier::All if no > 0 => Value::Bool(false),
                Quantifier::Any if yes > 0 => Value::Bool(true),
                Quantifier::None if yes > 0 => Value::Bool(false),
                Quantifier::Single if yes > 1 => Value::Bool(false),
                _ if unknown > 0 => Value::Null,
                Quantifier::All => Value::Bool(true),
                Quantifier::Any => Value::Bool(false),
                Quantifier::None => Value::Bool(true),
                Quantifier::Single => Value::Bool(yes == 1),
            })
        }
        Expr::Reduce {
            acc,
            init,
            var,
            list,
            expr,
        } => {
            let mut value = eval(ctx, init, scope)?;
            let items = items_of(eval(ctx, list, scope)?, "reduce()")?;
            let mut row = scope.row();
            for item in items {
                row.insert(acc.clone(), value);
                row.insert(var.clone(), item);
                value = eval(ctx, expr, Scope::Row(&row))?;
            }
            Ok(value)
        }
        Expr::Exists { patterns, filter } => Ok(Value::Bool(crate::exec::exists(
            ctx,
            &scope.row(),
            patterns,
            filter.as_deref(),
        )?)),
        Expr::Call {
            name,
            distinct,
            args,
        } if AGGREGATES.contains(&name.as_str()) => aggregate(ctx, name, *distinct, args, scope),
        Expr::Call { name, args, .. } => {
            let values = args
                .iter()
                .map(|arg| eval(ctx, arg, scope))
                .collect::<Result<Vec<_>, _>>()?;
            function(ctx, name, values)
        }
    }
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

fn arithmetic(
    op: BinOp,
    left: &Value,
    right: &Value,
    ints: fn(i64, i64) -> Option<i64>,
    floats: fn(f64, f64) -> f64,
) -> Result<Value, QueryError> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => ints(*a, *b).map(Value::Int).ok_or_else(|| {
            error(if matches!(op, BinOp::Div | BinOp::Mod) && *b == 0 {
                "division by zero"
            } else {
                "integer overflow"
            })
        }),
        _ => match (number(left), number(right)) {
            (Some(a), Some(b)) => Ok(Value::Float(floats(a, b))),
            _ => Err(error(format!(
                "cannot apply {op:?} to a {} and a {}",
                left.type_name(),
                right.type_name()
            ))),
        },
    }
}

fn binary(op: BinOp, left: &Value, right: &Value) -> Result<Value, QueryError> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let ordering = || compare(left, right);
    Ok(match op {
        BinOp::Eq => from_truth(equals(left, right)),
        BinOp::Neq => from_truth(equals(left, right).map(|b| !b)),
        BinOp::Lt => from_truth(ordering().map(|o| o.is_lt())),
        BinOp::Le => from_truth(ordering().map(|o| o.is_le())),
        BinOp::Gt => from_truth(ordering().map(|o| o.is_gt())),
        BinOp::Ge => from_truth(ordering().map(|o| o.is_ge())),
        BinOp::Add => match (left, right) {
            (Value::List(a), Value::List(b)) => Value::List(a.iter().chain(b).cloned().collect()),
            (Value::List(a), other) => {
                Value::List(a.iter().cloned().chain([other.clone()]).collect())
            }
            (Value::Str(_), _) | (_, Value::Str(_)) => {
                Value::Str(format!("{}{}", display(left), display(right)))
            }
            _ => arithmetic(op, left, right, i64::checked_add, |a, b| a + b)?,
        },
        BinOp::Sub => arithmetic(op, left, right, i64::checked_sub, |a, b| a - b)?,
        BinOp::Mul => arithmetic(op, left, right, i64::checked_mul, |a, b| a * b)?,
        BinOp::Div => arithmetic(op, left, right, i64::checked_div, |a, b| a / b)?,
        BinOp::Mod => arithmetic(op, left, right, i64::checked_rem, |a, b| a % b)?,
        // As in Cypher, `^` always gives a float.
        BinOp::Pow => match (number(left), number(right)) {
            (Some(a), Some(b)) => Value::Float(a.powf(b)),
            _ => {
                return Err(error(format!(
                    "cannot raise a {} to a {}",
                    left.type_name(),
                    right.type_name()
                )));
            }
        },
        BinOp::Contains | BinOp::StartsWith | BinOp::EndsWith => match (left, right) {
            (Value::Str(a), Value::Str(b)) => Value::Bool(match op {
                BinOp::Contains => a.contains(b.as_str()),
                BinOp::StartsWith => a.starts_with(b.as_str()),
                _ => a.ends_with(b.as_str()),
            }),
            _ => {
                return Err(error(format!(
                    "{op:?} needs two strings, got a {} and a {}",
                    left.type_name(),
                    right.type_name()
                )));
            }
        },
        BinOp::In => match right {
            Value::List(items) => {
                let mut unknown = false;
                for item in items {
                    match equals(left, item) {
                        Some(true) => return Ok(Value::Bool(true)),
                        None => unknown = true,
                        Some(false) => {}
                    }
                }
                if unknown {
                    Value::Null
                } else {
                    Value::Bool(false)
                }
            }
            other => {
                return Err(error(format!(
                    "IN needs a list, got a {}",
                    other.type_name()
                )));
            }
        },
        BinOp::And | BinOp::Or | BinOp::Xor => unreachable!("handled with short-circuit"),
    })
}

fn aggregate(
    ctx: &Context,
    name: &str,
    distinct: bool,
    args: &[Expr],
    scope: Scope,
) -> Result<Value, QueryError> {
    let Scope::Group(rows) = scope else {
        return Err(error(format!(
            "{name}() is an aggregate: use it in RETURN or WITH"
        )));
    };
    let [arg] = args else {
        return Err(error(format!("{name}() takes one argument")));
    };
    let mut values = Vec::new();
    let mut seen = HashSet::new();
    for row in rows {
        let value = eval(ctx, arg, Scope::Row(row))?;
        if value.is_null() || (distinct && !seen.insert(value.key())) {
            continue;
        }
        values.push(value);
    }
    Ok(match name {
        "count" => Value::Int(values.len() as i64),
        "collect" => Value::List(values),
        "min" => values.into_iter().min_by(sort_order).unwrap_or(Value::Null),
        "max" => values.into_iter().max_by(sort_order).unwrap_or(Value::Null),
        "sum" | "avg" => {
            if values.iter().any(|v| number(v).is_none()) {
                return Err(error(format!("{name}() needs numbers")));
            }
            if name == "avg" {
                if values.is_empty() {
                    return Ok(Value::Null);
                }
                let total: f64 = values.iter().filter_map(number).sum();
                Value::Float(total / values.len() as f64)
            } else if values.iter().all(|v| matches!(v, Value::Int(_))) {
                let mut total = 0i64;
                for value in &values {
                    if let Value::Int(i) = value {
                        total = total
                            .checked_add(*i)
                            .ok_or_else(|| error("integer overflow"))?;
                    }
                }
                Value::Int(total)
            } else {
                Value::Float(values.iter().filter_map(number).sum())
            }
        }
        _ => unreachable!("not an aggregate"),
    })
}

fn arity(
    name: &str,
    args: &[Value],
    range: std::ops::RangeInclusive<usize>,
) -> Result<(), QueryError> {
    if range.contains(&args.len()) {
        Ok(())
    } else if range.start() == range.end() {
        Err(error(format!(
            "{name}() takes {} argument(s), got {}",
            range.start(),
            args.len()
        )))
    } else {
        Err(error(format!(
            "{name}() takes {} to {} arguments, got {}",
            range.start(),
            range.end(),
            args.len()
        )))
    }
}

fn string_arg<'a>(name: &str, value: &'a Value) -> Result<&'a str, QueryError> {
    match value {
        Value::Str(s) => Ok(s),
        other => Err(error(format!(
            "{name}() needs a string, got a {}",
            other.type_name()
        ))),
    }
}

fn int_arg(name: &str, value: &Value) -> Result<i64, QueryError> {
    match value {
        Value::Int(i) => Ok(*i),
        other => Err(error(format!(
            "{name}() needs an integer, got a {}",
            other.type_name()
        ))),
    }
}

fn node_of(ctx: &Context, id: NodeId) -> Result<Value, QueryError> {
    Ok(ctx
        .node(id)?
        .map_or(Value::Null, |node| Value::Node(Arc::new(node))))
}

fn function(ctx: &Context, name: &str, args: Vec<Value>) -> Result<Value, QueryError> {
    // Most functions return null for a null argument.
    let strict = !matches!(name, "coalesce" | "now" | "datetime" | "time" | "range");
    if strict && args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let first = args.first();
    Ok(match name {
        "id" => {
            arity(name, &args, 1..=1)?;
            match &args[0] {
                Value::Node(node) => Value::Str(node.id.to_string()),
                Value::Edge(edge) => Value::Str(edge.id.to_string()),
                other => {
                    return Err(error(format!(
                        "id() needs a node or edge, not a {}",
                        other.type_name()
                    )));
                }
            }
        }
        "labels" => {
            arity(name, &args, 1..=1)?;
            match &args[0] {
                Value::Node(node) => node_meta(ctx, node, "labels")?,
                other => {
                    return Err(error(format!(
                        "labels() needs a node, not a {}",
                        other.type_name()
                    )));
                }
            }
        }
        "type" | "label" => {
            arity(name, &args, 1..=1)?;
            match &args[0] {
                Value::Edge(edge) => Value::Str(edge.label.clone()),
                other => {
                    return Err(error(format!(
                        "{name}() needs an edge, not a {}",
                        other.type_name()
                    )));
                }
            }
        }
        "properties" | "props" => {
            arity(name, &args, 1..=1)?;
            match &args[0] {
                Value::Node(node) => {
                    Value::from_json(&serde_json::Value::Object(node.props.clone()))
                }
                Value::Edge(edge) => {
                    Value::from_json(&serde_json::Value::Object(edge.props.clone()))
                }
                Value::Map(map) => Value::Map(map.clone()),
                other => {
                    return Err(error(format!(
                        "{name}() needs a node, edge or map, not a {}",
                        other.type_name()
                    )));
                }
            }
        }
        "keys" => {
            arity(name, &args, 1..=1)?;
            let keys: Vec<String> = match &args[0] {
                Value::Node(node) => node.props.keys().cloned().collect(),
                Value::Edge(edge) => edge.props.keys().cloned().collect(),
                Value::Map(map) => map.keys().cloned().collect(),
                other => {
                    return Err(error(format!(
                        "keys() needs a node, edge or map, not a {}",
                        other.type_name()
                    )));
                }
            };
            Value::List(keys.into_iter().map(Value::Str).collect())
        }
        "text" => {
            arity(name, &args, 1..=1)?;
            match &args[0] {
                Value::Node(node) => node_meta(ctx, node, "text")?,
                other => {
                    return Err(error(format!(
                        "text() needs a node, not a {}",
                        other.type_name()
                    )));
                }
            }
        }
        "startnode" | "endnode" => {
            arity(name, &args, 1..=1)?;
            match &args[0] {
                Value::Edge(edge) => node_of(
                    ctx,
                    if name == "startnode" {
                        edge.from
                    } else {
                        edge.to
                    },
                )?,
                other => {
                    return Err(error(format!(
                        "{name}() needs an edge, not a {}",
                        other.type_name()
                    )));
                }
            }
        }
        "size" | "length" => {
            arity(name, &args, 1..=1)?;
            match &args[0] {
                Value::Str(s) => Value::Int(s.chars().count() as i64),
                Value::List(items) => Value::Int(items.len() as i64),
                Value::Map(map) => Value::Int(map.len() as i64),
                other => {
                    return Err(error(format!(
                        "{name}() needs a string or list, not a {}",
                        other.type_name()
                    )));
                }
            }
        }
        "lower" | "tolower" => {
            Value::Str(string_arg(name, first.unwrap_or(&Value::Null))?.to_lowercase())
        }
        "upper" | "toupper" => {
            Value::Str(string_arg(name, first.unwrap_or(&Value::Null))?.to_uppercase())
        }
        "trim" => Value::Str(
            string_arg(name, first.unwrap_or(&Value::Null))?
                .trim()
                .to_owned(),
        ),
        "substring" => {
            arity(name, &args, 2..=3)?;
            let text = string_arg(name, &args[0])?;
            let start = usize::try_from(int_arg(name, &args[1])?).unwrap_or(0);
            let chars = text.chars().skip(start);
            match args.get(2) {
                Some(length) => {
                    let length = usize::try_from(int_arg(name, length)?).unwrap_or(0);
                    Value::Str(chars.take(length).collect())
                }
                None => Value::Str(chars.collect()),
            }
        }
        "replace" => {
            arity(name, &args, 3..=3)?;
            let text = string_arg(name, &args[0])?;
            Value::Str(text.replace(string_arg(name, &args[1])?, string_arg(name, &args[2])?))
        }
        "split" => {
            arity(name, &args, 2..=2)?;
            let text = string_arg(name, &args[0])?;
            let separator = string_arg(name, &args[1])?;
            Value::List(
                text.split(separator)
                    .map(|part| Value::Str(part.to_owned()))
                    .collect(),
            )
        }
        "coalesce" => args
            .into_iter()
            .find(|v| !v.is_null())
            .unwrap_or(Value::Null),
        "tostring" => {
            arity(name, &args, 1..=1)?;
            Value::Str(display(&args[0]))
        }
        "tointeger" => {
            arity(name, &args, 1..=1)?;
            match &args[0] {
                Value::Int(i) => Value::Int(*i),
                Value::Float(f) if f.is_finite() => Value::Int(f.trunc() as i64),
                Value::Str(s) => s
                    .trim()
                    .parse::<i64>()
                    .map(Value::Int)
                    .or_else(|_| {
                        s.trim()
                            .parse::<f64>()
                            .map(|f| Value::Int(f.trunc() as i64))
                    })
                    .unwrap_or(Value::Null),
                Value::Bool(b) => Value::Int(i64::from(*b)),
                _ => Value::Null,
            }
        }
        "tofloat" => {
            arity(name, &args, 1..=1)?;
            match &args[0] {
                Value::Int(i) => Value::Float(*i as f64),
                Value::Float(f) => Value::Float(*f),
                Value::Str(s) => s.trim().parse().map(Value::Float).unwrap_or(Value::Null),
                _ => Value::Null,
            }
        }
        "abs" | "round" | "floor" | "ceil" | "sqrt" => {
            arity(name, &args, 1..=1)?;
            match (&args[0], name) {
                (Value::Int(i), "abs") => {
                    Value::Int(i.checked_abs().ok_or_else(|| error("integer overflow"))?)
                }
                (Value::Int(i), "round" | "floor" | "ceil") => Value::Int(*i),
                (value, _) => {
                    let x = number(value).ok_or_else(|| {
                        error(format!(
                            "{name}() needs a number, not a {}",
                            value.type_name()
                        ))
                    })?;
                    Value::Float(match name {
                        "abs" => x.abs(),
                        "round" => x.round(),
                        "floor" => x.floor(),
                        "ceil" => x.ceil(),
                        _ => x.sqrt(),
                    })
                }
            }
        }
        "datetime" | "time" => {
            arity(name, &args, 0..=1)?;
            match first {
                None => Value::Time(Timestamp::now()),
                Some(Value::Null) => Value::Null,
                Some(value) => Value::Time(to_time(value)?),
            }
        }
        "date" => {
            arity(name, &args, 1..=1)?;
            Value::Str(to_time(&args[0])?.date())
        }
        "now" => {
            arity(name, &args, 0..=0)?;
            Value::Time(Timestamp::now())
        }
        "head" | "last" => {
            arity(name, &args, 1..=1)?;
            match &args[0] {
                Value::List(items) => {
                    let item = if name == "head" {
                        items.first()
                    } else {
                        items.last()
                    };
                    item.cloned().unwrap_or(Value::Null)
                }
                other => {
                    return Err(error(format!(
                        "{name}() needs a list, not a {}",
                        other.type_name()
                    )));
                }
            }
        }
        "range" => {
            arity(name, &args, 2..=3)?;
            let (start, end) = (int_arg(name, &args[0])?, int_arg(name, &args[1])?);
            let step = args
                .get(2)
                .map(|step| int_arg(name, step))
                .transpose()?
                .unwrap_or(1);
            if step == 0 {
                return Err(error("range() step must not be zero"));
            }
            let mut out = Vec::new();
            let mut current = start;
            while (step > 0 && current <= end) || (step < 0 && current >= end) {
                if out.len() >= 1_000_000 {
                    return Err(error("range() would return more than a million items"));
                }
                out.push(Value::Int(current));
                current += step;
            }
            Value::List(out)
        }
        other => return Err(error(format!("unknown function {other}()"))),
    })
}
