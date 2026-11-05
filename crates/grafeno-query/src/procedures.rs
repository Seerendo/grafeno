//! Built-in procedures for `CALL name(args) YIELD …`.

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::QueryError;
use crate::algorithms::{self, ALGORITHMS};
use crate::eval::{Context, to_time};
use crate::temporal::{self, Duration};
use crate::value::Value;

/// `temporal.series` stops at this many points.
const MAX_SERIES: usize = 10_000;

/// A procedure: its name, arguments, the columns it yields, and what it does.
pub(crate) struct Procedure {
    pub(crate) name: &'static str,
    pub(crate) args: &'static str,
    pub(crate) columns: &'static [&'static str],
    pub(crate) description: &'static str,
}

pub(crate) const PROCEDURES: &[Procedure] = &[
    Procedure {
        name: "db.labels",
        args: "",
        columns: &["label", "count"],
        description: "Node labels of the namespace and how many nodes have each.",
    },
    Procedure {
        name: "db.edgeLabels",
        args: "",
        columns: &["label", "count"],
        description: "Edge labels of the namespace and how many edges have each (also db.relationshipTypes).",
    },
    Procedure {
        name: "db.propertyKeys",
        args: "",
        columns: &["key"],
        description: "Property names used by nodes and edges (reads every record).",
    },
    Procedure {
        name: "db.indexes",
        args: "",
        columns: &["name", "label", "property", "unique", "entries", "fuzzy"],
        description: "Property indexes, fuzzy indexes and unique constraints (also SHOW INDEXES); a fuzzy index counts trigram entries.",
    },
    Procedure {
        name: "db.match",
        args: "label, properties, limit = 10",
        columns: &["node", "score", "decision", "fields"],
        description: "Nodes that may be the record described by the properties, by the label's match rule: score, decision (same, review, different) and each field's similarity.",
    },
    Procedure {
        name: "db.searchFuzzy",
        args: "label, property, text, limit = 10, measure = 'dice'",
        columns: &["node", "score"],
        description: "Nodes whose property looks like the text (typos, word order), through the fuzzy index on it; measure: dice, jaro_winkler, edit, tokens, …",
    },
    Procedure {
        name: "db.stats",
        args: "",
        columns: &["nodes", "edges", "texts", "vectors", "dimension"],
        description: "Counts of the namespace.",
    },
    Procedure {
        name: "db.analyze",
        args: "",
        columns: &[
            "kind",
            "name",
            "count",
            "distinct",
            "out_degree",
            "in_degree",
            "common",
        ],
        description: "Rebuilds the planner's statistics now and lists them: per index its entries, distinct values and most common values; per edge label its edges and average degree each way. They are otherwise rebuilt on their own when the data drifts.",
    },
    Procedure {
        name: "db.search",
        args: "text, limit = 10",
        columns: &["node", "score"],
        description: "Full-text search (BM25), best first.",
    },
    Procedure {
        name: "db.searchVector",
        args: "embedding, limit = 10",
        columns: &["node", "score"],
        description: "Nearest nodes by embedding (cosine similarity), best first.",
    },
    Procedure {
        name: "temporal.series",
        args: "start, end, step",
        columns: &["at"],
        description: "Times from start to end (included) every step (a duration such as 'P1M'), for counting what was valid at each one.",
    },
    Procedure {
        name: "dbms.procedures",
        args: "",
        columns: &["name", "arguments", "columns", "description"],
        description: "The procedures CALL can run.",
    },
];

/// Accepted alternative names, from Cypher.
fn canonical(name: &str) -> &str {
    match name {
        "db.relationshiptypes" => "db.edgelabels",
        other => other,
    }
}

pub(crate) fn find(name: &str) -> Result<&'static Procedure, QueryError> {
    let name = canonical(name);
    PROCEDURES
        .iter()
        .chain(ALGORITHMS)
        .find(|procedure| procedure.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| {
            QueryError::runtime(format!(
                "unknown procedure {name}() (CALL dbms.procedures() lists them)"
            ))
        })
}

fn limit_arg(args: &[Value], at: usize) -> Result<usize, QueryError> {
    match args.get(at) {
        None | Some(Value::Null) => Ok(10),
        Some(Value::Int(n)) if *n > 0 => Ok(*n as usize),
        Some(other) => Err(QueryError::runtime(format!(
            "the limit must be a positive integer, got {}",
            other.to_json()
        ))),
    }
}

/// Runs a procedure: one row per result, in the order of its columns.
pub(crate) fn call(
    ctx: &Context,
    name: &str,
    args: &[Value],
) -> Result<Vec<Vec<Value>>, QueryError> {
    let procedure = find(name)?;
    if procedure.name.starts_with("algo.") {
        return algorithms::call(ctx, procedure, args);
    }
    let read = ctx.read;
    let ns = ctx.ns;
    let counted = |counts: std::collections::BTreeMap<String, u64>| -> Vec<Vec<Value>> {
        counts
            .into_iter()
            .map(|(label, count)| vec![Value::Str(label), Value::Int(count as i64)])
            .collect()
    };
    Ok(match procedure.name {
        "db.labels" => counted(read.stats(ns)?.node_labels),
        "db.edgeLabels" => counted(read.stats(ns)?.edge_labels),
        "db.propertyKeys" => {
            let mut keys = BTreeSet::new();
            for node in read.nodes(ns)? {
                keys.extend(node.props.keys().cloned());
            }
            for edge in read.edges(ns)? {
                keys.extend(edge.props.keys().cloned());
            }
            keys.into_iter().map(|key| vec![Value::Str(key)]).collect()
        }
        "db.indexes" => read
            .indexes(ns)?
            .into_iter()
            .map(|info| {
                vec![
                    Value::Str(info.def.name),
                    Value::Str(info.def.label),
                    Value::Str(info.def.key),
                    Value::Bool(info.def.unique),
                    Value::Int(info.entries as i64),
                ]
            })
            .collect(),
        "db.stats" => {
            let stats = read.stats(ns)?;
            vec![vec![
                Value::Int(stats.nodes as i64),
                Value::Int(stats.edges as i64),
                Value::Int(stats.texts as i64),
                Value::Int(stats.vectors as i64),
                stats
                    .vector_dimension
                    .map_or(Value::Null, |dimension| Value::Int(dimension as i64)),
            ]]
        }
        "db.analyze" => {
            let stats = read.analyze(ns)?;
            let mut rows = Vec::new();
            for index in &stats.indexes {
                rows.push(vec![
                    Value::Str("index".into()),
                    Value::Str(index.name.clone()),
                    Value::Int(index.entries as i64),
                    Value::Int(index.distinct as i64),
                    Value::Null,
                    Value::Null,
                    Value::List(
                        index
                            .common
                            .iter()
                            .take(10)
                            .map(|(value, count)| {
                                Value::List(vec![
                                    Value::from_json(value),
                                    Value::Int(*count as i64),
                                ])
                            })
                            .collect(),
                    ),
                ]);
            }
            for edge in &stats.edge_labels {
                rows.push(vec![
                    Value::Str("edge".into()),
                    Value::Str(edge.label.clone()),
                    Value::Int(edge.edges as i64),
                    Value::Null,
                    Value::Float(edge.out_degree()),
                    Value::Float(edge.in_degree()),
                    Value::Null,
                ]);
            }
            rows
        }
        "db.search" | "db.searchVector" => {
            let limit = limit_arg(args, 1)?;
            let hits = if procedure.name == "db.search" {
                let Some(Value::Str(text)) = args.first() else {
                    return Err(QueryError::runtime("db.search() needs the text to search"));
                };
                read.search_text(ns, text, limit)?
            } else {
                let Some(Value::List(items)) = args.first() else {
                    return Err(QueryError::runtime(
                        "db.searchVector() needs an embedding (a list of numbers)",
                    ));
                };
                let mut embedding = Vec::with_capacity(items.len());
                for item in items {
                    embedding.push(match item {
                        Value::Int(i) => *i as f32,
                        Value::Float(f) => *f as f32,
                        other => {
                            return Err(QueryError::runtime(format!(
                                "an embedding holds numbers, not a {}",
                                other.type_name()
                            )));
                        }
                    });
                }
                read.search_vector(ns, &embedding, limit)?
            };
            let mut rows = Vec::new();
            for hit in hits {
                if let Some(node) = ctx.node(hit.node)? {
                    rows.push(vec![
                        Value::Node(Arc::new(node)),
                        Value::Float(f64::from(hit.score)),
                    ]);
                }
            }
            rows
        }
        "db.match" => {
            let (Some(Value::Str(label)), Some(Value::Map(props))) = (args.first(), args.get(1))
            else {
                return Err(QueryError::runtime(
                    "db.match(label, properties, limit) needs a label and a map of properties",
                ));
            };
            let probe = grafeno::Probe {
                label: label.clone(),
                props: props
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_json()))
                    .collect(),
                neighbors: Vec::new(),
            };
            let limit = limit_arg(args, 2)?;
            let mut rows = Vec::new();
            for found in read.find_matches(ns, &probe, None, limit, None)? {
                let Some(node) = ctx.node(found.node)? else {
                    continue;
                };
                let decision = serde_json::to_value(found.decision)
                    .map_err(|e| QueryError::runtime(e.to_string()))?;
                let fields = serde_json::to_value(&found.fields)
                    .map_err(|e| QueryError::runtime(e.to_string()))?;
                rows.push(vec![
                    Value::Node(Arc::new(node)),
                    Value::Float(found.score),
                    Value::from_json(&decision),
                    Value::from_json(&fields),
                ]);
            }
            rows
        }
        "db.searchFuzzy" => {
            let text_at = |at: usize, what: &str| -> Result<String, QueryError> {
                match args.get(at) {
                    Some(Value::Str(s)) => Ok(s.clone()),
                    _ => Err(QueryError::runtime(format!(
                        "db.searchFuzzy(label, property, text, limit, measure) needs {what}"
                    ))),
                }
            };
            let (label, key, text) = (
                text_at(0, "a label")?,
                text_at(1, "a property")?,
                text_at(2, "the text to look for")?,
            );
            let limit = limit_arg(args, 3)?;
            let measure = match args.get(4) {
                None | Some(Value::Null) => grafeno::textsim::Measure::Dice { n: 3 },
                Some(Value::Str(name)) => {
                    grafeno::textsim::Measure::by_name(name).ok_or_else(|| {
                        QueryError::runtime(format!("db.searchFuzzy(): unknown measure {name:?}"))
                    })?
                }
                Some(other) => {
                    return Err(QueryError::runtime(format!(
                        "db.searchFuzzy(): the measure is a name, not a {}",
                        other.type_name()
                    )));
                }
            };
            let mut rows = Vec::new();
            for hit in read.fuzzy_search(ns, &label, &key, &text, measure, limit)? {
                if let Some(node) = ctx.node(hit.node)? {
                    rows.push(vec![Value::Node(Arc::new(node)), Value::Float(hit.score)]);
                }
            }
            rows
        }
        "temporal.series" => {
            let [start, end, step] = args else {
                return Err(QueryError::runtime(
                    "temporal.series() takes a start, an end and a step (such as 'P1M')",
                ));
            };
            let (start, end) = (to_time(start)?, to_time(end)?);
            let step = match step {
                Value::Duration(d) => *d,
                Value::Str(text) => Duration::parse(text)?,
                other => {
                    return Err(QueryError::runtime(format!(
                        "temporal.series() needs a duration as step, not a {}",
                        other.type_name()
                    )));
                }
            };
            if step.approx_micros() <= 0.0 {
                return Err(QueryError::runtime(
                    "temporal.series() needs a positive step",
                ));
            }
            let mut rows = Vec::new();
            // Each point is start + n × step, so month steps stay on the
            // calendar (Jan 31, Feb 29, Mar 31, …).
            loop {
                let at = temporal::add(start, step.scale(rows.len() as f64)?)?;
                if at > end {
                    break;
                }
                if rows.len() == MAX_SERIES {
                    return Err(QueryError::runtime(format!(
                        "temporal.series() would give more than {MAX_SERIES} points: use a larger step"
                    )));
                }
                rows.push(vec![Value::Time(at)]);
            }
            rows
        }
        "dbms.procedures" => PROCEDURES
            .iter()
            .chain(ALGORITHMS)
            .map(|procedure| {
                vec![
                    Value::Str(procedure.name.into()),
                    Value::Str(procedure.args.into()),
                    Value::List(
                        procedure
                            .columns
                            .iter()
                            .map(|column| Value::Str((*column).into()))
                            .collect(),
                    ),
                    Value::Str(procedure.description.into()),
                ]
            })
            .collect(),
        other => unreachable!("procedure {other} without a body"),
    })
}
