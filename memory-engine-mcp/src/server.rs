use std::borrow::Cow;
use std::sync::Arc;

use memory_engine::engine::MemoryEngine;
use rmcp::RoleServer;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Implementation, InitializeResult, ListToolsResult,
    PaginatedRequestParams, ProtocolVersion, ServerCapabilities, Tool, ToolsCapability,
};
use rmcp::service::RequestContext;

use crate::embedding::HttpEmbeddingProvider;
use crate::tools;

/// MCP server wrapping a [`MemoryEngine`] instance.
///
/// All tool calls are dispatched to `tokio::task::spawn_blocking` via the engine's
/// internal connection pool. The server holds an `Arc<MemoryEngine>` for thread-safe sharing.
pub struct MemoryMcpServer {
    pub engine: Arc<MemoryEngine>,
    pub embedder: Option<Arc<HttpEmbeddingProvider>>,
    pub embed_dim: usize,
}

impl MemoryMcpServer {
    pub fn new(
        engine: Arc<MemoryEngine>,
        embedder: Option<Arc<HttpEmbeddingProvider>>,
        embed_dim: usize,
    ) -> Self {
        Self {
            engine,
            embedder,
            embed_dim,
        }
    }

    fn tool_definitions() -> Vec<Tool> {
        tools::all_tool_definitions()
    }
}

impl ServerHandler for MemoryMcpServer {
    fn get_info(&self) -> InitializeResult {
        let mut capabilities = ServerCapabilities::default();
        capabilities.tools = Some(ToolsCapability {
            list_changed: Some(false),
        });
        InitializeResult::new(capabilities)
            .with_server_info(
                Implementation::new("memory-engine-mcp", env!("CARGO_PKG_VERSION"))
                    .with_description(
                        "MCP server for memory-engine — durable long-term memory for AI agents",
                    ),
            )
            .with_protocol_version(ProtocolVersion::V_2025_06_18)
            .with_instructions(
                "Memory engine MCP server. Tools: memory_ingest, memory_add_fact, memory_query, \
             memory_resume_context, memory_list_due, memory_next_due_time, memory_explain_fact, \
             memory_get_fact, memory_statistics, memory_flush_insights. \
             Use depth=sparse|standard|full on query tools to control response verbosity.",
            )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::model::ErrorData> {
        Ok(ListToolsResult::with_all_items(Self::tool_definitions()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let name = request.name.clone();
        let args = request.arguments.unwrap_or_default();

        let engine = Arc::clone(&self.engine);
        let embedder = self.embedder.clone();
        let embed_dim = self.embed_dim;

        // Dispatch to tool handlers on the blocking thread pool.
        // Engine operations are sync (SQLite) — must not run on the async runtime.
        let result = tokio::task::spawn_blocking(move || {
            tools::dispatch(&name, args, &engine, embedder.as_deref(), embed_dim)
        })
        .await
        .map_err(|e| {
            rmcp::model::ErrorData::internal_error(
                Cow::Owned(format!("task join error: {e}")),
                None,
            )
        })??;

        Ok(result)
    }
}
