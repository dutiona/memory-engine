use std::sync::Arc;

use memory_engine::Backoff;
use memory_engine::engine::MemoryEngine;
use rmcp::RoleServer;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ErrorData, Extensions, Implementation, InitializeResult,
    ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ToolsCapability,
};
use rmcp::service::RequestContext;
use serde_json::{Map, Value};

use crate::auth::Machine;
use crate::embedding::HttpEmbeddingProvider;
use crate::home::User;
use crate::project::ProjectFacts;
use crate::tools;

/// MCP server wrapping a [`MemoryEngine`] instance.
///
/// The engine is async-native (#631): every DB-touching method is an `async fn`, so tool
/// calls are `.await`ed directly on the runtime. The server holds an `Arc<MemoryEngine>` for
/// thread-safe sharing.
#[derive(Clone)]
pub struct MemoryMcpServer {
    pub(crate) engine: Arc<MemoryEngine>,
    pub(crate) embedder: Option<Arc<HttpEmbeddingProvider>>,
    pub(crate) user: User,
    /// Held for reading by every tool call and every drain settle; `http::serve` takes it
    /// for writing to stop them.
    pub(crate) in_flight: Arc<tokio::sync::RwLock<()>>,
    /// The embedding drain's retry schedule.
    pub(crate) embed_backoff: Backoff,
    /// The author denylist the backfill route (#1026) and live capture (#1121) redact content
    /// with; without one, the route refuses every upload and live capture applies signatures only.
    pub(crate) denylist: Option<Arc<[String]>>,
    /// The one backfill upload slot: an upload holds it from before its body is read.
    pub(crate) uploads: Arc<tokio::sync::Semaphore>,
}

impl MemoryMcpServer {
    /// Construct a server over an already-opened [`MemoryEngine`] for the owner `user`.
    #[must_use]
    pub fn new(
        engine: Arc<MemoryEngine>,
        embedder: Option<Arc<HttpEmbeddingProvider>>,
        user: User,
    ) -> Self {
        Self {
            engine,
            embedder,
            user,
            in_flight: Arc::new(tokio::sync::RwLock::new(())),
            embed_backoff: Backoff::DEFAULT,
            denylist: None,
            uploads: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }

    /// The author denylist the backfill route (#1026) and live capture (#1121) redact content
    /// with, never an identifier, as `main` loads it from `ME_REDACT_DENYLIST_FILE`; an empty list
    /// is an explicit "none". Without this, the route answers 503 to every upload and live capture
    /// applies signatures only.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::Arc;
    /// use memory_engine::MemoryEngine;
    /// use memory_engine_mcp::home::User;
    /// use memory_engine_mcp::server::MemoryMcpServer;
    ///
    /// let engine = Arc::new(MemoryEngine::builder(4).build()?);
    /// let server = MemoryMcpServer::new(engine, None, User::parse("alice")?)
    ///     .with_denylist(vec!["a-host-name".to_owned()]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn with_denylist(mut self, literals: Vec<String>) -> Self {
        self.denylist = Some(literals.into());
        self
    }

    /// The embedding drain's retry schedule, in place of [`Backoff::DEFAULT`].
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::Arc;
    /// use std::time::Duration;
    /// use memory_engine::{Backoff, MemoryEngine};
    /// use memory_engine_mcp::home::User;
    /// use memory_engine_mcp::server::MemoryMcpServer;
    ///
    /// let engine = Arc::new(MemoryEngine::builder(4).build()?);
    /// let fast = Backoff { base: Duration::from_millis(10), cap: Duration::from_millis(100) };
    /// let server = MemoryMcpServer::new(engine, None, User::parse("alice")?).with_embed_backoff(fast);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub const fn with_embed_backoff(mut self, backoff: Backoff) -> Self {
        self.embed_backoff = backoff;
        self
    }

    /// Run one tool call for the authenticated `machine`, with the project `facts` its
    /// request carried: the seam `call_tool`, the fuzz target and the tests drive.
    ///
    /// # Errors
    ///
    /// A JSON-RPC error for a name that is not a tool; the model tools answer every failure
    /// as a tool result instead.
    pub async fn dispatch(
        &self,
        machine: &Machine,
        facts: &ProjectFacts,
        name: &str,
        args: Map<String, Value>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = tools::CallContext {
            engine: &self.engine,
            embedder: self.embedder.as_ref(),
            user: &self.user,
            machine,
            facts,
            key: facts.key(machine),
        };
        tools::call_model_tool(name, args, &ctx)
            .await
            .unwrap_or_else(|| {
                Err(ErrorData::invalid_params(
                    format!("unknown tool: {name}"),
                    None,
                ))
            })
    }
}

/// The instructions name exactly the listed tools, so the two cannot drift apart.
fn instructions() -> String {
    let names: Vec<_> = tools::definitions().into_iter().map(|t| t.name).collect();
    format!(
        "memory-engine: long-term memory shared by your sessions. Tools: {}. The first line of \
         every answer is its call ID.",
        names.join(", ")
    )
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
            .with_instructions(instructions())
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, rmcp::model::ErrorData>> + Send + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(tools::definitions())))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let (machine, headers) = calling_request(&context.extensions)?;
        let facts = ProjectFacts::from_headers(headers);
        let _in_flight = self.in_flight.read().await;
        let args = request.arguments.unwrap_or_default();
        self.dispatch(machine, &facts, &request.name, args).await
    }
}

/// The machine the HTTP layer authenticated, and the request's headers. Without a machine,
/// nothing runs: the router always supplies it, so its absence means a mount that bypassed
/// the Bearer layer.
fn calling_request(
    extensions: &Extensions,
) -> Result<(&Machine, &axum::http::HeaderMap), ErrorData> {
    extensions
        .get::<axum::http::request::Parts>()
        .and_then(|parts| Some((parts.extensions.get::<Machine>()?, &parts.headers)))
        .ok_or_else(|| ErrorData::internal_error("no authenticated machine on this call", None))
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
        let err = calling_request(&Extensions::new()).unwrap_err();
        assert_eq!(err.code, ErrorCode::INTERNAL_ERROR);
    }

    #[test]
    fn parts_without_a_machine_are_refused() {
        let mut extensions = Extensions::new();
        extensions.insert(parts(None));
        let err = calling_request(&extensions).unwrap_err();
        assert_eq!(err.code, ErrorCode::INTERNAL_ERROR);
    }

    #[test]
    fn the_authenticated_machine_is_returned() {
        let mut extensions = Extensions::new();
        extensions.insert(parts(Some(laptop())));
        assert_eq!(calling_request(&extensions).unwrap().0.as_str(), "laptop");
    }
}
