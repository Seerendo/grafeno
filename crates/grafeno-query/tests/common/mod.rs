//! Shared by test suites: checks that a query split across members (phase
//! 13.4) returns what it returns on one server.

#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};

use grafeno::Grafeno;
use grafeno_query::{
    Limits, Params, Partial, execute_part, execute_read_with, merge_parts, split_plan,
};

/// Queries split and compared so far, in this test binary.
pub static SPLIT: AtomicUsize = AtomicUsize::new(0);

/// When `query` can be split, runs it whole and split in 2, 3 and 5 parts
/// (each part's answer through JSON, as between members) and panics unless
/// they give the same columns and rows, in the same order (or all fail).
/// Returns whether it was split.
pub fn check_split(db: &Grafeno, ns: &str, query: &str, params: &Params) -> bool {
    let read = db.begin_read().unwrap();
    let plan = match split_plan(&read, ns, query, params) {
        Ok(Some(plan)) => plan,
        _ => return false,
    };
    let limits = Limits::none();
    let whole = execute_read_with(&read, ns, query, params, &limits);
    for parts in [2, 3, 5] {
        let split = (0..parts)
            .map(|part| {
                let partial = execute_part(&read, ns, query, params, &limits, &plan, part, parts)?;
                let text = serde_json::to_string(&partial).unwrap();
                Ok(serde_json::from_str::<Partial>(&text).unwrap())
            })
            .collect::<Result<Vec<_>, grafeno_query::QueryError>>()
            .and_then(|partials| merge_parts(&read, ns, query, params, &limits, &plan, partials));
        match (&whole, &split) {
            (Ok(one), Ok(many)) => {
                assert_eq!(many.columns, one.columns, "{query} in {parts} parts");
                assert_eq!(
                    many.to_json()["rows"],
                    one.to_json()["rows"],
                    "{query} in {parts} parts"
                );
            }
            (Err(_), Err(_)) => {}
            (one, many) => panic!(
                "{query} in {parts} parts: one server {:?}, split {:?}",
                one.as_ref().map(|r| r.to_json()["rows"].clone()),
                many.as_ref().map(|r| r.to_json()["rows"].clone())
            ),
        }
    }
    SPLIT.fetch_add(1, Ordering::Relaxed);
    true
}
