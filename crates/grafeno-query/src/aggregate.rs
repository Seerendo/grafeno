//! Batched aggregation: rows fold into one accumulator per group as the
//! match makes them, instead of being held until the end.
//!
//! A `MATCH` followed by a `RETURN` or `WITH` with aggregates hands its rows
//! over in batches of up to [`BATCH`]. Each batch is first turned into
//! columns, one vector per group key and per aggregate argument (on the
//! thread that matched the rows, so a parallel scan computes them in
//! parallel); the columns then fold, in the order the rows came, into the
//! groups. A group keeps its first row and one accumulator per aggregate:
//! a counter for `count`, running totals for `sum` and `avg`, the best value
//! for `min` and `max`, and the values themselves only for those that need
//! them all (`collect`, `median`, percentiles, `stdev`, `variance`, `mode`).
//! The results equal the row-at-a-time ones, group order and ties included.

use std::collections::{HashMap, HashSet};

use crate::QueryError;
use crate::ast::Expr;
use crate::eval::{Context, Row, Scope, eval, number, percentile_of, reduce};
use crate::temporal::Duration;
use crate::value::{Value, sort_order};

/// Rows per batch handed from a match to the aggregation.
pub(crate) const BATCH: usize = 1024;

/// One aggregate call in the projection (`count(*)`, `sum(p.age)`…).
struct Call {
    expr: Expr,
    name: String,
    distinct: bool,
    /// `None` for `count(*)`.
    arg: Option<Expr>,
    percentile: Option<Expr>,
}

/// A batch turned into columns: each row's group key and the argument of
/// each aggregate, column by column.
pub(crate) struct Columns {
    rows: Vec<Row>,
    keys: Vec<String>,
    /// `args[call][row]`.
    args: Vec<Vec<Value>>,
}

struct Group {
    first: Row,
    accs: Vec<Acc>,
}

/// What to compute: the projection's items and the aggregate calls in
/// them. Shared by the threads that turn batches into columns.
pub(crate) struct Spec {
    items: Vec<(Expr, String)>,
    calls: Vec<Call>,
    grouped: bool,
}

/// The groups folded so far, in the order they first appeared.
#[derive(Default)]
pub(crate) struct Groups {
    groups: Vec<Group>,
    index: HashMap<String, usize>,
    /// Rows folded so far.
    pub(crate) rows: u64,
}

impl Spec {
    /// For the items of a projection (with `*` already expanded); `None`
    /// when no item aggregates.
    pub(crate) fn new(items: &[(Expr, String)]) -> Option<Self> {
        if !items.iter().any(|(expr, _)| expr.has_aggregate()) {
            return None;
        }
        let mut calls: Vec<Call> = Vec::new();
        for (expr, _) in items {
            expr.visit(&mut |inner| {
                let call = match inner {
                    Expr::CountStar => Call {
                        expr: inner.clone(),
                        name: "count".into(),
                        distinct: false,
                        arg: None,
                        percentile: None,
                    },
                    Expr::Call {
                        name,
                        distinct,
                        args,
                    } if crate::ast::AGGREGATES.contains(&name.as_str()) => Call {
                        expr: inner.clone(),
                        name: name.clone(),
                        distinct: *distinct,
                        arg: args.first().cloned(),
                        percentile: args.get(1).cloned(),
                    },
                    _ => return,
                };
                if !calls.iter().any(|known| known.expr == call.expr) {
                    calls.push(call);
                }
            });
        }
        Some(Self {
            grouped: items.iter().any(|(expr, _)| !expr.has_aggregate()),
            items: items.to_vec(),
            calls,
        })
    }

    /// Group keys and aggregate arguments of a batch, as columns. Reads
    /// nothing but the rows, so any thread can run it.
    pub(crate) fn columns(&self, ctx: &Context, rows: Vec<Row>) -> Result<Columns, QueryError> {
        let mut keys = Vec::with_capacity(rows.len());
        let mut args: Vec<Vec<Value>> = self
            .calls
            .iter()
            .map(|_| Vec::with_capacity(rows.len()))
            .collect();
        for row in &rows {
            let mut key = String::new();
            for (expr, _) in self.items.iter().filter(|(expr, _)| !expr.has_aggregate()) {
                key.push_str(&eval(ctx, expr, Scope::Row(row))?.key());
                key.push('\u{1}');
            }
            keys.push(key);
            for (call, column) in self.calls.iter().zip(&mut args) {
                column.push(match &call.arg {
                    Some(arg) => eval(ctx, arg, Scope::Row(row))?,
                    None => Value::Null,
                });
            }
        }
        Ok(Columns { rows, keys, args })
    }
}

impl Groups {
    /// Folds a batch into the groups, in row order.
    pub(crate) fn fold(&mut self, spec: &Spec, columns: Columns) -> Result<(), QueryError> {
        let Columns { rows, keys, args } = columns;
        let mut args: Vec<std::vec::IntoIter<Value>> =
            args.into_iter().map(Vec::into_iter).collect();
        for (row, key) in rows.into_iter().zip(keys) {
            self.rows += 1;
            let at = match self.index.get(&key) {
                Some(&at) => at,
                None => {
                    self.index.insert(key, self.groups.len());
                    self.groups.push(Group {
                        first: row,
                        accs: spec.calls.iter().map(Acc::new).collect(),
                    });
                    self.groups.len() - 1
                }
            };
            let group = &mut self.groups[at];
            for ((call, acc), column) in spec.calls.iter().zip(&mut group.accs).zip(&mut args) {
                let value = column.next().unwrap_or(Value::Null);
                acc.add(call, value)?;
            }
        }
        Ok(())
    }

    /// Each group's projected values and its first row, in the order the
    /// groups first appeared.
    pub(crate) fn finish(
        self,
        spec: &Spec,
        ctx: &Context,
    ) -> Result<Vec<(Vec<Value>, Row)>, QueryError> {
        let mut groups = self.groups;
        let empty = groups.is_empty();
        if empty && !spec.grouped {
            // Aggregates over nothing still give one row: count(*) = 0.
            groups.push(Group {
                first: Row::new(),
                accs: spec.calls.iter().map(Acc::new).collect(),
            });
        }
        let mut output = Vec::with_capacity(groups.len());
        for group in groups {
            let mut folded = Vec::with_capacity(spec.calls.len());
            for (call, acc) in spec.calls.iter().zip(group.accs) {
                let percentile = match &call.percentile {
                    Some(expr) => {
                        percentile_of(&call.name, &eval(ctx, expr, Scope::Row(&group.first))?)?
                    }
                    None => 0.0,
                };
                folded.push((call.expr.clone(), acc.finish(&call.name, percentile)?));
            }
            let values = spec
                .items
                .iter()
                .map(|(expr, _)| eval(ctx, expr, Scope::Folded(&group.first, &folded, empty)))
                .collect::<Result<Vec<_>, _>>()?;
            output.push((values, group.first));
        }
        Ok(output)
    }
}

/// One aggregate's state in one group.
enum Acc {
    Count {
        n: i64,
        seen: Option<HashSet<String>>,
    },
    Sum(Box<Sum>),
    Extreme {
        max: bool,
        best: Option<Value>,
    },
    Values {
        values: Vec<Value>,
        seen: Option<HashSet<String>>,
    },
}

/// Running totals for `sum` and `avg`, kept so that the result equals
/// adding the values in order: integers checked (an overflow on the way is
/// an error, as it is row by row), floats in order, durations apart.
#[derive(Default)]
struct Sum {
    n: usize,
    seen: Option<HashSet<String>>,
    ints: Option<i64>,
    all_int: bool,
    floats: f64,
    not_number: bool,
    all_duration: bool,
    duration: Option<Result<Duration, QueryError>>,
}

impl Acc {
    fn new(call: &Call) -> Self {
        let seen = call.distinct.then(HashSet::new);
        match call.name.as_str() {
            "count" => Acc::Count { n: 0, seen },
            "sum" | "avg" => Acc::Sum(Box::new(Sum {
                seen,
                ints: Some(0),
                all_int: true,
                all_duration: true,
                duration: Some(Ok(Duration::ZERO)),
                ..Sum::default()
            })),
            "min" | "max" => Acc::Extreme {
                max: call.name == "max",
                best: None,
            },
            _ => Acc::Values {
                values: Vec::new(),
                seen,
            },
        }
    }

    fn add(&mut self, call: &Call, value: Value) -> Result<(), QueryError> {
        // count(*) counts rows; every other aggregate skips nulls.
        if call.arg.is_none() {
            if let Acc::Count { n, .. } = self {
                *n += 1;
            }
            return Ok(());
        }
        if value.is_null() {
            return Ok(());
        }
        let fresh = |seen: &mut Option<HashSet<String>>, value: &Value| {
            seen.as_mut().is_none_or(|seen| seen.insert(value.key()))
        };
        match self {
            Acc::Count { n, seen } => {
                if fresh(seen, &value) {
                    *n += 1;
                }
            }
            Acc::Sum(sum) => {
                if !fresh(&mut sum.seen, &value) {
                    return Ok(());
                }
                sum.n += 1;
                match &value {
                    Value::Duration(d) => {
                        if let Some(Ok(total)) = &sum.duration {
                            sum.duration = Some(total.plus(*d));
                        }
                    }
                    _ => sum.all_duration = false,
                }
                match number(&value) {
                    Some(f) => sum.floats += f,
                    None => sum.not_number = true,
                }
                match value {
                    Value::Int(i) => sum.ints = sum.ints.and_then(|total| total.checked_add(i)),
                    _ => sum.all_int = false,
                }
            }
            Acc::Extreme { max, best } => {
                // Like min_by and max_by: the first of equal minimums, the
                // last of equal maximums.
                let better = match best {
                    None => true,
                    Some(current) => {
                        let order = sort_order(&value, current);
                        if *max { !order.is_lt() } else { order.is_lt() }
                    }
                };
                if better {
                    *best = Some(value);
                }
            }
            Acc::Values { values, seen } => {
                if fresh(seen, &value) {
                    values.push(value);
                }
            }
        }
        Ok(())
    }

    fn finish(self, name: &str, percentile: f64) -> Result<Value, QueryError> {
        Ok(match self {
            Acc::Count { n, .. } => Value::Int(n),
            Acc::Extreme { best, .. } => best.unwrap_or(Value::Null),
            Acc::Values { values, .. } => reduce(name, values, percentile)?,
            Acc::Sum(sum) => {
                let avg = name == "avg";
                if sum.n > 0 && sum.all_duration {
                    let total = sum.duration.unwrap_or(Ok(Duration::ZERO))?;
                    return Ok(Value::Duration(if avg {
                        total.scale(1.0 / sum.n as f64)?
                    } else {
                        total
                    }));
                }
                if sum.not_number {
                    return Err(QueryError::runtime(format!(
                        "{name}() needs numbers (or durations)"
                    )));
                }
                if avg {
                    if sum.n == 0 {
                        Value::Null
                    } else {
                        Value::Float(sum.floats / sum.n as f64)
                    }
                } else if sum.all_int {
                    Value::Int(
                        sum.ints
                            .ok_or_else(|| QueryError::runtime("integer overflow"))?,
                    )
                } else {
                    Value::Float(sum.floats)
                }
            }
        })
    }
}
