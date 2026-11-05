//! The HTTP API: JSON over `/v1/{namespace}/...`, with a bearer token: the
//! server's own (the owner) or an API key with grants per namespace (see
//! [`crate::auth`]).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, FromRequest, FromRequestParts, Path, Query, Request, State};
use axum::http::request::Parts;
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use grafeno::{
    Direction, EdgeFilter, EdgeId, Error, Grafeno, Grant, NodeId, Principal, Props, Role,
    Timestamp, WriteTx,
};
use grafeno_memory::{context_for, recall};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};

use crate::api::{
    ContextInput, EdgeInput, ErrorKind, FilterInput, NodeInput, NodeOutput, NodeRef, RecallInput,
    TimeInput, error_kind, parse_id, resolve_ref,
};
use crate::auth::{self, Caller, Keyring, Need};

const BODY_LIMIT: usize = 32 * 1024 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Grafeno>,
    /// Commits concurrent writes in groups.
    pub writer: Arc<grafeno::Writer>,
    pub token: Option<Arc<str>>,
    /// API keys, loaded from the database.
    pub keys: Arc<Keyring>,
    /// Limits for each GQ query.
    pub limits: grafeno_query::Limits,
    /// Requests being served under `/v1`; beyond its permits the server
    /// answers 503 instead of piling up work.
    pub in_flight: Arc<tokio::sync::Semaphore>,
    /// Counters for `GET /v1/metrics`.
    pub metrics: Arc<crate::metrics::ServerMetrics>,
    /// Records per transaction of a bulk import.
    pub import_chunk: usize,
}

/// Requests served at once under `/v1` by default.
pub const MAX_REQUESTS: usize = 512;

/// Records per transaction of a bulk import, by default. Small enough that
/// an agent's write waits for one chunk at most (tens of milliseconds); at
/// 20 000 it waited most of a second (`examples/saturation.rs`).
pub const IMPORT_CHUNK: usize = 2_000;

impl AppState {
    /// The state for `db`, with its group-commit writer, the standard query
    /// limits and room for [`MAX_REQUESTS`] requests at once.
    pub fn new(db: Arc<Grafeno>, token: Option<&str>) -> Self {
        Self::with_writer(db, token, grafeno::WriterOptions::default())
    }

    /// [`AppState::new`] with a writer of these options.
    pub fn with_writer(
        db: Arc<Grafeno>,
        token: Option<&str>,
        options: grafeno::WriterOptions,
    ) -> Self {
        Self {
            writer: Arc::new(grafeno::Writer::new(db.clone(), options)),
            keys: Arc::new(Keyring::load(&db).unwrap_or_default()),
            db,
            token: token.map(Arc::from),
            limits: grafeno_query::Limits::standard(),
            in_flight: Arc::new(tokio::sync::Semaphore::new(MAX_REQUESTS)),
            metrics: Arc::default(),
            import_chunk: IMPORT_CHUNK,
        }
    }

    pub fn with_import_chunk(mut self, chunk: usize) -> Self {
        self.import_chunk = chunk.max(1);
        self
    }

    pub fn with_limits(mut self, limits: grafeno_query::Limits) -> Self {
        self.limits = limits;
        self
    }

    pub fn with_max_requests(mut self, max: usize) -> Self {
        self.in_flight = Arc::new(tokio::sync::Semaphore::new(max.max(1)));
        self
    }
}

/// What the HTTP server offers besides the API.
#[derive(Clone, Debug)]
pub struct Options {
    /// Serve Grafeno Studio at `/studio/` (when the binary includes it).
    pub studio: bool,
    /// Origins allowed to call the API from a browser (CORS), such as a
    /// Studio hosted elsewhere; `*` allows any origin.
    pub cors_origins: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            studio: true,
            cors_origins: Vec::new(),
        }
    }
}

/// Serves until Ctrl+C (or SIGTERM on Unix, as sent by systemd).
pub async fn serve(state: AppState, addr: SocketAddr, options: Options) -> std::io::Result<()> {
    serve_until(state, addr, options, shutdown_signal()).await
}

/// Serves until `shutdown` completes, then finishes the requests in flight.
pub async fn serve_until(
    state: AppState,
    addr: SocketAddr,
    options: Options,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    eprintln!("grafeno listening on http://{local}");
    if options.studio && crate::studio::INCLUDED {
        eprintln!("Grafeno Studio at http://{local}/studio/");
    }
    axum::serve(listener, router_with(state, &options))
        .with_graceful_shutdown(shutdown)
        .await
}

/// Ctrl+C, or SIGTERM on Unix (what systemd sends to stop a service).
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

pub fn router(state: AppState) -> Router {
    router_with(state, &Options::default())
}

pub fn router_with(state: AppState, options: &Options) -> Router {
    let v1 = Router::new()
        .route("/namespaces", get(namespaces).post(create_namespace))
        .route(
            "/namespaces/{ns}",
            get(get_namespace)
                .patch(describe_namespace)
                .delete(drop_namespace),
        )
        .route("/export", get(export))
        .route("/backup", get(backup))
        .route("/import", post(import))
        .route("/{ns}/stats", get(stats))
        .route("/{ns}/check", get(check))
        .route("/{ns}/rebuild", post(rebuild))
        .route("/{ns}/compact", post(compact))
        .route("/{ns}/vectors/recall", get(vector_recall))
        .route("/{ns}/nodes", get(list_nodes).post(create_node))
        .route(
            "/{ns}/nodes/{id}",
            get(get_node)
                .put(put_node)
                .patch(patch_node)
                .delete(delete_node),
        )
        .route("/{ns}/nodes/{id}/neighbors", get(neighbors))
        .route("/{ns}/edges", get(list_edges).post(create_edge))
        .route("/{ns}/edges/{id}", get(get_edge).delete(delete_edge))
        .route("/{ns}/edges/{id}/invalidate", post(invalidate_edge))
        .route("/{ns}/edges/{id}/retract", post(retract_edge))
        .route("/{ns}/k-hop", post(k_hop))
        .route("/{ns}/path", post(path))
        .route("/{ns}/subgraph", post(subgraph))
        .route("/{ns}/search/text", post(search_text))
        .route("/{ns}/search/vector", post(search_vector))
        .route("/{ns}/recall", post(recall_handler))
        .route("/{ns}/context", post(context_handler))
        .route("/{ns}/batch", post(batch))
        .route("/{ns}/query", post(query))
        .route("/algorithms", get(algorithms))
        .route("/whoami", get(whoami))
        .route("/keys", get(list_keys).post(create_key))
        .route("/keys/{name}", axum::routing::delete(revoke_key))
        .route("/{ns}/algo/{name}", post(algorithm))
        .route("/{ns}/resolve", get(resolve))
        .route("/{ns}/duplicates", get(duplicates))
        .route("/{ns}/fuzzy", get(fuzzy))
        .route("/{ns}/match", post(find_matches))
        .route("/{ns}/matches", get(match_pairs))
        .route("/{ns}/match-rules", get(match_rules))
        .route(
            "/{ns}/match-rules/{label}",
            axum::routing::put(set_match_rule).delete(delete_match_rule),
        )
        .route("/{ns}/merge", post(merge))
        .route("/{ns}/merges", get(merges))
        .route("/{ns}/merges/{id}/undo", post(undo_merge))
        .route("/{ns}/touch", post(touch))
        .route("/{ns}/weak", get(weak))
        .route("/{ns}/archive", post(archive_nodes))
        .route("/{ns}/consolidation", get(consolidation))
        .route("/{ns}/summaries", post(summaries))
        .route("/metrics", get(metrics))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth))
        .route_layer(middleware::from_fn_with_state(state.clone(), admit))
        .route_layer(middleware::from_fn_with_state(state.clone(), observe));
    let mut app = Router::new().route("/health", get(health)).nest("/v1", v1);
    if options.studio {
        app = app
            .route("/", get(crate::studio::redirect))
            .route("/studio", get(crate::studio::redirect))
            .route("/studio/", get(crate::studio::index))
            .route("/studio/{*path}", get(crate::studio::asset));
    }
    // A panic while serving a request (a damaged page, a bug) answers 500
    // instead of dropping the connection.
    let mut app = app.layer(DefaultBodyLimit::max(BODY_LIMIT)).layer(
        tower_http::catch_panic::CatchPanicLayer::custom(panic_response),
    );
    if !options.cors_origins.is_empty() {
        app = app.layer(cors(&options.cors_origins));
    }
    app.with_state(state)
}

fn panic_response(panic: Box<dyn std::any::Any + Send + 'static>) -> Response {
    let detail = panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_default();
    ApiError::internal(format!(
        "the server failed while serving this request ({detail}); if the database file may be damaged, stop the server and run `grafeno check`"
    ))
    .into_response()
}

fn cors(origins: &[String]) -> tower_http::cors::CorsLayer {
    use axum::http::{HeaderValue, Method};
    use tower_http::cors::{AllowOrigin, CorsLayer};

    let origin = if origins.iter().any(|origin| origin == "*") {
        AllowOrigin::any()
    } else {
        AllowOrigin::list(
            origins
                .iter()
                .filter_map(|origin| HeaderValue::from_str(origin.trim_end_matches('/')).ok()),
        )
    };
    CorsLayer::new()
        .allow_origin(origin)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
}

// ---------------------------------------------------------------- plumbing

pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    /// More fields of the error object (the conflicts of a refused merge).
    details: Option<serde_json::Map<String, Value>>,
}

impl ApiError {
    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: message.into(),
            details: None,
        }
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "forbidden",
            message: message.into(),
            details: None,
        }
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "bad_request",
            message: message.into(),
            details: None,
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal",
            message: message.into(),
            details: None,
        }
    }
}

impl From<Error> for ApiError {
    fn from(error: Error) -> Self {
        let (status, code) = match error_kind(&error) {
            ErrorKind::BadRequest => (StatusCode::BAD_REQUEST, "invalid"),
            ErrorKind::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            ErrorKind::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
            ErrorKind::Busy => (StatusCode::SERVICE_UNAVAILABLE, "busy"),
            ErrorKind::Conflict => (StatusCode::CONFLICT, "conflict"),
            ErrorKind::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
        };
        Self {
            status,
            code,
            message: error.to_string(),
            details: None,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut error = serde_json::Map::new();
        error.insert("code".into(), Value::from(self.code));
        error.insert("message".into(), Value::from(self.message));
        error.extend(self.details.unwrap_or_default());
        let body = json!({ "error": error });
        let mut response = (self.status, Json(body)).into_response();
        if self.status == StatusCode::SERVICE_UNAVAILABLE {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, header::HeaderValue::from_static("1"));
        }
        response
    }
}

type ApiResult<T> = Result<T, ApiError>;

/// Runs a write through the group-commit writer. `work` may run more than
/// once (when another write of its group fails), so it takes its inputs by
/// reference or clones them.
async fn write<T: Send + 'static>(
    state: &AppState,
    work: impl Fn(&mut WriteTx) -> grafeno::Result<T> + Send + 'static,
) -> ApiResult<T> {
    let writer = state.writer.clone();
    // Writes of a group share a transaction: each one acts as its caller.
    let actor = auth::current().actor();
    let work = move |tx: &mut WriteTx| {
        tx.act_as(actor.clone());
        let result = work(tx);
        tx.act_as(None);
        result
    };
    tokio::task::spawn_blocking(move || writer.write(work))
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
        .map_err(ApiError::from)
}

/// Runs a long write (a rebuild, a compaction) in the writer's bulk lane:
/// in a transaction of its own, after the interactive writes waiting.
async fn write_bulk<T: Send + 'static>(
    state: &AppState,
    work: impl Fn(&mut WriteTx) -> grafeno::Result<T> + Send + 'static,
) -> ApiResult<T> {
    let writer = state.writer.clone();
    let actor = auth::current().actor();
    let work = move |tx: &mut WriteTx| {
        tx.act_as(actor.clone());
        let result = work(tx);
        tx.act_as(None);
        result
    };
    tokio::task::spawn_blocking(move || {
        let value = writer.write_bulk(work)?;
        writer.flush()?;
        Ok::<T, grafeno::Error>(value)
    })
    .await
    .map_err(|error| ApiError::internal(error.to_string()))?
    .map_err(ApiError::from)
}

/// Runs engine work on the blocking pool: redb calls are synchronous.
async fn blocking<T: Send + 'static>(
    state: &AppState,
    work: impl FnOnce(&Grafeno) -> grafeno::Result<T> + Send + 'static,
) -> ApiResult<T> {
    let db = state.db.clone();
    tokio::task::spawn_blocking(move || work(&db))
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
        .map_err(ApiError::from)
}

/// A JSON body whose errors are reported as JSON.
pub struct Body<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for Body<T> {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(request, state)
            .await
            .map(|Json(value)| Body(value))
            .map_err(|rejection| ApiError::bad_request(rejection.body_text()))
    }
}

/// Query parameters whose errors are reported as JSON.
pub struct Params<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequestParts<S> for Params<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(value)| Params(value))
            .map_err(|rejection| ApiError::bad_request(rejection.body_text()))
    }
}

/// Finds the caller (the owner token or an API key), checks what the route
/// needs, and serves the request as that caller.
async fn auth(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let given = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_owned);
    let open = state.token.is_none() && state.keys.is_empty();
    let caller = match given {
        _ if open => Some(Caller::Owner),
        Some(given)
            if state
                .token
                .as_ref()
                .is_some_and(|token| constant_time_eq(given.as_bytes(), token.as_bytes())) =>
        {
            Some(Caller::Owner)
        }
        Some(given) => state.keys.find(&given).map(Caller::Key),
        None => None,
    };
    let Some(caller) = caller else {
        return refuse(
            request,
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or wrong bearer token or API key".into(),
        )
        .await;
    };
    let allowed = match auth::need(request.method().as_str(), request.uri().path()) {
        Need::Caller => Ok(()),
        Need::Owner if caller.is_owner() => Ok(()),
        Need::Owner => Err(format!("the key {:?} is not an owner", caller.name())),
        Need::Namespace(ns, role) if caller.may(&ns, role) => Ok(()),
        Need::Namespace(ns, role) => Err(caller.refusal(&ns, role)),
    };
    if let Err(message) = allowed {
        return refuse(request, StatusCode::FORBIDDEN, "forbidden", message).await;
    }
    auth::CALLER.scope(caller, next.run(request)).await
}

async fn refuse(
    request: Request,
    status: StatusCode,
    code: &'static str,
    message: String,
) -> Response {
    // Read (a bounded part of) the body first: closing the connection while
    // the client still sends it makes some clients see a reset instead of
    // this response.
    let _ = axum::body::to_bytes(request.into_body(), 1024 * 1024).await;
    ApiError {
        status,
        code,
        message,
        details: None,
    }
    .into_response()
}

/// 403 unless the caller of this request has `role` on `ns`.
fn require(ns: &str, role: Role) -> ApiResult<()> {
    let caller = auth::current();
    if caller.may(ns, role) {
        Ok(())
    } else {
        Err(ApiError::forbidden(caller.refusal(ns, role)))
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Absent → `None`, `null` → `Some(None)`, a value → `Some(Some(value))`.
fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// A time in a query string: microseconds or ISO 8601.
fn parse_time_param(text: &str) -> grafeno::Result<Timestamp> {
    match text.parse::<i64>() {
        Ok(micros) => Ok(Timestamp::from_micros(micros)),
        Err(_) => Timestamp::parse(text),
    }
}

fn parse_edge_id(text: &str) -> grafeno::Result<EdgeId> {
    parse_id(text).map(|id| EdgeId::from_u128(id.as_u128()))
}

// ---------------------------------------------------------------- handlers

/// Turns requests away with 503 (and `Retry-After`) when too many are in
/// flight, instead of queueing without bound.
/// Counts every request under `/v1` and how long it took.
async fn observe(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let started = std::time::Instant::now();
    let response = next.run(request).await;
    state
        .metrics
        .request(response.status().as_u16(), started.elapsed().as_secs_f64());
    response
}

/// The server's and the writer's counters, in the Prometheus text format.
async fn metrics(State(state): State<AppState>) -> Response {
    let text = state
        .metrics
        .render(&state.writer.metrics(), state.db.projection_cache_bytes());
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        text,
    )
        .into_response()
}

async fn admit(State(state): State<AppState>, request: Request, next: Next) -> Response {
    match state.in_flight.clone().try_acquire_owned() {
        Ok(_permit) => next.run(request).await,
        Err(_) => {
            state.metrics.busy();
            busy_response()
        }
    }
}

fn busy_response() -> Response {
    ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "busy",
        message: "the server is busy: retry in a moment".into(),
        details: None,
    }
    .into_response()
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

#[derive(Serialize)]
struct Upserted {
    id: NodeId,
    created: bool,
}

async fn create_node(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Body(input): Body<NodeInput>,
) -> ApiResult<(StatusCode, Json<Upserted>)> {
    let result = write(&state, move |tx| match input.id {
        Some(id) => {
            let created = tx.upsert_node(&ns, id, input.clone().into_new())?;
            Ok(Upserted { id, created })
        }
        None => Ok(Upserted {
            id: tx.create_node(&ns, input.clone().into_new())?,
            created: true,
        }),
    })
    .await?;
    let status = if result.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(result)))
}

async fn put_node(
    State(state): State<AppState>,
    Path((ns, id)): Path<(String, String)>,
    Body(input): Body<NodeInput>,
) -> ApiResult<(StatusCode, Json<Upserted>)> {
    let id = parse_id(&id)?;
    if input.id.is_some_and(|body_id| body_id != id) {
        return Err(ApiError::bad_request(
            "the id in the body does not match the path",
        ));
    }
    let created = write(&state, move |tx| {
        tx.upsert_node(&ns, id, input.clone().into_new())
    })
    .await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(Upserted { id, created })))
}

const DEFAULT_PAGE: usize = 100;
const MAX_PAGE: usize = 1_000;

#[derive(Deserialize)]
struct ListParams {
    label: Option<String>,
    /// The `next` value of the previous page.
    after: Option<String>,
    limit: Option<usize>,
}

impl ListParams {
    fn limit(&self) -> usize {
        self.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE)
    }
}

/// A page of a listing. `next` is set when there may be more: pass it as
/// `after` to get them.
#[derive(Serialize)]
struct Page<T> {
    items: Vec<T>,
    next: Option<String>,
}

impl<T> Page<T> {
    fn new(items: Vec<T>, limit: usize, id: impl Fn(&T) -> String) -> Self {
        let next = (items.len() == limit)
            .then(|| items.last().map(id))
            .flatten();
        Self { items, next }
    }
}

async fn list_nodes(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Params(params): Params<ListParams>,
) -> ApiResult<Json<Page<grafeno::Node>>> {
    let limit = params.limit();
    let after = params.after.as_deref().map(parse_id).transpose()?;
    let nodes = blocking(&state, move |db| {
        db.begin_read()?
            .nodes_page(&ns, params.label.as_deref(), after, limit)
    })
    .await?;
    Ok(Json(Page::new(nodes, limit, |node| node.id.to_string())))
}

async fn list_edges(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Params(params): Params<ListParams>,
) -> ApiResult<Json<Page<grafeno::Edge>>> {
    let limit = params.limit();
    let after = params.after.as_deref().map(parse_edge_id).transpose()?;
    let edges = blocking(&state, move |db| {
        db.begin_read()?
            .edges_page(&ns, params.label.as_deref(), after, limit)
    })
    .await?;
    Ok(Json(Page::new(edges, limit, |edge| edge.id.to_string())))
}

async fn get_node(
    State(state): State<AppState>,
    Path((ns, id)): Path<(String, String)>,
) -> ApiResult<Json<NodeOutput>> {
    let id = parse_id(&id)?;
    let node = blocking(&state, move |db| {
        let read = db.begin_read()?;
        let node = read.node(&ns, id)?.ok_or(Error::NodeNotFound(id))?;
        NodeOutput::load(&read, &ns, node)
    })
    .await?;
    Ok(Json(node))
}

/// A partial update. `text` and `embedding` set to `null` remove them.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchNode {
    /// Merged into the current properties; `null` values remove properties.
    #[serde(default)]
    props: Option<Props>,
    #[serde(default)]
    add_labels: Vec<String>,
    #[serde(default)]
    remove_labels: Vec<String>,
    #[serde(default, deserialize_with = "double_option")]
    text: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    embedding: Option<Option<Vec<f32>>>,
    /// From 0 to 1.
    #[serde(default)]
    importance: Option<f32>,
}

fn apply_patch(tx: &mut WriteTx, ns: &str, id: NodeId, patch: PatchNode) -> grafeno::Result<()> {
    tx.node(ns, id)?.ok_or(Error::NodeNotFound(id))?;
    if let Some(importance) = patch.importance {
        tx.set_importance(ns, id, importance)?;
    }
    if let Some(props) = patch.props {
        tx.set_node_props(ns, id, props)?;
    }
    for label in &patch.add_labels {
        tx.add_label(ns, id, label)?;
    }
    for label in &patch.remove_labels {
        tx.remove_label(ns, id, label)?;
    }
    match patch.text {
        Some(Some(text)) => tx.set_text(ns, id, &text)?,
        Some(None) => {
            tx.remove_text(ns, id)?;
        }
        None => {}
    }
    match patch.embedding {
        Some(Some(embedding)) => tx.set_embedding(ns, id, &embedding)?,
        Some(None) => {
            tx.remove_embedding(ns, id)?;
        }
        None => {}
    }
    Ok(())
}

async fn patch_node(
    State(state): State<AppState>,
    Path((ns, id)): Path<(String, String)>,
    Body(patch): Body<PatchNode>,
) -> ApiResult<Json<NodeOutput>> {
    let id = parse_id(&id)?;
    let patched = ns.clone();
    write(&state, move |tx| {
        apply_patch(tx, &patched, id, patch.clone())
    })
    .await?;
    let node = blocking(&state, move |db| {
        let read = db.begin_read()?;
        let node = read.node(&ns, id)?.ok_or(Error::NodeNotFound(id))?;
        NodeOutput::load(&read, &ns, node)
    })
    .await?;
    Ok(Json(node))
}

async fn delete_node(
    State(state): State<AppState>,
    Path((ns, id)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let id = parse_id(&id)?;
    write(&state, move |tx| tx.delete_node(&ns, id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct NeighborParams {
    direction: Option<Direction>,
    /// Comma-separated labels.
    label: Option<String>,
    valid_at: Option<String>,
    known_at: Option<String>,
}

async fn neighbors(
    State(state): State<AppState>,
    Path((ns, id)): Path<(String, String)>,
    Params(params): Params<NeighborParams>,
) -> ApiResult<Json<Vec<grafeno::Neighbor>>> {
    let id = parse_id(&id)?;
    let mut filter = EdgeFilter::any();
    for label in params.label.iter().flat_map(|labels| labels.split(',')) {
        if !label.is_empty() {
            filter = filter.label(label);
        }
    }
    if let Some(at) = &params.valid_at {
        filter = filter.valid_at(parse_time_param(at)?);
    }
    if let Some(at) = &params.known_at {
        filter = filter.known_at(parse_time_param(at)?);
    }
    let direction = params.direction.unwrap_or_default();
    let neighbors = blocking(&state, move |db| {
        db.begin_read()?.neighbors(&ns, id, direction, &filter)
    })
    .await?;
    Ok(Json(neighbors))
}

async fn create_edge(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Body(input): Body<EdgeInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let new = input.into_new(&HashMap::new())?;
    let id = write(&state, move |tx| tx.create_edge(&ns, new.clone())).await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

async fn get_edge(
    State(state): State<AppState>,
    Path((ns, id)): Path<(String, String)>,
) -> ApiResult<Json<grafeno::Edge>> {
    let id = parse_edge_id(&id)?;
    let edge = blocking(&state, move |db| {
        db.begin_read()?
            .edge(&ns, id)?
            .ok_or(Error::EdgeNotFound(id))
    })
    .await?;
    Ok(Json(edge))
}

async fn delete_edge(
    State(state): State<AppState>,
    Path((ns, id)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let id = parse_edge_id(&id)?;
    write(&state, move |tx| tx.delete_edge(&ns, id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InvalidateInput {
    at: TimeInput,
}

async fn invalidate_edge(
    State(state): State<AppState>,
    Path((ns, id)): Path<(String, String)>,
    Body(input): Body<InvalidateInput>,
) -> ApiResult<Json<grafeno::Edge>> {
    let id = parse_edge_id(&id)?;
    let at = input.at.resolve()?;
    let edge = write(&state, move |tx| {
        tx.invalidate_edge(&ns, id, at)?;
        tx.edge(&ns, id)?.ok_or(Error::EdgeNotFound(id))
    })
    .await?;
    Ok(Json(edge))
}

async fn retract_edge(
    State(state): State<AppState>,
    Path((ns, id)): Path<(String, String)>,
) -> ApiResult<Json<grafeno::Edge>> {
    let id = parse_edge_id(&id)?;
    let edge = write(&state, move |tx| {
        tx.retract_edge(&ns, id)?;
        tx.edge(&ns, id)?.ok_or(Error::EdgeNotFound(id))
    })
    .await?;
    Ok(Json(edge))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KHopInput {
    start: NodeId,
    #[serde(default = "one")]
    depth: usize,
    #[serde(default)]
    direction: Direction,
    #[serde(default)]
    filter: FilterInput,
}

fn one() -> usize {
    1
}

async fn k_hop(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Body(input): Body<KHopInput>,
) -> ApiResult<Json<Vec<Value>>> {
    let filter = input.filter.resolve()?;
    let found = blocking(&state, move |db| {
        db.begin_read()?
            .k_hop(&ns, input.start, input.depth, input.direction, &filter)
    })
    .await?;
    Ok(Json(
        found
            .into_iter()
            .map(|(node, hops)| json!({ "node": node, "hops": hops }))
            .collect(),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathInput {
    from: NodeId,
    to: NodeId,
    #[serde(default = "default_max_depth")]
    max_depth: usize,
    #[serde(default)]
    direction: Direction,
    #[serde(default)]
    filter: FilterInput,
}

fn default_max_depth() -> usize {
    6
}

async fn path(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Body(input): Body<PathInput>,
) -> ApiResult<Json<Value>> {
    let filter = input.filter.resolve()?;
    let path = blocking(&state, move |db| {
        db.begin_read()?.shortest_path(
            &ns,
            input.from,
            input.to,
            input.max_depth,
            input.direction,
            &filter,
        )
    })
    .await?;
    Ok(Json(json!({ "path": path })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SubgraphInput {
    seeds: Vec<NodeId>,
    #[serde(default = "one")]
    depth: usize,
    #[serde(default)]
    direction: Direction,
    #[serde(default)]
    filter: FilterInput,
}

async fn subgraph(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Body(input): Body<SubgraphInput>,
) -> ApiResult<Json<grafeno::Subgraph>> {
    let filter = input.filter.resolve()?;
    let subgraph = blocking(&state, move |db| {
        db.begin_read()?
            .subgraph(&ns, &input.seeds, input.depth, input.direction, &filter)
    })
    .await?;
    Ok(Json(subgraph))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextSearchInput {
    query: String,
    #[serde(default = "ten")]
    limit: usize,
}

fn ten() -> usize {
    10
}

async fn search_text(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Body(input): Body<TextSearchInput>,
) -> ApiResult<Json<Vec<grafeno::Hit>>> {
    let hits = blocking(&state, move |db| {
        db.begin_read()?.search_text(&ns, &input.query, input.limit)
    })
    .await?;
    Ok(Json(hits))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VectorSearchInput {
    embedding: Vec<f32>,
    #[serde(default = "ten")]
    limit: usize,
    #[serde(default)]
    ef: Option<usize>,
}

async fn search_vector(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Body(input): Body<VectorSearchInput>,
) -> ApiResult<Json<Vec<grafeno::Hit>>> {
    let hits = blocking(&state, move |db| {
        let read = db.begin_read()?;
        match input.ef {
            Some(ef) => read.search_vector_with(&ns, &input.embedding, input.limit, ef),
            None => read.search_vector(&ns, &input.embedding, input.limit),
        }
    })
    .await?;
    Ok(Json(hits))
}

async fn recall_handler(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Body(input): Body<RecallInput>,
) -> ApiResult<Json<Vec<grafeno_memory::Recalled>>> {
    let query = input.resolve()?;
    let touch = input.touch;
    let read_ns = ns.clone();
    let results = blocking(&state, move |db| {
        recall(&db.begin_read()?, &read_ns, &query)
    })
    .await?;
    if touch {
        let ids: Vec<NodeId> = results.iter().map(|r| r.node.id).collect();
        write(&state, move |tx| tx.touch(&ns, &ids, Timestamp::now())).await?;
    }
    Ok(Json(results))
}

async fn context_handler(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Body(input): Body<ContextInput>,
) -> ApiResult<Json<grafeno_memory::Context>> {
    let (query, options) = input.resolve()?;
    let touch = input.query.touch;
    let read_ns = ns.clone();
    let context = blocking(&state, move |db| {
        context_for(&db.begin_read()?, &read_ns, &query, &options)
    })
    .await?;
    if touch {
        let ids: Vec<NodeId> = context.items.iter().map(|item| item.node).collect();
        write(&state, move |tx| tx.touch(&ns, &ids, Timestamp::now())).await?;
    }
    Ok(Json(context))
}

/// One operation of an atomic batch.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum BatchOp {
    /// Creates a node; `ref` (`$name`) lets later operations point to it.
    CreateNode {
        #[serde(default, rename = "ref")]
        reference: Option<String>,
        node: NodeInput,
    },
    /// Creates or replaces the node with `node.id`.
    UpsertNode {
        #[serde(default, rename = "ref")]
        reference: Option<String>,
        node: NodeInput,
    },
    UpdateNode {
        id: NodeRef,
        patch: PatchNode,
    },
    DeleteNode {
        id: NodeRef,
    },
    CreateEdge {
        edge: EdgeInput,
    },
    DeleteEdge {
        id: EdgeId,
    },
    DeleteEdgesFrom {
        from: NodeRef,
        #[serde(default)]
        label: Option<String>,
    },
    InvalidateEdge {
        id: EdgeId,
        at: TimeInput,
    },
    RetractEdge {
        id: EdgeId,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchInput {
    ops: Vec<BatchOp>,
}

#[derive(Serialize)]
struct BatchOutput {
    results: Vec<Value>,
    refs: HashMap<String, NodeId>,
}

fn remember_ref(
    refs: &mut HashMap<String, NodeId>,
    reference: Option<String>,
    id: NodeId,
) -> grafeno::Result<()> {
    if let Some(reference) = reference {
        if !reference.starts_with('$') || reference.len() < 2 {
            return Err(Error::Invalid(format!(
                "reference {reference:?} must look like \"$name\""
            )));
        }
        refs.insert(reference, id);
    }
    Ok(())
}

fn apply_op(
    tx: &mut WriteTx,
    ns: &str,
    op: BatchOp,
    refs: &mut HashMap<String, NodeId>,
) -> grafeno::Result<Value> {
    Ok(match op {
        BatchOp::CreateNode { reference, node } => {
            if node.id.is_some() {
                return Err(Error::Invalid(
                    "create_node does not take an id; use upsert_node".into(),
                ));
            }
            let id = tx.create_node(ns, node.into_new())?;
            remember_ref(refs, reference, id)?;
            json!({ "id": id, "created": true })
        }
        BatchOp::UpsertNode { reference, node } => {
            let id = node
                .id
                .ok_or_else(|| Error::Invalid("upsert_node needs node.id".into()))?;
            let created = tx.upsert_node(ns, id, node.into_new())?;
            remember_ref(refs, reference, id)?;
            json!({ "id": id, "created": created })
        }
        BatchOp::UpdateNode { id, patch } => {
            let id = resolve_ref(&id, refs)?;
            apply_patch(tx, ns, id, patch)?;
            json!({ "id": id })
        }
        BatchOp::DeleteNode { id } => {
            tx.delete_node(ns, resolve_ref(&id, refs)?)?;
            json!({})
        }
        BatchOp::CreateEdge { edge } => {
            let id = tx.create_edge(ns, edge.into_new(refs)?)?;
            json!({ "id": id })
        }
        BatchOp::DeleteEdge { id } => {
            tx.delete_edge(ns, id)?;
            json!({})
        }
        BatchOp::DeleteEdgesFrom { from, label } => {
            let deleted = tx.delete_edges_from(ns, resolve_ref(&from, refs)?, label.as_deref())?;
            json!({ "deleted": deleted })
        }
        BatchOp::InvalidateEdge { id, at } => {
            tx.invalidate_edge(ns, id, at.resolve()?)?;
            json!({})
        }
        BatchOp::RetractEdge { id } => {
            tx.retract_edge(ns, id)?;
            json!({})
        }
    })
}

/// Applies every operation in one transaction: all of them or none.
async fn batch(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Body(input): Body<BatchInput>,
) -> ApiResult<Json<BatchOutput>> {
    let writer = state.writer.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        let failed_at = Arc::new(std::sync::Mutex::new(None));
        let failed = failed_at.clone();
        let result = writer.write(move |tx| {
            let mut refs = HashMap::new();
            let mut results = Vec::with_capacity(input.ops.len());
            for (index, op) in input.ops.iter().cloned().enumerate() {
                match apply_op(tx, &ns, op, &mut refs) {
                    Ok(value) => results.push(value),
                    Err(error) => {
                        *failed.lock().expect("not poisoned") = Some(index);
                        return Err(error);
                    }
                }
            }
            Ok(BatchOutput { results, refs })
        });
        let failed_at = *failed_at.lock().expect("not poisoned");
        result.map_err(|error| (failed_at, error))
    })
    .await
    .map_err(|error| ApiError::internal(error.to_string()))?;

    match outcome {
        Ok(output) => Ok(Json(output)),
        Err((failed_at, error)) => {
            let mut api_error = ApiError::from(error);
            if let Some(index) = failed_at {
                api_error.message = format!("operation {index}: {}", api_error.message);
            }
            Err(api_error)
        }
    }
}

// ---------------------------------------------------------------- administration

const IMPORT_LIMIT: usize = 1024 * 1024 * 1024;

async fn namespaces(State(state): State<AppState>) -> ApiResult<Json<Vec<grafeno::NamespaceInfo>>> {
    let caller = auth::current();
    let namespaces = blocking(&state, |db| db.begin_read()?.namespaces()).await?;
    Ok(Json(
        namespaces
            .into_iter()
            .filter(|info| caller.may(&info.name, Role::Read))
            .collect(),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewNamespace {
    name: String,
    description: Option<String>,
}

/// Creates an empty namespace: 201, or 409 if it exists.
async fn create_namespace(
    State(state): State<AppState>,
    Body(input): Body<NewNamespace>,
) -> ApiResult<(StatusCode, Json<grafeno::NamespaceInfo>)> {
    require(&input.name, Role::Admin)?;
    let info = write(&state, move |tx| {
        tx.create_namespace(&input.name, input.description.as_deref())?;
        tx.namespace(&input.name)?
            .ok_or_else(|| Error::NamespaceNotFound(input.name.clone()))
    })
    .await?;
    Ok((StatusCode::CREATED, Json(info)))
}

async fn get_namespace(
    State(state): State<AppState>,
    Path(ns): Path<String>,
) -> ApiResult<Json<grafeno::NamespaceInfo>> {
    let info = blocking(&state, move |db| {
        db.begin_read()?
            .namespace(&ns)?
            .ok_or(Error::NamespaceNotFound(ns))
    })
    .await?;
    Ok(Json(info))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NamespacePatch {
    description: Option<String>,
}

/// Sets the description (`null` removes it).
async fn describe_namespace(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Body(input): Body<NamespacePatch>,
) -> ApiResult<Json<grafeno::NamespaceInfo>> {
    let info = write(&state, move |tx| {
        tx.describe_namespace(&ns, input.description.as_deref())?;
        tx.namespace(&ns)?
            .ok_or_else(|| Error::NamespaceNotFound(ns.clone()))
    })
    .await?;
    Ok(Json(info))
}

/// Deletes a namespace and everything in it.
async fn drop_namespace(
    State(state): State<AppState>,
    Path(ns): Path<String>,
) -> ApiResult<Json<grafeno::DropStats>> {
    let dropped = write(&state, move |tx| tx.drop_namespace(&ns)).await?;
    Ok(Json(dropped))
}

async fn stats(
    State(state): State<AppState>,
    Path(ns): Path<String>,
) -> ApiResult<Json<grafeno::NamespaceStats>> {
    let stats = blocking(&state, move |db| db.begin_read()?.stats(&ns)).await?;
    Ok(Json(stats))
}

async fn check(
    State(state): State<AppState>,
    Path(ns): Path<String>,
) -> ApiResult<Json<grafeno::CheckReport>> {
    let report = blocking(&state, move |db| db.begin_read()?.check(&ns)).await?;
    Ok(Json(report))
}

async fn rebuild(
    State(state): State<AppState>,
    Path(ns): Path<String>,
) -> ApiResult<Json<grafeno::RebuildReport>> {
    let report = write_bulk(&state, move |tx| tx.rebuild_indexes(&ns)).await?;
    Ok(Json(report))
}

/// Builds the namespace's vector graph again (admin).
async fn compact(
    State(state): State<AppState>,
    Path(ns): Path<String>,
) -> ApiResult<Json<grafeno::CompactReport>> {
    let report = write_bulk(&state, move |tx| tx.compact_vectors(&ns)).await?;
    Ok(Json(report))
}

#[derive(Deserialize)]
struct RecallParams {
    #[serde(default = "default_recall_sample")]
    sample: usize,
    #[serde(default = "default_recall_ef")]
    ef: usize,
}

fn default_recall_sample() -> usize {
    100
}

fn default_recall_ef() -> usize {
    64
}

/// An estimate of the recall@10 of vector searches, from a sample of the
/// namespace's own vectors (reads every vector once).
async fn vector_recall(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Params(params): Params<RecallParams>,
) -> ApiResult<Json<Value>> {
    let sample = params.sample.clamp(1, 1_000);
    let ef = params.ef.clamp(1, 1_000);
    let recall = blocking(&state, move |db| {
        db.begin_read()?.vector_recall(&ns, sample, ef)
    })
    .await?;
    Ok(Json(
        json!({ "sample": sample, "ef": ef, "recall": recall }),
    ))
}

#[derive(Deserialize)]
struct ExportParams {
    /// Comma-separated namespaces; all of them when absent.
    ns: Option<String>,
}

/// The whole export as JSON Lines, from one consistent snapshot.
async fn export(
    State(state): State<AppState>,
    Params(params): Params<ExportParams>,
) -> ApiResult<Response> {
    let namespaces: Vec<String> = params
        .ns
        .iter()
        .flat_map(|names| names.split(','))
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect();
    let caller = auth::current();
    for ns in &namespaces {
        require(ns, Role::Read)?;
    }
    let body = blocking(&state, move |db| {
        let read = db.begin_read()?;
        // "Everything" is what the caller may read.
        let namespaces = if namespaces.is_empty() && !caller.is_owner() {
            let readable: Vec<String> = read
                .namespaces()?
                .into_iter()
                .map(|info| info.name)
                .filter(|ns| caller.may(ns, Role::Read))
                .collect();
            if readable.is_empty() {
                return Err(Error::Forbidden(format!(
                    "the key {:?} may read no namespace",
                    caller.name()
                )));
            }
            readable
        } else {
            namespaces
        };
        let mut body = Vec::new();
        read.export(&namespaces, &mut body)?;
        Ok(body)
    })
    .await?;
    Ok(([(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response())
}

/// A copy of the whole database file, consistent, taken while the server
/// keeps serving (owners only): open it with `grafeno serve --db`.
async fn backup(State(state): State<AppState>) -> ApiResult<Response> {
    let bytes = blocking(&state, |db| {
        let path =
            std::env::temp_dir().join(format!("grafeno-backup-{}.redb", grafeno::NodeId::new()));
        let outcome = db.begin_read()?.backup(&path).and_then(|_| {
            std::fs::read(&path)
                .map_err(|error| Error::Storage(format!("cannot read the backup: {error}")))
        });
        let _ = std::fs::remove_file(&path);
        outcome
    })
    .await?;
    let name = format!(
        "attachment; filename=\"grafeno-{}.redb\"",
        Timestamp::now().date()
    );
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
            (header::CONTENT_DISPOSITION, name),
        ],
        bytes,
    )
        .into_response())
}

/// Imports a JSON Lines export (up to 1 GiB) in one transaction.
#[derive(Deserialize)]
struct ImportParams {
    /// Load in chunks (faster; an error keeps the chunks before it).
    #[serde(default)]
    bulk: bool,
}

async fn import(
    State(state): State<AppState>,
    Params(params): Params<ImportParams>,
    request: Request,
) -> ApiResult<Json<grafeno::DumpStats>> {
    let body = axum::body::to_bytes(request.into_body(), IMPORT_LIMIT)
        .await
        .map_err(|error| ApiError::bad_request(format!("cannot read the export: {error}")))?;
    // Through the bulk lane, so agents keep writing during a load: in
    // chunks with `bulk`, else in one transaction (all of it or nothing).
    let chunk = if params.bulk {
        state.import_chunk
    } else {
        usize::MAX
    };
    let writer = state.writer.clone();
    let stats = tokio::task::spawn_blocking(move || writer.import(body.as_ref(), chunk))
        .await
        .map_err(|error| ApiError::internal(error.to_string()))??;
    Ok(Json(stats))
}

// ---------------------------------------------------------------- queries

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryInput {
    query: String,
    #[serde(default)]
    params: serde_json::Map<String, Value>,
}

impl From<grafeno_query::QueryError> for ApiError {
    fn from(error: grafeno_query::QueryError) -> Self {
        Self {
            status: if error.internal {
                StatusCode::INTERNAL_SERVER_ERROR
            } else if error.forbidden {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::BAD_REQUEST
            },
            code: if error.internal {
                "internal"
            } else if error.forbidden {
                "forbidden"
            } else if error.limit {
                "query_limit"
            } else {
                "invalid_query"
            },
            message: error.to_string(),
            details: None,
        }
    }
}

/// Runs one GQ statement; statements with writes run in one transaction.
/// Sets the flag when dropped.
struct CancelOnDrop(Arc<std::sync::atomic::AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

async fn query(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Body(input): Body<QueryInput>,
) -> ApiResult<Json<Value>> {
    let params = grafeno_query::params_from_json(&input.params);
    run_query(&state, ns, input.query, params).await.map(Json)
}

/// Runs a GQ query, counted in the metrics.
async fn run_query(
    state: &AppState,
    ns: String,
    query: String,
    params: grafeno_query::Params,
) -> ApiResult<Value> {
    let started = std::time::Instant::now();
    let outcome = run_query_counted(state, ns, query, params).await;
    let limit = match &outcome {
        Err(error) if error.code == "query_limit" => Some(error.message.as_str()),
        _ => None,
    };
    state.metrics.query(started.elapsed().as_secs_f64(), limit);
    outcome
}

/// Runs a GQ query: reads on a snapshot, writes in the next group commit.
async fn run_query_counted(
    state: &AppState,
    ns: String,
    query: String,
    params: grafeno_query::Params,
) -> ApiResult<Value> {
    let db = state.db.clone();
    // When the client goes away, this handler is dropped: the guard then
    // tells the query to stop.
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _guard = CancelOnDrop(cancel.clone());
    let caller = auth::current();
    match grafeno_query::statement_kind(&query)? {
        grafeno_query::StatementKind::Read => require(&ns, Role::Read)?,
        grafeno_query::StatementKind::Write => require(&ns, Role::Write)?,
        grafeno_query::StatementKind::Schema => require(&ns, Role::Admin)?,
    }
    let limits = grafeno_query::Limits {
        cancel: Some(cancel),
        visible: (!caller.is_owner())
            .then(|| grafeno_query::NamespaceFilter::new(move |ns| caller.may(ns, Role::Read))),
        ..state.limits.clone()
    };
    if !grafeno_query::is_write(&query)? {
        let result = tokio::task::spawn_blocking(move || {
            grafeno_query::execute_read_with(&db.begin_read()?, &ns, &query, &params, &limits)
        })
        .await
        .map_err(|error| ApiError::internal(error.to_string()))??;
        return Ok(result.to_json());
    }
    // Writes join the next group commit. A failing query must make the
    // writer discard the group's transaction, so its error travels as an
    // engine error and the query error itself is kept aside.
    let failure: Arc<std::sync::Mutex<Option<grafeno_query::QueryError>>> = Arc::default();
    let kept = failure.clone();
    let outcome = write(state, move |tx| {
        let read = db.begin_read()?;
        grafeno_query::execute_in(tx, &read, &ns, &query, &params, &limits).map_err(|error| {
            let message = error.to_string();
            *kept.lock().expect("not poisoned") = Some(error);
            Error::Invalid(message)
        })
    })
    .await;
    match outcome {
        Ok(result) => Ok(result.to_json()),
        Err(error) => match failure.lock().expect("not poisoned").take() {
            Some(query_error) => Err(query_error.into()),
            None => Err(error),
        },
    }
}

// ---------------------------------------------------------------- keys

/// What a key looks like over the API: never its secret or its hash.
#[derive(Serialize)]
struct KeyOutput {
    name: String,
    grants: Vec<Grant>,
    owner: bool,
    own_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    created_at: Timestamp,
}

impl From<&Principal> for KeyOutput {
    fn from(principal: &Principal) -> Self {
        Self {
            name: principal.name.clone(),
            grants: principal.grants.clone(),
            owner: principal.owner,
            own_only: principal.own_only,
            description: principal.description.clone(),
            created_at: principal.created_at,
        }
    }
}

/// Who the caller is and what it may do.
async fn whoami() -> Json<Value> {
    Json(match auth::current() {
        Caller::Owner => json!({ "name": "owner", "owner": true, "grants": [], "own_only": false }),
        Caller::Key(principal) => {
            serde_json::to_value(KeyOutput::from(principal.as_ref())).unwrap_or(Value::Null)
        }
    })
}

async fn list_keys(State(state): State<AppState>) -> ApiResult<Json<Vec<KeyOutput>>> {
    let principals = blocking(&state, |db| db.begin_read()?.principals()).await?;
    Ok(Json(principals.iter().map(KeyOutput::from).collect()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewKey {
    name: String,
    #[serde(default)]
    grants: Vec<Grant>,
    #[serde(default)]
    owner: bool,
    #[serde(default)]
    own_only: bool,
    description: Option<String>,
}

/// Creates a key and returns its secret, once: only its hash is kept.
async fn create_key(
    State(state): State<AppState>,
    Body(input): Body<NewKey>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if input.grants.is_empty() && !input.owner {
        return Err(ApiError::bad_request(
            "a key needs at least one grant ({namespace, role}) or owner: true",
        ));
    }
    // Without a token, keys close the server: the first one must be an
    // owner, or nobody could manage keys any more.
    if state.token.is_none() && !input.owner {
        let owners = blocking(&state, |db| {
            Ok(db.begin_read()?.principals()?.iter().any(|p| p.owner))
        })
        .await?;
        if !owners {
            return Err(ApiError::bad_request(
                "this server has no token: create an owner key first (owner: true), or the server would lock everyone out",
            ));
        }
    }
    let secret = auth::new_secret();
    let principal = Principal {
        name: input.name,
        secret_hash: auth::hash_secret(&secret),
        grants: input.grants,
        owner: input.owner,
        own_only: input.own_only,
        description: input.description.filter(|d| !d.trim().is_empty()),
        created_at: Timestamp::now(),
    };
    let stored = principal.clone();
    write(&state, move |tx| tx.create_principal(&stored)).await?;
    state.keys.reload(&state.db)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "key": KeyOutput::from(&principal), "secret": secret })),
    ))
}

async fn revoke_key(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    let wanted = name.clone();
    let removed = write(&state, move |tx| tx.delete_principal(&wanted)).await?;
    state.keys.reload(&state.db)?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!("no key named {name:?}")))
    }
}

// ---------------------------------------------------------------- algorithms

/// The graph algorithms, as `CALL dbms.procedures()` lists them.
async fn algorithms() -> Json<Value> {
    Json(Value::Array(
        grafeno_query::algorithms()
            .map(|algorithm| {
                json!({
                    "name": algorithm.name,
                    "arguments": algorithm.args,
                    "columns": algorithm.columns,
                    "description": algorithm.description,
                })
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AlgorithmInput {
    /// The arguments before the settings: nodes by id, `k`.
    #[serde(default)]
    args: Vec<Value>,
    #[serde(default)]
    settings: serde_json::Map<String, Value>,
    /// Stores the result in this node property (or `@embedding`) instead
    /// of returning rows.
    write: Option<String>,
    /// With `write`, the column to store; the one after `node` by default.
    column: Option<String>,
}

/// `CALL algo.<name>` over HTTP: the rows it yields (with nodes as
/// `{@id, @labels, name}`), or, with `write`, how many nodes got the result.
async fn algorithm(
    State(state): State<AppState>,
    Path((ns, name)): Path<(String, String)>,
    Body(input): Body<AlgorithmInput>,
) -> ApiResult<Json<Value>> {
    let wanted = format!("algo.{name}");
    let algorithm = grafeno_query::algorithms()
        .find(|algorithm| {
            algorithm.name.eq_ignore_ascii_case(&wanted)
                || algorithm.name.eq_ignore_ascii_case(&name)
        })
        .ok_or_else(|| {
            ApiError::bad_request(format!(
                "no algorithm {name} (GET /v1/algorithms lists them)"
            ))
        })?;
    let mut params = grafeno_query::Params::new();
    let mut call = Vec::new();
    for (at, arg) in input.args.iter().enumerate() {
        params.insert(format!("arg{at}"), grafeno_query::Value::from_json(arg));
        call.push(format!("$arg{at}"));
    }
    params.insert(
        "settings".into(),
        grafeno_query::Value::from_json(&Value::Object(input.settings)),
    );
    call.push("$settings".into());
    let call = format!("CALL {}({})", algorithm.name, call.join(", "));
    let query = match input.write {
        None => {
            let columns: Vec<String> = algorithm
                .columns
                .iter()
                .map(|column| match *column {
                    "node" | "other" => {
                        format!("{{id: {column}.@id, labels: {column}.@labels, name: {column}.name}} AS {column}")
                    }
                    "nodes" => {
                        "[n IN nodes | {id: n.@id, labels: n.@labels, name: n.name}] AS nodes"
                            .to_owned()
                    }
                    "edges" => "[e IN edges | e.@id] AS edges".to_owned(),
                    other => other.to_owned(),
                })
                .collect();
            format!(
                "{call} YIELD {} RETURN {}",
                algorithm.columns.join(", "),
                columns.join(", ")
            )
        }
        Some(property) => {
            let valid = property == "@embedding"
                || (!property.is_empty()
                    && property
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !property.starts_with(|c: char| c.is_ascii_digit()));
            if !valid {
                return Err(ApiError::bad_request(format!(
                    "write needs a property name (letters, digits, _) or @embedding, not {property:?}"
                )));
            }
            if algorithm.columns.first() != Some(&"node") {
                return Err(ApiError::bad_request(format!(
                    "{} yields no column per node to write",
                    algorithm.name
                )));
            }
            let column = input
                .column
                .unwrap_or_else(|| algorithm.columns[1].to_owned());
            if !algorithm.columns.contains(&column.as_str()) || column == "node" {
                return Err(ApiError::bad_request(format!(
                    "{} yields {}: no column {column}",
                    algorithm.name,
                    algorithm.columns.join(", ")
                )));
            }
            let target = if property == "@embedding" {
                "node.@embedding".to_owned()
            } else {
                format!("node.{property}")
            };
            format!(
                "{call} YIELD node, {column} SET {target} = {column} RETURN count(node) AS written"
            )
        }
    };
    run_query(&state, ns, query, params).await.map(Json)
}

// ---------------------------------------------------------------- memory

#[derive(Deserialize)]
struct ResolveParams {
    name: String,
    label: Option<String>,
}

/// Nodes named `name` (or with it as an alias), compared normalized.
async fn resolve(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Params(params): Params<ResolveParams>,
) -> ApiResult<Json<Vec<grafeno::Node>>> {
    let nodes = blocking(&state, move |db| {
        let read = db.begin_read()?;
        let mut nodes = Vec::new();
        for id in read.find_by_name(&ns, &params.name)? {
            if let Some(node) = read.node(&ns, id)?
                && params
                    .label
                    .as_deref()
                    .is_none_or(|label| node.has_label(label))
            {
                nodes.push(node);
            }
        }
        Ok(nodes)
    })
    .await?;
    Ok(Json(nodes))
}

#[derive(Deserialize)]
struct FuzzyParams {
    label: String,
    key: String,
    q: String,
    limit: Option<usize>,
    /// A measure name (`dice` by default, `jaro_winkler`, `edit`, …).
    measure: Option<String>,
}

fn measure(name: Option<&str>) -> ApiResult<grafeno::textsim::Measure> {
    match name {
        None => Ok(grafeno::textsim::Measure::Dice { n: 3 }),
        Some(name) => grafeno::textsim::Measure::by_name(name)
            .ok_or_else(|| ApiError::bad_request(format!("unknown measure {name:?}"))),
    }
}

/// Nodes whose property looks like `q`, through the fuzzy index on it.
async fn fuzzy(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Params(params): Params<FuzzyParams>,
) -> ApiResult<Json<Vec<grafeno::FuzzyHit>>> {
    let measure = measure(params.measure.as_deref())?;
    let limit = params.limit.unwrap_or(10).min(MAX_PAGE);
    let hits = blocking(&state, move |db| {
        db.begin_read()?
            .fuzzy_search(&ns, &params.label, &params.key, &params.q, measure, limit)
    })
    .await?;
    Ok(Json(hits))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MatchInput {
    label: String,
    #[serde(default)]
    props: grafeno::Props,
    /// Nodes the record is linked to, for rules that weigh neighbors.
    #[serde(default)]
    neighbors: Vec<NodeId>,
    #[serde(default)]
    limit: Option<usize>,
    /// A rule to try instead of the stored one.
    #[serde(default)]
    rule: Option<grafeno::MatchRule>,
}

/// Nodes that may be this record, with scores, decisions and fields.
async fn find_matches(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Body(input): Body<MatchInput>,
) -> ApiResult<Json<Vec<grafeno::Match>>> {
    let limit = input.limit.unwrap_or(10).min(MAX_PAGE);
    let found = blocking(&state, move |db| {
        let probe = grafeno::Probe {
            label: input.label,
            props: input.props,
            neighbors: input.neighbors,
        };
        db.begin_read()?
            .find_matches(&ns, &probe, input.rule.as_ref(), limit, None)
    })
    .await?;
    Ok(Json(found))
}

#[derive(Deserialize)]
struct PairsParams {
    label: String,
    limit: Option<usize>,
}

/// Pairs of nodes of a label that its rule calls the same or to review.
async fn match_pairs(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Params(params): Params<PairsParams>,
) -> ApiResult<Json<Value>> {
    let limit = params.limit.unwrap_or(50).min(MAX_PAGE);
    let pairs = blocking(&state, move |db| {
        db.begin_read()?.match_pairs(&ns, &params.label, limit)
    })
    .await?;
    Ok(Json(json!(
        pairs
            .into_iter()
            .map(|(node, found)| json!({ "a": node, "b": found.node, "match": found }))
            .collect::<Vec<_>>()
    )))
}

async fn match_rules(
    State(state): State<AppState>,
    Path(ns): Path<String>,
) -> ApiResult<Json<Vec<grafeno::MatchRule>>> {
    let rules = blocking(&state, move |db| db.begin_read()?.match_rules(&ns)).await?;
    Ok(Json(rules))
}

/// Declares the match rule of a label (its `label` is the path's).
async fn set_match_rule(
    State(state): State<AppState>,
    Path((ns, label)): Path<(String, String)>,
    axum::Json(mut rule): axum::Json<Value>,
) -> ApiResult<Json<grafeno::MatchRule>> {
    if let Some(object) = rule.as_object_mut() {
        object.insert("label".into(), Value::from(label));
    }
    let rule: grafeno::MatchRule =
        serde_json::from_value(rule).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let stored = rule.clone();
    write(&state, move |tx| tx.set_match_rule(&ns, &rule)).await?;
    Ok(Json(stored))
}

async fn delete_match_rule(
    State(state): State<AppState>,
    Path((ns, label)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let existed = write(&state, move |tx| tx.delete_match_rule(&ns, &label)).await?;
    if existed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("no match rule for that label"))
    }
}

#[derive(Deserialize)]
struct DuplicateParams {
    label: Option<String>,
    min_score: Option<f32>,
    /// Compare embeddings too (default true).
    vectors: Option<bool>,
    limit: Option<usize>,
}

async fn duplicates(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Params(params): Params<DuplicateParams>,
) -> ApiResult<Json<Vec<grafeno_memory::Duplicate>>> {
    let base = grafeno_memory::DuplicateOptions::default();
    let options = grafeno_memory::DuplicateOptions {
        label: params.label,
        min_score: params.min_score.unwrap_or(base.min_score),
        vector_threshold: if params.vectors.unwrap_or(true) {
            base.vector_threshold
        } else {
            None
        },
        limit: params.limit.unwrap_or(base.limit).min(MAX_PAGE),
        ..base
    };
    let found = blocking(&state, move |db| {
        grafeno_memory::find_duplicates(&db.begin_read()?, &ns, &options)
    })
    .await?;
    Ok(Json(found))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MergeInput {
    /// The node that stays.
    keep: NodeId,
    /// The node merged into it, then deleted.
    merge: NodeId,
    /// Merge in spite of conflicts (the kept node's values win).
    #[serde(default)]
    force: bool,
    /// Only report the conflicts; merge nothing.
    #[serde(default)]
    dry_run: bool,
    /// Edge labels a node has one of at a time (`born_in`).
    #[serde(default)]
    functional: Vec<String>,
    /// Identifying properties; `email` and `external_id` when missing.
    #[serde(default)]
    strong_keys: Option<Vec<String>>,
}

/// Merges two nodes. Conflicts (different emails or labels, two birth
/// dates) refuse the merge with 409 and the list of `conflicts`, unless
/// `force`; `dry_run` only lists them.
async fn merge(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Body(input): Body<MergeInput>,
) -> ApiResult<Json<Value>> {
    let base = grafeno::MergeOptions::default();
    let options = grafeno::MergeOptions {
        force: input.force,
        functional: input.functional,
        strong_keys: input.strong_keys.unwrap_or(base.strong_keys.clone()),
        ..base
    };
    let (keep, merged, dry_run) = (input.keep, input.merge, input.dry_run);
    let outcome = write(&state, move |tx| {
        let conflicts = tx.merge_conflicts(&ns, keep, merged, &options)?;
        if dry_run || (!conflicts.is_empty() && !options.force) {
            return Ok(Err(conflicts));
        }
        tx.merge_nodes_with(&ns, keep, merged, &options).map(Ok)
    })
    .await?;
    match outcome {
        Ok(report) => Ok(Json(
            serde_json::to_value(report).map_err(|e| ApiError::internal(e.to_string()))?,
        )),
        Err(conflicts) if dry_run => Ok(Json(json!({ "conflicts": conflicts }))),
        Err(conflicts) => {
            let listed: Vec<String> = conflicts.iter().map(grafeno::Conflict::describe).collect();
            let mut details = serde_json::Map::new();
            details.insert("conflicts".into(), json!(conflicts));
            Err(ApiError {
                status: StatusCode::CONFLICT,
                code: "conflict",
                message: format!(
                    "the nodes disagree, so they may not be the same: {}; send force: true to merge anyway",
                    listed.join("; ")
                ),
                details: Some(details),
            })
        }
    }
}

#[derive(Deserialize)]
struct MergesParams {
    node: Option<NodeId>,
    limit: Option<usize>,
}

/// Merges recorded in the namespace, newest first (`?node=` for one node's).
async fn merges(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Params(params): Params<MergesParams>,
) -> ApiResult<Json<Vec<grafeno::MergeRecord>>> {
    let limit = params.limit.unwrap_or(50).min(MAX_PAGE);
    let mut records = blocking(&state, move |db| db.begin_read()?.merges(&ns, params.node)).await?;
    records.truncate(limit);
    Ok(Json(records))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct UndoInput {
    /// Undo even if the kept node changed since (its changes are lost).
    #[serde(default)]
    force: bool,
}

/// Undoes a merge: the merged node and its facts come back.
async fn undo_merge(
    State(state): State<AppState>,
    Path((ns, id)): Path<(String, grafeno::MergeId)>,
    body: axum::body::Bytes,
) -> ApiResult<Json<grafeno::MergeRecord>> {
    // The body is optional: none is `{}`.
    let input: UndoInput = if body.iter().all(u8::is_ascii_whitespace) {
        UndoInput::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(e.to_string()))?
    };
    let force = input.force;
    let record = write(&state, move |tx| tx.undo_merge(&ns, id, force)).await?;
    Ok(Json(record))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdsInput {
    ids: Vec<NodeId>,
}

/// Records an access to each node now (as recall with `touch` does).
async fn touch(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Body(input): Body<IdsInput>,
) -> ApiResult<StatusCode> {
    write(&state, move |tx| {
        tx.touch(&ns, &input.ids, Timestamp::now())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct WeakParams {
    half_life_days: Option<f64>,
    threshold: Option<f32>,
    label: Option<String>,
    limit: Option<usize>,
}

async fn weak(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Params(params): Params<WeakParams>,
) -> ApiResult<Json<Vec<grafeno_memory::WeakMemory>>> {
    let base = grafeno_memory::DecayOptions::default();
    let options = grafeno_memory::DecayOptions {
        half_life_secs: params
            .half_life_days
            .map_or(base.half_life_secs, |days| days * 86_400.0),
        threshold: params.threshold.unwrap_or(base.threshold),
        labels: params.label.into_iter().collect(),
        limit: params.limit.unwrap_or(base.limit).min(MAX_PAGE),
        now: Timestamp::now(),
    };
    let found = blocking(&state, move |db| {
        grafeno_memory::weak_memories(&db.begin_read()?, &ns, &options)
    })
    .await?;
    Ok(Json(found))
}

/// Adds the `Archived` label: recall skips these nodes, nothing is deleted.
async fn archive_nodes(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Body(input): Body<IdsInput>,
) -> ApiResult<Json<Value>> {
    let archived = write(&state, move |tx| {
        grafeno_memory::archive(tx, &ns, &input.ids)
    })
    .await?;
    Ok(Json(json!({ "archived": archived })))
}

#[derive(Deserialize)]
struct ConsolidationParams {
    older_than_days: Option<i64>,
    min_size: Option<usize>,
    limit: Option<usize>,
    /// Comma-separated labels of the memories to group.
    labels: Option<String>,
}

async fn consolidation(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Params(params): Params<ConsolidationParams>,
) -> ApiResult<Json<Vec<grafeno_memory::ConsolidationGroup>>> {
    let base = grafeno_memory::ConsolidationOptions::default();
    let options = grafeno_memory::ConsolidationOptions {
        memory_labels: params
            .labels
            .map(|labels| {
                labels
                    .split(',')
                    .filter(|l| !l.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or(base.memory_labels),
        older_than_secs: params
            .older_than_days
            .map_or(base.older_than_secs, |days| days * 86_400),
        min_size: params.min_size.unwrap_or(base.min_size),
        limit: params.limit.unwrap_or(base.limit).min(MAX_PAGE),
        now: Timestamp::now(),
    };
    let groups = blocking(&state, move |db| {
        grafeno_memory::consolidation_groups(&db.begin_read()?, &ns, &options)
    })
    .await?;
    Ok(Json(groups))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SummaryInput {
    text: String,
    sources: Vec<NodeId>,
    #[serde(default)]
    about: Option<NodeId>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    embedding: Option<Vec<f32>>,
    /// Archive the summarized memories (default true).
    #[serde(default)]
    archive_sources: Option<bool>,
}

/// Stores a summary of several memories (written by an agent), linked to
/// them with `summarizes`, and archives them.
async fn summaries(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Body(input): Body<SummaryInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let summary = grafeno_memory::Summary {
        text: input.text,
        name: input.name,
        labels: input.labels,
        sources: input.sources,
        about: input.about,
        embedding: input.embedding,
        archive_sources: input.archive_sources.unwrap_or(true),
    };
    let id = write(&state, move |tx| {
        grafeno_memory::write_summary(tx, &ns, summary.clone())
    })
    .await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}
