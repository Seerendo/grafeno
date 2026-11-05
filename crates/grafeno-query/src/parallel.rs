//! Parallel scans: the candidates of a large start split across threads.
//!
//! A read-only `MATCH` that starts from many nodes (a large label, or an
//! index range that holds many of them) hands chunks of the candidates to
//! worker threads, which share the snapshot and the query's budget (its
//! limits count every thread's reads, and time out or cancel them all).
//! Each chunk's rows come back in chunk order, so the rows are the same,
//! in the same order, as on one thread.
//!
//! Threads are shared by every query of the process: at most one per CPU
//! (or [`set_parallelism`]) work at once, so many concurrent queries do
//! not multiply them; a query that finds none free runs on its own thread.
//! Work inside a worker (a pattern predicate, a subquery) does not split
//! again.

use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use grafeno::NodeId;

use crate::QueryError;
use crate::eval::{Context, Row};

/// Starts with fewer candidates than this run on one thread.
pub const MIN_CANDIDATES: usize = 4096;
/// Candidates per chunk handed to a worker, at least.
const MIN_CHUNK: usize = 512;

/// The most threads parallel scans may use at once; 0 is one per CPU.
static PARALLELISM: AtomicUsize = AtomicUsize::new(0);
/// Worker threads running now, over every query.
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static IN_WORKER: Cell<bool> = const { Cell::new(false) };
}

/// Sets how many threads parallel scans may use at once, over all
/// queries (1 turns them off; 0 is one per CPU, the default).
pub fn set_parallelism(threads: usize) {
    PARALLELISM.store(threads, Ordering::Relaxed);
}

/// How many threads parallel scans may use at once.
pub fn parallelism() -> usize {
    match PARALLELISM.load(Ordering::Relaxed) {
        0 => std::thread::available_parallelism().map_or(1, |n| n.get()),
        n => n,
    }
}

/// Whether this thread may split a scan now.
pub(crate) fn available() -> bool {
    parallelism() > 1 && !IN_WORKER.with(Cell::get)
}

/// Takes up to `want` worker threads from the shared allowance.
fn reserve(want: usize) -> usize {
    let max = parallelism().saturating_sub(1);
    let mut current = ACTIVE.load(Ordering::Relaxed);
    loop {
        let take = want.min(max.saturating_sub(current));
        if take == 0 {
            return 0;
        }
        match ACTIVE.compare_exchange(current, current + take, Ordering::AcqRel, Ordering::Relaxed)
        {
            Ok(_) => return take,
            Err(now) => current = now,
        }
    }
}

struct Reserved(usize);

impl Drop for Reserved {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(self.0, Ordering::AcqRel);
    }
}

/// Marks the current thread as a worker until dropped.
struct WorkerMark(bool);

impl WorkerMark {
    fn new() -> Self {
        Self(IN_WORKER.with(|flag| flag.replace(true)))
    }
}

impl Drop for WorkerMark {
    fn drop(&mut self) {
        IN_WORKER.with(|flag| flag.set(self.0));
    }
}

/// The rows of one chunk, by its position.
type ChunkRows = (usize, Result<Vec<Row>, QueryError>);

/// Runs `work` on chunks of `ids` on this thread and the workers it can
/// get, and joins the rows in chunk order. The first error (in chunk
/// order) stops the others.
pub(crate) fn map_chunks<F>(ctx: &Context, ids: &[NodeId], work: F) -> Result<Vec<Row>, QueryError>
where
    F: Fn(&Context, &[NodeId]) -> Result<Vec<Row>, QueryError> + Sync,
{
    let wanted = (ids.len() / MIN_CHUNK).saturating_sub(1);
    let reserved = Reserved(if ids.len() >= MIN_CANDIDATES {
        reserve(wanted)
    } else {
        0
    });
    let _mark = WorkerMark::new();
    if reserved.0 == 0 {
        return work(ctx, ids);
    }
    let threads = reserved.0 + 1;
    // Several chunks per thread, so a slow chunk does not hold the rest.
    let size = ids.len().div_ceil(threads * 8).max(MIN_CHUNK);
    let chunks: Vec<&[NodeId]> = ids.chunks(size).collect();
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let results: Mutex<Vec<ChunkRows>> = Mutex::new(Vec::new());
    let run = |ctx: &Context| {
        loop {
            if failed.load(Ordering::Relaxed) {
                break;
            }
            let index = next.fetch_add(1, Ordering::Relaxed);
            let Some(chunk) = chunks.get(index) else {
                break;
            };
            let result = work(ctx, chunk);
            if result.is_err() {
                failed.store(true, Ordering::Relaxed);
            }
            results
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((index, result));
        }
    };
    let (read, ns, params, edges, info, budget) = (
        ctx.read, ctx.ns, ctx.params, ctx.edges, ctx.info, ctx.budget,
    );
    std::thread::scope(|scope| {
        for _ in 1..threads {
            scope.spawn(|| {
                let _mark = WorkerMark::new();
                let ctx = Context {
                    read,
                    write: None,
                    ns,
                    params,
                    edges,
                    info,
                    budget,
                };
                run(&ctx);
            });
        }
        run(ctx);
    });
    let mut results = results.into_inner().unwrap_or_else(|e| e.into_inner());
    results.sort_by_key(|(index, _)| *index);
    let mut rows = Vec::new();
    for (_, result) in results {
        rows.extend(result?);
    }
    Ok(rows)
}
