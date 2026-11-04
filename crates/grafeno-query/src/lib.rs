//! GQ, the Grafeno query language: a subset of Cypher/GQL with Grafeno's
//! time and memory features.
//!
//! ```text
//! MATCH (a:Person {name: 'Ana'})-[w:works_at]->(c:Company)
//! VALID AT '2024-06-01'
//! WHERE c.name STARTS WITH 'Ac'
//! RETURN a.name, c.name, w.@valid_from
//! ORDER BY c.name LIMIT 10
//! ```
//!
//! - **Reading**: `MATCH` and `OPTIONAL MATCH` patterns
//!   (`(n:Label {key: value})`, `-[r:a|b]->`, `<-`, `-`, `*min..max`),
//!   `VALID AT` / `KNOWN AT`, `WHERE` (with pattern predicates), `WITH`,
//!   `UNWIND`, `UNION [ALL]`, `CALL procedure() YIELD …`, and `RETURN` with
//!   `DISTINCT`, aggregates (`count`, `sum`, `avg`, `min`, `max`,
//!   `collect`), `ORDER BY`, `SKIP` and `LIMIT`.
//! - **Writing**: `CREATE`, `MERGE` (with `ON CREATE` / `ON MATCH`), `SET`,
//!   `REMOVE`, `DELETE` (deletes a node with its edges), `RETRACT` (marks an
//!   edge as a mistake, keeping its history) and `FOREACH`. A statement with
//!   writes runs in one transaction, and its later clauses see its writes.
//! - **Expressions**: `CASE`, list comprehensions, `all`/`any`/`none`/
//!   `single`, `reduce`, slices, `^`.
//! - **Statistics and time**: `stdev`, `variance`, `median`, percentiles,
//!   `mode`; durations (`duration('P1M')`, calendar arithmetic,
//!   `duration.between`), date parts (`t.year`), `date.truncate`, and
//!   `temporal.series` with `VALID AT` per row.
//! - `EXPLAIN` returns the plan; `PROFILE` adds rows and time per step.
//! - **Pseudo-properties** (`@name`) expose what is not a property: `@id`,
//!   `@labels`, `@text`, `@embedding`, `@sources`, `@created_at` on nodes;
//!   `@id`, `@label`, `@from`, `@to`, `@valid_from`, `@valid_to`,
//!   `@recorded_at`, `@expired_at` on edges.
//!
//! The full reference is in `docs/query-language.md`.

mod ast;
mod eval;
mod exec;
mod lexer;
mod parser;
mod procedures;
mod temporal;
mod value;

use std::fmt;

pub use eval::Params;
pub use exec::{
    MAX_PATH_LENGTH, PlanStep, QueryResult, WriteStats, execute, execute_read, is_write,
};
pub use temporal::Duration;
pub use value::Value;

/// A query that could not be parsed or run. Syntax errors carry the line
/// and column (1-based) where they were found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryError {
    pub message: String,
    pub line: Option<usize>,
    pub column: Option<usize>,
    /// The engine failed (storage or serialization), not the query.
    pub internal: bool,
}

impl QueryError {
    pub(crate) fn at(source: &str, offset: usize, message: impl Into<String>) -> Self {
        let before = &source[..offset.min(source.len())];
        let line = before.matches('\n').count() + 1;
        let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
        Self {
            message: message.into(),
            line: Some(line),
            column: Some(column),
            internal: false,
        }
    }

    pub(crate) fn runtime(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            line: None,
            column: None,
            internal: false,
        }
    }
}

impl From<grafeno::Error> for QueryError {
    fn from(error: grafeno::Error) -> Self {
        let internal = matches!(
            error,
            grafeno::Error::Storage(_) | grafeno::Error::Serialization(_)
        );
        Self {
            internal,
            ..Self::runtime(error.to_string())
        }
    }
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.line, self.column) {
            (Some(line), Some(column)) => {
                write!(f, "{} (line {line}, column {column})", self.message)
            }
            _ => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for QueryError {}

/// Parameters from a JSON object (`{"name": "Ana"}` → `$name`).
pub fn params_from_json(object: &serde_json::Map<String, serde_json::Value>) -> Params {
    object
        .iter()
        .map(|(key, value)| (key.clone(), Value::from_json(value)))
        .collect()
}
