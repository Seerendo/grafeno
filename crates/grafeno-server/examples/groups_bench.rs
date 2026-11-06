//! Write throughput with one, two and three Raft groups (phase 13.5):
//! three members on this machine, one namespace per group, clients writing
//! one node per request to every namespace at once through all members.
//! Then a namespace of `records` nodes moves between groups while clients
//! keep writing to it, and the move's pause is reported.
//!
//! `cargo build --release -p grafeno-server` first, then
//! `cargo run --release -p grafeno-server --example groups_bench [seconds] [clients per namespace] [records]`
//! (defaults 10, 8, 20000). `GRAFENO_BIN` points to another binary. Prints
//! CSV: groups, writes, writes per second, p50 and p99 ms.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use grafeno::NodeId;
use serde_json::{Value, json};

const TOKEN: &str = "bench-token";

fn free_ports(count: usize) -> Vec<u16> {
    let listeners: Vec<TcpListener> = (0..count)
        .map(|_| TcpListener::bind("127.0.0.1:0").unwrap())
        .collect();
    listeners
        .iter()
        .map(|l| l.local_addr().unwrap().port())
        .collect()
}

struct Members(Vec<Child>);

impl Drop for Members {
    fn drop(&mut self) {
        for child in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn binary() -> PathBuf {
    std::env::var_os("GRAFENO_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/release")
                .join(format!("grafeno{}", std::env::consts::EXE_SUFFIX))
        })
}

fn start(dir: &Path, groups: u64) -> (Members, Vec<u16>) {
    let ports = free_ports(6);
    let peers: Vec<String> = (0..3)
        .map(|i| {
            format!(
                "{{ id = {}, addr = \"127.0.0.1:{}\", api = \"127.0.0.1:{}\" }}",
                i + 1,
                ports[i],
                ports[3 + i]
            )
        })
        .collect();
    let more: Vec<String> = (1..groups)
        .map(|g| format!("{{ id = {g}, voters = [1, 2, 3] }}"))
        .collect();
    let mut children = Vec::new();
    for i in 0..3 {
        let config = format!(
            "db = \"{}\"\naddr = \"127.0.0.1:{}\"\ntoken = \"{TOKEN}\"\nstudio = false\n\n\
             [cluster]\nid = {}\naddr = \"127.0.0.1:{}\"\nsecret = \"s\"\nvoters = [1, 2, 3]\n\
             peers = [{}]\ngroups = [{}]\n",
            dir.join(format!("g{groups}-m{}.redb", i + 1))
                .to_string_lossy()
                .replace('\\', "/"),
            ports[3 + i],
            i + 1,
            ports[i],
            peers.join(", "),
            more.join(", ")
        );
        let path = dir.join(format!("g{groups}-m{}.toml", i + 1));
        std::fs::write(&path, config).unwrap();
        children.push(
            Command::new(binary())
                .args(["serve", "--config"])
                .arg(&path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
    }
    (Members(children), ports[3..].to_vec())
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(120)))
        .build()
        .into()
}

fn post(
    agent: &ureq::Agent,
    port: u16,
    path: &str,
    body: Value,
    key: Option<&str>,
) -> (u16, Value) {
    let mut request = agent
        .post(&format!("http://127.0.0.1:{port}{path}"))
        .header("Authorization", &format!("Bearer {TOKEN}"));
    if let Some(key) = key {
        request = request.header("Idempotency-Key", key);
    }
    match request.send_json(body) {
        Ok(mut response) => {
            let status = response.status().as_u16();
            let text = response.body_mut().read_to_string().unwrap_or_default();
            (status, serde_json::from_str(&text).unwrap_or(Value::Null))
        }
        Err(_) => (0, Value::Null),
    }
}

fn get(port: u16, path: &str) -> Option<Value> {
    let mut response = agent()
        .get(&format!("http://127.0.0.1:{port}{path}"))
        .header("Authorization", &format!("Bearer {TOKEN}"))
        .call()
        .ok()?;
    serde_json::from_str(&response.body_mut().read_to_string().ok()?).ok()
}

/// Clients writing to `namespaces` for `seconds`; the latency of every
/// acknowledged write.
fn load(apis: &[u16], namespaces: &[String], clients: usize, seconds: u64) -> Vec<f64> {
    let latencies = Arc::new(Mutex::new(Vec::new()));
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let threads: Vec<_> = namespaces
        .iter()
        .flat_map(|ns| (0..clients).map(move |c| (ns.clone(), c)))
        .enumerate()
        .map(|(at, (ns, c))| {
            let (latencies, api) = (latencies.clone(), apis[at % apis.len()]);
            std::thread::spawn(move || {
                let agent = agent();
                let mut mine = Vec::new();
                let mut i = 0;
                while Instant::now() < deadline {
                    i += 1;
                    let started = Instant::now();
                    let (code, _) = post(
                        &agent,
                        api,
                        &format!("/v1/{ns}/query"),
                        json!({ "query": "CREATE (:N {c: $c, i: $i})", "params": { "c": c, "i": i } }),
                        None,
                    );
                    if code == 200 {
                        mine.push(started.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                if std::env::var("DEBUG").is_ok() && !mine.is_empty() {
                    let mut sorted = mine.clone();
                    sorted.sort_by(f64::total_cmp);
                    eprintln!("{ns} via {api}: {} writes, median {:.1} ms", sorted.len(), sorted[sorted.len() / 2]);
                }
                latencies.lock().unwrap().extend(mine);
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let mut latencies = latencies.lock().unwrap().clone();
    latencies.sort_by(f64::total_cmp);
    latencies
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let seconds: u64 = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(10);
    let clients: usize = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(8);
    let records: usize = args.get(3).and_then(|a| a.parse().ok()).unwrap_or(20_000);
    let dir = std::env::temp_dir().join(format!("grafeno-groups-bench-{}", NodeId::new()));
    std::fs::create_dir_all(&dir).unwrap();
    println!("groups,writes,writes_per_s,p50_ms,p99_ms");
    // `CONFIGS=1,2,3` (the default) chooses the configurations.
    let configurations: Vec<u64> = std::env::var("CONFIGS")
        .ok()
        .map(|list| {
            list.split(',')
                .filter_map(|g| g.trim().parse().ok())
                .collect()
        })
        .unwrap_or_else(|| vec![1, 2, 3]);
    for groups in configurations {
        let (members, apis) = start(&dir, groups);
        let deadline = Instant::now() + Duration::from_secs(60);
        // Every group with a leader.
        loop {
            let ready = get(apis[0], "/v1/cluster/groups").is_some_and(|list| {
                let list = list.as_array().cloned().unwrap_or_default();
                list.len() as u64 == groups || (groups == 1 && list.is_empty())
            }) && (0..groups).all(|g| {
                apis.iter().any(|api| {
                    get(*api, "/v1/cluster/groups").is_some_and(|list| {
                        list.as_array().is_some_and(|l| {
                            l.iter()
                                .any(|x| x["id"] == g && x["status"]["role"] == "leader")
                        }) || (groups == 1
                            && get(*api, "/v1/cluster").is_some_and(|s| s["role"] == "leader"))
                    })
                })
            });
            if ready {
                break;
            }
            assert!(Instant::now() < deadline, "groups did not start");
            std::thread::sleep(Duration::from_millis(200));
        }
        // Settled, the same for every configuration.
        std::thread::sleep(Duration::from_secs(3));
        // Three namespaces, one per group (all in group 0 with one group).
        let namespaces: Vec<String> = (0..3).map(|i| format!("ns{i}")).collect();
        for (i, ns) in namespaces.iter().enumerate() {
            let group = i as u64 % groups;
            if group > 0 {
                let (code, body) = post(
                    &agent(),
                    apis[0],
                    "/v1/cluster/placement",
                    json!({ "namespace": ns, "group": group }),
                    None,
                );
                assert_eq!(code, 200, "{body}");
            }
        }
        load(&apis, &namespaces, 1, 2); // warm-up
        let term = || get(apis[0], "/v1/cluster").map(|s| s["term"].clone());
        let before = term();
        let latencies = load(&apis, &namespaces, clients, seconds);
        eprintln!("group 0 term {before:?} -> {:?}", term());
        let at =
            |q: f64| latencies[((latencies.len() as f64 * q) as usize).min(latencies.len() - 1)];
        println!(
            "{groups},{},{:.0},{:.1},{:.1}",
            latencies.len(),
            latencies.len() as f64 / seconds as f64,
            at(0.5),
            at(0.99)
        );
        if groups == 3 {
            // A namespace of `records` nodes moves while clients write to it.
            // In writes of 1 000: one huge entry outlasts an election
            // timeout of 20 ms ticks.
            for from in (0..records).step_by(1_000) {
                let mut tries = 0;
                loop {
                    let (code, body) = post(
                        &agent(),
                        apis[0],
                        "/v1/ns1/query",
                        json!({ "query": "UNWIND range($from, $to) AS k CREATE (:Big {k: k, text: 'record ' + toString(k)})",
                                "params": { "from": from + 1, "to": (from + 1_000).min(records) } }),
                        Some(&format!("load-{from}")),
                    );
                    if code == 200 {
                        break;
                    }
                    tries += 1;
                    assert!(tries < 20, "{body}");
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
            let running = Arc::new(AtomicBool::new(true));
            let writer = {
                let (running, api) = (running.clone(), apis[1]);
                std::thread::spawn(move || {
                    let agent = agent();
                    let (mut done, mut busy, mut i) = (0, 0, 0);
                    while running.load(Ordering::Relaxed) {
                        i += 1;
                        let key = format!("move-{i}");
                        loop {
                            let (code, _) = post(
                                &agent,
                                api,
                                "/v1/ns1/query",
                                json!({ "query": "CREATE (:Late {i: $i})", "params": { "i": i } }),
                                Some(&key),
                            );
                            if code == 200 {
                                done += 1;
                                break;
                            }
                            busy += 1;
                            std::thread::sleep(Duration::from_millis(10));
                        }
                    }
                    (done, busy)
                })
            };
            std::thread::sleep(Duration::from_millis(500));
            let (code, moved) = post(
                &agent(),
                apis[2],
                "/v1/cluster/placement",
                json!({ "namespace": "ns1", "group": 2 }),
                None,
            );
            assert_eq!(code, 200, "{moved}");
            std::thread::sleep(Duration::from_millis(500));
            running.store(false, Ordering::Relaxed);
            let (done, busy) = writer.join().unwrap();
            let (_, counted) = post(
                &agent(),
                apis[0],
                "/v1/ns1/query",
                json!({ "query": "MATCH (l:Late) RETURN count(l), count(DISTINCT l.i)" }),
                None,
            );
            eprintln!(
                "moved {} nodes: {moved}; {done} writes during it ({busy} busy answers retried), found {}",
                records, counted["rows"]
            );
        }
        drop(members);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
