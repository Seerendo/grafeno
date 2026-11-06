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
    /// Where it listens for other members.
    pub addr: String,
    /// Its API (`host:port`), where followers forward what only the leader
    /// can serve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
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
///   { id = 1, addr = "10.0.0.1:7811", api = "10.0.0.1:7810" },
///   { id = 2, addr = "10.0.0.2:7811", api = "10.0.0.2:7810" },
///   { id = 3, addr = "10.0.0.3:7811", api = "10.0.0.3:7810" },
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
        Error::NotLeader(Some(leader)) => {
            grafeno::Error::Busy(format!("{NOT_LEADER_MESSAGE}: write to member {leader}"))
        }
        Error::NotLeader(None) => {
            grafeno::Error::Busy(format!("{NOT_LEADER_MESSAGE}, and none is known yet"))
        }
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

// ------------------------------------------------------------- routing (13.3)

/// Marks a request one member forwarded to the leader (never forwarded
/// again).
pub const FORWARDED: &str = "x-grafeno-forwarded";
/// `linearizable` (through the leader, after confirming it still leads),
/// `bounded` (this member, unless it is far behind), or `local` (the
/// default: whatever this member has applied).
pub const CONSISTENCY: &str = "x-grafeno-consistency";
/// Read your writes: wait until this member has applied this position.
pub const AFTER: &str = "x-grafeno-after";
/// On every response from a member: the position it has applied (after a
/// write, at least the write's).
pub const POSITION: &str = "x-grafeno-position";
/// On a refusal because this member is not the leader.
pub const NOT_LEADER: &str = "x-grafeno-not-leader";
/// The start of the message of that refusal (see [`engine_error`]).
pub const NOT_LEADER_MESSAGE: &str = "this member is not the cluster's leader";
/// A `bounded` read is forwarded when this member is more entries behind.
const MAX_LAG: u64 = 1_000;
/// Bodies a member buffers to forward them.
const MAX_FORWARDED_BODY: usize = 64 * 1024 * 1024;

fn refuse(status: axum::http::StatusCode, code: &str, message: &str) -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        status,
        axum::Json(serde_json::json!({ "error": { "code": code, "message": message } })),
    )
        .into_response()
}

/// Sends a request on to the leader's API and returns its answer.
fn forward(
    api: &str,
    from: NodeId,
    method: &axum::http::Method,
    uri: &axum::http::Uri,
    headers: &HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let url = format!(
        "http://{api}{}",
        uri.path_and_query().map_or("/", |p| p.as_str())
    );
    let mut builder = ureq::http::Request::builder()
        .method(method.as_str())
        .uri(&url);
    // Only what the leader needs to serve it as the member would have: the
    // caller's credentials and the request's meaning, never transport
    // headers (lengths, encodings, connection) of the first hop.
    for (name, value) in headers {
        let name = name.as_str();
        let kept = matches!(
            name,
            "authorization" | "content-type" | "accept" | "idempotency-key" | "last-event-id"
        ) || name.starts_with("x-grafeno-");
        if kept {
            builder = builder.header(name, value.as_bytes());
        }
    }
    builder = builder.header(FORWARDED, from.to_string());
    let Ok(request) = builder.body(body.to_vec()) else {
        return refuse(
            axum::http::StatusCode::BAD_GATEWAY,
            "forward_failed",
            "cannot forward the request",
        );
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(60)))
        .build()
        .into();
    let mut answer = match agent.run(request) {
        Ok(answer) => answer,
        Err(error) => {
            return refuse(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "busy",
                &format!("the leader did not answer: {error}"),
            );
        }
    };
    let status = axum::http::StatusCode::from_u16(answer.status().as_u16())
        .unwrap_or(axum::http::StatusCode::BAD_GATEWAY);
    let mut response = axum::response::Response::builder().status(status);
    for (name, value) in answer.headers() {
        if name == "content-type" || name == "retry-after" || name.as_str().starts_with("x-grafeno")
        {
            response = response.header(name.as_str(), value.as_bytes());
        }
    }
    let body = answer.body_mut().read_to_vec().unwrap_or_default();
    response
        .body(axum::body::Body::from(body))
        .unwrap_or_else(|_| {
            refuse(
                axum::http::StatusCode::BAD_GATEWAY,
                "forward_failed",
                "bad answer",
            )
        })
}

/// The cluster's part of serving a request on a member: read-your-writes,
/// consistency levels, forwarding to the leader what only it can do, and
/// the member's position on every response.
pub async fn route(
    axum::extract::State(state): axum::extract::State<crate::http::AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::StatusCode;
    let Some(node) = state.writer.cluster().cloned() else {
        return next.run(request).await;
    };
    let headers = request.headers().clone();
    let forwarded = headers.contains_key(FORWARDED);
    let consistency = headers
        .get(CONSISTENCY)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("local")
        .to_ascii_lowercase();
    let after: Option<u64> = headers
        .get(AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok());
    if let Some(after) = after {
        let waiting = node.clone();
        let caught_up = tokio::task::spawn_blocking(move || {
            waiting
                .wait_for(Duration::from_secs(5), |s| s.applied >= after)
                .is_some()
        })
        .await
        .unwrap_or(false);
        if !caught_up {
            return refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "busy",
                &format!("this member has not applied position {after} yet"),
            );
        }
    }
    let (parts, body) = request.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_FORWARDED_BODY).await else {
        return refuse(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            "the body is too large",
        );
    };
    // Inside a nested router the URI lost its prefix (`/v1`): forward the
    // one the client sent.
    let uri = parts
        .extensions
        .get::<axum::extract::OriginalUri>()
        .map_or_else(|| parts.uri.clone(), |original| original.0.clone());
    let method = parts.method.clone();
    let leader_api = |node: &Node| node.status().leader.and_then(|id| state.writer.api_of(id));
    let forward_now = |api: String, bytes: Bytes| {
        let (method, uri, headers, id) = (method.clone(), uri.clone(), headers.clone(), node.id());
        async move {
            tokio::task::spawn_blocking(move || forward(&api, id, &method, &uri, &headers, bytes))
                .await
                .unwrap_or_else(|_| {
                    refuse(
                        StatusCode::BAD_GATEWAY,
                        "forward_failed",
                        "the forward failed",
                    )
                })
        }
    };
    let status = node.status();
    let leading = status.role == "leader";
    match consistency.as_str() {
        "linearizable" if leading => {
            let reading = node.clone();
            let confirmed = tokio::task::spawn_blocking(move || reading.read_barrier())
                .await
                .map(|outcome| outcome.is_ok())
                .unwrap_or(false);
            if !confirmed {
                return refuse(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "busy",
                    "the leader could not confirm its leadership: try again",
                );
            }
        }
        "linearizable" if !forwarded => match leader_api(&node) {
            Some(api) => return forward_now(api, bytes).await,
            None => {
                return refuse(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "busy",
                    "the cluster has no leader to read from",
                );
            }
        },
        "bounded" if !leading && !forwarded => {
            let behind =
                status.leader.is_none() || status.commit.saturating_sub(status.applied) > MAX_LAG;
            if behind && let Some(api) = leader_api(&node) {
                return forward_now(api, bytes).await;
            }
        }
        _ => {}
    }
    let request = axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes.clone()));
    let mut response = next.run(request).await;
    // A write refused on a follower goes to the leader.
    if response.status() == StatusCode::MISDIRECTED_REQUEST
        && response.headers().contains_key(NOT_LEADER)
        && !forwarded
        && let Some(api) = leader_api(&node)
    {
        return forward_now(api, bytes).await;
    }
    if let Ok(value) = axum::http::HeaderValue::from_str(&node.status().applied.to_string()) {
        response.headers_mut().insert(POSITION, value);
    }
    response
}
