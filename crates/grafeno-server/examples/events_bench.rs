//! Streaming ingestion and change streams through the HTTP API (in
//! process, no network): NDJSON lines per second, the same lines again
//! (all duplicates), and how long a commit takes to reach a server-sent
//! events stream.
//!
//! `cargo run --release -p grafeno-server --example events_bench [lines]`

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, header};
use grafeno::{Grafeno, NewNode};
use grafeno_server::http::{AppState, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

#[tokio::main]
async fn main() {
    let lines: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(100_000);
    let db = Arc::new(Grafeno::in_memory().unwrap());
    let app = router(AppState::new(db.clone(), None));

    // People, then a fact between consecutive ones, by key.
    let people = lines / 2;
    let mut body = String::new();
    for i in 0..people {
        body.push_str(
            &json!({ "key": format!("p{i}"), "node": { "labels": ["Person"], "props": { "name": format!("Person {i}") }, "text": format!("person {i} likes topic {}", i % 100) } })
                .to_string(),
        );
        body.push('\n');
    }
    for i in 1..lines - people + 1 {
        body.push_str(
            &json!({ "key": format!("e{i}"), "edge": { "from": format!("$p{}", i - 1), "label": "knows", "to": format!("$p{}", i % people) } })
                .to_string(),
        );
        body.push('\n');
    }
    let bytes = body.len();
    let ingest = |body: String| {
        let app = app.clone();
        async move {
            let request = Request::builder()
                .method("POST")
                .uri("/v1/bench/ingest")
                .header(header::CONTENT_TYPE, "application/x-ndjson")
                .body(Body::from(body))
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            serde_json::from_slice::<Value>(&bytes).unwrap()
        }
    };
    println!(
        "{lines} NDJSON lines ({:.1} MiB): {people} nodes, then edges between them by key",
        bytes as f64 / 1048576.0
    );
    let started = Instant::now();
    let report = ingest(body.clone()).await;
    let first = started.elapsed().as_secs_f64();
    println!(
        "| first time | {first:.2} s | {:.0} lines/s | {} nodes, {} edges, {} failed |",
        lines as f64 / first,
        report["nodes"],
        report["edges"],
        report["failed"]
    );
    let started = Instant::now();
    let report = ingest(body).await;
    let again = started.elapsed().as_secs_f64();
    println!(
        "| sent again (a retry) | {again:.2} s | {:.0} lines/s | {} duplicates, {} written |",
        lines as f64 / again,
        report["duplicates"],
        report["nodes"].as_u64().unwrap_or(0) + report["edges"].as_u64().unwrap_or(0)
    );

    // Commit to event: a stream from the end of the log, then one commit
    // at a time.
    let last = db
        .begin_read()
        .unwrap()
        .changes("bench", 0, 0)
        .unwrap()
        .last;
    let request = Request::builder()
        .uri(format!("/v1/bench/changes?stream=true&since={last}"))
        .body(Body::empty())
        .unwrap();
    let mut stream = app.clone().oneshot(request).await.unwrap().into_body();
    let mut latencies = Vec::new();
    let mut text = String::new();
    for k in 0..300 {
        let started = Instant::now();
        db.write(|tx| tx.create_node("bench", NewNode::new("Note")).map(|_| ()))
            .unwrap();
        let wanted = format!("id: {}\n", last + k + 1);
        while !text.contains(&wanted) {
            let frame = tokio::time::timeout(Duration::from_secs(5), stream.frame())
                .await
                .expect("an event")
                .unwrap()
                .unwrap();
            if let Ok(data) = frame.into_data() {
                text.push_str(&String::from_utf8_lossy(&data));
            }
        }
        latencies.push(started.elapsed().as_secs_f64() * 1000.0);
        text.clear();
    }
    latencies.sort_by(f64::total_cmp);
    let at = |q: f64| latencies[((latencies.len() - 1) as f64 * q) as usize];
    println!(
        "| commit to server-sent event | p50 {:.2} ms | p99 {:.2} ms | 300 commits |",
        at(0.5),
        at(0.99)
    );
    assert!(db.begin_read().unwrap().check("bench").unwrap().is_ok());
}
