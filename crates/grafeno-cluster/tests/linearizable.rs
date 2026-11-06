//! Phase 13.3: fault injection on members in one process. Clients write
//! distinct values to one register and read it (linearizable reads),
//! while a nemesis cuts members off, restarts them (the leader too) and
//! compacts their logs. The history must be linearizable, and the members
//! must end with the same value. Prints how long writes stopped after each
//! fault aimed at the leader, and the share of time writes succeeded.
//!
//! `FAULT_SEED` replays a run; `FAULT_SECONDS` sets its length (default 8).
//! `FAULT_UNSAFE_READS=1` reads members' local state without ReadIndex: the
//! control, which the checker should reject.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use grafeno::{Grafeno, NodeId};
use grafeno_cluster::check::{Kind, Op, linearizable};
use grafeno_cluster::memory::Hub;
use grafeno_cluster::{Error, Node, NodeOptions, Storage};

const NS: &str = "c";
const MEMBERS: u64 = 5;
const CLIENTS: u64 = 4;

/// xorshift64*: small, seeded, good enough to pick faults.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn between(&mut self, low: u64, high: u64) -> u64 {
        low + self.below(high - low + 1)
    }
}

fn options(id: u64, dir: &Path) -> NodeOptions {
    let mut options = NodeOptions::new(id, dir.join(format!("snapshots-{id}")));
    options.tick = Duration::from_millis(20);
    options.write_timeout = Duration::from_secs(1);
    options
}

struct Cluster {
    hub: Arc<Hub>,
    dir: PathBuf,
    dbs: Vec<Arc<Grafeno>>,
    nodes: Vec<Mutex<Arc<Node>>>,
}

impl Cluster {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("grafeno-faults-{}", NodeId::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let hub = Hub::new();
        let voters: Vec<u64> = (1..=MEMBERS).collect();
        let dbs: Vec<Arc<Grafeno>> = (0..MEMBERS)
            .map(|_| Arc::new(Grafeno::in_memory().unwrap()))
            .collect();
        let nodes = (1..=MEMBERS)
            .map(|id| Mutex::new(Self::boot(&hub, &dir, &dbs, id, &voters)))
            .collect();
        Self {
            hub,
            dir,
            dbs,
            nodes,
        }
    }

    fn boot(
        hub: &Arc<Hub>,
        dir: &Path,
        dbs: &[Arc<Grafeno>],
        id: u64,
        voters: &[u64],
    ) -> Arc<Node> {
        let storage = Storage::open(&dir.join(format!("raft-{id}.redb"))).unwrap();
        let node = Arc::new(
            Node::start(
                dbs[(id - 1) as usize].clone(),
                storage,
                hub.transport(),
                voters,
                options(id, dir),
            )
            .unwrap(),
        );
        hub.register(&node);
        node
    }

    fn node(&self, id: u64) -> Arc<Node> {
        self.nodes[(id - 1) as usize]
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Stops a member, waits, and starts it again from what it persisted.
    fn restart(&self, id: u64, down: Duration) {
        let mut slot = self.nodes[(id - 1) as usize]
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        slot.stop();
        std::thread::sleep(down);
        let voters: Vec<u64> = (1..=MEMBERS).collect();
        *slot = Self::boot(&self.hub, &self.dir, &self.dbs, id, &voters);
    }

    fn leader(&self) -> Option<u64> {
        (1..=MEMBERS).find(|id| self.node(*id).status().role == "leader")
    }

    fn value(&self, id: u64) -> u64 {
        self.dbs[(id - 1) as usize]
            .begin_read()
            .unwrap()
            .state(NS, "register", "r")
            .unwrap()
            .map_or(0, |bytes| u64::from_le_bytes(bytes.try_into().unwrap()))
    }
}

impl Drop for Cluster {
    fn drop(&mut self) {
        for id in 1..=MEMBERS {
            self.node(id).stop();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A fault the nemesis caused: when, what, and whether it hit the leader.
struct Fault {
    at: u64,
    what: String,
    leader: bool,
}

#[test]
fn register_stays_linearizable_under_faults() {
    let seed: u64 = std::env::var("FAULT_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64
                | 1
        });
    let seconds: u64 = std::env::var("FAULT_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);
    eprintln!("FAULT_SEED={seed}");
    let unsafe_reads = std::env::var("FAULT_UNSAFE_READS").is_ok_and(|v| v == "1");
    let cluster = Arc::new(Cluster::new());
    let start = Instant::now();
    let now = move || start.elapsed().as_nanos() as u64;
    let running = Arc::new(AtomicBool::new(true));
    let history = Arc::new(Mutex::new(Vec::<Op>::new()));

    let clients: Vec<_> = (1..=CLIENTS)
        .map(|client| {
            let (cluster, running, history) = (cluster.clone(), running.clone(), history.clone());
            std::thread::spawn(move || {
                let mut rng = Rng(seed ^ (client * 0x9E37_79B9_7F4A_7C15));
                let mut target = rng.between(1, MEMBERS);
                let unsafe_reads = unsafe_reads;
                let mut counter = 0u64;
                while running.load(Ordering::Relaxed) {
                    let node = cluster.node(target);
                    let invoke = now();
                    let outcome = if rng.below(2) == 0 {
                        counter += 1;
                        let value = (client << 32) | counter;
                        let written = node
                            .write(|tx| tx.put_state(NS, "register", "r", &value.to_le_bytes()));
                        match written {
                            Ok(()) => Some(Op {
                                kind: Kind::Write(value),
                                invoke,
                                complete: Some(now()),
                            }),
                            // Refused before it was proposed: it did not happen.
                            Err(Error::NotLeader(leader)) => {
                                target = leader.unwrap_or_else(|| rng.between(1, MEMBERS));
                                None
                            }
                            // It may or may not have happened.
                            Err(_) => Some(Op {
                                kind: Kind::Write(value),
                                invoke,
                                complete: None,
                            }),
                        }
                    } else {
                        // The control: reading what a member has applied,
                        // without asking the leader, is not linearizable.
                        let barrier = if unsafe_reads {
                            Ok(0)
                        } else {
                            node.read_barrier()
                        };
                        match barrier {
                            Ok(_) => {
                                let value = node
                                    .db()
                                    .begin_read()
                                    .unwrap()
                                    .state(NS, "register", "r")
                                    .unwrap()
                                    .map_or(0, |b| u64::from_le_bytes(b.try_into().unwrap()));
                                Some(Op {
                                    kind: Kind::Read(value),
                                    invoke,
                                    complete: Some(now()),
                                })
                            }
                            Err(Error::NotLeader(leader)) => {
                                target = leader.unwrap_or_else(|| rng.between(1, MEMBERS));
                                None
                            }
                            Err(_) => None,
                        }
                    };
                    if let Some(op) = outcome {
                        history.lock().unwrap().push(op);
                    }
                    // Now and then, a client tries another member.
                    if rng.below(10) == 0 {
                        target = rng.between(1, MEMBERS);
                    }
                }
            })
        })
        .collect();

    // The nemesis: one fault at a time, each healed before the next.
    let mut rng = Rng(seed);
    let mut faults: Vec<Fault> = Vec::new();
    let end = Duration::from_secs(seconds);
    std::thread::sleep(Duration::from_millis(500));
    while start.elapsed() < end {
        std::thread::sleep(Duration::from_millis(rng.between(150, 400)));
        let leader = cluster.leader();
        let at = now();
        let pause = Duration::from_millis(rng.between(300, 900));
        match rng.below(6) {
            0 => {
                let id = rng.between(1, MEMBERS);
                faults.push(Fault {
                    at,
                    what: format!("cut off {id}"),
                    leader: Some(id) == leader,
                });
                cluster.hub.isolate(id, true);
                std::thread::sleep(pause);
                cluster.hub.isolate(id, false);
            }
            1 if leader.is_some() => {
                let id = leader.unwrap();
                faults.push(Fault {
                    at,
                    what: format!("cut off leader {id}"),
                    leader: true,
                });
                cluster.hub.isolate(id, true);
                std::thread::sleep(pause);
                cluster.hub.isolate(id, false);
            }
            2 => {
                let id = rng.between(1, MEMBERS);
                faults.push(Fault {
                    at,
                    what: format!("restart {id}"),
                    leader: Some(id) == leader,
                });
                cluster.restart(id, pause / 3);
            }
            3 if leader.is_some() => {
                let id = leader.unwrap();
                faults.push(Fault {
                    at,
                    what: format!("restart leader {id}"),
                    leader: true,
                });
                cluster.restart(id, pause / 3);
            }
            4 => {
                let id = rng.between(1, MEMBERS);
                faults.push(Fault {
                    at,
                    what: format!("compact {id}"),
                    leader: false,
                });
                let _ = cluster.node(id).compact();
            }
            _ => {
                // A minority of two cut off from the rest and each other.
                let a = rng.between(1, MEMBERS);
                let b = a % MEMBERS + 1;
                faults.push(Fault {
                    at,
                    what: format!("cut off {a} and {b}"),
                    leader: leader == Some(a) || leader == Some(b),
                });
                cluster.hub.isolate(a, true);
                cluster.hub.isolate(b, true);
                std::thread::sleep(pause);
                cluster.hub.isolate(a, false);
                cluster.hub.isolate(b, false);
            }
        }
    }
    std::thread::sleep(Duration::from_millis(500));
    running.store(false, Ordering::Relaxed);
    for client in clients {
        client.join().unwrap();
    }
    let finished = now();
    let history = history.lock().unwrap().clone();

    // Every member ends with the register's last value.
    let mut settled = false;
    for _ in 0..500 {
        let statuses: Vec<_> = (1..=MEMBERS).map(|id| cluster.node(id).status()).collect();
        let commit = statuses.iter().map(|s| s.commit).max().unwrap_or(0);
        if statuses.iter().all(|s| s.applied >= commit) {
            settled = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(settled, "members did not catch up");
    let values: Vec<u64> = (1..=MEMBERS).map(|id| cluster.value(id)).collect();
    assert!(
        values.iter().all(|v| *v == values[0]),
        "members differ: {values:?}"
    );

    let writes = history
        .iter()
        .filter(|op| matches!(op.kind, Kind::Write(_)) && op.complete.is_some())
        .count();
    let reads = history
        .iter()
        .filter(|op| matches!(op.kind, Kind::Read(_)))
        .count();
    let unknown = history.iter().filter(|op| op.complete.is_none()).count();
    let checking = Instant::now();
    let ok = linearizable(&history);
    eprintln!(
        "{} operations: {writes} writes, {reads} linearizable reads, {unknown} writes with unknown outcome; \
         {} faults; checked in {:?}",
        history.len(),
        faults.len(),
        checking.elapsed()
    );

    // Failover: after a fault that hit the leader, how long until a write
    // sent after it succeeded.
    let completions: Vec<u64> = {
        let mut done: Vec<(u64, u64)> = history
            .iter()
            .filter(|op| matches!(op.kind, Kind::Write(_)))
            .filter_map(|op| op.complete.map(|c| (op.invoke, c)))
            .collect();
        done.sort_by_key(|&(_, c)| c);
        done.iter().map(|&(_, c)| c).collect()
    };
    let mut gaps: Vec<f64> = Vec::new();
    for fault in faults.iter().filter(|f| f.leader) {
        let first = history
            .iter()
            .filter(|op| matches!(op.kind, Kind::Write(_)) && op.invoke >= fault.at)
            .filter_map(|op| op.complete)
            .min();
        if let Some(first) = first {
            gaps.push((first - fault.at) as f64 / 1e6);
        }
    }
    gaps.sort_by(f64::total_cmp);
    // Availability: the share of 100 ms windows in which a write completed.
    let window = 100_000_000u64;
    let from = 500_000_000u64;
    let windows = (finished.saturating_sub(from) / window).max(1);
    let mut served = vec![false; windows as usize];
    for c in &completions {
        if *c >= from {
            let at = ((c - from) / window) as usize;
            if at < served.len() {
                served[at] = true;
            }
        }
    }
    let available = served.iter().filter(|s| **s).count() as f64 / windows as f64;
    if !gaps.is_empty() {
        eprintln!(
            "writes resumed after {} faults on the leader: median {:.0} ms, max {:.0} ms; \
             writes succeeded in {:.1}% of 100 ms windows",
            gaps.len(),
            gaps[gaps.len() / 2],
            gaps[gaps.len() - 1],
            available * 100.0
        );
    }
    for fault in &faults {
        eprintln!("  {:>6} ms  {}", fault.at / 1_000_000, fault.what);
    }
    assert!(writes > 0 && reads > 0, "the clients got nothing done");
    assert!(ok, "the history is not linearizable (FAULT_SEED={seed})");
}
