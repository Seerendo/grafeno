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
use serde_json::{Value as Json, json};

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

/// Running totals for `sum` and `avg`. Integers add in 128 bits (the
/// result must fit 64), floats exactly ([`ExactSum`]), durations apart, so
/// the result does not depend on the order of the values: partial sums of
/// a query split across members merge into what one server computes.
#[derive(Default)]
struct Sum {
    n: usize,
    seen: Option<HashSet<String>>,
    /// With DISTINCT: the distinct values, so that partial sums merge.
    distinct: Vec<Value>,
    ints: i128,
    all_int: bool,
    floats: ExactSum,
    not_number: bool,
    all_duration: bool,
    duration: Option<Result<Duration, QueryError>>,
}

/// A float sum without rounding errors (Shewchuk's partials, as Python's
/// `math.fsum`): the result is the exact sum rounded once, whatever the
/// order of the values.
#[derive(Clone, Default)]
pub(crate) struct ExactSum {
    partials: Vec<f64>,
    nan: bool,
    positive_infinity: bool,
    negative_infinity: bool,
}

impl ExactSum {
    pub(crate) fn add(&mut self, value: f64) {
        if !value.is_finite() {
            if value.is_nan() {
                self.nan = true;
            } else if value > 0.0 {
                self.positive_infinity = true;
            } else {
                self.negative_infinity = true;
            }
            return;
        }
        let mut x = value;
        let mut kept = 0;
        for j in 0..self.partials.len() {
            let mut y = self.partials[j];
            if x.abs() < y.abs() {
                std::mem::swap(&mut x, &mut y);
            }
            let high = x + y;
            if !high.is_finite() {
                // The sum left the range of floats.
                if high > 0.0 {
                    self.positive_infinity = true;
                } else {
                    self.negative_infinity = true;
                }
                return;
            }
            let low = y - (high - x);
            if low != 0.0 {
                self.partials[kept] = low;
                kept += 1;
            }
            x = high;
        }
        self.partials.truncate(kept);
        self.partials.push(x);
    }

    pub(crate) fn merge(&mut self, other: &ExactSum) {
        for partial in &other.partials {
            self.add(*partial);
        }
        self.nan |= other.nan;
        self.positive_infinity |= other.positive_infinity;
        self.negative_infinity |= other.negative_infinity;
    }

    pub(crate) fn value(&self) -> f64 {
        if self.nan || (self.positive_infinity && self.negative_infinity) {
            return f64::NAN;
        }
        if self.positive_infinity {
            return f64::INFINITY;
        }
        if self.negative_infinity {
            return f64::NEG_INFINITY;
        }
        let p = &self.partials;
        let mut n = p.len();
        if n == 0 {
            return 0.0;
        }
        n -= 1;
        let mut high = p[n];
        let mut low = 0.0;
        while n > 0 {
            let x = high;
            n -= 1;
            let y = p[n];
            high = x + y;
            let rounded = high - x;
            low = y - rounded;
            if low != 0.0 {
                break;
            }
        }
        // Round half to even across the remaining partials.
        if n > 0 && ((low < 0.0 && p[n - 1] < 0.0) || (low > 0.0 && p[n - 1] > 0.0)) {
            let y = low * 2.0;
            let x = high + y;
            if y == x - high {
                high = x;
            }
        }
        high
    }
}

impl Acc {
    fn new(call: &Call) -> Self {
        let seen = call.distinct.then(HashSet::new);
        match call.name.as_str() {
            "count" => Acc::Count { n: 0, seen },
            "sum" | "avg" => Acc::Sum(Box::new(Sum {
                seen,
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
                if sum.seen.is_some() {
                    sum.distinct.push(value.clone());
                }
                sum.add(value);
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

    /// Folds in the state of the same aggregate over rows that came after
    /// this one's (the next part of a split query).
    fn merge(&mut self, call: &Call, other: Acc) -> Result<(), QueryError> {
        match (self, other) {
            (Acc::Count { n, seen: None }, Acc::Count { n: more, .. }) => *n += more,
            (
                Acc::Count {
                    n,
                    seen: Some(seen),
                },
                Acc::Count {
                    seen: Some(more), ..
                },
            ) => {
                for key in more {
                    if seen.insert(key) {
                        *n += 1;
                    }
                }
            }
            (Acc::Sum(sum), Acc::Sum(more)) => {
                if sum.seen.is_some() {
                    for value in more.distinct {
                        let fresh = sum
                            .seen
                            .as_mut()
                            .is_some_and(|seen| seen.insert(value.key()));
                        if fresh {
                            sum.distinct.push(value.clone());
                            sum.add(value);
                        }
                    }
                } else {
                    sum.n += more.n;
                    sum.ints += more.ints;
                    sum.all_int &= more.all_int;
                    sum.floats.merge(&more.floats);
                    sum.not_number |= more.not_number;
                    sum.all_duration &= more.all_duration;
                    sum.duration = match (sum.duration.take(), more.duration) {
                        (Some(Ok(a)), Some(Ok(b))) => Some(a.plus(b)),
                        (Some(Err(error)), _) | (_, Some(Err(error))) => Some(Err(error)),
                        (a, b) => a.or(b),
                    };
                }
            }
            (this @ Acc::Extreme { .. }, Acc::Extreme { best, .. }) => {
                if let Some(best) = best {
                    this.add(call, best)?;
                }
            }
            (Acc::Values { values, seen }, Acc::Values { values: more, .. }) => match seen {
                None => values.extend(more),
                Some(seen) => {
                    for value in more {
                        if seen.insert(value.key()) {
                            values.push(value);
                        }
                    }
                }
            },
            _ => return Err(QueryError::runtime("partial aggregates do not match")),
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
                        Value::Float(sum.floats.value() / sum.n as f64)
                    }
                } else if sum.all_int {
                    Value::Int(
                        i64::try_from(sum.ints)
                            .map_err(|_| QueryError::runtime("integer overflow"))?,
                    )
                } else {
                    Value::Float(sum.floats.value())
                }
            }
        })
    }

    fn to_wire(&self) -> Json {
        let keys = |seen: &Option<HashSet<String>>| {
            seen.as_ref()
                .map(|seen| json!(seen.iter().collect::<Vec<_>>()))
                .unwrap_or(Json::Null)
        };
        let wires = |values: &[Value]| Json::Array(values.iter().map(Value::to_wire).collect());
        match self {
            Acc::Count { n, seen } => json!({"count": n, "seen": keys(seen)}),
            Acc::Extreme { best, .. } => {
                json!({"best": best.as_ref().map(Value::to_wire)})
            }
            Acc::Values { values, seen } => json!({"values": wires(values), "seen": keys(seen)}),
            Acc::Sum(sum) => json!({"sum": {
                "n": sum.n,
                "seen": keys(&sum.seen),
                "distinct": wires(&sum.distinct),
                "ints": sum.ints.to_string(),
                "all_int": sum.all_int,
                "partials": sum.floats.partials.iter().map(|f| f.to_bits()).collect::<Vec<_>>(),
                "nan": sum.floats.nan,
                "pinf": sum.floats.positive_infinity,
                "ninf": sum.floats.negative_infinity,
                "not_number": sum.not_number,
                "all_duration": sum.all_duration,
                "duration": match &sum.duration {
                    Some(Ok(d)) => json!([d.months, d.days, d.micros]),
                    Some(Err(error)) => json!({"error": error.message}),
                    None => Json::Null,
                },
            }}),
        }
    }

    fn from_wire(call: &Call, json: &Json) -> Result<Acc, QueryError> {
        let bad = || QueryError::runtime("a partial aggregate is malformed");
        let keys = |json: &Json| -> Option<HashSet<String>> {
            json.as_array().map(|keys| {
                keys.iter()
                    .filter_map(|k| k.as_str().map(str::to_owned))
                    .collect()
            })
        };
        let values = |json: &Json| -> Result<Vec<Value>, QueryError> {
            json.as_array()
                .ok_or_else(bad)?
                .iter()
                .map(Value::from_wire)
                .collect()
        };
        let mut acc = Acc::new(call);
        match &mut acc {
            Acc::Count { n, seen } => {
                *n = json["count"].as_i64().ok_or_else(bad)?;
                if seen.is_some() {
                    *seen = keys(&json["seen"]);
                }
            }
            Acc::Extreme { best, .. } => {
                *best = match &json["best"] {
                    Json::Null => None,
                    wire => Some(Value::from_wire(wire)?),
                };
            }
            Acc::Values { values: list, seen } => {
                *list = values(&json["values"])?;
                if seen.is_some() {
                    *seen = keys(&json["seen"]);
                }
            }
            Acc::Sum(sum) => {
                let w = &json["sum"];
                sum.n = w["n"].as_u64().ok_or_else(bad)? as usize;
                if sum.seen.is_some() {
                    sum.seen = keys(&w["seen"]);
                }
                sum.distinct = values(&w["distinct"])?;
                sum.ints = w["ints"]
                    .as_str()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(bad)?;
                sum.all_int = w["all_int"].as_bool().ok_or_else(bad)?;
                sum.floats = ExactSum {
                    partials: w["partials"]
                        .as_array()
                        .ok_or_else(bad)?
                        .iter()
                        .map(|f| f.as_u64().map(f64::from_bits).ok_or_else(bad))
                        .collect::<Result<_, _>>()?,
                    nan: w["nan"].as_bool().unwrap_or(false),
                    positive_infinity: w["pinf"].as_bool().unwrap_or(false),
                    negative_infinity: w["ninf"].as_bool().unwrap_or(false),
                };
                sum.not_number = w["not_number"].as_bool().unwrap_or(false);
                sum.all_duration = w["all_duration"].as_bool().unwrap_or(false);
                sum.duration = match &w["duration"] {
                    Json::Array(d) if d.len() == 3 => Some(Ok(Duration {
                        months: d[0].as_i64().ok_or_else(bad)?,
                        days: d[1].as_i64().ok_or_else(bad)?,
                        micros: d[2].as_i64().ok_or_else(bad)?,
                    })),
                    Json::Object(error) => Some(Err(QueryError::runtime(
                        error
                            .get("error")
                            .and_then(Json::as_str)
                            .unwrap_or("duration overflow"),
                    ))),
                    _ => None,
                };
            }
        }
        Ok(acc)
    }
}

impl Sum {
    fn add(&mut self, value: Value) {
        self.n += 1;
        match &value {
            Value::Duration(d) => {
                if let Some(Ok(total)) = &self.duration {
                    self.duration = Some(total.plus(*d));
                }
            }
            _ => self.all_duration = false,
        }
        match number(&value) {
            Some(f) => self.floats.add(f),
            None => self.not_number = true,
        }
        match value {
            Value::Int(i) => self.ints += i as i128,
            _ => self.all_int = false,
        }
    }
}

impl Groups {
    /// Folds in the groups of the rows that came after these (the next
    /// part of a split query): groups already here merge their
    /// accumulators, new ones are added after, so groups keep the order
    /// in which they first appeared over all the rows.
    pub(crate) fn merge(&mut self, spec: &Spec, other: Groups) -> Result<(), QueryError> {
        self.rows += other.rows;
        let Groups { groups, index, .. } = other;
        let mut keyed: Vec<(String, usize)> = index.into_iter().collect();
        keyed.sort_by_key(|(_, at)| *at);
        let mut groups: Vec<Option<Group>> = groups.into_iter().map(Some).collect();
        for (key, at) in keyed {
            let Some(group) = groups[at].take() else {
                continue;
            };
            match self.index.get(&key) {
                Some(&mine) => {
                    let target = &mut self.groups[mine];
                    for ((call, acc), more) in
                        spec.calls.iter().zip(&mut target.accs).zip(group.accs)
                    {
                        acc.merge(call, more)?;
                    }
                }
                None => {
                    self.index.insert(key, self.groups.len());
                    self.groups.push(group);
                }
            }
        }
        Ok(())
    }

    /// The groups as exact JSON, for the member that merges a split query.
    pub(crate) fn to_wire(&self) -> Json {
        let mut keyed: Vec<(&String, usize)> = self.index.iter().map(|(k, at)| (k, *at)).collect();
        keyed.sort_by_key(|(_, at)| *at);
        let groups: Vec<Json> = keyed
            .into_iter()
            .map(|(key, at)| {
                let group = &self.groups[at];
                json!({
                    "key": key,
                    "first": crate::eval::row_to_wire(&group.first),
                    "accs": group.accs.iter().map(Acc::to_wire).collect::<Vec<_>>(),
                })
            })
            .collect();
        json!({"rows": self.rows, "groups": groups})
    }

    /// Groups from [`Groups::to_wire`].
    pub(crate) fn from_wire(spec: &Spec, json: &Json) -> Result<Groups, QueryError> {
        let bad = || QueryError::runtime("partial groups are malformed");
        let mut out = Groups {
            rows: json["rows"].as_u64().ok_or_else(bad)?,
            ..Groups::default()
        };
        for group in json["groups"].as_array().ok_or_else(bad)? {
            let key = group["key"].as_str().ok_or_else(bad)?.to_owned();
            let first = crate::eval::row_from_wire(&group["first"])?;
            let wires = group["accs"].as_array().ok_or_else(bad)?;
            if wires.len() != spec.calls.len() {
                return Err(bad());
            }
            let accs = spec
                .calls
                .iter()
                .zip(wires)
                .map(|(call, wire)| Acc::from_wire(call, wire))
                .collect::<Result<_, _>>()?;
            out.index.insert(key, out.groups.len());
            out.groups.push(Group { first, accs });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::ExactSum;

    fn exact(values: &[f64]) -> f64 {
        let mut sum = ExactSum::default();
        values.iter().for_each(|v| sum.add(*v));
        sum.value()
    }

    #[test]
    fn exact_sums_do_not_depend_on_order() {
        assert_eq!(exact(&[0.1, 0.2, 0.3]), 0.6);
        assert_eq!(exact(&[1e100, 1.0, -1e100]), 1.0);
        assert_eq!(exact(&[]), 0.0);
        assert!(exact(&[f64::INFINITY, f64::NEG_INFINITY]).is_nan());
        let values: Vec<f64> = (0..1000).map(|i| (i as f64 * 0.37).sin() * 1e3).collect();
        let mut reversed = values.clone();
        reversed.reverse();
        assert_eq!(exact(&values), exact(&reversed));
        // Split in three and merged.
        let mut total = ExactSum::default();
        for chunk in values.chunks(333) {
            let mut part = ExactSum::default();
            chunk.iter().for_each(|v| part.add(*v));
            total.merge(&part);
        }
        assert_eq!(total.value(), exact(&values));
    }
}
