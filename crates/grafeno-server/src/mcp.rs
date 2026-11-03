//! MCP (Model Context Protocol) server over stdio: newline-delimited
//! JSON-RPC 2.0 messages on stdin and stdout. Logs go to stderr.
//!
//! The tools are shaped for language models: few of them, text in and out,
//! names instead of raw records, and dates instead of microseconds.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};

use grafeno::{
    Direction, EdgeFilter, Error, Grafeno, NewEdge, NewNode, Node, NodeId, Props, ReadTx, Value,
};
use grafeno_memory::{ContextOptions, Expansion, RecallQuery, Signal, context_for, recall};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::api::{TimeInput, parse_id};

/// Supported protocol versions, newest first.
const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "Grafeno is your long-term memory: a graph of nodes (memories, \
people, projects, documents) and facts (labeled edges between nodes, with the dates they \
hold). Before answering anything that may depend on earlier conversations or stored \
knowledge, call `context` (or `recall` to get ids). Store new facts, decisions and \
preferences with `remember`, and connect nodes with `relate`. When something stops being \
true, `forget` it with mode \"invalidate\" so its history is kept; use \"retract\" only for \
mistakes. Ids are the UUIDs these tools return.";

const SNIPPET_CHARS: usize = 400;

pub struct McpServer {
    db: Grafeno,
    namespace: String,
}

/// Serves MCP on stdin and stdout until stdin closes.
pub fn run(db: Grafeno, namespace: String) -> io::Result<()> {
    let server = McpServer::new(db, namespace);
    eprintln!(
        "grafeno MCP server ready (namespace {:?})",
        server.namespace
    );
    let mut stdout = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = server.handle_line(&line) {
            writeln!(stdout, "{response}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

type ToolResult = Result<ToolOutput, String>;

enum ToolOutput {
    /// Shown to the model as pretty JSON, and returned as structured content.
    Json(Value),
    Text(String),
}

impl McpServer {
    pub fn new(db: Grafeno, namespace: String) -> Self {
        Self { db, namespace }
    }

    /// Handles one line of input and returns the response, if any
    /// (notifications get none).
    pub fn handle_line(&self, line: &str) -> Option<Value> {
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(error) => {
                return Some(error_response(
                    Value::Null,
                    -32700,
                    &format!("parse error: {error}"),
                ));
            }
        };
        if let Value::Array(messages) = message {
            let responses: Vec<Value> = messages
                .into_iter()
                .filter_map(|message| self.handle(message))
                .collect();
            return (!responses.is_empty()).then_some(Value::Array(responses));
        }
        self.handle(message)
    }

    fn handle(&self, message: Value) -> Option<Value> {
        // Messages without a method are responses to requests we never send.
        let method = message.get("method")?.as_str()?.to_owned();
        // Messages without an id are notifications: no response.
        let id = message.get("id")?.clone();
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
        let result = match method.as_str() {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => self.call_tool(&params),
            _ => Err((-32601, format!("method not found: {method}"))),
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => error_response(id, code, &message),
        })
    }

    fn initialize(&self, params: &Value) -> Value {
        let requested = params.get("protocolVersion").and_then(Value::as_str);
        let version = requested
            .filter(|version| PROTOCOL_VERSIONS.contains(version))
            .unwrap_or(PROTOCOL_VERSIONS[0]);
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "grafeno", "version": env!("CARGO_PKG_VERSION") },
            "instructions": INSTRUCTIONS,
        })
    }

    fn call_tool(&self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((-32602, "missing tool name".to_owned()))?;
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let outcome = match name {
            "remember" => self.remember(arguments),
            "recall" => self.recall(arguments),
            "context" => self.context(arguments),
            "relate" => self.relate(arguments),
            "update" => self.update(arguments),
            "forget" => self.forget(arguments),
            "get_node" => self.get_node(arguments),
            _ => return Err((-32602, format!("unknown tool: {name}"))),
        };
        Ok(match outcome {
            Ok(ToolOutput::Json(value)) => json!({
                "content": [{ "type": "text", "text": pretty(&value) }],
                "structuredContent": value,
                "isError": false,
            }),
            Ok(ToolOutput::Text(text)) => json!({
                "content": [{ "type": "text", "text": text }],
                "isError": false,
            }),
            Err(message) => json!({
                "content": [{ "type": "text", "text": message }],
                "isError": true,
            }),
        })
    }

    fn ns<'a>(&'a self, namespace: &'a Option<String>) -> &'a str {
        namespace.as_deref().unwrap_or(&self.namespace)
    }

    // ------------------------------------------------------------ tools

    fn remember(&self, arguments: Value) -> ToolResult {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Link {
            label: String,
            to: String,
            #[serde(default)]
            valid_from: Option<TimeInput>,
            #[serde(default)]
            valid_to: Option<TimeInput>,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            text: String,
            #[serde(default)]
            name: Option<String>,
            #[serde(default)]
            labels: Vec<String>,
            #[serde(default)]
            props: Props,
            #[serde(default)]
            sources: Vec<String>,
            #[serde(default)]
            embedding: Option<Vec<f32>>,
            #[serde(default)]
            links: Vec<Link>,
            #[serde(default)]
            namespace: Option<String>,
        }
        let args: Args = parse_args(arguments)?;
        let ns = self.ns(&args.namespace);
        let mut new = NewNode {
            labels: if args.labels.is_empty() {
                vec!["Memory".into()]
            } else {
                args.labels
            },
            props: args.props,
            sources: parse_ids(&args.sources)?,
            text: None,
            embedding: args.embedding,
        };
        // The name is searchable too.
        let text = match &args.name {
            Some(name) if !args.text.to_lowercase().contains(&name.to_lowercase()) => {
                format!("{name}: {}", args.text)
            }
            _ => args.text,
        };
        new.text = Some(text);
        if let Some(name) = args.name {
            new.props.insert("name".into(), Value::String(name));
        }
        let (id, edges) = self
            .db
            .write(|tx| {
                let id = tx.create_node(ns, new)?;
                let mut edges = Vec::new();
                for link in args.links {
                    let mut edge = NewEdge::new(id, link.label, parse_id(&link.to)?);
                    edge.valid_from = link
                        .valid_from
                        .as_ref()
                        .map(TimeInput::resolve)
                        .transpose()?;
                    edge.valid_to = link.valid_to.as_ref().map(TimeInput::resolve).transpose()?;
                    edges.push(tx.create_edge(ns, edge)?);
                }
                Ok((id, edges))
            })
            .map_err(describe)?;
        Ok(ToolOutput::Json(json!({ "id": id, "edges": edges })))
    }

    fn recall(&self, arguments: Value) -> ToolResult {
        let args: QueryArgs = parse_args(arguments)?;
        let ns = self.ns(&args.namespace);
        let query = args.query(10)?;
        let read = self.db.begin_read().map_err(describe)?;
        let results = recall(&read, ns, &query).map_err(describe)?;
        let names: HashMap<NodeId, String> = results
            .iter()
            .map(|result| (result.node.id, result.node.display_name()))
            .collect();
        let mut out = Vec::new();
        for result in results {
            let text = read
                .text(ns, result.node.id)
                .map_err(describe)?
                .unwrap_or_default();
            let mut why = Vec::new();
            for signal in &result.signals {
                why.push(match signal {
                    Signal::Text { rank, .. } => format!("text match #{rank}"),
                    Signal::Vector { rank, similarity } => {
                        format!("similar meaning #{rank} ({similarity:.2})")
                    }
                    Signal::Graph { seed, hops } => {
                        let seed_name = match names.get(seed) {
                            Some(name) => name.clone(),
                            None => name_of(&read, ns, *seed),
                        };
                        format!("{hops} hop(s) from {seed_name}")
                    }
                });
            }
            out.push(json!({
                "id": result.node.id,
                "name": result.node.display_name(),
                "labels": result.node.labels,
                "score": result.score,
                "text": snippet(&text),
                "why": why,
            }));
        }
        Ok(ToolOutput::Json(json!({ "results": out })))
    }

    fn context(&self, arguments: Value) -> ToolResult {
        let args: QueryArgs = parse_args(arguments)?;
        let ns = self.ns(&args.namespace);
        let query = args.query(20)?;
        let options = ContextOptions::with_budget(args.budget.unwrap_or(1_500));
        let read = self.db.begin_read().map_err(describe)?;
        let context = context_for(&read, ns, &query, &options).map_err(describe)?;
        Ok(ToolOutput::Text(if context.items.is_empty() {
            "No relevant memories found.".into()
        } else {
            context.rendered
        }))
    }

    fn relate(&self, arguments: Value) -> ToolResult {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            from: String,
            label: String,
            to: String,
            #[serde(default)]
            valid_from: Option<TimeInput>,
            #[serde(default)]
            valid_to: Option<TimeInput>,
            #[serde(default)]
            props: Props,
            #[serde(default)]
            sources: Vec<String>,
            #[serde(default)]
            namespace: Option<String>,
        }
        let args: Args = parse_args(arguments)?;
        let ns = self.ns(&args.namespace);
        let mut edge = NewEdge::new(
            parse_id(&args.from).map_err(describe)?,
            args.label,
            parse_id(&args.to).map_err(describe)?,
        );
        edge.props = args.props;
        edge.sources = parse_ids(&args.sources)?;
        edge.valid_from = args
            .valid_from
            .as_ref()
            .map(TimeInput::resolve)
            .transpose()
            .map_err(describe)?;
        edge.valid_to = args
            .valid_to
            .as_ref()
            .map(TimeInput::resolve)
            .transpose()
            .map_err(describe)?;
        let id = self
            .db
            .write(|tx| tx.create_edge(ns, edge))
            .map_err(describe)?;
        Ok(ToolOutput::Json(json!({ "id": id })))
    }

    fn update(&self, arguments: Value) -> ToolResult {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            id: String,
            #[serde(default)]
            text: Option<String>,
            #[serde(default)]
            name: Option<String>,
            #[serde(default)]
            props: Props,
            #[serde(default)]
            add_labels: Vec<String>,
            #[serde(default)]
            remove_labels: Vec<String>,
            #[serde(default)]
            namespace: Option<String>,
        }
        let args: Args = parse_args(arguments)?;
        let ns = self.ns(&args.namespace);
        let id = parse_id(&args.id).map_err(describe)?;
        let mut props = args.props;
        if let Some(name) = args.name {
            props.insert("name".into(), Value::String(name));
        }
        self.db
            .write(|tx| {
                tx.node(ns, id)?.ok_or(Error::NodeNotFound(id))?;
                if !props.is_empty() {
                    tx.set_node_props(ns, id, props)?;
                }
                for label in &args.add_labels {
                    tx.add_label(ns, id, label)?;
                }
                for label in &args.remove_labels {
                    tx.remove_label(ns, id, label)?;
                }
                if let Some(text) = &args.text {
                    tx.set_text(ns, id, text)?;
                }
                Ok(())
            })
            .map_err(describe)?;
        self.get_node(json!({ "id": id, "namespace": ns }))
    }

    fn forget(&self, arguments: Value) -> ToolResult {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            id: String,
            #[serde(default)]
            mode: Option<String>,
            #[serde(default)]
            at: Option<TimeInput>,
            #[serde(default)]
            namespace: Option<String>,
        }
        let args: Args = parse_args(arguments)?;
        let ns = self.ns(&args.namespace);
        let id = parse_id(&args.id).map_err(describe)?;
        let edge_id = grafeno::EdgeId::from_u128(id.as_u128());
        let read = self.db.begin_read().map_err(describe)?;
        let is_node = read.node(ns, id).map_err(describe)?.is_some();
        let is_edge = read.edge(ns, edge_id).map_err(describe)?.is_some();
        drop(read);
        let mode = args.mode.as_deref();
        let done = match (is_node, is_edge, mode) {
            (true, _, None | Some("delete")) => {
                self.db
                    .write(|tx| tx.delete_node(ns, id))
                    .map_err(describe)?;
                "deleted node and its edges"
            }
            (true, _, Some(other)) => {
                return Err(format!(
                    "a node can only be forgotten with mode \"delete\", not {other:?}"
                ));
            }
            (false, true, None | Some("invalidate")) => {
                let at = args
                    .at
                    .as_ref()
                    .map(TimeInput::resolve)
                    .transpose()
                    .map_err(describe)?;
                self.db
                    .write(|tx| {
                        tx.invalidate_edge(ns, edge_id, at.unwrap_or_else(grafeno::Timestamp::now))
                    })
                    .map_err(describe)?;
                "invalidated fact (kept in history)"
            }
            (false, true, Some("retract")) => {
                self.db
                    .write(|tx| tx.retract_edge(ns, edge_id))
                    .map_err(describe)?;
                "retracted fact (kept in history as a mistake)"
            }
            (false, true, Some("delete")) => {
                self.db
                    .write(|tx| tx.delete_edge(ns, edge_id))
                    .map_err(describe)?;
                "deleted fact"
            }
            (false, true, Some(other)) => {
                return Err(format!(
                    "unknown mode {other:?}: use \"invalidate\", \"retract\" or \"delete\""
                ));
            }
            (false, false, _) => return Err(format!("no node or fact with id {id}")),
        };
        Ok(ToolOutput::Json(json!({ "id": id, "result": done })))
    }

    fn get_node(&self, arguments: Value) -> ToolResult {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            id: String,
            #[serde(default)]
            namespace: Option<String>,
        }
        let args: Args = parse_args(arguments)?;
        let ns = self.ns(&args.namespace);
        let id = parse_id(&args.id).map_err(describe)?;
        let read = self.db.begin_read().map_err(describe)?;
        let node = read
            .node(ns, id)
            .map_err(describe)?
            .ok_or_else(|| format!("no node with id {id}"))?;
        let text = read.text(ns, id).map_err(describe)?;
        let mut facts = Vec::new();
        for neighbor in read
            .neighbors(ns, id, Direction::Both, &EdgeFilter::any())
            .map_err(describe)?
            .into_iter()
            .take(50)
        {
            let edge = &neighbor.edge;
            facts.push(json!({
                "id": edge.id,
                "direction": if edge.from == id { "out" } else { "in" },
                "label": edge.label,
                "other": { "id": neighbor.node, "name": name_of(&read, ns, neighbor.node) },
                "valid_from": edge.valid_from.map(|at| at.date()),
                "valid_to": edge.valid_to.map(|at| at.date()),
            }));
        }
        Ok(ToolOutput::Json(json!({
            "id": node.id,
            "name": node.display_name(),
            "labels": node.labels,
            "props": node.props,
            "text": text,
            "sources": node.sources,
            "created": node.created_at.date(),
            "updated": node.updated_at.date(),
            "facts": facts,
        })))
    }
}

/// Arguments shared by `recall` and `context`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryArgs {
    query: String,
    #[serde(default)]
    embedding: Option<Vec<f32>>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    depth: Option<usize>,
    #[serde(default)]
    budget: Option<usize>,
    #[serde(default)]
    namespace: Option<String>,
}

impl QueryArgs {
    fn query(&self, default_limit: usize) -> Result<RecallQuery, String> {
        if self.query.trim().is_empty() && self.embedding.is_none() {
            return Err("`query` must not be empty".into());
        }
        let depth = self.depth.unwrap_or(1);
        Ok(RecallQuery {
            text: Some(self.query.clone()),
            embedding: self.embedding.clone(),
            limit: self.limit.unwrap_or(default_limit),
            labels: self.labels.clone(),
            candidates: 50,
            expand: (depth > 0).then(|| Expansion {
                depth,
                ..Expansion::default()
            }),
        })
    }
}

fn parse_args<T: DeserializeOwned>(arguments: Value) -> Result<T, String> {
    serde_json::from_value(arguments).map_err(|error| format!("invalid arguments: {error}"))
}

fn parse_ids(ids: &[String]) -> Result<Vec<NodeId>, String> {
    ids.iter()
        .map(|id| parse_id(id).map_err(describe))
        .collect()
}

fn describe(error: Error) -> String {
    error.to_string()
}

fn name_of(read: &ReadTx, ns: &str, id: NodeId) -> String {
    read.node(ns, id)
        .ok()
        .flatten()
        .map(|node: Node| node.display_name())
        .unwrap_or_else(|| id.to_string())
}

fn snippet(text: &str) -> String {
    match text.char_indices().nth(SNIPPET_CHARS) {
        Some((end, _)) => format!("{}…", text[..end].trim_end()),
        None => text.to_owned(),
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn tools() -> Value {
    let namespace = json!({
        "type": "string",
        "description": "Memory namespace; defaults to the server's namespace."
    });
    let date = json!({
        "type": "string",
        "description": "ISO 8601 date or date-time, such as 2024-05-01 or 2024-05-01T10:30:00Z."
    });
    let id = |description: &str| json!({ "type": "string", "description": description });
    json!([
        {
            "name": "remember",
            "title": "Remember",
            "description": "Store a memory: a fact, decision, preference, person, project or document. \
                It becomes searchable by `recall` and `context`. Optionally link it to existing nodes.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "What to remember, in plain language." },
                    "name": { "type": "string", "description": "Short name, for entities such as a person or project." },
                    "labels": { "type": "array", "items": { "type": "string" }, "description": "Kinds of node, such as Person, Project, Preference. Default: Memory." },
                    "props": { "type": "object", "description": "Extra structured properties." },
                    "sources": { "type": "array", "items": { "type": "string" }, "description": "Ids of the nodes this memory comes from." },
                    "embedding": { "type": "array", "items": { "type": "number" }, "description": "Optional embedding of the text." },
                    "links": {
                        "type": "array",
                        "description": "Facts from the new node to existing nodes.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "label": { "type": "string", "description": "Relation, such as works_on or prefers." },
                                "to": id("Id of the target node."),
                                "valid_from": date,
                                "valid_to": date
                            },
                            "required": ["label", "to"]
                        }
                    },
                    "namespace": namespace
                },
                "required": ["text"]
            }
        },
        {
            "name": "recall",
            "title": "Recall",
            "description": "Search memory by meaning, words and connections. Returns ids, names, \
                snippets and why each result matched. Use it to find ids for `relate`, `update` or `forget`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "What you are looking for." },
                    "limit": { "type": "integer", "minimum": 1, "description": "Maximum results. Default: 10." },
                    "labels": { "type": "array", "items": { "type": "string" }, "description": "Only nodes with one of these labels." },
                    "depth": { "type": "integer", "minimum": 0, "description": "Graph hops to expand from the best matches. Default: 1." },
                    "embedding": { "type": "array", "items": { "type": "number" }, "description": "Optional embedding of the query." },
                    "namespace": namespace
                },
                "required": ["query"]
            }
        },
        {
            "name": "context",
            "title": "Context",
            "description": "Get the memories and facts relevant to a question as compact text that fits \
                a token budget. Call it before answering questions that may depend on what you know.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "The question or topic." },
                    "budget": { "type": "integer", "minimum": 50, "description": "Maximum tokens. Default: 1500." },
                    "labels": { "type": "array", "items": { "type": "string" }, "description": "Only nodes with one of these labels." },
                    "depth": { "type": "integer", "minimum": 0, "description": "Graph hops to expand. Default: 1." },
                    "limit": { "type": "integer", "minimum": 1, "description": "Maximum memories considered. Default: 20." },
                    "embedding": { "type": "array", "items": { "type": "number" }, "description": "Optional embedding of the query." },
                    "namespace": namespace
                },
                "required": ["query"]
            }
        },
        {
            "name": "relate",
            "title": "Relate",
            "description": "Record a fact between two existing nodes, such as Ana works_on Grafeno, \
                optionally with the dates it holds.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from": id("Id of the source node."),
                    "label": { "type": "string", "description": "Relation, in snake_case, such as works_on." },
                    "to": id("Id of the target node."),
                    "valid_from": date,
                    "valid_to": date,
                    "props": { "type": "object", "description": "Extra properties of the fact." },
                    "sources": { "type": "array", "items": { "type": "string" }, "description": "Ids of the nodes this fact comes from." },
                    "namespace": namespace
                },
                "required": ["from", "label", "to"]
            }
        },
        {
            "name": "update",
            "title": "Update",
            "description": "Change a node: replace its text, rename it, merge properties (null removes one) \
                or change its labels.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": id("Id of the node."),
                    "text": { "type": "string", "description": "New text; replaces the old one." },
                    "name": { "type": "string", "description": "New name." },
                    "props": { "type": "object", "description": "Properties to merge; null removes a property." },
                    "add_labels": { "type": "array", "items": { "type": "string" } },
                    "remove_labels": { "type": "array", "items": { "type": "string" } },
                    "namespace": namespace
                },
                "required": ["id"]
            }
        },
        {
            "name": "forget",
            "title": "Forget",
            "description": "Forget a node or a fact. For facts, mode \"invalidate\" (default) means it \
                stopped being true at `at` (default now) and keeps its history; \"retract\" means it was \
                a mistake; \"delete\" erases it. Nodes are always deleted with their facts.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": id("Id of the node or fact."),
                    "mode": { "type": "string", "enum": ["invalidate", "retract", "delete"] },
                    "at": date,
                    "namespace": namespace
                },
                "required": ["id"]
            }
        },
        {
            "name": "get_node",
            "title": "Get node",
            "description": "Read a node: its text, properties, sources and the facts that connect it \
                to other nodes.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": id("Id of the node."), "namespace": namespace },
                "required": ["id"]
            }
        }
    ])
}
