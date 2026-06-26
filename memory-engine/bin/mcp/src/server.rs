use std::sync::Arc;

use memory_engine::ActivityFilterConfig;
use memory_engine::engine::MemoryEngine;
use memory_engine::traits::SummaryGenerator;
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
/// The engine is async-native (#631): every DB-touching method is an `async fn`
/// whose storage I/O is offloaded below the `StorageBackend` seam and whose
/// consumer-trait (HTTP/CPU) calls are offloaded internally via `spawn_blocking`.
/// Tool calls are therefore `.await`ed directly on the async runtime — no
/// `spawn_blocking` hop at the dispatch boundary. The server holds an
/// `Arc<MemoryEngine>` for thread-safe sharing.
pub struct MemoryMcpServer {
    pub(crate) engine: Arc<MemoryEngine>,
    pub(crate) embedder: Option<Arc<HttpEmbeddingProvider>>,
    pub(crate) summary_gen: Option<Arc<dyn SummaryGenerator + Send + Sync>>,
    pub(crate) embed_dim: usize,
    pub(crate) filter_config: Arc<ActivityFilterConfig>,
}

impl MemoryMcpServer {
    /// Construct a server over an already-opened [`MemoryEngine`].
    ///
    /// # Activity filter
    ///
    /// The activity-stream filter (`memory_record_activity` dedup window plus the
    /// ignore/promote tool-name patterns) is **hardwired** to the Claude Code
    /// defaults from [`activity_policy::default_filter_config`] — this constructor
    /// takes no filter argument. The `filter_config` field is `pub(crate)`, so an
    /// external consumer that needs a different policy (other tool names, a
    /// different dedup window) cannot override it today; doing so requires adding a
    /// constructor parameter or builder. Tracked as a follow-up; in-crate callers
    /// (e.g. tests) can set the field directly.
    ///
    /// [`activity_policy::default_filter_config`]: crate::activity_policy::default_filter_config
    pub fn new(
        engine: Arc<MemoryEngine>,
        embedder: Option<Arc<HttpEmbeddingProvider>>,
        summary_gen: Option<Arc<dyn SummaryGenerator + Send + Sync>>,
        embed_dim: usize,
    ) -> Self {
        Self {
            engine,
            embedder,
            summary_gen,
            embed_dim,
            filter_config: Arc::new(crate::activity_policy::default_filter_config()),
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
             memory_get_fact, memory_statistics, memory_flush_insights, \
             memory_consolidate, memory_forget, memory_dump_state, \
             memory_pin_fact, memory_unpin_fact, \
             memory_replay_events, memory_fact_history, memory_bootstrap_session, \
             memory_record_outcome, memory_outcome_counts, memory_record_activity, \
             memory_checkpoint_session, memory_load_context. \
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

        // The engine is async-native (#631): await the tool dispatch directly on the
        // runtime. Storage I/O is offloaded below the `StorageBackend` seam and any
        // consumer-trait (blocking HTTP) call is offloaded inside the engine, so no
        // `spawn_blocking` hop is needed here. `MemoryEngine` is `Send + Sync` and its
        // futures are `Send`, so the borrowed `&self` dispatch is sound across `.await`.
        let result = tools::dispatch(
            &name,
            args,
            &self.engine,
            self.embedder.clone(),
            self.summary_gen.clone(),
            self.embed_dim,
            &self.filter_config,
        )
        .await?;

        Ok(result)
    }
}
