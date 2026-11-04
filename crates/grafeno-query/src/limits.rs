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
    /// The namespaces the caller may see, for statements that list them
    /// (`SHOW NAMESPACES`); all when `None`.
    pub visible: Option<NamespaceFilter>,
}

/// Which namespaces a caller may see.
#[derive(Clone)]
pub struct NamespaceFilter(pub Arc<dyn Fn(&str) -> bool + Send + Sync>);

impl NamespaceFilter {
    pub fn new(filter: impl Fn(&str) -> bool + Send + Sync + 'static) -> Self {
        Self(Arc::new(filter))
    }

    pub fn allows(&self, ns: &str) -> bool {
        (self.0)(ns)
    }
}

impl std::fmt::Debug for NamespaceFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NamespaceFilter")
    }
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
            visible: None,
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

    /// A check of the time limit and cancellation that other threads can
    /// run (graph algorithms).
    pub(crate) fn stopper(&self) -> impl Fn() -> bool + Sync + 'static {
        let deadline = self.limits.timeout.map(|timeout| self.started + timeout);
        let cancel = self.limits.cancel.clone();
        move || {
            deadline.is_some_and(|deadline| Instant::now() > deadline)
                || cancel
                    .as_ref()
                    .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
        }
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
