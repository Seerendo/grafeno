use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use grafeno::Grafeno;
use grafeno_server::http::{AppState, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

fn app(token: Option<&str>) -> Router {
    router(AppState {
        db: Arc::new(Grafeno::in_memory().unwrap()),
        token: token.map(Arc::from),
    })
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    call_with(app, method, uri, body, None).await
}

async fn call_with(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let request = match body {
        Some(body) => request
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string())),
        None => request.body(Body::empty()),
    }
    .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

fn id(value: &Value) -> String {
    value["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn health_is_public() {
    let app = app(Some("secret"));
    let (status, body) = call(&app, "GET", "/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn token_is_required_when_configured() {
    let app = app(Some("secret"));
    let (status, body) = call(&app, "GET", "/v1/ns/nodes", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "unauthorized");
    let (status, _) = call_with(&app, "GET", "/v1/ns/nodes", None, Some("wrong")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call_with(&app, "GET", "/v1/ns/nodes", None, Some("secret")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn node_and_edge_lifecycle() {
    let app = app(None);
    let (status, ana) = call(
        &app,
        "POST",
        "/v1/vault/nodes",
        Some(json!({ "labels": ["Person"], "props": { "name": "Ana" } })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, note) = call(
        &app,
        "POST",
        "/v1/vault/nodes",
        Some(json!({ "labels": ["Note"], "props": { "title": "Rust" }, "text": "ownership and borrowing" })),
    )
    .await;
    let (ana, note) = (id(&ana), id(&note));

    let (status, edge) = call(
        &app,
        "POST",
        "/v1/vault/edges",
        Some(json!({ "from": ana, "label": "wrote", "to": note, "valid_from": "2024-01-01" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let edge = id(&edge);

    let (status, body) = call(&app, "GET", &format!("/v1/vault/nodes/{note}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["text"], "ownership and borrowing");
    assert_eq!(body["labels"], json!(["Note"]));

    let uri =
        format!("/v1/vault/nodes/{ana}/neighbors?direction=out&label=wrote&valid_at=2023-06-01");
    let (_, body) = call(&app, "GET", &uri, None).await;
    assert_eq!(body.as_array().unwrap().len(), 0, "not valid yet");
    let uri = format!("/v1/vault/nodes/{ana}/neighbors?direction=out&valid_at=2024-06-01");
    let (_, body) = call(&app, "GET", &uri, None).await;
    assert_eq!(body[0]["node"], note.as_str());
    assert_eq!(body[0]["edge"]["label"], "wrote");

    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/v1/vault/nodes/{note}"),
        Some(json!({ "props": { "title": null, "pinned": true }, "add_labels": ["Pinned"], "text": null })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["props"], json!({ "pinned": true }));
    assert_eq!(body["labels"], json!(["Note", "Pinned"]));
    assert!(body.get("text").is_none());

    let (status, body) = call(
        &app,
        "POST",
        &format!("/v1/vault/edges/{edge}/invalidate"),
        Some(json!({ "at": "2025-01-01T00:00:00Z" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["valid_to"], 1_735_689_600_000_000i64);

    let (status, _) = call(&app, "DELETE", &format!("/v1/vault/nodes/{ana}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = call(&app, "GET", &format!("/v1/vault/edges/{edge}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");
}

#[tokio::test]
async fn put_upserts_with_client_ids() {
    let app = app(None);
    let uri = "/v1/vault/nodes/0190f5c2-7a3e-7cde-8f01-23456789abcd";
    let body = json!({ "labels": ["Note"], "text": "first" });
    let (status, created) = call(&app, "PUT", uri, Some(body)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["created"], true);
    let (status, updated) = call(
        &app,
        "PUT",
        uri,
        Some(json!({ "labels": ["Note"], "text": "second" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["created"], false);
    let (_, node) = call(&app, "GET", uri, None).await;
    assert_eq!(node["text"], "second");
}

#[tokio::test]
async fn bad_input_is_reported_as_json() {
    let app = app(None);
    let (status, body) = call(
        &app,
        "POST",
        "/v1/vault/nodes",
        Some(json!({ "labels": [] })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid");

    let (status, body) = call(
        &app,
        "POST",
        "/v1/vault/nodes",
        Some(json!({ "labelz": ["x"] })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "bad_request");

    let (status, _) = call(&app, "GET", "/v1/vault/nodes/not-a-uuid", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn batch_is_atomic_and_resolves_refs() {
    let app = app(None);
    let note_id = "0190f5c2-7a3e-7cde-8f01-000000000001";
    let ops = json!({ "ops": [
        { "op": "upsert_node", "ref": "$note", "node": { "id": note_id, "labels": ["Note"], "props": { "title": "Index" }, "text": "graph engines" } },
        { "op": "create_node", "ref": "$tag", "node": { "labels": ["Tag"], "props": { "name": "rust" } } },
        { "op": "create_edge", "edge": { "from": "$note", "label": "tagged", "to": "$tag" } },
        { "op": "delete_edges_from", "from": "$note", "label": "links_to" }
    ]});
    let (status, body) = call(&app, "POST", "/v1/vault/batch", Some(ops)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["refs"]["$note"], note_id);
    assert_eq!(body["results"][3]["deleted"], 0);
    let tag = body["refs"]["$tag"].as_str().unwrap().to_owned();

    // A failing operation rolls back the whole batch.
    let ops = json!({ "ops": [
        { "op": "create_node", "node": { "labels": ["Note"], "text": "should not stay" } },
        { "op": "create_edge", "edge": { "from": "$missing", "label": "x", "to": tag } }
    ]});
    let (status, body) = call(&app, "POST", "/v1/vault/batch", Some(ops)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("operation 1:")
    );
    let (_, nodes) = call(&app, "GET", "/v1/vault/nodes", None).await;
    assert_eq!(nodes["items"].as_array().unwrap().len(), 2);
    assert!(nodes["next"].is_null());

    let (_, tagged) = call(&app, "GET", "/v1/vault/nodes?label=Tag", None).await;
    assert_eq!(tagged["items"][0]["id"], tag.as_str());
}

#[tokio::test]
async fn listings_are_paged() {
    let app = app(None);
    let ops: Vec<Value> = (0..5)
        .map(|i| json!({ "op": "create_node", "ref": format!("$n{i}"), "node": { "labels": ["N"] } }))
        .chain((0..4).map(|i| {
            json!({ "op": "create_edge", "edge": { "from": format!("$n{i}"), "label": "next", "to": format!("$n{}", i + 1) } })
        }))
        .collect();
    call(&app, "POST", "/v1/p/batch", Some(json!({ "ops": ops }))).await;

    let (_, first) = call(&app, "GET", "/v1/p/nodes?limit=2", None).await;
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    let next = first["next"].as_str().unwrap().to_owned();
    assert_eq!(next, first["items"][1]["id"].as_str().unwrap());
    let (_, second) = call(
        &app,
        "GET",
        &format!("/v1/p/nodes?limit=2&after={next}"),
        None,
    )
    .await;
    assert_ne!(second["items"][0]["id"], first["items"][0]["id"]);
    let (_, last) = call(
        &app,
        "GET",
        &format!("/v1/p/nodes?limit=4&after={next}"),
        None,
    )
    .await;
    assert_eq!(last["items"].as_array().unwrap().len(), 3);
    assert!(last["next"].is_null());

    let (_, edges) = call(&app, "GET", "/v1/p/edges?label=next&limit=10", None).await;
    assert_eq!(edges["items"].as_array().unwrap().len(), 4);
    let (_, none) = call(&app, "GET", "/v1/p/edges?label=other", None).await;
    assert!(none["items"].as_array().unwrap().is_empty());
    let (status, _) = call(&app, "GET", "/v1/p/nodes?after=nope", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn administration_endpoints() {
    let app = app(None);
    let ops = json!({ "ops": [
        { "op": "create_node", "ref": "$a", "node": { "labels": ["Person"], "props": { "name": "Ana" }, "text": "writes Rust", "embedding": [1.0, 0.0] } },
        { "op": "create_node", "ref": "$b", "node": { "labels": ["Person"], "text": "reads Go" } },
        { "op": "create_edge", "edge": { "from": "$a", "label": "knows", "to": "$b" } }
    ]});
    call(&app, "POST", "/v1/team/batch", Some(ops)).await;
    call(
        &app,
        "POST",
        "/v1/solo/nodes",
        Some(json!({ "labels": ["Note"] })),
    )
    .await;

    let (_, namespaces) = call(&app, "GET", "/v1/namespaces", None).await;
    assert_eq!(
        namespaces,
        json!([{ "name": "solo", "nodes": 1, "edges": 0 }, { "name": "team", "nodes": 2, "edges": 1 }])
    );
    let (_, stats) = call(&app, "GET", "/v1/team/stats", None).await;
    assert_eq!(stats["node_labels"]["Person"], 2);
    assert_eq!(stats["vector_dimension"], 2);

    let (_, report) = call(&app, "GET", "/v1/team/check", None).await;
    assert_eq!(report["problems"], json!([]));
    let (status, report) = call(&app, "POST", "/v1/team/rebuild", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(report["texts"], 2);

    // Export one namespace and import it into another server.
    let request = Request::builder()
        .uri("/v1/export?ns=team")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/x-ndjson"
    );
    let dump = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(String::from_utf8(dump.to_vec()).unwrap().lines().count(), 4);

    let other = self::app(None);
    let request = Request::builder()
        .method("POST")
        .uri("/v1/import")
        .body(Body::from(dump))
        .unwrap();
    let response = other.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let (_, namespaces) = call(&other, "GET", "/v1/namespaces", None).await;
    assert_eq!(
        namespaces,
        json!([{ "name": "team", "nodes": 2, "edges": 1 }])
    );
    let (_, hits) = call(
        &other,
        "POST",
        "/v1/team/search/text",
        Some(json!({ "query": "rust" })),
    )
    .await;
    assert_eq!(hits.as_array().unwrap().len(), 1);

    let request = Request::builder()
        .method("POST")
        .uri("/v1/import")
        .body(Body::from("not an export"))
        .unwrap();
    let response = other.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn search_recall_and_context() {
    let app = app(None);
    let ops = json!({ "ops": [
        { "op": "create_node", "ref": "$ana", "node": { "labels": ["Person"], "props": { "name": "Ana" } } },
        { "op": "create_node", "ref": "$note", "node": { "labels": ["Note"], "props": { "title": "Rust notes" },
          "text": "the borrow checker enforces ownership", "embedding": [1.0, 0.0] } },
        { "op": "create_node", "node": { "labels": ["Note"], "text": "pasta recipes", "embedding": [0.0, 1.0] } },
        { "op": "create_edge", "edge": { "from": "$ana", "label": "wrote", "to": "$note" } }
    ]});
    let (status, body) = call(&app, "POST", "/v1/agent/batch", Some(ops)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let note = body["refs"]["$note"].as_str().unwrap().to_owned();

    let (_, hits) = call(
        &app,
        "POST",
        "/v1/agent/search/text",
        Some(json!({ "query": "borrow" })),
    )
    .await;
    assert_eq!(hits[0]["node"], note.as_str());
    let (_, hits) = call(
        &app,
        "POST",
        "/v1/agent/search/vector",
        Some(json!({ "embedding": [0.9, 0.1], "limit": 1 })),
    )
    .await;
    assert_eq!(hits[0]["node"], note.as_str());

    let (_, results) = call(
        &app,
        "POST",
        "/v1/agent/recall",
        Some(json!({ "text": "ownership" })),
    )
    .await;
    assert_eq!(results[0]["node"]["id"], note.as_str());
    assert_eq!(results[0]["signals"][0]["kind"], "text");
    assert_eq!(results[1]["node"]["props"]["name"], "Ana");
    assert_eq!(results[1]["signals"][0]["kind"], "graph");

    let (_, results) = call(
        &app,
        "POST",
        "/v1/agent/recall",
        Some(json!({ "text": "ownership", "expand": false })),
    )
    .await;
    assert_eq!(results.as_array().unwrap().len(), 1);

    let (status, context) = call(
        &app,
        "POST",
        "/v1/agent/context",
        Some(json!({ "text": "ownership", "budget": 200 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let rendered = context["rendered"].as_str().unwrap();
    assert!(rendered.contains("[1] Rust notes (Note)"), "{rendered}");
    assert!(rendered.contains("- Ana wrote Rust notes"), "{rendered}");
    assert!(context["tokens"].as_u64().unwrap() <= 200);

    let (status, body) = call(
        &app,
        "POST",
        "/v1/agent/recall",
        Some(json!({ "limit": 3 })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn traversals() {
    let app = app(None);
    let ops = json!({ "ops": [
        { "op": "create_node", "ref": "$a", "node": { "labels": ["N"] } },
        { "op": "create_node", "ref": "$b", "node": { "labels": ["N"] } },
        { "op": "create_node", "ref": "$c", "node": { "labels": ["N"] } },
        { "op": "create_edge", "edge": { "from": "$a", "label": "next", "to": "$b" } },
        { "op": "create_edge", "edge": { "from": "$b", "label": "next", "to": "$c" } }
    ]});
    let (_, body) = call(&app, "POST", "/v1/g/batch", Some(ops)).await;
    let (a, c) = (body["refs"]["$a"].clone(), body["refs"]["$c"].clone());

    let (_, path) = call(
        &app,
        "POST",
        "/v1/g/path",
        Some(json!({ "from": a, "to": c, "direction": "out" })),
    )
    .await;
    assert_eq!(path["path"]["nodes"].as_array().unwrap().len(), 3);
    let (_, path) = call(
        &app,
        "POST",
        "/v1/g/path",
        Some(json!({ "from": c, "to": a, "direction": "out" })),
    )
    .await;
    assert!(path["path"].is_null());

    let (_, hops) = call(
        &app,
        "POST",
        "/v1/g/k-hop",
        Some(json!({ "start": a, "depth": 2, "direction": "out" })),
    )
    .await;
    assert_eq!(hops.as_array().unwrap().len(), 2);

    let (_, sub) = call(
        &app,
        "POST",
        "/v1/g/subgraph",
        Some(json!({ "seeds": [a], "depth": 1 })),
    )
    .await;
    assert_eq!(sub["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(sub["edges"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn queries_read_and_write() {
    let app = app(None);
    let (status, body) = call(
        &app,
        "POST",
        "/v1/g/query",
        Some(json!({ "query": "CREATE (a:Person {name: $name})-[:knows]->(b:Person {name: 'Bob'}) RETURN a.name",
                     "params": { "name": "Ana" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["rows"], json!([["Ana"]]));
    assert_eq!(body["stats"]["nodes_created"], 2);

    let (_, body) = call(
        &app,
        "POST",
        "/v1/g/query",
        Some(json!({ "query": "MATCH (a)-[r:knows]->(b) RETURN a.name, type(r), b" })),
    )
    .await;
    assert_eq!(body["columns"], json!(["a.name", "type(r)", "b"]));
    assert_eq!(body["rows"][0][2]["props"]["name"], "Bob");

    let (status, body) = call(
        &app,
        "POST",
        "/v1/g/query",
        Some(json!({ "query": "MATCH (n RETURN n" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_query");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("line 1, column")
    );
}

#[tokio::test]
async fn studio_and_cors_options() {
    use grafeno_server::http::{Options, router_with};
    let state = || AppState {
        db: Arc::new(Grafeno::in_memory().unwrap()),
        token: Some(Arc::from("secret")),
    };
    let request = |uri: &str| Request::builder().uri(uri).body(Body::empty()).unwrap();

    // The Studio is public (the API it calls needs the token).
    let app = router_with(state(), &Options::default());
    let response = app.clone().oneshot(request("/")).await.unwrap();
    assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
    assert_eq!(response.headers()[header::LOCATION], "/studio/");
    let response = app.clone().oneshot(request("/studio/")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    let response = app
        .clone()
        .oneshot(request("/studio/explorer/some-client-route"))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "unknown paths get the app"
    );
    if grafeno_server::studio::INCLUDED {
        let response = app
            .clone()
            .oneshot(request("/studio/assets/missing.js"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    // No CORS headers unless origins are configured.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health")
                .header(header::ORIGIN, "http://elsewhere")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        !response
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
    );

    let app = router_with(
        state(),
        &Options {
            studio: false,
            cors_origins: vec!["http://studio.example".into()],
        },
    );
    assert_eq!(
        app.clone()
            .oneshot(request("/studio/"))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let preflight = Request::builder()
        .method("OPTIONS")
        .uri("/v1/namespaces")
        .header(header::ORIGIN, "http://studio.example")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(preflight).await.unwrap();
    assert_eq!(
        response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "http://studio.example"
    );
    let other = Request::builder()
        .uri("/health")
        .header(header::ORIGIN, "http://evil.example")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(other).await.unwrap();
    assert!(
        !response
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
    );
}

#[tokio::test]
async fn memory_endpoints() {
    let app = app(None);
    let ops = json!({ "ops": [
        { "op": "create_node", "ref": "$ana", "node": { "labels": ["Person"], "props": { "name": "Ana García" } } },
        { "op": "create_node", "ref": "$dup", "node": { "labels": ["Person"], "props": { "name": "ana garcia" } } },
        { "op": "create_node", "ref": "$m1", "node": { "labels": ["Memory"], "text": "Ana likes tea" } },
        { "op": "create_node", "ref": "$m2", "node": { "labels": ["Memory"], "text": "Ana moved to Lima" } },
        { "op": "create_node", "ref": "$m3", "node": { "labels": ["Memory"], "text": "Ana works at Acme" } },
        { "op": "create_edge", "edge": { "from": "$m1", "label": "about", "to": "$ana" } },
        { "op": "create_edge", "edge": { "from": "$m2", "label": "about", "to": "$ana" } },
        { "op": "create_edge", "edge": { "from": "$m3", "label": "about", "to": "$dup" } }
    ]});
    let (status, body) = call(&app, "POST", "/v1/agent/batch", Some(ops)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let refs = |name: &str| body["refs"][name].as_str().unwrap().to_owned();
    let (ana, dup, m1) = (refs("$ana"), refs("$dup"), refs("$m1"));

    let (status, found) = call(
        &app,
        "GET",
        "/v1/agent/resolve?name=ANA%20GARCIA&label=Person",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(found.as_array().unwrap().len(), 2);

    let (_, pairs) = call(&app, "GET", "/v1/agent/duplicates", None).await;
    assert_eq!(pairs[0]["score"], 1.0, "{pairs}");

    let (status, report) = call(
        &app,
        "POST",
        "/v1/agent/merge",
        Some(json!({ "keep": ana, "merge": dup })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["edges_moved"], 1);
    let (_, pairs) = call(&app, "GET", "/v1/agent/duplicates", None).await;
    assert!(pairs.as_array().unwrap().is_empty());

    // Importance through PATCH, accesses through touch and recall.
    let (status, node) = call(
        &app,
        "PATCH",
        &format!("/v1/agent/nodes/{m1}"),
        Some(json!({ "importance": 0.9 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{node}");
    let (status, _) = call(
        &app,
        "POST",
        "/v1/agent/touch",
        Some(json!({ "ids": [m1] })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, results) = call(
        &app,
        "POST",
        "/v1/agent/recall",
        Some(json!({ "text": "tea", "touch": true, "expand": false })),
    )
    .await;
    assert_eq!(results[0]["node"]["id"], m1.as_str());
    assert!(
        results[0]["signals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["kind"] == "memory")
    );
    let (_, node) = call(&app, "GET", &format!("/v1/agent/nodes/{m1}"), None).await;
    assert_eq!(node["memory"]["accesses"], 2, "{node}");
    assert!((node["memory"]["importance"].as_f64().unwrap() - 0.9).abs() < 1e-6);
    let (status, _) = call(
        &app,
        "PATCH",
        &format!("/v1/agent/nodes/{m1}"),
        Some(json!({ "importance": 3 })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, weak) = call(&app, "GET", "/v1/agent/weak?threshold=0.2", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(weak.as_array().unwrap().is_empty(), "{weak}");

    let (_, groups) = call(
        &app,
        "GET",
        "/v1/agent/consolidation?older_than_days=0",
        None,
    )
    .await;
    assert_eq!(groups.as_array().unwrap().len(), 1, "{groups}");
    assert_eq!(groups[0]["entity"]["id"], ana.as_str());
    let sources: Vec<Value> = groups[0]["memories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].clone())
        .collect();
    assert_eq!(sources.len(), 3);

    let (status, summary) = call(
        &app,
        "POST",
        "/v1/agent/summaries",
        Some(json!({ "text": "Ana likes tea, lives in Lima and works at Acme.", "sources": sources, "about": ana })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{summary}");
    let (_, results) = call(
        &app,
        "POST",
        "/v1/agent/recall",
        Some(json!({ "text": "Lima", "expand": false })),
    )
    .await;
    assert_eq!(results.as_array().unwrap().len(), 1);
    assert_eq!(results[0]["node"]["id"], summary["id"]);
    let (_, results) = call(
        &app,
        "POST",
        "/v1/agent/recall",
        Some(
            json!({ "text": "Lima", "expand": false, "include_archived": true, "ranking": false }),
        ),
    )
    .await;
    assert_eq!(results.as_array().unwrap().len(), 2);

    let (status, archived) = call(
        &app,
        "POST",
        "/v1/agent/archive",
        Some(json!({ "ids": [summary["id"]] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(archived["archived"], 1);
    let (_, check) = call(&app, "GET", "/v1/agent/check", None).await;
    assert_eq!(check["problems"], json!([]), "{check}");
}
