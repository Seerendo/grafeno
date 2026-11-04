//! Group commit: concurrent writes share one transaction and one commit.
//!
//! redb has one writer at a time, and a commit with immediate durability
//! waits for the disk. A [`Writer`] runs on its own thread: it takes every
//! write waiting in its queue (up to a batch size), runs them one after the
//! other in a single transaction, and commits once. Each write still gets
//! its own result:
//!
//! - writes run in arrival order, and each sees the writes before it;
//! - when one fails, the transaction is discarded, that write gets its
//!   error, and the others run again in a new transaction (so a write must
//!   be safe to run twice: it is given as `Fn`, and only its last run
//!   counts);
//! - when the commit fails, every write of the group gets that error.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use grafeno_core::{Error, Result};

use crate::db::{Grafeno, WriteTx};

/// How a [`Writer`] groups writes.
#[derive(Clone, Copy, Debug)]
pub struct WriterOptions {
    /// Writes per transaction at most.
    pub max_batch: usize,
    /// Writes that may wait; beyond it, [`Writer::write`] answers
    /// [`Error::Busy`] at once.
    pub queue: usize,
    /// With eventual durability, how often waiting commits reach the disk.
    pub flush_every: Duration,
}

impl Default for WriterOptions {
    fn default() -> Self {
        Self {
            max_batch: 128,
            queue: 4_096,
            flush_every: Duration::from_millis(500),
        }
    }
}

/// What a write did, kept while the group is not committed.
type Attempt = Box<dyn FnMut(&mut WriteTx) -> Result<()> + Send>;
type Done = Box<dyn FnOnce(Result<()>) + Send>;

struct Job {
    attempt: Attempt,
    done: Done,
}

/// A thread that commits writes in groups. Dropping it lets the queued
/// writes finish and stops the thread.
pub struct Writer {
    queue: Option<SyncSender<Job>>,
    worker: Option<JoinHandle<()>>,
}

impl Writer {
    pub fn new(db: Arc<Grafeno>, options: WriterOptions) -> Self {
        let (queue, jobs) = mpsc::sync_channel(options.queue.max(1));
        let worker = std::thread::Builder::new()
            .name("grafeno-writer".into())
            .spawn(move || work(&db, jobs, options))
            .expect("the writer thread starts");
        Self {
            queue: Some(queue),
            worker: Some(worker),
        }
    }

    /// Runs `f` in the next group and waits for its commit. `f` may run more
    /// than once (when another write of its group fails); only the last
    /// run's changes and value count.
    pub fn write<T: Send + 'static>(
        &self,
        f: impl Fn(&mut WriteTx) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let slot = Arc::new(Mutex::new(None));
        let kept = slot.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        let job = Job {
            attempt: Box::new(move |tx| {
                let value = f(tx)?;
                *kept.lock().expect("not poisoned") = Some(value);
                Ok(())
            }),
            done: Box::new(move |outcome| {
                let result = outcome.map(|()| {
                    slot.lock()
                        .expect("not poisoned")
                        .take()
                        .expect("a committed write has a value")
                });
                let _ = sender.send(result);
            }),
        };
        let queue = self.queue.as_ref().expect("open until dropped");
        match queue.try_send(job) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                return Err(Error::Busy("too many writes are waiting".into()));
            }
            Err(TrySendError::Disconnected(_)) => {
                return Err(Error::Storage("the writer stopped".into()));
            }
        }
        receiver
            .recv()
            .unwrap_or_else(|_| Err(Error::Storage("the writer stopped".into())))
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        // Closing the queue ends the thread once it has drained it.
        self.queue.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn work(db: &Grafeno, jobs: Receiver<Job>, options: WriterOptions) {
    let mut last_flush = Instant::now();
    loop {
        let first = match jobs.recv_timeout(options.flush_every) {
            Ok(job) => job,
            Err(RecvTimeoutError::Timeout) => {
                let _ = db.flush();
                last_flush = Instant::now();
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let mut batch = vec![first];
        while batch.len() < options.max_batch {
            match jobs.try_recv() {
                Ok(job) => batch.push(job),
                Err(_) => break,
            }
        }
        run(db, batch);
        if last_flush.elapsed() >= options.flush_every {
            let _ = db.flush();
            last_flush = Instant::now();
        }
    }
    let _ = db.flush();
}

/// Runs a group to its commit, dropping the writes that fail.
fn run(db: &Grafeno, mut jobs: Vec<Job>) {
    while !jobs.is_empty() {
        let mut tx = match db.begin_write() {
            Ok(tx) => tx,
            Err(error) => {
                for job in jobs {
                    (job.done)(Err(error.clone()));
                }
                return;
            }
        };
        let mut failed = None;
        for (index, job) in jobs.iter_mut().enumerate() {
            match catch_unwind(AssertUnwindSafe(|| (job.attempt)(&mut tx))) {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    failed = Some((index, error));
                    break;
                }
                Err(_) => {
                    failed = Some((index, Error::Storage("the write panicked".into())));
                    break;
                }
            }
        }
        match failed {
            Some((index, error)) => {
                let _ = tx.abort();
                let job = jobs.remove(index);
                (job.done)(Err(error));
            }
            None => {
                let outcome = tx.commit();
                for job in jobs {
                    (job.done)(outcome.clone());
                }
                return;
            }
        }
    }
}
