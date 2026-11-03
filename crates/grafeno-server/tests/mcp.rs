use grafeno::Grafeno;
use grafeno_server::mcp::McpServer;
use serde_json::{Value, json};

fn server() -> McpServer {
    McpServer::new(Grafeno::in_memory().unwrap(), "agent".into())
}

fn request(server: &McpServer, id: u64, method: &str, params: Value) -> Value {
    let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    let response = server
        .handle_line(&message.to_string())
        .expect("a response");
    assert_eq!(response["id"], id);
    response
}

/// Calls a tool and returns its result, failing on protocol errors.
fn call(server: &McpServer, tool: &str, arguments: Value) -> Value {
    let response = request(
        server,
        1,
        "tools/call",
        json!({ "name": tool, "arguments": arguments }),
    );
    response
        .get("result")
        .cloned()
        .unwrap_or_else(|| panic!("protocol error: {response}"))
}

fn structured(result: &Value) -> &Value {
    assert_eq!(result["isError"], false, "{result}");
    &result["structuredContent"]
}

#[test]
fn handshake_and_tool_list() {
    let server = server();
    let response = request(
        &server,
        1,
        "initialize",
        json!({ "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } }),
    );
    assert_eq!(response["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(response["result"]["serverInfo"]["name"], "grafeno");
    assert!(response["result"]["capabilities"]["tools"].is_object());

    let response = request(
        &server,
        2,
        "initialize",
        json!({ "protocolVersion": "1999-01-01" }),
    );
    assert_eq!(response["result"]["protocolVersion"], "2025-06-18");

    let notification = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    assert!(server.handle_line(&notification.to_string()).is_none());

    let response = request(&server, 3, "tools/list", json!({}));
    let names: Vec<&str> = response["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "remember", "recall", "context", "relate", "update", "forget", "get_node", "query"
        ]
    );
    for tool in response["result"]["tools"].as_array().unwrap() {
        assert_eq!(tool["inputSchema"]["type"], "object");
    }

    assert_eq!(request(&server, 4, "ping", json!({}))["result"], json!({}));
}

#[test]
fn protocol_errors() {
    let server = server();
    let response = server.handle_line("{not json").unwrap();
    assert_eq!(response["error"]["code"], -32700);
    assert_eq!(
        request(&server, 5, "nope", json!({}))["error"]["code"],
        -32601
    );
    let response = request(&server, 6, "tools/call", json!({ "name": "nope" }));
    assert_eq!(response["error"]["code"], -32602);
}

#[test]
fn remember_relate_recall_and_context() {
    let server = server();
    let ana = call(
        &server,
        "remember",
        json!({ "name": "Ana", "text": "Backend developer who loves Rust", "labels": ["Person"] }),
    );
    let ana = structured(&ana)["id"].as_str().unwrap().to_owned();
    let grafeno = call(
        &server,
        "remember",
        json!({ "name": "Grafeno", "text": "Graph engine for agent memory", "labels": ["Project"],
                "links": [{ "label": "maintained_by", "to": ana, "valid_from": "2026-11-03" }] }),
    );
    let grafeno = structured(&grafeno)["id"].as_str().unwrap().to_owned();
    let preference = call(
        &server,
        "remember",
        json!({ "text": "Ana prefers answers in Spanish", "labels": ["Preference"] }),
    );
    let preference = structured(&preference)["id"].as_str().unwrap().to_owned();
    let fact = call(
        &server,
        "relate",
        json!({ "from": preference, "label": "about", "to": ana }),
    );
    structured(&fact);

    let recalled = call(
        &server,
        "recall",
        json!({ "query": "who maintains the graph engine?" }),
    );
    let results = structured(&recalled)["results"].as_array().unwrap();
    assert_eq!(results[0]["id"], grafeno.as_str());
    assert_eq!(results[0]["why"][0], "text match #1");
    let ana_result = results
        .iter()
        .find(|r| r["id"] == ana.as_str())
        .expect("Ana via the graph");
    assert!(
        ana_result["why"]
            .as_array()
            .unwrap()
            .iter()
            .any(|why| why == "1 hop(s) from Grafeno"),
        "{ana_result}"
    );

    let context = call(&server, "context", json!({ "query": "Ana", "budget": 300 }));
    assert_eq!(context["isError"], false);
    let text = context["content"][0]["text"].as_str().unwrap();
    // The short preference matches "Ana" best. Unnamed memories show only
    // their labels here, and the start of their text where facts name them.
    assert!(
        text.starts_with("## Memory\n[1] (Preference)\nAna prefers answers in Spanish\n"),
        "{text}"
    );
    assert!(text.contains("[2] Ana (Person)"), "{text}");
    assert!(
        text.contains("\n\n## Facts\n") && !text.contains("\n\n\n"),
        "{text}"
    );
    assert!(
        text.contains("- “Ana prefers answers in Spanish” about Ana"),
        "{text}"
    );
    assert!(
        text.contains("- Grafeno maintained_by Ana (from 2026-11-03)"),
        "{text}"
    );

    let empty = call(
        &server,
        "context",
        json!({ "query": "quantum chromodynamics" }),
    );
    assert_eq!(empty["content"][0]["text"], "No relevant memories found.");
}

#[test]
fn update_get_and_forget() {
    let server = server();
    let ana = call(
        &server,
        "remember",
        json!({ "name": "Ana", "text": "Works at Acme", "labels": ["Person"] }),
    );
    let ana = structured(&ana)["id"].as_str().unwrap().to_owned();
    let acme = call(
        &server,
        "remember",
        json!({ "name": "Acme", "text": "A company", "labels": ["Company"] }),
    );
    let acme = structured(&acme)["id"].as_str().unwrap().to_owned();
    let job = call(
        &server,
        "relate",
        json!({ "from": ana, "label": "works_at", "to": acme, "valid_from": "2024-01-01" }),
    );
    let job = structured(&job)["id"].as_str().unwrap().to_owned();

    let updated = call(
        &server,
        "update",
        json!({ "id": ana, "text": "Freelance developer", "props": { "city": "Lima" } }),
    );
    let node = structured(&updated);
    assert_eq!(node["text"], "Freelance developer");
    assert_eq!(node["props"]["city"], "Lima");
    assert_eq!(node["facts"][0]["label"], "works_at");
    assert_eq!(node["facts"][0]["other"]["name"], "Acme");
    assert_eq!(node["facts"][0]["valid_from"], "2024-01-01");

    let forgotten = call(&server, "forget", json!({ "id": job, "at": "2026-06-01" }));
    assert_eq!(
        structured(&forgotten)["result"],
        "invalidated fact (kept in history)"
    );
    let node = call(&server, "get_node", json!({ "id": ana }));
    assert_eq!(structured(&node)["facts"][0]["valid_to"], "2026-06-01");

    let wrong = call(&server, "forget", json!({ "id": ana, "mode": "retract" }));
    assert_eq!(wrong["isError"], true);
    let deleted = call(&server, "forget", json!({ "id": ana }));
    assert_eq!(structured(&deleted)["result"], "deleted node and its edges");
    let missing = call(&server, "get_node", json!({ "id": ana }));
    assert_eq!(missing["isError"], true);

    let bad = call(&server, "remember", json!({ "txt": "typo" }));
    assert_eq!(bad["isError"], true);
    assert!(
        bad["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("invalid arguments")
    );
}

#[test]
fn read_only_queries() {
    let server = server();
    let ana = call(
        &server,
        "remember",
        json!({ "name": "Ana", "text": "Engineer", "labels": ["Person"] }),
    );
    let ana = structured(&ana)["id"].as_str().unwrap().to_owned();
    let acme = call(
        &server,
        "remember",
        json!({ "name": "Acme", "text": "Company", "labels": ["Company"] }),
    );
    let acme = structured(&acme)["id"].as_str().unwrap().to_owned();
    call(
        &server,
        "relate",
        json!({ "from": ana, "label": "works_at", "to": acme, "valid_from": "2024-01-01" }),
    );

    let result = call(
        &server,
        "query",
        json!({ "query": "MATCH (p:Person)-[w:works_at]->(c) WHERE c.name = $company RETURN p.name, date(w.@valid_from)",
                "params": { "company": "Acme" } }),
    );
    let output = structured(&result);
    assert_eq!(output["columns"], json!(["p.name", "date(w.@valid_from)"]));
    assert_eq!(output["rows"], json!([["Ana", "2024-01-01"]]));
    assert_eq!(output["truncated"], false);

    let write = call(&server, "query", json!({ "query": "MATCH (n) DELETE n" }));
    assert_eq!(write["isError"], true);
    assert!(
        write["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("writes")
    );
    let syntax = call(&server, "query", json!({ "query": "MATCH (n RETURN n" }));
    assert!(
        syntax["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("line 1")
    );
}
