//! `memory-engine-mcp` — the memory service: the model's MCP tools over streamable HTTP,
//! behind per-machine Bearer authentication (ADR-0020).
//!
//! Every module is `pub`: the binary (`main.rs`, a separate crate), the integration tests and
//! the fuzz target consume them.

pub mod auth;
pub mod config;
pub mod embedding;
pub mod home;
pub mod http;
pub mod project;
pub mod server;
pub mod tools;
