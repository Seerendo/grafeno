//! Phase 13.2 end to end: three `grafeno serve` processes with `[cluster]`
//! settings on this machine. Writes go through the leader's API and reach
//! the others; a follower names the leader; when the leader's process is
//! killed, the others elect a new one and writes go on.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use grafeno::NodeId;
use serde_json::{Value, json};

const TOKEN: &str = "cluster-test-token";

/// Distinct free ports: all bound at once, then released.
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
    id: u64,
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

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .into()
}

fn call(port: u16, method: &str, path: &str, body: Option<Value>) -> Option<(u16, Value)> {
    let url = format!("http://127.0.0.1:{port}{path}");
    let auth = format!("Bearer {TOKEN}");
    let agent = agent();
    let response = if method == "GET" {
        agent.get(&url).header("Authorization", &auth).call()
    } else {
        agent
            .post(&url)
            .header("Authorization", &auth)
            .send_json(body.unwrap_or(json!({})))
    };
    let mut response = response.ok()?;
    let status = response.status().as_u16();
    let text = response.body_mut().read_to_string().ok()?;
    Some((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
}

fn status(member: &Member) -> Option<Value> {
    call(member.api, "GET", "/v1/cluster", None).map(|(_, body)| body)
}

fn leader(members: &[Member]) -> usize {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        for (at, member) in members.iter().enumerate() {
            if member.child.is_some() && status(member).is_some_and(|s| s["role"] == "leader") {
                return at;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let logs: Vec<String> = members
        .iter()
        .map(|m| format!("member {} ({}): {:?}", m.id, m.api, status(m)))
        .collect();
    panic!("no leader: {logs:#?}");
}

fn query(port: u16, text: &str) -> (u16, Value) {
    call(port, "POST", "/v1/c/query", Some(json!({ "query": text }))).expect("an answer")
}

fn count(port: u16) -> i64 {
    query(port, "MATCH (n:Note) RETURN count(n)").1["rows"][0][0]
        .as_i64()
        .unwrap_or(-1)
}

fn start(dir: &Path, id: u64, api: u16, cluster: &[(u64, u16)]) -> Child {
    let peers: Vec<String> = cluster
        .iter()
        .map(|(id, port)| format!("{{ id = {id}, addr = \"127.0.0.1:{port}\" }}"))
        .collect();
    let own = cluster.iter().find(|(peer, _)| *peer == id).unwrap().1;
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

#[test]
fn three_processes_replicate_and_fail_over() {
    let dir: PathBuf = std::env::temp_dir().join(format!("grafeno-cluster-e2e-{}", NodeId::new()));
    std::fs::create_dir_all(&dir).unwrap();
    let ports = free_ports(6);
    let cluster: Vec<(u64, u16)> = (1..=3).map(|id| (id, ports[id as usize - 1])).collect();
    let mut members: Vec<Member> = (1..=3)
        .map(|id| {
            let api = ports[id as usize + 2];
            Member {
                id,
                api,
                child: Some(start(&dir, id, api, &cluster)),
            }
        })
        .collect();

    let first = leader(&members);
    for i in 0..5 {
        let (code, body) = query(members[first].api, &format!("CREATE (:Note {{i: {i}}})"));
        assert_eq!(code, 200, "{body}");
    }
    // A follower refuses writes and names the leader.
    let follower = (first + 1) % 3;
    let (code, body) = query(members[follower].api, "CREATE (:Note {i: 99})");
    assert_eq!(code, 503, "{body}");
    assert!(
        body.to_string()
            .contains(&format!("member {}", members[first].id)),
        "{body}"
    );
    // Every member reads the same notes (followers apply shortly after).
    let deadline = Instant::now() + Duration::from_secs(10);
    while members.iter().any(|m| count(m.api) != 5) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    for member in &members {
        assert_eq!(count(member.api), 5, "member {}", member.id);
    }

    // The leader's process dies: the others go on.
    let killed = members[first].id;
    if let Some(mut child) = members[first].child.take() {
        // Dropping a Child does not end the process: kill it.
        child.kill().unwrap();
        child.wait().unwrap();
    }
    let second = leader(&members);
    assert_ne!(members[second].id, killed);
    let (code, body) = query(members[second].api, "CREATE (:Note {i: 5})");
    assert_eq!(code, 200, "{body}");
    let survivor = members
        .iter()
        .find(|m| m.child.is_some() && m.id != members[second].id)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while count(survivor.api) != 6 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(count(survivor.api), 6);
    assert_eq!(
        status(&members[second]).unwrap()["voters"],
        json!([1, 2, 3])
    );
    drop(members);
    let _ = std::fs::remove_dir_all(&dir);
}
