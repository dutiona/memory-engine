//! `memory-engine-mcp` — MCP server exposing `memory-engine` as MCP tools over streamable HTTP.
//!
//! ## Module visibility
//!
//! Modules consumed by the binary (`main.rs`, a separate crate) or by integration
//! tests under `tests/` are `pub`; modules used only within this library are
//! `pub(crate)` so they are not part of the crate's published surface:
//!
//! - `pub`: [`auth`], [`config`], [`embedding`], [`http`], [`server`], [`summary`], [`tools`]
//!   — referenced by `main.rs` and/or `tests/`.
//! - `pub(crate)`: [`activity_policy`], [`depth`], [`error`]
//!   — internal helpers with no external (binary or test) consumer.

pub(crate) mod activity_policy;
pub mod auth;
pub mod config;
pub(crate) mod depth;
pub mod embedding;
pub(crate) mod error;
pub mod http;
pub mod server;
pub mod summary;
pub mod tools;
