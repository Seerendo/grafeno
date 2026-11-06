//! Reads split across cluster members (phase 13.4): the same queries on one
//! server and split across 3 and 5 members, every member limited to the
//! same number of scan threads, so that each stands for a machine of that
//! size (all run on this one machine: members × threads must fit its
//! cores). Every member starts from a copy of one database file.
//!
//! `cargo build --release -p grafeno-server` first, then
//! `cargo run --release -p grafeno-server --example split_bench [people] [threads] [runs]`
//! (defaults 200000, 3, 5). `MEMBERS=1,3,5` (the default) chooses the
//! cluster sizes; `GRAFENO_BIN` points to another binary. Prints
//! CSV: query, members, threads per member, median ms, speed-up.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use grafeno::{Grafeno, IndexDef, NewEdge, NewNode, NodeId};
use serde_json::{Value, json};

const NS: &str = "bench";
const TOKEN: &str = "bench-token";

const QUERIES: [(&str, &str); 4] = [
    (
        "scan",
        "MATCH (p:Person) WHERE p.name CONTAINS '77' RETURN p.uid, p.age",
    ),
    (
        "aggregate",
        "MATCH (p:Person) RETURN p.city, count(*), avg(p.age), sum(p.score), max(p.uid)",
    ),
    (
        "two hops",
        "MATCH (p:Person)-[:knows]->(q)-[:knows]->(r) WHERE r.age = 7 RETURN count(*)",
    ),
    (
        "top 20",
        "MATCH (p:Person)-[:works_at]->(c) WHERE p.age > 30 RETURN p.uid, c.name ORDER BY p.score DESC, p.uid LIMIT 20",
    ),
];

fn build(path: &Path, people: usize) {
    let db = Grafeno::open(path).unwrap();
    db.write(|tx| {
        tx.create_index(NS, IndexDef::new("Person", "uid"))
            .map(|_| ())
    })
    .unwrap();
    let companies = db
        .write(|tx| {
            (0..1_000)
                .map(|i| tx.create_node(NS, NewNode::new("Company").prop("name", format!("c{i}"))))
                .collect::<grafeno::Result<Vec<_>>>()
        })
        .unwrap();
    let cities = ["Lima", "Quito", "Bogotá", "La Paz", "Santiago", "Caracas"];
    let mut ids = Vec::with_capacity(people);
    for chunk in 0..people.div_ceil(5_000) {
        ids.extend(
            db.write(|tx| {
                let mut made = Vec::new();
                for uid in chunk * 5_000..(chunk * 5_000 + 5_000).min(people) {
                    let id = tx.create_node(
                        NS,
                        NewNode::new("Person")
                            .prop("uid", uid as i64)
                            .prop("name", format!("person {uid}"))
                            .prop("age", (uid % 90) as i64)
                            .prop("score", ((uid * 7919) % 10_000) as f64 / 100.0)
                            .prop("city", cities[uid % cities.len()]),
                    )?;
                    tx.create_edge(NS, NewEdge::new(id, "works_at", companies[uid % 1_000]))?;
                    made.push(id);
                }
                Ok(made)
            })
            .unwrap(),
        );
    }
    let mut state = 88172645463325252u64;
    let mut next = move |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % n as u64) as usize
    };
    for chunk in ids.chunks(5_000) {
        let pairs: Vec<(NodeId, NodeId)> = chunk
            .iter()
            .flat_map(|from| {
                (0..3)
                    .map(|_| (*from, ids[next(ids.len())]))
                    .collect::<Vec<_>>()
            })
            .collect();
        db.write(|tx| {
            for (from, to) in &pairs {
                tx.create_edge(NS, NewEdge::new(*from, "knows", *to))?;
            }
            Ok(())
        })
        .unwrap();
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

/// Starts `count` members on copies of `seed`; returns them and their API
/// ports.
fn start(dir: &Path, seed: &Path, count: u64, threads: usize) -> (Members, Vec<u16>) {
    let ports = free_ports(count as usize * 2);
    let peers: Vec<String> = (0..count)
        .map(|i| {
            format!(
                "{{ id = {}, addr = \"127.0.0.1:{}\", api = \"127.0.0.1:{}\" }}",
                i + 1,
                ports[i as usize],
                ports[(count + i) as usize]
            )
        })
        .collect();
    let voters: Vec<String> = (1..=count).map(|i| i.to_string()).collect();
    let mut children = Vec::new();
    for i in 0..count {
        let db = dir.join(format!("m{count}-{}.redb", i + 1));
        std::fs::copy(seed, &db).unwrap();
        let mut config = format!(
            "db = \"{}\"\naddr = \"127.0.0.1:{}\"\ntoken = \"{TOKEN}\"\nstudio = false\n\n\
             [query]\nscan_threads = {threads}\ntimeout_secs = 0\nmax_reads = 0\nmax_rows = 0\nmax_memory_mb = 0\n",
            db.to_string_lossy().replace('\\', "/"),
            ports[(count + i) as usize],
        );
        if count > 1 {
            config.push_str(&format!(
                "\n[cluster]\nid = {}\naddr = \"127.0.0.1:{}\"\nsecret = \"s\"\nvoters = [{}]\ntick_ms = 20\npeers = [{}]\n",
                i + 1,
                ports[i as usize],
                voters.join(", "),
                peers.join(", ")
            ));
        }
        let path = dir.join(format!("m{count}-{}.toml", i + 1));
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
    let apis = ports[count as usize..].to_vec();
    (Members(children), apis)
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(600)))
        .build()
        .into()
}

fn query(port: u16, text: &str, split: &str) -> Option<(f64, Value)> {
    let started = Instant::now();
    let mut response = agent()
        .post(&format!("http://127.0.0.1:{port}/v1/{NS}/query"))
        .header("Authorization", &format!("Bearer {TOKEN}"))
        .header("x-grafeno-split", split)
        .send_json(json!({ "query": text }))
        .ok()?;
    let body: Value = serde_json::from_str(&response.body_mut().read_to_string().ok()?).ok()?;
    Some((started.elapsed().as_secs_f64() * 1000.0, body))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let people: usize = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(200_000);
    let threads: usize = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(3);
    let runs: usize = args.get(3).and_then(|a| a.parse().ok()).unwrap_or(5);
    let dir = std::env::temp_dir().join(format!("grafeno-split-bench-{}", NodeId::new()));
    std::fs::create_dir_all(&dir).unwrap();
    let seed = dir.join("seed.redb");
    let started = Instant::now();
    build(&seed, people);
    eprintln!("built {people} people in {:.1?}", started.elapsed());

    let mut baseline: Vec<f64> = Vec::new();
    println!("query,members,threads_per_member,median_ms,speedup");
    // `MEMBERS=1` measures one server alone (with `threads` threads).
    let counts: Vec<u64> = std::env::var("MEMBERS")
        .ok()
        .map(|list| {
            list.split(',')
                .filter_map(|c| c.trim().parse().ok())
                .collect()
        })
        .unwrap_or_else(|| vec![1, 3, 5]);
    for count in counts {
        let (members, apis) = start(&dir, &seed, count, threads);
        // Up, and (on a cluster) with a leader.
        let deadline = Instant::now() + Duration::from_secs(60);
        while apis
            .iter()
            .any(|api| query(*api, "RETURN 1", "off").is_none())
        {
            assert!(Instant::now() < deadline, "members did not start");
            std::thread::sleep(Duration::from_millis(200));
        }
        std::thread::sleep(Duration::from_secs(2));
        let split = if count == 1 { "off" } else { "on" };
        let port = apis[0];
        for (at, (name, text)) in QUERIES.iter().enumerate() {
            // A first run warms the caches (and checks the answer).
            let (_, body) = query(port, text, split).expect("an answer");
            assert!(body.get("error").is_none(), "{name}: {body}");
            if count > 1 {
                assert_eq!(
                    body["split"]["parts"], count,
                    "{name} was not split: {body}"
                );
                assert_eq!(body["split"]["here"], 1, "{name}: a member did not answer");
                let (_, whole) = query(port, text, "off").unwrap();
                assert_eq!(whole["rows"], body["rows"], "{name}: split rows differ");
            }
            let mut times: Vec<f64> = (0..runs)
                .map(|_| query(port, text, split).unwrap().0)
                .collect();
            times.sort_by(f64::total_cmp);
            let median = times[times.len() / 2];
            if count == 1 {
                baseline.push(median);
            }
            println!(
                "{name},{count},{threads},{median:.1},{:.2}",
                baseline[at] / median
            );
        }
        drop(members);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
