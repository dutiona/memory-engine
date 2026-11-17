use std::sync::Arc;

use memory_engine::ActivityFilterConfig;
use memory_engine::engine::MemoryEngine;
use memory_engine::traits::SummaryGenerator;
use rmcp::RoleServer;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ErrorData, Extensions, Implementation, InitializeResult,
    ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities, Tool,
    ToolsCapability,
};
use rmcp::service::RequestContext;
use serde_json::{Map, Value};

use crate::auth::Machine;
use crate::embedding::HttpEmbeddingProvider;
use crate::home::User;
use crate::tools;

/// MCP server wrapping a [`MemoryEngine`] instance.
///
/// The engine is async-native (#631): every DB-touching method is an `async fn`
/// whose storage I/O is offloaded below the `StorageBackend` seam and whose
/// consumer-trait (HTTP/CPU) calls are offloaded internally via `spawn_blocking`.
/// Tool calls are therefore `.await`ed directly on the async runtime — no
/// `spawn_blocking` hop at the dispatch boundary. The server holds an
/// `Arc<MemoryEngine>` for thread-safe sharing.
#[derive(Clone)]
pub struct MemoryMcpServer {
    pub(crate) engine: Arc<MemoryEngine>,
    pub(crate) embedder: Option<Arc<HttpEmbeddingProvider>>,
    pub(crate) summary_gen: Option<Arc<dyn SummaryGenerator + Send + Sync>>,
    pub(crate) embed_dim: usize,
    pub(crate) filter_config: Arc<ActivityFilterConfig>,
    pub(crate) user: User,
    /// Held for reading by every tool call; `http::serve` takes it for writing to drain.
    pub(crate) in_flight: Arc<tokio::sync::RwLock<()>>,
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
        user: User,
    ) -> Self {
        Self {
            engine,
            embedder,
            summary_gen,
            embed_dim,
            filter_config: Arc::new(crate::activity_policy::default_filter_config()),
            user,
            in_flight: Arc::new(tokio::sync::RwLock::new(())),
        }
    }

    fn tool_definitions() -> Vec<Tool> {
        tools::all_tool_definitions()
    }

    /// Run one tool call for the authenticated `machine`: the seam `call_tool`, the fuzz
    /// target and the tests drive.
    ///
    /// # Errors
    ///
    /// A JSON-RPC error for a name that is not a tool; the model tools answer every failure
    /// as a tool result instead.
    pub async fn dispatch(
        &self,
        machine: &Machine,
        name: &str,
        mut args: Map<String, Value>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = tools::CallContext {
            engine: &self.engine,
            embedder: self.embedder.as_ref(),
            user: &self.user,
            machine,
        };
        if let Some(result) = tools::call_model_tool(name, args.clone(), &ctx).await {
            return result;
        }
        if name == "memory_add_fact" {
            record_machine(&mut args, machine)?;
        }
        tools::dispatch(
            name,
            args,
            &self.engine,
            self.embedder.clone(),
            self.summary_gen.clone(),
            self.embed_dim,
            &self.filter_config,
        )
        .await
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

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, rmcp::model::ErrorData>> + Send + '_ {
        std::future::ready(Ok(
            ListToolsResult::with_all_items(Self::tool_definitions()),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let machine = calling_machine(&context.extensions)?;
        let _in_flight = self.in_flight.read().await;
        let args = request.arguments.unwrap_or_default();
        self.dispatch(machine, &request.name, args).await
    }
}

/// The machine the HTTP layer authenticated. Without one, nothing runs: the router always
/// supplies it, so its absence means a mount that bypassed the Bearer layer.
fn calling_machine(extensions: &Extensions) -> Result<&Machine, ErrorData> {
    extensions
        .get::<axum::http::request::Parts>()
        .and_then(|parts| parts.extensions.get::<Machine>())
        .ok_or_else(|| ErrorData::internal_error("no authenticated machine on this call", None))
}

/// The server, not the client, says which machine a remembered fact came from.
fn record_machine(args: &mut Map<String, Value>, machine: &Machine) -> Result<(), ErrorData> {
    let metadata = args
        .entry("metadata")
        .and_modify(|m| {
            if m.is_null() {
                *m = Value::Object(Map::new());
            }
        })
        .or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(metadata) = metadata else {
        return Err(ErrorData::invalid_params(
            "metadata must be a JSON object",
            None,
        ));
    };
    metadata.insert(
        "machine".to_owned(),
        Value::String(machine.as_str().to_owned()),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::TokenTable;
    use rmcp::model::{ErrorCode, Extensions};
    use sha2::{Digest, Sha256};

    fn laptop() -> Machine {
        let token = format!("me_{}", "L".repeat(32));
        let hash: String = Sha256::digest(token.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .concat();
        let table = TokenTable::parse(format!("laptop {hash}\n").as_bytes()).unwrap();
        table
            .authenticate(format!("Bearer {token}").as_bytes())
            .unwrap()
            .clone()
    }

    fn parts(machine: Option<Machine>) -> axum::http::request::Parts {
        let (mut parts, ()) = axum::http::Request::new(()).into_parts();
        if let Some(machine) = machine {
            parts.extensions.insert(machine);
        }
        parts
    }

    #[test]
    fn no_parts_is_refused() {
        let err = calling_machine(&Extensions::new()).unwrap_err();
        assert_eq!(err.code, ErrorCode::INTERNAL_ERROR);
    }

    #[test]
    fn parts_without_a_machine_are_refused() {
        let mut extensions = Extensions::new();
        extensions.insert(parts(None));
        let err = calling_machine(&extensions).unwrap_err();
        assert_eq!(err.code, ErrorCode::INTERNAL_ERROR);
    }

    #[test]
    fn the_authenticated_machine_is_returned() {
        let mut extensions = Extensions::new();
        extensions.insert(parts(Some(laptop())));
        assert_eq!(calling_machine(&extensions).unwrap().as_str(), "laptop");
    }
}
