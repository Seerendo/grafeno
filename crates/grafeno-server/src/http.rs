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

pub async fn serve(state: AppState, addr: SocketAddr) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("grafeno listening on http://{}", listener.local_addr()?);
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
}

pub fn router(state: AppState) -> Router {
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
        .route_layer(middleware::from_fn_with_state(state.clone(), auth));
    Router::new()
        .route("/health", get(health))
        .nest("/v1", v1)
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(state)
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
}

fn apply_patch(tx: &mut WriteTx, ns: &str, id: NodeId, patch: PatchNode) -> grafeno::Result<()> {
    tx.node(ns, id)?.ok_or(Error::NodeNotFound(id))?;
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
    let results = blocking(&state, move |db| recall(&db.begin_read()?, &ns, &query)).await?;
    Ok(Json(results))
}

async fn context_handler(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Body(input): Body<ContextInput>,
) -> ApiResult<Json<grafeno_memory::Context>> {
    let (query, options) = input.resolve()?;
    let context = blocking(&state, move |db| {
        context_for(&db.begin_read()?, &ns, &query, &options)
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
