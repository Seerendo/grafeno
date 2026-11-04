//! Limits that keep one query from taking the service down: time, records
//! read, rows held in memory, and cancellation.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::QueryError;

/// What one query may use. `None` is no limit.
#[derive(Clone, Debug, Default)]
pub struct Limits {
    /// Wall time, from parsing to the last row.
    pub timeout: Option<Duration>,
    /// Records read: nodes, edges, adjacency and index entries.
    pub max_reads: Option<u64>,
    /// Rows held at once between clauses, in a match, a sort or a group.
    pub max_rows: Option<usize>,
    /// Set by the caller to stop the query (when its client goes away).
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Limits {
    /// The server's defaults: 30 seconds, 50 million records read, a
    /// million rows in memory.
    pub fn standard() -> Self {
        Self {
            timeout: Some(Duration::from_secs(30)),
            max_reads: Some(50_000_000),
            max_rows: Some(1_000_000),
            cancel: None,
        }
    }

    /// No limits: for the library and the local shell.
    pub fn none() -> Self {
        Self::default()
    }
}

/// The running account of a query against its [`Limits`].
pub(crate) struct Budget {
    limits: Limits,
    started: Instant,
    reads: Cell<u64>,
}

impl Budget {
    pub(crate) fn new(limits: &Limits) -> Self {
        Self {
            limits: limits.clone(),
            started: Instant::now(),
            reads: Cell::new(0),
        }
    }

    pub(crate) fn reads(&self) -> u64 {
        self.reads.get()
    }

    /// Counts `n` records read, and checks every limit.
    pub(crate) fn read(&self, n: usize) -> Result<(), QueryError> {
        let reads = self.reads.get().saturating_add(n as u64);
        self.reads.set(reads);
        if let Some(max) = self.limits.max_reads
            && reads > max
        {
            return Err(QueryError::limit(format!(
                "the query read more than {max} records (limit max_reads); add an index, a label or a LIMIT"
            )));
        }
        self.tick()
    }

    /// Checks the time limit and cancellation.
    pub(crate) fn tick(&self) -> Result<(), QueryError> {
        if let Some(timeout) = self.limits.timeout
            && self.started.elapsed() > timeout
        {
            return Err(QueryError::limit(format!(
                "the query ran longer than {} s (limit timeout)",
                timeout.as_secs_f64()
            )));
        }
        if self
            .limits
            .cancel
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
        {
            return Err(QueryError::limit("the query was cancelled"));
        }
        Ok(())
    }

    /// Checks how many rows are held at once.
    pub(crate) fn rows(&self, n: usize) -> Result<(), QueryError> {
        if let Some(max) = self.limits.max_rows
            && n > max
        {
            return Err(QueryError::limit(format!(
                "the query held more than {max} rows at once (limit max_rows); narrow the match or aggregate earlier"
            )));
        }
        Ok(())
    }
}
