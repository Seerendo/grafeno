//! Grafeno servers: a JSON HTTP API ([`http`]) and an MCP server over
//! stdio ([`mcp`]), and an interactive console ([`shell`]). The `grafeno`
//! binary runs them.

pub mod api;
pub mod auth;
pub mod config;
pub mod http;
pub mod keys;
pub mod mcp;
pub mod service;
pub mod shell;
pub mod studio;
