//! Running the server as a member of a cluster (phase 13.2): its `[cluster]`
//! settings, the transport between members over HTTP, and the member's own
//! listener.
//!
//! Members talk on a port of their own (`[cluster] addr`), apart from the
//! API: `POST /raft` carries one Raft message, framed as a little-endian
//! `u32` length, the message as JSON, then a snapshot's bytes if it has
//! one. Every request carries the cluster secret in `x-grafeno-cluster`;
//! anything else is refused. The traffic is not encrypted yet: members
//! should talk over a private network (TLS between members comes later).
//!
//! Sending never blocks the member's Raft thread: each peer has a thread
//! with a bounded queue, and a message that does not fit, or does not get
//! through, is dropped (Raft resends what matters).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use grafeno_cluster::{Node, NodeOptions, Storage, Transport};
use grafeno_raft::{Envelope, NodeId};
use serde::{Deserialize, Serialize};

const SECRET_HEADER: &str = "x-grafeno-cluster";

/// A member of the cluster and where it listens for other members.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Peer {
    pub id: NodeId,
    pub addr: String,
}

/// `[cluster]` in `grafeno.toml`.
///
/// ```toml
/// [cluster]
/// id = 1
/// addr = "10.0.0.1:7811"      # where this member listens for the others
/// secret = "…"                # the same on every member
/// voters = [1, 2, 3]          # the first members (empty when joining)
/// peers = [
///   { id = 1, addr = "10.0.0.1:7811" },
///   { id = 2, addr = "10.0.0.2:7811" },
///   { id = 3, addr = "10.0.0.3:7811" },
/// ]
/// tick_ms = 50
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterConfig {
    pub id: NodeId,
    pub addr: SocketAddr,
    pub secret: String,
    #[serde(default)]
    pub voters: Vec<NodeId>,
    #[serde(default)]
    pub peers: Vec<Peer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick_ms: Option<u64>,
}

/// A message for a peer, with a snapshot's bytes.
type Outgoing = (Envelope, Option<Arc<Vec<u8>>>);

/// Sends messages to peers over HTTP, a thread and a queue per peer.
pub struct HttpTransport {
    peers: HashMap<NodeId, SyncSender<Outgoing>>,
}

impl HttpTransport {
    pub fn new(config: &ClusterConfig) -> Self {
        let mut peers = HashMap::new();
        for peer in config.peers.iter().filter(|peer| peer.id != config.id) {
            let (sender, receiver) = sync_channel::<Outgoing>(1024);
            let url = format!("http://{}/raft", peer.addr);
            let secret = config.secret.clone();
            std::thread::Builder::new()
                .name(format!("grafeno-raft-send-{}", peer.id))
                .spawn(move || {
                    let agent: ureq::Agent = ureq::Agent::config_builder()
                        .timeout_global(Some(Duration::from_secs(5)))
                        .http_status_as_error(false)
                        .build()
                        .into();
                    while let Ok((envelope, snapshot)) = receiver.recv() {
                        let Ok(json) = serde_json::to_vec(&envelope) else {
                            continue;
                        };
                        let mut body = Vec::with_capacity(4 + json.len());
                        body.extend_from_slice(&(json.len() as u32).to_le_bytes());
                        body.extend_from_slice(&json);
                        if let Some(snapshot) = snapshot {
                            body.extend_from_slice(&snapshot);
                        }
                        let _ = agent
                            .post(&url)
                            .header(SECRET_HEADER, &secret)
                            .send(&body[..]);
                    }
                })
                .expect("a sender thread starts");
            peers.insert(peer.id, sender);
        }
        Self { peers }
    }
}

impl Transport for HttpTransport {
    fn send(&self, envelope: Envelope, snapshot: Option<Arc<Vec<u8>>>) {
        if let Some(sender) = self.peers.get(&envelope.to) {
            match sender.try_send((envelope, snapshot)) {
                Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
            }
        }
    }
}

/// Compares secrets in time that does not depend on where they differ.
fn same_secret(given: &[u8], expected: &[u8]) -> bool {
    given.len() == expected.len()
        && given
            .iter()
            .zip(expected)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

#[derive(Clone)]
struct Listener {
    node: Arc<Node>,
    secret: Arc<str>,
}

async fn receive(State(state): State<Listener>, headers: HeaderMap, body: Bytes) -> StatusCode {
    let given = headers
        .get(SECRET_HEADER)
        .map(|value| value.as_bytes())
        .unwrap_or_default();
    if !same_secret(given, state.secret.as_bytes()) {
        return StatusCode::UNAUTHORIZED;
    }
    let Some(length) = body.get(..4) else {
        return StatusCode::BAD_REQUEST;
    };
    let length = u32::from_le_bytes([length[0], length[1], length[2], length[3]]) as usize;
    let Some(json) = body.get(4..4 + length) else {
        return StatusCode::BAD_REQUEST;
    };
    let Ok(envelope) = serde_json::from_slice::<Envelope>(json) else {
        return StatusCode::BAD_REQUEST;
    };
    let rest = &body[4 + length..];
    let snapshot = (!rest.is_empty()).then(|| Arc::new(rest.to_vec()));
    state.node.deliver(envelope, snapshot);
    StatusCode::NO_CONTENT
}

async fn status(State(state): State<Listener>) -> axum::Json<grafeno_cluster::Status> {
    axum::Json(state.node.status())
}

/// Starts this member: its Raft state next to the database file (`.raft`),
/// snapshots in a folder beside it, the transport to its peers.
pub fn start(
    db: Arc<grafeno::Grafeno>,
    db_path: Option<&std::path::Path>,
    config: &ClusterConfig,
) -> Result<Arc<Node>, Box<dyn std::error::Error>> {
    let (storage, snapshots) = match db_path {
        Some(path) => {
            let mut raft = path.as_os_str().to_owned();
            raft.push(".raft");
            let mut snapshots = path.as_os_str().to_owned();
            snapshots.push(".snapshots");
            (
                Storage::open(&PathBuf::from(raft))?,
                PathBuf::from(snapshots),
            )
        }
        None => (
            Storage::in_memory()?,
            std::env::temp_dir().join(format!("grafeno-snapshots-{}", config.id)),
        ),
    };
    let mut options = NodeOptions::new(config.id, snapshots);
    if let Some(tick) = config.tick_ms {
        options.tick = Duration::from_millis(tick.max(1));
    }
    let transport = Arc::new(HttpTransport::new(config));
    Ok(Arc::new(Node::start(
        db,
        storage,
        transport,
        &config.voters,
        options,
    )?))
}

/// Serves other members' messages on `[cluster] addr` until the process
/// ends.
pub async fn listen(node: Arc<Node>, config: &ClusterConfig) -> std::io::Result<()> {
    let state = Listener {
        node,
        secret: Arc::from(config.secret.as_str()),
    };
    let app = Router::new()
        .route("/raft", post(receive).layer(DefaultBodyLimit::disable()))
        .route("/status", get(status))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(config.addr).await?;
    eprintln!(
        "grafeno cluster member {} listening on {}",
        config.id,
        listener.local_addr()?
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(())
}

/// A cluster error as an engine error, for the API's handlers: a member
/// that cannot take the write answers "busy", naming the leader.
pub fn engine_error(error: grafeno_cluster::Error) -> grafeno::Error {
    use grafeno_cluster::Error;
    match error {
        Error::NotLeader(Some(leader)) => grafeno::Error::Busy(format!(
            "this member is not the cluster's leader: write to member {leader}"
        )),
        Error::NotLeader(None) => grafeno::Error::Busy("the cluster has no leader yet".into()),
        Error::Lost => {
            grafeno::Error::Busy("the write was lost to a change of leader: try again".into())
        }
        Error::Timeout => {
            grafeno::Error::Busy("the cluster did not commit the write in time".into())
        }
        Error::ChangeInProgress => {
            grafeno::Error::Conflict("a membership change is under way".into())
        }
        Error::Stopped => grafeno::Error::Storage("the cluster member is stopped".into()),
        Error::Storage(message) => grafeno::Error::Storage(message),
        Error::Engine(error) => error,
    }
}
