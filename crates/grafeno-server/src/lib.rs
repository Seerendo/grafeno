//! Grafeno servers: a JSON HTTP API ([`http`]) and an MCP server over
//! stdio ([`mcp`]). The `grafeno` binary runs either one.

pub mod api;
pub mod http;
pub mod mcp;
