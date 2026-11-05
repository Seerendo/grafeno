//! Parsed statements, kept for queries that run again (the same text with
//! other parameters): a bounded cache shared by every thread, the least
//! recently used dropped first.
//!
//! Only the parse is kept, not the plan: where each pattern starts and in
//! which order patterns run is decided at run time from the snapshot's
//! counts and the planner's statistics, so a cached statement never runs
//! with a plan made for data that has since changed (or for an index that
//! was dropped).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::QueryError;
use crate::ast::Statement;
use crate::parser::parse;

/// Statements kept.
pub const STATEMENT_CACHE: usize = 1024;
/// Longer query texts are parsed every time (they are seldom repeated).
const MAX_TEXT: usize = 16 * 1024;

#[derive(Default)]
struct Cache {
    entries: HashMap<String, (Arc<Statement>, u64)>,
    tick: u64,
}

static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);

fn cache() -> &'static Mutex<Cache> {
    CACHE.get_or_init(Mutex::default)
}

/// The parsed statement for `query`, from the cache when it was parsed
/// before. Syntax errors are not kept.
pub(crate) fn parsed(query: &str) -> Result<Arc<Statement>, QueryError> {
    if query.len() > MAX_TEXT {
        return parse(query).map(Arc::new);
    }
    {
        let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
        cache.tick += 1;
        let tick = cache.tick;
        if let Some((statement, used)) = cache.entries.get_mut(query) {
            *used = tick;
            HITS.fetch_add(1, Ordering::Relaxed);
            return Ok(statement.clone());
        }
    }
    MISSES.fetch_add(1, Ordering::Relaxed);
    let statement = Arc::new(parse(query)?);
    let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
    if cache.entries.len() >= STATEMENT_CACHE {
        // Drop the least recently used eighth at once, so that eviction
        // stays rare.
        let mut ticks: Vec<u64> = cache.entries.values().map(|(_, used)| *used).collect();
        ticks.sort_unstable();
        let cut = ticks[STATEMENT_CACHE / 8];
        cache.entries.retain(|_, (_, used)| *used > cut);
    }
    let tick = cache.tick;
    cache
        .entries
        .insert(query.to_owned(), (statement.clone(), tick));
    Ok(statement)
}

/// How the statement cache has done since the process started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct StatementCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub entries: usize,
}

pub fn statement_cache_stats() -> StatementCacheStats {
    StatementCacheStats {
        hits: HITS.load(Ordering::Relaxed),
        misses: MISSES.load(Ordering::Relaxed),
        entries: cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .len(),
    }
}

/// Empties the statement cache.
pub fn clear_statement_cache() {
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entries
        .clear();
}
