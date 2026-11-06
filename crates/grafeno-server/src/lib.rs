//! Grafeno servers: a JSON HTTP API ([`http`]) and an MCP server over
//! stdio ([`mcp`]), and an interactive console ([`shell`]). The `grafeno`
//! binary runs them.

pub mod access_cli;
pub mod accounts;
pub mod admin;
pub mod api;
pub mod auth;
pub mod cluster;
pub mod config;
pub mod events;
pub mod groups;
pub mod http;
pub mod keys;
pub mod mcp;
pub mod metrics;
pub mod roles;
pub mod service;
pub mod shell;
pub mod studio;
pub mod users;
