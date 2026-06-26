//! Tool handler implementations, grouped by phase.
//!
//! Each `handle_*` function is `pub(crate)` so the dispatcher in
//! [`crate::tools`] can route to it. Handlers translate validated MCP arguments
//! into engine calls and shape the result back into a [`rmcp::model::CallToolResult`].

pub mod cognitive;
pub mod outcome;
pub mod p0;
pub mod p1;
pub mod p2;
pub mod session;
