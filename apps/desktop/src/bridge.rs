//! Transporte HTTP del escritorio, independiente de la ventana Tauri.

use axum::{Router, body::Body};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use http_body_util::BodyExt;
use serde::Serialize;
use std::collections::BTreeMap;
use tower::ServiceExt;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Reply {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_base64: Option<String>,
}

impl Reply {
    fn new(status: u16, headers: BTreeMap<String, String>, bytes: Vec<u8>) -> Self {
        match String::from_utf8(bytes) {
            Ok(body) => Self {
                status,
                headers,
                body,
                body_base64: None,
            },
            Err(error) => Self {
                status,
                headers,
                body: String::new(),
                body_base64: Some(STANDARD.encode(error.as_bytes())),
            },
        }
    }
}

/// The text body stays available to existing callers.
/// A binary body has a single representation: the two are never mixed.
pub(super) fn request_body(
    body: Option<String>,
    body_base64: Option<String>,
) -> Result<Vec<u8>, String> {
    match (body, body_base64) {
        (Some(_), Some(_)) => Err("provide body or bodyBase64, not both".into()),
        (None, Some(encoded)) => STANDARD
            .decode(encoded)
            .map_err(|error| format!("invalid base64 request body: {error}")),
        (body, None) => Ok(body.unwrap_or_default().into_bytes()),
    }
}

/// A request to the open database's router, in this process.
pub(super) async fn local_request(
    router: Router,
    method: &str,
    path: &str,
    headers: &BTreeMap<String, String>,
    body: Vec<u8>,
) -> Result<Reply, String> {
    let mut builder = axum::http::Request::builder()
        .method(method)
        .uri(if path.is_empty() { "/" } else { path });
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    let request = builder
        .body(Body::from(body))
        .map_err(|error| error.to_string())?;
    let response = router
        .oneshot(request)
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| Some((name.to_string(), value.to_str().ok()?.to_owned())))
        .collect();
    let bytes = response
        .into_body()
        .collect()
        .await
        .map_err(|error| error.to_string())?
        .to_bytes();
    Ok(Reply::new(status, headers, bytes.to_vec()))
}

/// A request to a Grafeno server, sent by the app (blocking).
pub(super) fn remote_request(
    agent: &ureq::Agent,
    method: &str,
    url: &str,
    headers: &BTreeMap<String, String>,
    body: Vec<u8>,
) -> Result<Reply, String> {
    let mut builder = ureq::http::Request::builder().method(method).uri(url);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    let request = builder.body(body).map_err(|error| error.to_string())?;
    let mut response = agent
        .run(request)
        .map_err(|error| format!("cannot reach {url}: {error}"))?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| Some((name.to_string(), value.to_str().ok()?.to_owned())))
        .collect();
    let body = response
        .body_mut()
        .with_config()
        .limit(1 << 30)
        .read_to_vec()
        .map_err(|error| format!("cannot read the response: {error}"))?;
    Ok(Reply::new(status, headers, body))
}

#[cfg(test)]
mod tests {
    use super::super::local_database::router_for;
    use super::*;
    use grafeno::Grafeno;
    use std::sync::Arc;

    async fn call(router: &Router, method: &str, path: &str, body: Option<&str>) -> Reply {
        let headers = BTreeMap::from([("content-type".to_owned(), "application/json".to_owned())]);
        local_request(
            router.clone(),
            method,
            path,
            &headers,
            body.unwrap_or_default().as_bytes().to_vec(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn the_bridge_reads_and_writes_in_process() {
        let router = router_for(Arc::new(Grafeno::in_memory().unwrap()), None);
        let health = call(&router, "GET", "/health", None).await;
        assert_eq!(health.status, 200);
        let created = call(&router, "POST", "/v1/namespaces", Some(r#"{"name":"lab"}"#)).await;
        assert_eq!(created.status, 201, "{}", created.body);
        let wrote = call(
            &router,
            "POST",
            "/v1/lab/query",
            Some(r#"{"query":"CREATE (:Person {name: 'Ana'})-[:knows]->(:Person {name: 'Bob'})"}"#),
        )
        .await;
        assert_eq!(wrote.status, 200, "{}", wrote.body);
        let read = call(
            &router,
            "POST",
            "/v1/lab/query",
            Some(r#"{"query":"MATCH (p:Person) RETURN count(p)"}"#),
        )
        .await;
        assert!(read.body.contains("[[2]]"), "{}", read.body);
        assert!(
            read.headers
                .get("content-type")
                .is_some_and(|t| t.contains("json"))
        );
        let deleted = call(&router, "DELETE", "/v1/namespaces/lab", None).await;
        assert_eq!(deleted.status, 200);
        let missing = call(&router, "GET", "/v1/namespaces/lab", None).await;
        assert_eq!(missing.status, 404);
        // The Studio is not served from the app's router.
        assert_eq!(call(&router, "GET", "/studio/", None).await.status, 404);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_bridge_reaches_a_server() {
        let router = router_for(Arc::new(Grafeno::in_memory().unwrap()), None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .new_agent();
        let url = format!("http://{address}");
        let reply = tokio::task::spawn_blocking(move || {
            let headers =
                BTreeMap::from([("content-type".to_owned(), "application/json".to_owned())]);
            let created = remote_request(
                &agent,
                "POST",
                &format!("{url}/v1/namespaces"),
                &headers,
                br#"{"name":"far"}"#.to_vec(),
            )
            .unwrap();
            let missing = remote_request(
                &agent,
                "GET",
                &format!("{url}/v1/namespaces/nope"),
                &headers,
                Vec::new(),
            )
            .unwrap();
            (created, missing)
        })
        .await
        .unwrap();
        assert_eq!(reply.0.status, 201, "{}", reply.0.body);
        assert_eq!(
            reply.1.status, 404,
            "errors come back as answers, not failures"
        );
    }

    fn bytes(reply: &Reply) -> Vec<u8> {
        match &reply.body_base64 {
            Some(body) => STANDARD.decode(body).unwrap(),
            None => reply.body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn binary_and_text_bodies_have_one_unambiguous_representation() {
        let binary: Vec<u8> = (0..=255).collect();
        let encoded = STANDARD.encode(&binary);
        assert_eq!(request_body(None, Some(encoded.clone())).unwrap(), binary);
        assert!(request_body(Some("text".into()), Some(encoded)).is_err());
        assert!(request_body(None, Some("no es base64!".into())).is_err());
        assert_eq!(
            request_body(Some("café".into()), None).unwrap(),
            "café".as_bytes()
        );
        let reply = Reply::new(200, BTreeMap::new(), binary.clone());
        assert_eq!(bytes(&reply), binary);
        let json = serde_json::to_value(&reply).unwrap();
        assert!(json["bodyBase64"].is_string());
        assert_eq!(json["body"], "");
        let text = Reply::new(200, BTreeMap::new(), "café".as_bytes().to_vec());
        assert_eq!(text.body, "café");
        assert!(
            serde_json::to_value(text)
                .unwrap()
                .get("bodyBase64")
                .is_none()
        );
    }

    fn echo_router() -> Router {
        Router::new().route(
            "/bytes",
            axum::routing::post(|body: axum::body::Bytes| async move {
                ([("content-type", "application/octet-stream")], body)
            }),
        )
    }

    #[tokio::test]
    async fn the_local_bridge_preserves_binary_requests_and_replies() {
        let binary: Vec<u8> = (0..=255).collect();
        let reply = local_request(
            echo_router(),
            "POST",
            "/bytes",
            &BTreeMap::new(),
            binary.clone(),
        )
        .await
        .unwrap();
        assert_eq!(reply.status, 200);
        assert_eq!(reply.headers["content-type"], "application/octet-stream");
        assert_eq!(bytes(&reply), binary);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_remote_bridge_preserves_binary_requests_and_replies() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server =
            tokio::spawn(async move { axum::serve(listener, echo_router()).await.unwrap() });
        tokio::task::spawn_blocking(move || {
            let agent = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build()
                .new_agent();
            let binary: Vec<u8> = (0..=255).collect();
            let reply = remote_request(
                &agent,
                "POST",
                &format!("http://{address}/bytes"),
                &BTreeMap::new(),
                binary.clone(),
            )
            .unwrap();
            assert_eq!(reply.status, 200);
            assert_eq!(reply.headers["content-type"], "application/octet-stream");
            assert_eq!(bytes(&reply), binary);
        })
        .await
        .unwrap();
        server.abort();
    }
}
