//! Phase 13.3: fault injection on real processes. Three `grafeno serve`
//! members; clients that know every member write distinct values to one
//! register (`MERGE … SET`) and read it with `x-grafeno-consistency:
//! linearizable`, on whichever member answers, while members' processes
//! are killed (the leader too) and started again. The history must be
//! linearizable (`grafeno_cluster::check`); prints how long writes stopped
//! after the leader was killed and the share of time writes succeeded.
//!
//! `FAULT_SEED` replays the nemesis; `FAULT_SECONDS` sets the length
//! (default 10).

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use grafeno::NodeId;
use grafeno_cluster::check::{Kind, Op, linearizable};
use serde_json::{Value, json};

const TOKEN: &str = "faults-test-token";
const CLIENTS: u64 = 3;

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
}

fn free_ports(count: usize) -> Vec<u16> {
    let listeners: Vec<TcpListener> = (0..count)
        .map(|_| TcpListener::bind("127.0.0.1:0").unwrap())
        .collect();
    listeners
        .iter()
        .map(|l| l.local_addr().unwrap().port())
        .collect()
}

/// Writes a member's settings file; returns its path.
fn configure(dir: &Path, id: u64, cluster: &[(u64, u16, u16)]) -> PathBuf {
    let peers: Vec<String> = cluster
        .iter()
        .map(|(id, port, api)| {
            format!("{{ id = {id}, addr = \"127.0.0.1:{port}\", api = \"127.0.0.1:{api}\" }}")
        })
        .collect();
    let (_, own, api) = *cluster.iter().find(|(peer, _, _)| *peer == id).unwrap();
    let config = format!(
        "db = \"{}\"\naddr = \"127.0.0.1:{api}\"\ntoken = \"{TOKEN}\"\nstudio = false\n\n\
         [cluster]\nid = {id}\naddr = \"127.0.0.1:{own}\"\nsecret = \"s3cret\"\nvoters = [1, 2, 3]\n\
         tick_ms = 20\npeers = [{}]\n",
        dir.join(format!("member-{id}.redb"))
            .to_string_lossy()
            .replace('\\', "/"),
        peers.join(", ")
    );
    let path = dir.join(format!("member-{id}.toml"));
    std::fs::write(&path, config).unwrap();
    path
}

fn spawn(dir: &Path, id: u64) -> Child {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("member-{id}.log")))
        .unwrap();
    Command::new(env!("CARGO_BIN_EXE_grafeno"))
        .args(["serve", "--config"])
        .arg(dir.join(format!("member-{id}.toml")))
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .unwrap()
}

struct Members {
    children: Vec<Mutex<Option<Child>>>,
}

impl Members {
    fn kill(&self, at: usize) {
        if let Some(mut child) = self.children[at].lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Members {
    fn drop(&mut self) {
        for at in 0..self.children.len() {
            self.kill(at);
        }
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(3)))
        // A dead member should fail fast (Windows takes 2 s to refuse).
        .timeout_connect(Some(Duration::from_millis(300)))
        .build()
        .into()
}

/// A query on a member: the status and body, or `None` without an answer.
fn query(
    agent: &ureq::Agent,
    api: u16,
    text: &str,
    params: Value,
    linearizable: bool,
) -> Option<(u16, Value)> {
    let mut request = agent
        .post(&format!("http://127.0.0.1:{api}/v1/c/query"))
        .header("Authorization", &format!("Bearer {TOKEN}"));
    if linearizable {
        request = request.header("x-grafeno-consistency", "linearizable");
    }
    let mut response = request
        .send_json(json!({ "query": text, "params": params }))
        .ok()?;
    let status = response.status().as_u16();
    let text = response.body_mut().read_to_string().ok()?;
    Some((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
}

fn role(agent: &ureq::Agent, api: u16) -> Option<String> {
    let mut response = agent
        .get(&format!("http://127.0.0.1:{api}/v1/cluster"))
        .header("Authorization", &format!("Bearer {TOKEN}"))
        .call()
        .ok()?;
    let body: Value = serde_json::from_str(&response.body_mut().read_to_string().ok()?).ok()?;
    body["role"].as_str().map(str::to_owned)
}

#[test]
fn register_stays_linearizable_when_processes_die() {
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
        .unwrap_or(10);
    eprintln!("FAULT_SEED={seed}");
    let dir = std::env::temp_dir().join(format!("grafeno-faults-e2e-{}", NodeId::new()));
    std::fs::create_dir_all(&dir).unwrap();
    let ports = free_ports(6);
    let cluster: Vec<(u64, u16, u16)> = (1..=3)
        .map(|id| (id, ports[id as usize - 1], ports[id as usize + 2]))
        .collect();
    let apis: Vec<u16> = cluster.iter().map(|(_, _, api)| *api).collect();
    for (id, _, _) in &cluster {
        configure(&dir, *id, &cluster);
    }
    let members = Arc::new(Members {
        children: (1..=3)
            .map(|id| Mutex::new(Some(spawn(&dir, id))))
            .collect(),
    });
    // Wait for a leader.
    let probe = agent();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !apis
        .iter()
        .any(|api| role(&probe, *api).as_deref() == Some("leader"))
    {
        assert!(Instant::now() < deadline, "no leader");
        std::thread::sleep(Duration::from_millis(100));
    }

    let start = Instant::now();
    let now = move || start.elapsed().as_nanos() as u64;
    let running = Arc::new(AtomicBool::new(true));
    let history = Arc::new(Mutex::new(Vec::<Op>::new()));
    let clients: Vec<_> = (1..=CLIENTS)
        .map(|client| {
            let (running, history, apis) = (running.clone(), history.clone(), apis.clone());
            std::thread::spawn(move || {
                let agent = agent();
                let mut rng = Rng(seed ^ (client * 0x9E37_79B9_7F4A_7C15));
                let mut at = rng.below(3) as usize;
                let mut counter = 0u64;
                while running.load(Ordering::Relaxed) {
                    let api = apis[at];
                    let invoke = now();
                    let op = if rng.below(2) == 0 {
                        counter += 1;
                        let value = (client << 32) | counter;
                        let answer = query(
                            &agent,
                            api,
                            "MERGE (r:Register {name: 'r'}) SET r.v = $v",
                            json!({ "v": value }),
                            false,
                        );
                        match answer {
                            Some((200, _)) => Some(Op {
                                kind: Kind::Write(value),
                                invoke,
                                complete: Some(now()),
                            }),
                            // Refused before it was proposed (no leader
                            // yet): it did not happen.
                            Some((421, _)) => {
                                at = (at + 1) % apis.len();
                                None
                            }
                            Some((code, body)) if (400..500).contains(&code) => {
                                panic!("write refused: {code} {body}")
                            }
                            // No answer, or the cluster was busy: it may or
                            // may not have happened.
                            other => {
                                if other.is_none() {
                                    at = (at + 1) % apis.len();
                                }
                                Some(Op {
                                    kind: Kind::Write(value),
                                    invoke,
                                    complete: None,
                                })
                            }
                        }
                    } else {
                        let answer = query(
                            &agent,
                            api,
                            "MATCH (r:Register {name: 'r'}) RETURN r.v",
                            json!({}),
                            true,
                        );
                        match answer {
                            Some((200, body)) => Some(Op {
                                kind: Kind::Read(body["rows"][0][0].as_u64().unwrap_or(0)),
                                invoke,
                                complete: Some(now()),
                            }),
                            None => {
                                at = (at + 1) % apis.len();
                                None
                            }
                            _ => None,
                        }
                    };
                    if let Some(op) = op {
                        history.lock().unwrap().push(op);
                    }
                }
            })
        })
        .collect();

    // The nemesis: kill a member's process (half the time, the leader's),
    // leave it down a while, start it again.
    let mut rng = Rng(seed);
    let mut kills: Vec<(u64, bool)> = Vec::new();
    std::thread::sleep(Duration::from_millis(1_000));
    while start.elapsed() < Duration::from_secs(seconds) {
        let leader = apis
            .iter()
            .position(|api| role(&probe, *api).as_deref() == Some("leader"));
        let target = match leader {
            Some(leader) if rng.below(2) == 0 => leader,
            _ => rng.below(3) as usize,
        };
        kills.push((now(), Some(target) == leader));
        members.kill(target);
        std::thread::sleep(Duration::from_millis(500 + rng.below(1_000)));
        *members.children[target].lock().unwrap() = Some(spawn(&dir, target as u64 + 1));
        std::thread::sleep(Duration::from_millis(1_000 + rng.below(1_000)));
    }
    running.store(false, Ordering::Relaxed);
    for client in clients {
        client.join().unwrap();
    }
    let finished = now();
    let history = history.lock().unwrap().clone();

    let writes = history
        .iter()
        .filter(|op| matches!(op.kind, Kind::Write(_)) && op.complete.is_some())
        .count();
    let reads = history
        .iter()
        .filter(|op| matches!(op.kind, Kind::Read(_)))
        .count();
    let unknown = history.iter().filter(|op| op.complete.is_none()).count();
    let ok = linearizable(&history);
    let mut gaps: Vec<f64> = kills
        .iter()
        .filter(|(_, leader)| *leader)
        .filter_map(|(at, _)| {
            history
                .iter()
                .filter(|op| matches!(op.kind, Kind::Write(_)) && op.invoke >= *at)
                .filter_map(|op| op.complete)
                .min()
                .map(|first| (first - at) as f64 / 1e6)
        })
        .collect();
    gaps.sort_by(f64::total_cmp);
    let window = 100_000_000u64;
    let windows = (finished / window).max(1) as usize;
    let mut served = vec![false; windows];
    for op in &history {
        if let (Kind::Write(_), Some(c)) = (op.kind, op.complete)
            && let Some(slot) = served.get_mut((c / window) as usize)
        {
            *slot = true;
        }
    }
    let available = served.iter().filter(|s| **s).count() as f64 / windows as f64;
    eprintln!(
        "{} operations: {writes} writes, {reads} linearizable reads, {unknown} writes with unknown outcome; \
         {} processes killed ({} leaders)",
        history.len(),
        kills.len(),
        kills.iter().filter(|(_, l)| *l).count()
    );
    if !gaps.is_empty() {
        eprintln!(
            "writes resumed after the leader was killed: median {:.0} ms, max {:.0} ms; \
             writes succeeded in {:.1}% of 100 ms windows",
            gaps[gaps.len() / 2],
            gaps[gaps.len() - 1],
            available * 100.0
        );
    }
    drop(members);
    assert!(writes > 0 && reads > 0, "the clients got nothing done");
    assert!(ok, "the history is not linearizable (FAULT_SEED={seed})");
    let _ = std::fs::remove_dir_all(&dir);
}
