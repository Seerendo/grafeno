//! Phase 13.4 end to end: three `grafeno serve` members. A read asked to
//! split (`x-grafeno-split: on`) runs in one part per member, every part
//! at the position of the member that splits it, and returns what the
//! same read returns unsplit, row order included; with a member down, its
//! part runs on the member that splits, and the answer is the same.

use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use grafeno::NodeId;
use serde_json::{Value, json};

const TOKEN: &str = "split-test-token";

fn free_ports(count: usize) -> Vec<u16> {
    let listeners: Vec<TcpListener> = (0..count)
        .map(|_| TcpListener::bind("127.0.0.1:0").unwrap())
        .collect();
    listeners
        .iter()
        .map(|l| l.local_addr().unwrap().port())
        .collect()
}

struct Member {
    api: u16,
    child: Option<Child>,
}

impl Drop for Member {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn start(dir: &Path, id: u64, cluster: &[(u64, u16, u16)]) -> Child {
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
    Command::new(env!("CARGO_BIN_EXE_grafeno"))
        .args(["serve", "--config"])
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(dir.join(format!("member-{id}.log"))).unwrap())
        .spawn()
        .unwrap()
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(60)))
        .build()
        .into()
}

fn query(port: u16, text: &str, split: &str) -> (u16, Value) {
    let mut response = agent()
        .post(&format!("http://127.0.0.1:{port}/v1/c/query"))
        .header("Authorization", &format!("Bearer {TOKEN}"))
        .header("x-grafeno-split", split)
        .send_json(json!({ "query": text }))
        .expect("an answer");
    let status = response.status().as_u16();
    let body = response.body_mut().read_to_string().unwrap();
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}

fn status(port: u16) -> Option<Value> {
    let mut response = agent()
        .get(&format!("http://127.0.0.1:{port}/v1/cluster"))
        .header("Authorization", &format!("Bearer {TOKEN}"))
        .call()
        .ok()?;
    serde_json::from_str(&response.body_mut().read_to_string().ok()?).ok()
}

const QUERIES: [&str; 8] = [
    "MATCH (p:Person) RETURN p.city, count(*), sum(p.age), avg(p.score), min(p.i), max(p.i)",
    "MATCH (p:Person)-[:works_at]->(c:Company) RETURN c.name, count(*), collect(p.i)[0..4] ORDER BY c.name",
    "MATCH (p:Person) WHERE p.age > 60 RETURN p.i, p.name ORDER BY p.score DESC, p.i LIMIT 25",
    "MATCH (p:Person) RETURN DISTINCT p.city ORDER BY p.city",
    "MATCH (p:Person) RETURN p.i SKIP 100 LIMIT 10",
    "MATCH (p:Person) WHERE p.i % 97 = 0 RETURN p",
    "MATCH (p:Person) OPTIONAL MATCH (p)-[:knows]->(q) RETURN count(q), count(DISTINCT p.city), stdev(p.age)",
    "MATCH (p:Person) WITH p, p.age / 10 AS decade UNWIND [1, 2] AS k RETURN decade, k, count(*) ORDER BY decade, k",
];

#[test]
fn split_reads_return_what_one_member_does() {
    let dir = std::env::temp_dir().join(format!("grafeno-split-e2e-{}", NodeId::new()));
    std::fs::create_dir_all(&dir).unwrap();
    let ports = free_ports(6);
    let cluster: Vec<(u64, u16, u16)> = (1..=3)
        .map(|id| (id, ports[id as usize - 1], ports[id as usize + 2]))
        .collect();
    let mut members: Vec<Member> = cluster
        .iter()
        .map(|(id, _, api)| Member {
            api: *api,
            child: Some(start(&dir, *id, &cluster)),
        })
        .collect();
    let deadline = Instant::now() + Duration::from_secs(30);
    let leader = loop {
        if let Some(at) = members
            .iter()
            .position(|m| status(m.api).is_some_and(|s| s["role"] == "leader"))
        {
            break at;
        }
        assert!(Instant::now() < deadline, "no leader");
        std::thread::sleep(Duration::from_millis(100));
    };
    // 3 000 people, 20 companies, some acquaintances.
    let (code, body) = query(
        members[leader].api,
        "UNWIND range(0, 19) AS c CREATE (:Company {name: 'company ' + toString(c), c: c})",
        "off",
    );
    assert_eq!(code, 200, "{body}");
    let (code, body) = query(
        members[leader].api,
        "UNWIND range(1, 3000) AS i \
         CREATE (p:Person {i: i, name: 'person ' + toString(i), age: i % 80, score: (i * 7919 % 1000) / 10.0, \
                 city: ['Lima', 'Quito', 'Bogotá', 'La Paz', 'Santiago'][i % 5]}) \
         WITH p, i MATCH (c:Company {c: i % 20}) CREATE (p)-[:works_at]->(c)",
        "off",
    );
    assert_eq!(code, 200, "{body}");
    let (code, body) = query(
        members[leader].api,
        "MATCH (a:Person), (b:Person) WHERE a.i % 50 = 0 AND b.i = a.i + 1 CREATE (a)-[:knows]->(b)",
        "off",
    );
    assert_eq!(code, 200, "{body}");
    // Every member has it all.
    let deadline = Instant::now() + Duration::from_secs(20);
    while members
        .iter()
        .any(|m| query(m.api, "MATCH (p:Person) RETURN count(p)", "off").1["rows"][0][0] != 3000)
    {
        assert!(Instant::now() < deadline, "members did not catch up");
        std::thread::sleep(Duration::from_millis(100));
    }

    let check = |port: u16, here: u64| {
        for text in QUERIES {
            let (code, whole) = query(port, text, "off");
            assert_eq!(code, 200, "{text}: {whole}");
            assert!(whole.get("split").is_none(), "{text}");
            let (code, split) = query(port, text, "on");
            assert_eq!(code, 200, "{text}: {split}");
            assert_eq!(split["split"]["parts"], 3, "{text} was not split");
            assert_eq!(
                split["split"]["here"], here,
                "{text}: parts run on the member asked"
            );
            assert_eq!(split["columns"], whole["columns"], "{text}");
            assert_eq!(split["rows"], whole["rows"], "{text}");
            assert!(!whole["rows"].as_array().unwrap().is_empty(), "{text}");
        }
    };
    // From a follower and from the leader.
    check(members[(leader + 1) % 3].api, 1);
    check(members[leader].api, 1);
    // Small reads do not split by default; writes never do.
    let (_, small) = query(
        members[leader].api,
        "MATCH (c:Company) RETURN count(*)",
        "auto",
    );
    assert!(small.get("split").is_none());

    // A member down: its part runs on the member that splits.
    let down = (leader + 2) % 3;
    if let Some(mut child) = members[down].child.take() {
        child.kill().unwrap();
        child.wait().unwrap();
    }
    check(members[(leader + 1) % 3].api, 2);
    drop(members);
    let _ = std::fs::remove_dir_all(&dir);
}
