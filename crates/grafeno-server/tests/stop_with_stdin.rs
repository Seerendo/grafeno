//! `grafeno serve --stop-with-stdin`, as Grafeno Desktop starts the engine
//! for a local file (phase 15.4): closing its standard input stops it
//! cleanly, so the file is closed as on Ctrl+C and opens again at once.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn closing_stdin_stops_the_engine_cleanly() {
    let dir = std::env::temp_dir().join(format!("grafeno-stdin-{}", grafeno::NodeId::new()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("local.redb");
    let mut child = Command::new(env!("CARGO_BIN_EXE_grafeno"))
        .args([
            "serve",
            "--stop-with-stdin",
            "--addr",
            "127.0.0.1:0",
            "--token",
            "t",
        ])
        .arg("--db")
        .arg(&db)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    let url = loop {
        let line = lines
            .next()
            .expect("the engine said where it listens")
            .unwrap();
        if let Some(url) = line.strip_prefix("grafeno listening on ") {
            break url.to_owned();
        }
    };
    let health: serde_json::Value = ureq::get(format!("{url}/health"))
        .call()
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    assert_eq!(health["status"], "ok");

    // The program that started it goes away: its end of stdin closes.
    drop(child.stdin.take());
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "the engine kept running after stdin closed"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "it stopped as on Ctrl+C: {status:?}");
    // The file was closed cleanly: it opens at once, with nothing to repair.
    assert_eq!(
        grafeno::Grafeno::verify_file(&db).unwrap(),
        grafeno::FileIntegrity::Clean
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
