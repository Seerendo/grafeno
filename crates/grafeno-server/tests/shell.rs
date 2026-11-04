//! `grafeno shell` run as a program, without a terminal (scripts).

use std::io::Write;
use std::process::{Command, Output, Stdio};

use grafeno::NodeId;

fn shell(args: &[&str], stdin: Option<&str>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_grafeno"))
        .arg("shell")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    if let Some(script) = stdin {
        input.write_all(script.as_bytes()).unwrap();
    }
    drop(input);
    child.wait_with_output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

#[test]
fn runs_statements_and_meta_commands_on_a_file() {
    let dir = std::env::temp_dir().join(format!("grafeno-shell-{}", NodeId::new()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("shell.redb");
    let db = db.to_str().unwrap();

    let output = shell(
        &[
            "--db",
            db,
            "-n",
            "team",
            "-c",
            "CREATE (a:Person {name: 'Ana'})-[:knows]->(b:Person {name: 'Bob'});\n\
             MATCH (a)-[:knows]->(b) RETURN a.name, b;\n\\ns",
        ],
        None,
    );
    assert!(output.status.success(), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    assert!(
        stdout.contains("2 nodes created, 1 edge created"),
        "{stdout}"
    );
    assert!(stdout.contains(" a.name | b\n"), "{stdout}");
    assert!(
        stdout.contains(" Ana    | (Bob:Person)\n(1 row)"),
        "{stdout}"
    );
    assert!(stdout.contains(" team      | 2     | 1"), "{stdout}");

    // The same file, from a script file and from standard input, as JSON.
    let script = dir.join("count.gq");
    std::fs::write(&script, "MATCH (p:Person)\nRETURN count(*) AS people;\n").unwrap();
    let output = shell(
        &[
            "--db",
            db,
            "-n",
            "team",
            "--json",
            "-f",
            script.to_str().unwrap(),
        ],
        None,
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["rows"], serde_json::json!([[2]]));

    let output = shell(
        &["--db", db, "-n", "team"],
        Some("\\check\nMATCH (n:Person) RETURN n.name ORDER BY n.name;\n"),
    );
    let stdout = text(&output.stdout);
    assert!(stdout.contains("team: 2 nodes, 1 edges"), "{stdout}");
    assert!(stdout.contains("ok\n"), "{stdout}");
    assert!(stdout.contains(" Ana\n Bob\n(2 rows)"), "{stdout}");

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn failures_set_the_exit_code() {
    let output = shell(
        &["--memory", "-c", "RETURN 1;\nMATCH (n RETURN n;\nRETURN 2;"],
        None,
    );
    assert!(!output.status.success());
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("error: expected `)` to close the node pattern"),
        "{stderr}"
    );
    // Statements after the failing one still run.
    assert!(text(&output.stdout).contains(" 2\n(1 row)"));

    let output = shell(&["--memory", "-c", "\\nope"], None);
    assert!(!output.status.success());
    assert!(text(&output.stderr).contains("unknown command \\nope"));
}

#[test]
fn creates_and_drops_namespaces() {
    let dir = std::env::temp_dir().join(format!("grafeno-shell-{}", NodeId::new()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("ns.redb");
    let db = db.to_str().unwrap();
    let output = shell(
        &[
            "--db",
            db,
            "-c",
            "\\ns create demo\n\\ns create demo2\n\\ns\n\\ns drop demo\n\\ns drop demo --yes\nSHOW NAMESPACES;",
        ],
        None,
    );
    let stdout = text(&output.stdout);
    let stderr = text(&output.stderr);
    assert!(
        stdout.contains("Created namespace \"demo\""),
        "{stdout}{stderr}"
    );
    assert!(stdout.contains("Created namespace \"demo2\""), "{stdout}");
    assert!(
        stderr.contains("--yes") || stdout.contains("--yes"),
        "asks to confirm: {stdout}{stderr}"
    );
    assert!(
        stdout.contains("Dropped namespace \"demo\""),
        "{stdout}{stderr}"
    );
    let listing = stdout.rsplit("Dropped namespace").next().unwrap();
    assert!(
        listing.contains("demo2") && !listing.contains("| demo "),
        "{listing}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
