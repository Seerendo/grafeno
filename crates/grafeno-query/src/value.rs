//! Values that queries compute with, and how they compare.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;

use grafeno::{Edge, Node, Timestamp};
use serde_json::json;

/// A value in a query: a property value, a node, an edge, or a time.
#[derive(Clone, Debug)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Time(Timestamp),
    List(Vec<Value>),
    Map(BTreeMap<String, Value>),
    Node(Arc<Node>),
    Edge(Arc<Edge>),
}

impl PartialEq for Value {
    /// Structural equality (used to compare syntax trees); query equality,
    /// where `null` is unknown, is [`equals`].
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl Value {
    pub fn from_json(value: &serde_json::Value) -> Value {
        match value {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Number(n) => match n.as_i64() {
                Some(i) => Value::Int(i),
                None => Value::Float(n.as_f64().unwrap_or(f64::NAN)),
            },
            serde_json::Value::String(s) => Value::Str(s.clone()),
            serde_json::Value::Array(items) => {
                Value::List(items.iter().map(Value::from_json).collect())
            }
            serde_json::Value::Object(map) => Value::Map(
                map.iter()
                    .map(|(key, value)| (key.clone(), Value::from_json(value)))
                    .collect(),
            ),
        }
    }

    /// JSON for results: nodes and edges as their records, times as ISO
    /// 8601 strings.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Value::Null => serde_json::Value::Null,
            Value::Bool(b) => json!(b),
            Value::Int(i) => json!(i),
            Value::Float(f) => json!(f),
            Value::Str(s) => json!(s),
            Value::Time(t) => json!(t.iso()),
            Value::List(items) => {
                serde_json::Value::Array(items.iter().map(Value::to_json).collect())
            }
            Value::Map(map) => serde_json::Value::Object(
                map.iter()
                    .map(|(key, value)| (key.clone(), value.to_json()))
                    .collect(),
            ),
            Value::Node(node) => serde_json::to_value(node.as_ref()).unwrap_or_default(),
            Value::Edge(edge) => serde_json::to_value(edge.as_ref()).unwrap_or_default(),
        }
    }

    /// A property value to store. Nodes, edges and times have no place in
    /// properties, except times, which are stored as ISO strings.
    pub(crate) fn to_property(&self) -> Result<serde_json::Value, String> {
        match self {
            Value::Node(_) | Value::Edge(_) => {
                Err(format!("a property cannot hold a {}", self.type_name()))
            }
            Value::List(items) => items
                .iter()
                .map(Value::to_property)
                .collect::<Result<Vec<_>, _>>()
                .map(serde_json::Value::Array),
            Value::Map(map) => map
                .iter()
                .map(|(key, value)| Ok((key.clone(), value.to_property()?)))
                .collect::<Result<serde_json::Map<_, _>, String>>()
                .map(serde_json::Value::Object),
            other => Ok(other.to_json()),
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Int(_) => "integer",
            Value::Float(_) => "float",
            Value::Str(_) => "string",
            Value::Time(_) => "time",
            Value::List(_) => "list",
            Value::Map(_) => "map",
            Value::Node(_) => "node",
            Value::Edge(_) => "edge",
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// A canonical text form: equal values (`1` and `1.0` included) get the
    /// same key. Used for `DISTINCT` and grouping.
    pub(crate) fn key(&self) -> String {
        match self {
            Value::Float(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => format!("n{}", *f as i64),
            Value::Float(f) => format!("n{f}"),
            Value::Int(i) => format!("n{i}"),
            Value::Null => "_".into(),
            Value::Bool(b) => format!("b{b}"),
            Value::Str(s) => format!("s{s:?}"),
            Value::Time(t) => format!("t{}", t.as_micros()),
            Value::List(items) => {
                let keys: Vec<String> = items.iter().map(Value::key).collect();
                format!("[{}]", keys.join(","))
            }
            Value::Map(map) => {
                let keys: Vec<String> = map
                    .iter()
                    .map(|(k, v)| format!("{k:?}:{}", v.key()))
                    .collect();
                format!("{{{}}}", keys.join(","))
            }
            Value::Node(node) => format!("N{}", node.id),
            Value::Edge(edge) => format!("E{}", edge.id),
        }
    }

    fn as_time(&self) -> Option<Timestamp> {
        match self {
            Value::Time(t) => Some(*t),
            Value::Str(s) => Timestamp::parse(s).ok(),
            _ => None,
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

/// Query equality: `None` (unknown) when either side is `null`.
pub(crate) fn equals(a: &Value, b: &Value) -> Option<bool> {
    match (a, b) {
        (Value::Null, _) | (_, Value::Null) => None,
        (Value::Int(x), Value::Int(y)) => Some(x == y),
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
            Some(number(a) == number(b))
        }
        (Value::Time(_), Value::Str(_)) | (Value::Str(_), Value::Time(_)) => {
            Some(a.as_time().is_some() && a.as_time() == b.as_time())
        }
        (Value::List(x), Value::List(y)) => {
            if x.len() != y.len() {
                return Some(false);
            }
            let mut unknown = false;
            for (left, right) in x.iter().zip(y) {
                match equals(left, right) {
                    Some(false) => return Some(false),
                    None => unknown = true,
                    Some(true) => {}
                }
            }
            if unknown { None } else { Some(true) }
        }
        (Value::Map(x), Value::Map(y)) => {
            if x.len() != y.len() || x.keys().ne(y.keys()) {
                return Some(false);
            }
            let mut unknown = false;
            for (left, right) in x.values().zip(y.values()) {
                match equals(left, right) {
                    Some(false) => return Some(false),
                    None => unknown = true,
                    Some(true) => {}
                }
            }
            if unknown { None } else { Some(true) }
        }
        _ => Some(a.key() == b.key()),
    }
}

/// The order of `<`, `<=`, `>`, `>=`: `None` when the values cannot be
/// compared (different types, or `null`).
pub(crate) fn compare(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Null, _) | (_, Value::Null) => None,
        (Value::Int(x), Value::Int(y)) => Some(x.cmp(y)),
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
            number(a)?.partial_cmp(&number(b)?)
        }
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
        (Value::Time(_), Value::Time(_) | Value::Str(_)) | (Value::Str(_), Value::Time(_)) => {
            Some(a.as_time()?.cmp(&b.as_time()?))
        }
        (Value::List(x), Value::List(y)) => {
            for (left, right) in x.iter().zip(y) {
                match compare(left, right)? {
                    Ordering::Equal => {}
                    other => return Some(other),
                }
            }
            Some(x.len().cmp(&y.len()))
        }
        _ => None,
    }
}

/// A total order for `ORDER BY`: values of different types sort by type,
/// and `null` sorts last.
pub(crate) fn sort_order(a: &Value, b: &Value) -> Ordering {
    fn rank(value: &Value) -> u8 {
        match value {
            Value::Map(_) => 0,
            Value::Node(_) => 1,
            Value::Edge(_) => 2,
            Value::List(_) => 3,
            Value::Str(_) => 4,
            Value::Bool(_) => 5,
            Value::Int(_) | Value::Float(_) => 6,
            Value::Time(_) => 7,
            Value::Null => 8,
        }
    }
    match (a, b) {
        (Value::Time(x), Value::Time(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.total_cmp(y),
        _ => compare(a, b)
            .filter(|_| rank(a) == rank(b))
            .unwrap_or_else(|| rank(a).cmp(&rank(b)).then_with(|| a.key().cmp(&b.key()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nulls_are_unknown_and_numbers_mix() {
        assert_eq!(equals(&Value::Null, &Value::Int(1)), None);
        assert_eq!(equals(&Value::Int(1), &Value::Float(1.0)), Some(true));
        assert_eq!(
            compare(&Value::Int(1), &Value::Float(1.5)),
            Some(Ordering::Less)
        );
        assert_eq!(compare(&Value::Int(1), &Value::Str("1".into())), None);
        assert_eq!(Value::Int(2).key(), Value::Float(2.0).key());
    }

    #[test]
    fn times_compare_with_iso_strings() {
        let time = Value::Time(Timestamp::parse("2024-05-01").unwrap());
        assert_eq!(
            compare(&time, &Value::Str("2024-01-01".into())),
            Some(Ordering::Greater)
        );
        assert_eq!(
            equals(&time, &Value::Str("2024-05-01T00:00:00Z".into())),
            Some(true)
        );
    }

    #[test]
    fn sorting_is_total_with_nulls_last() {
        let mut values = [
            Value::Null,
            Value::Int(3),
            Value::Str("a".into()),
            Value::Float(1.5),
        ];
        values.sort_by(sort_order);
        assert_eq!(
            values.iter().map(Value::type_name).collect::<Vec<_>>(),
            ["string", "float", "integer", "null"]
        );
    }
}
