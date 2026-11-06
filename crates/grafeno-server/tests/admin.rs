//! Phase 14.3: managing access over HTTP. Access statements in GQ (users
//! with passwords who then sign in, roles, grants, roles to accounts,
//! expiry, dropping an account and its keys), refused for lack of the
//! privilege to manage, `EXPLAIN` naming the privilege a statement lacks,
//! and `/v1/admin/users`, `/v1/admin/roles` and `/v1/admin/grants`.

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
    key: Option<&str>,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(key) = key {
        request = request.header(header::AUTHORIZATION, format!("Bearer {key}"));
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
    let value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, value)
}

async fn gq(app: &Router, key: &str, ns: &str, query: &str) -> (StatusCode, Value) {
    call(
        app,
        Some(key),
        "POST",
        &format!("/v1/{ns}/query"),
        Some(json!({ "query": query })),
    )
    .await
}

async fn ok(app: &Router, key: &str, query: &str) -> Value {
    let (code, body) = gq(app, key, "default", query).await;
    assert_eq!(code, StatusCode::OK, "{query}: {body}");
    body
}

async fn key_of(app: &Router, account: &str) -> String {
    let (code, made) = call(
        app,
        Some(OWNER),
        "POST",
        "/v1/keys",
        Some(json!({ "name": format!("{account}-key"), "account": account })),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "{made}");
    made["secret"].as_str().unwrap().to_owned()
}

fn app() -> Router {
    router(AppState::new(
        Arc::new(Grafeno::in_memory().unwrap()),
        Some(OWNER),
    ))
}

#[tokio::test]
async fn access_statements_over_http() {
    let app = app();
    // A user made in GQ signs in with the password it was given.
    ok(
        &app,
        OWNER,
        "CREATE USER ana SET PASSWORD 'correct horse battery'",
    )
    .await;
    let (code, body) = gq(
        &app,
        OWNER,
        "default",
        "CREATE USER bea SET PASSWORD 'short'",
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "the policy applies: {body}");
    let (code, session) = call(
        &app,
        None,
        "POST",
        "/v1/auth/signin",
        Some(json!({ "name": "ana", "password": "correct horse battery" })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{session}");
    let ana = session["access_token"].as_str().unwrap().to_owned();

    // A role that may manage roles, and nothing else.
    ok(&app, OWNER, "CREATE ROLE role_admin").await;
    ok(&app, OWNER, "GRANT manage_roles ON SERVER TO role_admin").await;
    ok(&app, OWNER, "GRANT ROLE role_admin TO ana").await;
    let (_, me) = gq(&app, &ana, "anything", "SHOW CURRENT USER").await;
    assert_eq!(me["rows"][0][4], json!(["role_admin"]), "{me}");
    // It runs grants in a namespace it cannot read: they need no namespace.
    let (code, body) = gq(&app, &ana, "secret", "CREATE ROLE analyst").await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let (code, _) = gq(
        &app,
        &ana,
        "secret",
        "GRANT reader ON NAMESPACE 'team-*' TO analyst",
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    let (code, _) = gq(&app, &ana, "secret", "MATCH (n) RETURN n").await;
    assert_eq!(code, StatusCode::FORBIDDEN, "but it reads nothing there");
    // Managing accounts it may not.
    for query in [
        "CREATE SERVICE ACCOUNT bot",
        "SHOW USERS",
        "GRANT ROLE analyst TO ana",
        "DROP USER ana",
    ] {
        let (code, body) = gq(&app, &ana, "default", query).await;
        assert_eq!(code, StatusCode::FORBIDDEN, "{query}: {body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("manage_accounts"),
            "{body}"
        );
    }
    // EXPLAIN says what it lacks, and runs nothing.
    let (code, plan) = gq(&app, &ana, "default", "EXPLAIN DROP USER ana").await;
    assert_eq!(code, StatusCode::OK, "{plan}");
    let rows = plan["rows"].as_array().unwrap();
    assert_eq!(rows.last().unwrap()[1], "DENIED");
    assert!(
        rows.last().unwrap()[2]
            .as_str()
            .unwrap()
            .contains("`manage_accounts`")
    );

    // Roles reach accounts through GQ, and so do expiry and dropping.
    ok(&app, OWNER, "CREATE SERVICE ACCOUNT bot").await;
    ok(&app, OWNER, "GRANT ROLE analyst TO bot").await;
    let bot = key_of(&app, "bot").await;
    let (code, _) = call(&app, Some(&bot), "GET", "/v1/team-a/stats", None).await;
    assert_eq!(code, StatusCode::OK);
    let (code, body) = gq(&app, &bot, "team-a", "EXPLAIN MATCH (n) DETACH DELETE n").await;
    assert_eq!(code, StatusCode::OK);
    let last = body["rows"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last[1], "DENIED");
    assert!(last[2].as_str().unwrap().contains("`create`"), "{last}");
    let (code, _) = gq(&app, &bot, "team-a", "MATCH (n) DETACH DELETE n").await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    ok(&app, OWNER, "ALTER USER bot SET EXPIRES '2020-01-01'").await;
    let (code, body) = call(&app, Some(&bot), "GET", "/v1/team-a/stats", None).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "account_expired");
    ok(&app, OWNER, "ALTER USER bot SET EXPIRES null").await;
    let (code, _) = call(&app, Some(&bot), "GET", "/v1/team-a/stats", None).await;
    assert_eq!(code, StatusCode::OK);
    ok(&app, OWNER, "DROP USER bot").await;
    let (code, _) = call(&app, Some(&bot), "GET", "/v1/team-a/stats", None).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED, "its key went with it");

    // Suspending a user ends its sessions.
    ok(&app, OWNER, "ALTER USER ana SET STATUS SUSPENDED").await;
    let (code, _) = gq(&app, &ana, "default", "SHOW CURRENT USER").await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_endpoints() {
    let app = app();
    ok(&app, OWNER, "CREATE SERVICE ACCOUNT hr").await;
    ok(&app, OWNER, "CREATE ROLE people").await;
    ok(&app, OWNER, "GRANT manage_accounts ON SERVER TO people").await;
    ok(&app, OWNER, "GRANT ROLE people TO hr").await;
    let hr = key_of(&app, "hr").await;

    // /v1/admin/users is the accounts under the design's name.
    let (code, users) = call(&app, Some(&hr), "GET", "/v1/admin/users", None).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(users.as_array().unwrap().len(), 1);
    let (code, _) = call(
        &app,
        Some(&hr),
        "POST",
        "/v1/admin/users",
        Some(json!({ "name": "cleo", "kind": "service" })),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED);
    let (code, changed) = call(
        &app,
        Some(&hr),
        "PATCH",
        "/v1/admin/users/cleo",
        Some(json!({ "expires_at": "2030-01-01" })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{changed}");
    assert!(changed["expires_at"].is_number());
    let (_, changed) = call(
        &app,
        Some(&hr),
        "PATCH",
        "/v1/admin/users/cleo",
        Some(json!({ "expires_at": null })),
    )
    .await;
    assert!(changed.get("expires_at").is_none(), "{changed}");
    // Roles are not its to manage.
    let (code, _) = call(&app, Some(&hr), "GET", "/v1/admin/roles", None).await;
    assert_eq!(code, StatusCode::FORBIDDEN);

    // Grants, by the owner.
    let (code, _) = call(
        &app,
        Some(OWNER),
        "PUT",
        "/v1/admin/roles/analyst",
        Some(json!({})),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED);
    let (code, body) = call(
        &app,
        Some(OWNER),
        "POST",
        "/v1/admin/grants",
        Some(json!({ "privileges": ["read", "export"], "on": { "on": "namespace", "namespace": "crm" }, "to": ["analyst"] })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(
        body["rows"][0][0],
        "granted 2 privileges on namespace crm to analyst"
    );
    let (code, _) = call(
        &app,
        Some(OWNER),
        "POST",
        "/v1/admin/grants",
        Some(json!({ "privileges": ["read"], "on": { "on": "property", "namespace": "crm", "label": "Person", "property": "email" }, "to": ["analyst"], "deny": true })),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    let (code, list) = call(
        &app,
        Some(OWNER),
        "GET",
        "/v1/admin/grants?holder=analyst",
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(list["rows"].as_array().unwrap().len(), 3, "{list}");
    let (code, body) = call(
        &app,
        Some(OWNER),
        "POST",
        "/v1/admin/grants/revoke",
        Some(json!({ "privileges": ["export"], "on": { "on": "namespace", "namespace": "crm" }, "from": ["analyst"] })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(
        body["rows"][0][0],
        "revoked 1 privilege on namespace crm from analyst"
    );
    let (code, _) = call(
        &app,
        Some(OWNER),
        "POST",
        "/v1/admin/grants",
        Some(json!({ "privileges": ["backup"], "on": { "on": "namespace", "namespace": "crm" }, "to": ["analyst"] })),
    )
    .await;
    assert_eq!(
        code,
        StatusCode::BAD_REQUEST,
        "a privilege must fit its object"
    );

    // Roles to accounts: managing accounts may; to roles: managing roles.
    let (code, body) = call(
        &app,
        Some(&hr),
        "POST",
        "/v1/admin/grants/roles",
        Some(json!({ "roles": ["analyst"], "to": ["cleo"] })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let (code, body) = call(
        &app,
        Some(&hr),
        "POST",
        "/v1/admin/grants/roles",
        Some(json!({ "roles": ["analyst"], "to": ["people"] })),
    )
    .await;
    assert_eq!(code, StatusCode::FORBIDDEN, "{body}");
    let (code, _) = call(
        &app,
        Some(&hr),
        "POST",
        "/v1/admin/grants",
        Some(json!({ "privileges": ["read"], "on": { "on": "server" }, "to": ["analyst"] })),
    )
    .await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    let (_, cleo) = call(&app, Some(OWNER), "GET", "/v1/admin/users/cleo", None).await;
    assert_eq!(cleo["roles"], json!(["analyst"]));
    let (code, _) = call(
        &app,
        Some(&hr),
        "POST",
        "/v1/admin/grants/roles/revoke",
        Some(json!({ "roles": ["analyst"], "from": ["cleo"] })),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    let (_, cleo) = call(&app, Some(OWNER), "GET", "/v1/admin/users/cleo", None).await;
    assert_eq!(cleo["roles"], json!([]));
    // Its own privileges, anyone may read.
    let (code, mine) = call(&app, Some(&hr), "GET", "/v1/admin/grants?holder=hr", None).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(mine["rows"][0][2], "manage_accounts");
    let (code, _) = call(
        &app,
        Some(&hr),
        "GET",
        "/v1/admin/grants?holder=analyst",
        None,
    )
    .await;
    assert_eq!(
        code,
        StatusCode::OK,
        "managing accounts may read roles' privileges"
    );
}

/// Without a token, accounts close the server: the first account must be a
/// superuser, and the last active superuser cannot go, in GQ or over HTTP.
#[tokio::test]
async fn an_open_server_keeps_a_superuser() {
    let app = router(AppState::new(Arc::new(Grafeno::in_memory().unwrap()), None));
    let (code, body) = call(
        &app,
        None,
        "POST",
        "/v1/default/query",
        Some(json!({ "query": "CREATE SERVICE ACCOUNT bot" })),
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("keep an active superuser")
    );
    let (code, _) = call(
        &app,
        None,
        "POST",
        "/v1/accounts",
        Some(json!({ "name": "bot", "kind": "service" })),
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    // Still open: nothing was written.
    let (code, _) = call(&app, None, "GET", "/v1/namespaces", None).await;
    assert_eq!(code, StatusCode::OK);
    let (code, body) = call(
        &app,
        None,
        "POST",
        "/v1/default/query",
        Some(json!({ "query": "CREATE USER root SET PASSWORD 'correct horse battery' SET SUPERUSER true" })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let (code, _) = call(&app, None, "GET", "/v1/namespaces", None).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED, "closed now");
    let (_, session) = call(
        &app,
        None,
        "POST",
        "/v1/auth/signin",
        Some(json!({ "name": "root", "password": "correct horse battery" })),
    )
    .await;
    let root = session["access_token"].as_str().unwrap().to_owned();
    for query in [
        "ALTER USER root SET SUPERUSER false",
        "ALTER USER root SET STATUS SUSPENDED",
        "DROP USER root",
    ] {
        let (code, body) = gq(&app, &root, "default", query).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{query}: {body}");
    }
    let (code, _) = call(&app, Some(&root), "DELETE", "/v1/accounts/root", None).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
}
