//! Phase 14.4: the leak matrix. A reader whose role reads the namespace
//! but not `Salary` nodes, not the `email` of people, and only the people
//! of its own team (a record policy) asks every surface of the server; no
//! answer may carry a hidden node, edge or property: listings, reads by
//! id, traversals, searches, recall and context, GQ, algorithms, exports,
//! the change log, statistics and the namespace list. Features that read
//! the whole namespace refuse it. Writes may not touch what it cannot see
//! or what its role may not write.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use grafeno::Grafeno;
use grafeno_server::http::{AppState, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const OWNER: &str = "owner-secret";

async fn call(
    app: &Router,
    key: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {key}"));
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
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn json_of(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

async fn owner(app: &Router, method: &str, uri: &str, body: Value) -> Value {
    let (code, text) = call(app, OWNER, method, uri, Some(body)).await;
    assert!(code.is_success(), "{method} {uri}: {code} {text}");
    json_of(&text)
}

async fn gq(app: &Router, key: &str, query: &str) -> (StatusCode, String) {
    call(
        app,
        key,
        "POST",
        "/v1/crm/query",
        Some(json!({ "query": query })),
    )
    .await
}

struct World {
    app: Router,
    rita: String,
    ana: String,
    bob: String,
    sal: String,
}

impl World {
    /// The answer must not carry anything hidden from the reader.
    fn clean(&self, what: &str, text: &str) {
        for secret in [
            self.bob.as_str(),
            self.sal.as_str(),
            "ana@example.com",
            "bob@example.com",
            "Bob",
            "salary",
        ] {
            assert!(!text.contains(secret), "{what} leaks {secret:?}: {text}");
        }
    }
}

async fn world() -> World {
    let app = router(AppState::new(
        Arc::new(Grafeno::in_memory().unwrap()),
        Some(OWNER),
    ));
    let node = |labels: &[&str], props: Value, text: &str, embedding: [f32; 3]| json!({ "labels": labels, "props": props, "text": text, "embedding": embedding });
    let ana = owner(
        &app,
        "POST",
        "/v1/crm/nodes",
        node(
            &["Person"],
            json!({ "name": "Ana", "email": "ana@example.com", "team": "red" }),
            "ana works on project zeta",
            [1.0, 0.0, 0.0],
        ),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let bob = owner(
        &app,
        "POST",
        "/v1/crm/nodes",
        node(
            &["Person"],
            json!({ "name": "Bob", "email": "bob@example.com", "team": "blue" }),
            "bob works on project zeta",
            [0.9, 0.1, 0.0],
        ),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let sal = owner(
        &app,
        "POST",
        "/v1/crm/nodes",
        node(
            &["Salary"],
            json!({ "amount": 100, "note": "salary of ana" }),
            "salary for project zeta",
            [1.0, 0.0, 0.1],
        ),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    for (from, to, label) in [
        (&ana, &bob, "knows"),
        (&ana, &sal, "paid"),
        (&bob, &sal, "paid"),
    ] {
        owner(
            &app,
            "POST",
            "/v1/crm/edges",
            json!({ "label": label, "from": from, "to": to }),
        )
        .await;
    }
    for statement in [
        "CREATE ROLE crm_reader",
        "GRANT read, export, algorithms, create, update, delete ON NAMESPACE crm TO crm_reader",
        "DENY read ON NODES Salary IN crm TO crm_reader",
        "DENY write ON NODES Salary IN crm TO crm_reader",
        "DENY read ON PROPERTY Person.email IN crm TO crm_reader",
        "RESTRICT crm_reader ON NAMESPACE crm NODES Person FOR READS WHERE team = $account.team",
        "CREATE SERVICE ACCOUNT rita",
        "ALTER USER rita SET ATTRIBUTE team = 'red'",
        "GRANT ROLE crm_reader TO rita",
    ] {
        owner(
            &app,
            "POST",
            "/v1/default/query",
            json!({ "query": statement }),
        )
        .await;
    }
    let rita = owner(
        &app,
        "POST",
        "/v1/keys",
        json!({ "name": "rita-key", "account": "rita" }),
    )
    .await["secret"]
        .as_str()
        .unwrap()
        .to_owned();
    World {
        app,
        rita,
        ana,
        bob,
        sal,
    }
}

#[tokio::test]
async fn every_surface_hides_what_the_reader_may_not_see() {
    let w = world().await;
    let (app, rita) = (&w.app, w.rita.as_str());
    let get = |uri: String| async move { call(app, rita, "GET", &uri, None).await };
    let post =
        |uri: String, body: Value| async move { call(app, rita, "POST", &uri, Some(body)).await };

    // Listings and reads by id.
    for uri in [
        "/v1/crm/nodes".to_owned(),
        "/v1/crm/edges".to_owned(),
        format!("/v1/crm/nodes/{}", w.ana),
        format!("/v1/crm/nodes/{}/neighbors?direction=both", w.ana),
        "/v1/crm/stats".to_owned(),
        "/v1/namespaces".to_owned(),
        "/v1/crm/changes".to_owned(),
        "/v1/export?ns=crm".to_owned(),
        "/v1/crm/export.graphml".to_owned(),
        "/v1/crm/export/nodes.csv".to_owned(),
        "/v1/crm/export/edges.csv".to_owned(),
        format!("/v1/crm/nodes/{}/history/email", w.ana),
    ] {
        let (code, text) = get(uri.clone()).await;
        assert!(code.is_success(), "{uri}: {code} {text}");
        w.clean(&uri, &text);
    }
    let (_, ana) = get(format!("/v1/crm/nodes/{}", w.ana)).await;
    let ana = json_of(&ana);
    assert_eq!(ana["props"]["name"], "Ana", "{ana}");
    assert!(ana["props"].get("email").is_none(), "{ana}");
    for hidden in [&w.bob, &w.sal] {
        let (code, _) = get(format!("/v1/crm/nodes/{hidden}")).await;
        assert_eq!(code, StatusCode::NOT_FOUND, "a hidden node does not exist");
    }
    let (_, stats) = get("/v1/crm/stats".to_owned()).await;
    let stats = json_of(&stats);
    assert_eq!(
        (stats["nodes"].as_u64(), stats["edges"].as_u64()),
        (Some(1), Some(0)),
        "{stats}"
    );
    let (_, list) = get("/v1/namespaces".to_owned()).await;
    let crm = json_of(&list)
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["name"] == "crm")
        .cloned()
        .unwrap();
    assert_eq!(crm["nodes"], 1, "{crm}");

    // Traversals, searches, recall and context.
    for (uri, body) in [
        ("read-many", json!({ "nodes": [w.ana, w.bob, w.sal] })),
        (
            "k-hop",
            json!({ "start": w.ana, "depth": 3, "direction": "both" }),
        ),
        (
            "path",
            json!({ "from": w.ana, "to": w.sal, "direction": "both" }),
        ),
        (
            "subgraph",
            json!({ "seeds": [w.ana], "depth": 2, "direction": "both" }),
        ),
        (
            "neighborhoods",
            json!({ "seeds": [w.ana], "direction": "both" }),
        ),
        ("search/text", json!({ "query": "zeta", "limit": 10 })),
        (
            "search/vector",
            json!({ "embedding": [1.0, 0.0, 0.0], "limit": 10 }),
        ),
        (
            "recall",
            json!({ "text": "zeta", "embedding": [1.0, 0.0, 0.0], "limit": 10 }),
        ),
        ("context", json!({ "text": "zeta", "limit": 10 })),
        ("algo/pageRank", json!({})),
    ] {
        let (code, text) = post(format!("/v1/crm/{uri}"), body).await;
        assert!(code.is_success(), "{uri}: {code} {text}");
        w.clean(uri, &text);
    }
    let (_, found) = post(
        "/v1/crm/search/text".to_owned(),
        json!({ "query": "zeta", "limit": 10 }),
    )
    .await;
    assert_eq!(
        json_of(&found).as_array().map(Vec::len),
        Some(1),
        "only Ana: {found}"
    );
    let (_, path) = post(
        "/v1/crm/path".to_owned(),
        json!({ "from": w.ana, "to": w.bob, "direction": "both" }),
    )
    .await;
    assert_eq!(json_of(&path)["path"], Value::Null);

    // GQ, its procedures and algorithms.
    for query in [
        "MATCH (n) RETURN n",
        "MATCH (a)-[r]-(b) RETURN a, r, b",
        "MATCH (n) RETURN count(n) AS c",
        "MATCH (n:Person) WHERE n.email IS NOT NULL RETURN n",
        "MATCH (n) WHERE n.team = 'blue' RETURN n",
        "MATCH p = (a)-[*1..4]-(b) RETURN p",
        "CALL db.labels() YIELD label, count RETURN label, count",
        "CALL db.propertyKeys() YIELD key RETURN key",
        "CALL db.search('zeta', 10) YIELD node RETURN node",
        "CALL algo.pageRank() YIELD node, score RETURN node, score",
        "MATCH (n) RETURN n.@text",
        "EXPLAIN MATCH (n:Salary) RETURN n",
        "SHOW NAMESPACES",
    ] {
        let (code, text) = gq(app, rita, query).await;
        assert!(code.is_success(), "{query}: {code} {text}");
        w.clean(query, &text);
    }
    let (_, count) = gq(app, rita, "MATCH (n) RETURN count(n) AS c").await;
    assert_eq!(json_of(&count)["rows"], json!([[1]]));

    // What reads the whole namespace refuses this reader.
    for (method, uri) in [
        ("GET", "/v1/crm/check"),
        ("GET", "/v1/crm/duplicates"),
        ("GET", "/v1/crm/communities"),
        ("GET", "/v1/crm/resolve?name=Bob"),
        ("GET", "/v1/crm/weak"),
        ("GET", "/v1/crm/review"),
        ("GET", "/v1/crm/merges"),
    ] {
        let (code, text) = call(app, rita, method, uri, None).await;
        assert_eq!(code, StatusCode::FORBIDDEN, "{uri}: {text}");
        w.clean(uri, &text);
    }
    let (code, text) = gq(
        app,
        rita,
        "CALL db.match('Person', {name: 'Bob'}, 5) YIELD node RETURN node",
    )
    .await;
    assert_eq!(code, StatusCode::FORBIDDEN, "{text}");
}

#[tokio::test]
async fn writes_stop_at_what_is_hidden_or_forbidden() {
    let w = world().await;
    let (app, rita) = (&w.app, w.rita.as_str());
    // What it cannot see does not exist for it.
    let (code, text) = call(
        app,
        rita,
        "PATCH",
        &format!("/v1/crm/nodes/{}", w.bob),
        Some(json!({ "props": { "x": 1 } })),
    )
    .await;
    // The answer is the one for a node that does not exist (it names only
    // the id the caller sent).
    assert_eq!(code, StatusCode::NOT_FOUND, "{text}");
    assert_eq!(
        json_of(&text)["error"]["message"],
        format!("node {} not found", w.bob)
    );
    let (code, _) = call(
        app,
        rita,
        "DELETE",
        &format!("/v1/crm/nodes/{}", w.sal),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    let (code, _) = call(
        app,
        rita,
        "POST",
        "/v1/crm/edges",
        Some(json!({ "label": "knows", "from": w.ana, "to": w.bob })),
    )
    .await;
    assert_eq!(code, StatusCode::NOT_FOUND, "an end it cannot see");
    // Salary may not be written.
    let (code, text) = call(
        app,
        rita,
        "POST",
        "/v1/crm/nodes",
        Some(json!({ "labels": ["Salary"], "props": { "amount": 1 } })),
    )
    .await;
    assert_eq!(code, StatusCode::FORBIDDEN, "{text}");
    assert!(text.contains("`create`"), "{text}");
    // A statement that would touch a hidden record skips it; one that
    // would write what it may not changes nothing.
    let (code, text) = gq(app, rita, "MATCH (n) SET n.seen = true").await;
    assert_eq!(code, StatusCode::OK, "{text}");
    let (code, text) = gq(app, rita, "CREATE (:Salary {amount: 5})").await;
    assert_eq!(code, StatusCode::FORBIDDEN, "{text}");
    // Ana is changed; Bob and the salary are as they were.
    let all = owner(
        app,
        "POST",
        "/v1/crm/query",
        json!({ "query": "MATCH (n) RETURN n.name, n.seen, labels(n) ORDER BY n.name" }),
    )
    .await;
    let rows = all["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "nothing created: {all}");
    assert_eq!(rows[0], json!(["Ana", true, ["Person"]]));
    assert_eq!(rows[1], json!(["Bob", null, ["Person"]]));
    assert_eq!(rows[2], json!([null, null, ["Salary"]]));
    // Deleting everything it sees leaves what it does not.
    let (code, text) = gq(app, rita, "MATCH (n:Person) DETACH DELETE n").await;
    assert_eq!(
        code,
        StatusCode::FORBIDDEN,
        "Ana has edges it cannot see: {text}"
    );
    w.clean("detach delete", &text);
}
