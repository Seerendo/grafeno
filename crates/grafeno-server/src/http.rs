//! The HTTP API: JSON over `/v1/{namespace}/...`, with an optional bearer token.

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
use grafeno::{Direction, EdgeFilter, EdgeId, Error, Grafeno, NodeId, Props, Timestamp, WriteTx};
use grafeno_memory::{context_for, recall};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};

use crate::api::{
    ContextInput, EdgeInput, ErrorKind, FilterInput, NodeInput, NodeOutput, NodeRef, RecallInput,
    TimeInput, error_kind, parse_id, resolve_ref,
};

const BODY_LIMIT: usize = 32 * 1024 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Grafeno>,
    pub token: Option<Arc<str>>,
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
        .route("/namespaces", get(namespaces))
        .route("/export", get(export))
        .route("/import", post(import))
        .route("/{ns}/stats", get(stats))
        .route("/{ns}/check", get(check))
        .route("/{ns}/rebuild", post(rebuild))
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
        .route("/{ns}/resolve", get(resolve))
        .route("/{ns}/duplicates", get(duplicates))
        .route("/{ns}/merge", post(merge))
        .route("/{ns}/touch", post(touch))
        .route("/{ns}/weak", get(weak))
        .route("/{ns}/archive", post(archive_nodes))
        .route("/{ns}/consolidation", get(consolidation))
        .route("/{ns}/summaries", post(summaries))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth));
    let mut app = Router::new().route("/health", get(health)).nest("/v1", v1);
    if options.studio {
        app = app
            .route("/", get(crate::studio::redirect))
            .route("/studio", get(crate::studio::redirect))
            .route("/studio/", get(crate::studio::index))
            .route("/studio/{*path}", get(crate::studio::asset));
    }
    let mut app = app.layer(DefaultBodyLimit::max(BODY_LIMIT));
    if !options.cors_origins.is_empty() {
        app = app.layer(cors(&options.cors_origins));
    }
    app.with_state(state)
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
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "bad_request",
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal",
            message: message.into(),
        }
    }
}

impl From<Error> for ApiError {
    fn from(error: Error) -> Self {
        let (status, code) = match error_kind(&error) {
            ErrorKind::BadRequest => (StatusCode::BAD_REQUEST, "invalid"),
            ErrorKind::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            ErrorKind::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };
        Self {
            status,
            code,
            message: error.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = json!({ "error": { "code": self.code, "message": self.message } });
        (self.status, Json(body)).into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

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

async fn auth(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if let Some(token) = &state.token {
        let given = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        if !given.is_some_and(|given| constant_time_eq(given.as_bytes(), token.as_bytes())) {
            // Read (a bounded part of) the body first: closing the connection
            // while the client still sends it makes some clients see a reset
            // instead of this response.
            let _ = axum::body::to_bytes(request.into_body(), 1024 * 1024).await;
            return ApiError {
                status: StatusCode::UNAUTHORIZED,
                code: "unauthorized",
                message: "missing or wrong bearer token".into(),
            }
            .into_response();
        }
    }
    next.run(request).await
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
    let result = blocking(&state, move |db| {
        db.write(|tx| match input.id {
            Some(id) => {
                let created = tx.upsert_node(&ns, id, input.into_new())?;
                Ok(Upserted { id, created })
            }
            None => Ok(Upserted {
                id: tx.create_node(&ns, input.into_new())?,
                created: true,
            }),
        })
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
    let created = blocking(&state, move |db| {
        db.write(|tx| tx.upsert_node(&ns, id, input.into_new()))
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
    let node = blocking(&state, move |db| {
        db.write(|tx| apply_patch(tx, &ns, id, patch))?;
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
    blocking(&state, move |db| db.write(|tx| tx.delete_node(&ns, id))).await?;
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
    let id = blocking(&state, move |db| db.write(|tx| tx.create_edge(&ns, new))).await?;
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
    blocking(&state, move |db| db.write(|tx| tx.delete_edge(&ns, id))).await?;
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
    let edge = blocking(&state, move |db| {
        db.write(|tx| {
            tx.invalidate_edge(&ns, id, at)?;
            tx.edge(&ns, id)?.ok_or(Error::EdgeNotFound(id))
        })
    })
    .await?;
    Ok(Json(edge))
}

async fn retract_edge(
    State(state): State<AppState>,
    Path((ns, id)): Path<(String, String)>,
) -> ApiResult<Json<grafeno::Edge>> {
    let id = parse_edge_id(&id)?;
    let edge = blocking(&state, move |db| {
        db.write(|tx| {
            tx.retract_edge(&ns, id)?;
            tx.edge(&ns, id)?.ok_or(Error::EdgeNotFound(id))
        })
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
    let results = blocking(&state, move |db| {
        let results = recall(&db.begin_read()?, &ns, &query)?;
        if touch {
            let ids: Vec<NodeId> = results.iter().map(|r| r.node.id).collect();
            db.write(|tx| tx.touch(&ns, &ids, Timestamp::now()))?;
        }
        Ok(results)
    })
    .await?;
    Ok(Json(results))
}

async fn context_handler(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Body(input): Body<ContextInput>,
) -> ApiResult<Json<grafeno_memory::Context>> {
    let (query, options) = input.resolve()?;
    let touch = input.query.touch;
    let context = blocking(&state, move |db| {
        let context = context_for(&db.begin_read()?, &ns, &query, &options)?;
        if touch {
            let ids: Vec<NodeId> = context.items.iter().map(|item| item.node).collect();
            db.write(|tx| tx.touch(&ns, &ids, Timestamp::now()))?;
        }
        Ok(context)
    })
    .await?;
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
    let db = state.db.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        let mut failed_at = None;
        let result = db.write(|tx| {
            let mut refs = HashMap::new();
            let mut results = Vec::with_capacity(input.ops.len());
            for (index, op) in input.ops.into_iter().enumerate() {
                match apply_op(tx, &ns, op, &mut refs) {
                    Ok(value) => results.push(value),
                    Err(error) => {
                        failed_at = Some(index);
                        return Err(error);
                    }
                }
            }
            Ok(BatchOutput { results, refs })
        });
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
    let namespaces = blocking(&state, |db| db.begin_read()?.namespaces()).await?;
    Ok(Json(namespaces))
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
    let report = blocking(&state, move |db| db.write(|tx| tx.rebuild_indexes(&ns))).await?;
    Ok(Json(report))
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
    let body = blocking(&state, move |db| {
        let mut body = Vec::new();
        db.begin_read()?.export(&namespaces, &mut body)?;
        Ok(body)
    })
    .await?;
    Ok(([(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response())
}

/// Imports a JSON Lines export (up to 1 GiB) in one transaction.
async fn import(
    State(state): State<AppState>,
    request: Request,
) -> ApiResult<Json<grafeno::DumpStats>> {
    let body = axum::body::to_bytes(request.into_body(), IMPORT_LIMIT)
        .await
        .map_err(|error| ApiError::bad_request(format!("cannot read the export: {error}")))?;
    let stats = blocking(&state, move |db| db.import(body.as_ref())).await?;
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
            } else {
                StatusCode::BAD_REQUEST
            },
            code: if error.internal {
                "internal"
            } else {
                "invalid_query"
            },
            message: error.to_string(),
        }
    }
}

/// Runs one GQ statement; statements with writes run in one transaction.
async fn query(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Body(input): Body<QueryInput>,
) -> ApiResult<Json<Value>> {
    let db = state.db.clone();
    let result = tokio::task::spawn_blocking(move || {
        let params = grafeno_query::params_from_json(&input.params);
        grafeno_query::execute(&db, &ns, &input.query, &params)
    })
    .await
    .map_err(|error| ApiError::internal(error.to_string()))??;
    Ok(Json(result.to_json()))
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
}

async fn merge(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Body(input): Body<MergeInput>,
) -> ApiResult<Json<grafeno::MergeReport>> {
    let report = blocking(&state, move |db| {
        db.write(|tx| tx.merge_nodes(&ns, input.keep, input.merge))
    })
    .await?;
    Ok(Json(report))
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
    blocking(&state, move |db| {
        db.write(|tx| tx.touch(&ns, &input.ids, Timestamp::now()))
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
    let archived = blocking(&state, move |db| {
        db.write(|tx| grafeno_memory::archive(tx, &ns, &input.ids))
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
    let id = blocking(&state, move |db| {
        db.write(|tx| grafeno_memory::write_summary(tx, &ns, summary))
    })
    .await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}
