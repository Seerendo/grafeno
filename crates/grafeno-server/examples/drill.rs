//! A load for cluster drills (phase 13.6): clients write distinct values
//! to one register and read it with linearizable reads, on random members,
//! until a stop file appears (or for a number of seconds), while an
//! operator (or a script) runs a runbook: a rolling upgrade, a member lost,
//! a majority lost, a disk replaced. At the end the history is checked for
//! linearizability (`grafeno_cluster::check`), and the share of time
//! writes succeeded and the longest stretch without one are printed.
//!
//! ```text
//! cargo run --release -p grafeno-server --example drill -- \
//!   --urls http://127.0.0.1:17841,http://127.0.0.1:17842,http://127.0.0.1:17843 \
//!   --token t --stop drill.stop [--seconds 60] [--clients 3] [--ns drill]
//! ```
//!
//! Prints one JSON line; exits with 1 if the history is not linearizable.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use grafeno_cluster::check::{Kind, Op, linearizable};
use serde_json::{Value, json};

struct Args {
    urls: Vec<String>,
    token: String,
    stop: Option<PathBuf>,
    seconds: Option<u64>,
    clients: u64,
    ns: String,
}

fn args() -> Args {
    let mut args = Args {
        urls: Vec::new(),
        token: String::new(),
        stop: None,
        seconds: None,
        clients: 3,
        ns: "drill".into(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next().unwrap_or_default();
        match flag.as_str() {
            "--urls" => {
                args.urls = value
                    .split(',')
                    .map(|u| u.trim_end_matches('/').to_owned())
                    .collect()
            }
            "--token" => args.token = value,
            "--stop" => args.stop = Some(PathBuf::from(value)),
            "--seconds" => args.seconds = value.parse().ok(),
            "--clients" => args.clients = value.parse().unwrap_or(3),
            "--ns" => args.ns = value,
            other => panic!("unknown flag {other}"),
        }
    }
    assert!(!args.urls.is_empty(), "--urls is needed");
    args
}

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) % n
    }
}

fn query(
    agent: &ureq::Agent,
    url: &str,
    token: &str,
    ns: &str,
    body: Value,
    linearizable: bool,
) -> Option<(u16, Value)> {
    let mut request = agent
        .post(&format!("{url}/v1/{ns}/query"))
        .header("Authorization", &format!("Bearer {token}"));
    if linearizable {
        request = request.header("x-grafeno-consistency", "linearizable");
    }
    let mut response = request.send_json(body).ok()?;
    let status = response.status().as_u16();
    let text = response.body_mut().read_to_string().ok()?;
    Some((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
}

fn main() {
    let args = Arc::new(args());
    let start = Instant::now();
    let now = move || start.elapsed().as_nanos() as u64;
    let running = Arc::new(AtomicBool::new(true));
    let history = Arc::new(Mutex::new(Vec::<Op>::new()));
    let refused = Arc::new(AtomicU64::new(0));
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
        | 1;
    let clients: Vec<_> = (1..=args.clients)
        .map(|client| {
            let (args, running, history, refused) =
                (args.clone(), running.clone(), history.clone(), refused.clone());
            std::thread::spawn(move || {
                let agent: ureq::Agent = ureq::Agent::config_builder()
                    .http_status_as_error(false)
                    .timeout_global(Some(Duration::from_secs(5)))
                    .timeout_connect(Some(Duration::from_millis(300)))
                    .build()
                    .into();
                let mut rng = Rng(seed ^ (client * 0x9E37_79B9_7F4A_7C15));
                let mut counter = 0u64;
                while running.load(Ordering::Relaxed) {
                    let url = &args.urls[rng.below(args.urls.len() as u64) as usize];
                    let invoke = now();
                    let op = if rng.below(2) == 0 {
                        counter += 1;
                        let value = (client << 32) | counter;
                        match query(
                            &agent,
                            url,
                            &args.token,
                            &args.ns,
                            json!({ "query": "MERGE (r:Register {name: 'r'}) SET r.v = $v", "params": { "v": value } }),
                            false,
                        ) {
                            Some((200, _)) => Some(Op { kind: Kind::Write(value), invoke, complete: Some(now()) }),
                            // Refused before it was proposed: it did not happen.
                            Some((421, _)) => {
                                refused.fetch_add(1, Ordering::Relaxed);
                                None
                            }
                            Some((code, body)) if (400..500).contains(&code) => {
                                panic!("a write was refused: {code} {body}")
                            }
                            _ => Some(Op { kind: Kind::Write(value), invoke, complete: None }),
                        }
                    } else {
                        match query(
                            &agent,
                            url,
                            &args.token,
                            &args.ns,
                            json!({ "query": "MATCH (r:Register {name: 'r'}) RETURN r.v" }),
                            true,
                        ) {
                            Some((200, body)) => Some(Op {
                                kind: Kind::Read(body["rows"][0][0].as_u64().unwrap_or(0)),
                                invoke,
                                complete: Some(now()),
                            }),
                            _ => None,
                        }
                    };
                    match op {
                        Some(op) => history.lock().unwrap().push(op),
                        None => std::thread::sleep(Duration::from_millis(20)),
                    }
                }
            })
        })
        .collect();
    loop {
        std::thread::sleep(Duration::from_millis(200));
        if args.stop.as_ref().is_some_and(|stop| stop.exists()) {
            break;
        }
        if args
            .seconds
            .is_some_and(|seconds| start.elapsed() >= Duration::from_secs(seconds))
        {
            break;
        }
    }
    running.store(false, Ordering::Relaxed);
    for client in clients {
        client.join().unwrap();
    }
    let finished = now();
    let history = history.lock().unwrap().clone();
    let ok = linearizable(&history);
    let writes: Vec<u64> = history
        .iter()
        .filter(|op| matches!(op.kind, Kind::Write(_)))
        .filter_map(|op| op.complete)
        .collect();
    let reads = history
        .iter()
        .filter(|op| matches!(op.kind, Kind::Read(_)))
        .count();
    let unknown = history.iter().filter(|op| op.complete.is_none()).count();
    // Availability: the share of 100 ms windows in which a write succeeded,
    // and the longest stretch without one.
    let window = 100_000_000u64;
    let windows = (finished / window).max(1) as usize;
    let mut served = vec![false; windows];
    let mut sorted = writes.clone();
    sorted.sort_unstable();
    for at in &sorted {
        if let Some(slot) = served.get_mut((at / window) as usize) {
            *slot = true;
        }
    }
    let longest_gap_ms = sorted
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .chain(sorted.first().copied())
        .max()
        .unwrap_or(finished) as f64
        / 1e6;
    println!(
        "{}",
        json!({
            "seconds": finished as f64 / 1e9,
            "operations": history.len(),
            "writes": writes.len(),
            "reads": reads,
            "unknown_writes": unknown,
            "refused_writes": refused.load(Ordering::Relaxed),
            "available_pct": served.iter().filter(|s| **s).count() as f64 * 100.0 / windows as f64,
            "longest_gap_ms": longest_gap_ms,
            "linearizable": ok,
        })
    );
    if !ok {
        std::process::exit(1);
    }
}
