//! Grafeno servers: a JSON HTTP API ([`http`]) and an MCP server over
//! stdio ([`mcp`]), and an interactive console ([`shell`]). The `grafeno`
//! binary runs them.

pub mod api;
pub mod config;
pub mod http;
pub mod mcp;
pub mod service;
pub mod shell;
pub mod studio;
