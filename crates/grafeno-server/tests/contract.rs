//! The HTTP API as a contract (phase 15.3): every route the server answers
//! is in the API tables of the server's README and every row there is a
//! route; `GET /v1/about` tells clients the version, the API version and
//! the capabilities; every response carries the API version.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use grafeno::Grafeno;
use grafeno_server::about::{API_HEADER, API_VERSION, API_VERSIONS, CAPABILITIES};
use grafeno_server::http::{AppState, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const HTTP_RS: &str = include_str!("../src/http.rs");
const README: &str = include_str!("../README.md");

/// Routes that are not the server's API (the note left where the Studio
/// used to be).
const NOT_API: &[&str] = &["/", "/studio", "/studio/", "/studio/{*path}"];

/// `(METHOD, /v1/path)` of every `.route(...)` in `http.rs`.
fn routes() -> BTreeSet<(String, String)> {
    let mut found = BTreeSet::new();
    let mut rest = HTTP_RS;
    while let Some(at) = rest.find(".route(") {
        let body_start = at + ".route(".len();
        let bytes = rest.as_bytes();
        let (mut depth, mut end) = (1, body_start);
        while depth > 0 {
            match bytes[end] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            end += 1;
        }
        let body = &rest[body_start..end - 1];
        let path = body.split('"').nth(1).expect("a route's path").to_owned();
        let full = if path == "/health" || NOT_API.contains(&path.as_str()) {
            path.clone()
        } else {
            format!("/v1{path}")
        };
        for method in ["get", "post", "put", "delete", "patch"] {
            let call = format!("{method}(");
            let named = body.match_indices(&call).any(|(i, _)| {
                i == 0
                    || !body.as_bytes()[i - 1].is_ascii_alphanumeric()
                        && body.as_bytes()[i - 1] != b'_'
            });
            if named {
                found.insert((method.to_uppercase(), full.clone()));
            }
        }
        rest = &rest[end..];
    }
    found
        .into_iter()
        .filter(|(_, path)| !NOT_API.contains(&path.as_str()))
        .collect()
}

/// `(METHOD, path)` of every row of the README's tables whose first cell
/// holds methods and whose second holds paths. A path without `/v1` or
/// `/health` in front continues the row's first path (`/export/edges.csv`
/// after `/v1/{ns}/export/nodes.csv`).
fn documented() -> BTreeSet<(String, String)> {
    let mut found = BTreeSet::new();
    for line in README.lines() {
        let cells: Vec<&str> = line.split(" | ").collect();
        if !line.starts_with("| `") || cells.len() < 2 {
            continue;
        }
        let methods: Vec<String> = cells[0]
            .trim_start_matches('|')
            .split(',')
            .map(|m| m.trim().trim_matches('`').to_owned())
            .collect();
        if !methods
            .iter()
            .all(|m| ["GET", "POST", "PUT", "DELETE", "PATCH"].contains(&m.as_str()))
        {
            continue;
        }
        let mut base = String::new();
        for path in cells[1].split('`').skip(1).step_by(2) {
            let path = path.split('?').next().unwrap_or(path);
            let full = if path.starts_with("/v1") || path == "/health" {
                base = path
                    .rsplit_once('/')
                    .map(|(b, _)| b.to_owned())
                    .unwrap_or_default();
                path.to_owned()
            } else {
                // Relative to the namespace: `/v1/{ns}` + `/export/edges.csv`.
                let ns = base.split('/').take(3).collect::<Vec<_>>().join("/");
                format!("{ns}{path}")
            };
            for method in &methods {
                found.insert((method.clone(), full.clone()));
            }
        }
    }
    found
}

#[test]
fn every_route_is_documented_and_every_documented_route_exists() {
    let routes = routes();
    let documented = documented();
    assert!(routes.len() > 100, "found only {} routes", routes.len());
    let undocumented: Vec<_> = routes.difference(&documented).collect();
    let missing: Vec<_> = documented.difference(&routes).collect();
    assert!(
        undocumented.is_empty() && missing.is_empty(),
        "routes not in the README: {undocumented:#?}\nREADME rows with no route: {missing:#?}"
    );
}

#[test]
fn every_capability_is_documented() {
    for capability in CAPABILITIES {
        assert!(
            README.contains(&format!("`{capability}`")),
            "the capability {capability:?} is not in the README"
        );
    }
    let unique: BTreeSet<_> = CAPABILITIES.iter().collect();
    assert_eq!(
        unique.len(),
        CAPABILITIES.len(),
        "a capability is listed twice"
    );
}

async fn call(
    app: &axum::Router,
    uri: &str,
    token: Option<&str>,
) -> (StatusCode, Option<String>, Value) {
    let mut request = Request::builder().uri(uri);
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let api = response
        .headers()
        .get(API_HEADER)
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, api, value)
}

#[tokio::test]
async fn about_settings_and_the_api_header() {
    let db = Arc::new(Grafeno::in_memory().unwrap());
    let app = router(
        AppState::new(db, Some("secret-token"))
            .with_settings(json!({ "addr": "127.0.0.1:7810", "token": true })),
    );

    // About is public: what a client checks before signing in.
    let (status, api, about) = call(&app, "/v1/about", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(api.as_deref(), Some(API_VERSION.to_string().as_str()));
    assert_eq!(about["product"], "grafeno");
    assert_eq!(about["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(about["api"]["version"], API_VERSION);
    assert_eq!(about["api"]["supported"], json!(API_VERSIONS));
    assert_eq!(about["layout"], grafeno::SCHEMA_VERSION);
    assert_eq!(about["auth"]["token"], true);
    assert_eq!(about["auth"]["open"], false);
    assert!(about["cluster"].is_null());
    let capabilities: Vec<&str> = about["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    assert_eq!(capabilities, CAPABILITIES);

    // Every answer carries the API version, refusals too.
    let (status, api, _) = call(&app, "/v1/namespaces", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(api.as_deref(), Some("1"));

    // The settings need the owner, and hold no secret.
    let (status, _, _) = call(&app, "/v1/settings", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, settings) = call(&app, "/v1/settings", Some("secret-token")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(settings["token"], true);
    assert!(!settings.to_string().contains("secret-token"));
}

#[test]
fn the_settings_view_hides_the_secrets() {
    let config: grafeno_server::config::Config = toml::from_str(
        r#"
        db = "/var/lib/grafeno/grafeno.redb"
        token = "the-owner-token"
        durability = "eventual"
        [cluster]
        id = 1
        addr = "10.0.0.1:7811"
        secret = "the-cluster-secret"
        voters = [1]
        peers = [{ id = 1, addr = "10.0.0.1:7811" }]
        "#,
    )
    .unwrap();
    let settings = grafeno_server::service::ServerSettings::resolve(
        Some(config),
        grafeno_server::service::Overrides::default(),
    );
    let view = settings.public_view();
    let text = view.to_string();
    assert!(!text.contains("the-owner-token") && !text.contains("the-cluster-secret"));
    assert_eq!(view["token"], true);
    assert_eq!(view["durability"], "eventual");
    assert_eq!(view["cluster"]["id"], 1);
    assert_eq!(view["copy_before_upgrade"], true);
}
