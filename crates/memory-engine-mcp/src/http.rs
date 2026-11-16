//! The HTTP surface: rmcp's stateless streamable-HTTP service on `/mcp`, behind one
//! Bearer layer that covers every route (ADR-0020 §3, §4).
//!
//! The layer answers 401 before rmcp runs, so an unauthenticated caller learns nothing
//! about the Host check (403) or the method check (405), and an unknown path is a 401 too.

use std::future::Future;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::net::TcpListener;

use crate::auth::TokenTable;
use crate::server::MemoryMcpServer;

/// Serve MCP on `listener` until `shutdown` resolves.
///
/// `allowed_hosts` are added to rmcp's loopback default, never in place of it, so the
/// Host list can never be empty (rmcp reads an empty list as "admit every host").
///
/// # Errors
///
/// Returns the I/O error that stops the server.
pub async fn serve(
    listener: TcpListener,
    server: MemoryMcpServer,
    tokens: TokenTable,
    allowed_hosts: Vec<String>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, router(server, tokens, allowed_hosts))
        .with_graceful_shutdown(shutdown)
        .await
}

fn router(server: MemoryMcpServer, tokens: TokenTable, allowed_hosts: Vec<String>) -> Router {
    let mut config = StreamableHttpServerConfig::default()
        .with_stateful_mode(false)
        .with_json_response(true);
    config.allowed_hosts.extend(allowed_hosts);
    let mcp = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(NeverSessionManager::default()),
        config,
    );
    Router::new()
        .route_service("/mcp", mcp)
        // Last on purpose: a route added after this layer would be served unauthenticated.
        .layer(from_fn_with_state(Arc::new(tokens), require_machine))
}

async fn require_machine(
    State(tokens): State<Arc<TokenTable>>,
    mut request: Request,
    next: Next,
) -> Response {
    let machine = {
        let mut values = request.headers().get_all(AUTHORIZATION).iter();
        match (values.next(), values.next()) {
            (Some(value), None) => tokens.authenticate(value.as_bytes()).cloned(),
            _ => None,
        }
    };
    let Some(machine) = machine else {
        return (StatusCode::UNAUTHORIZED, [(WWW_AUTHENTICATE, "Bearer")]).into_response();
    };
    // Nothing behind this layer needs the token, so nothing behind it can leak it.
    request.headers_mut().remove(AUTHORIZATION);
    request.extensions_mut().insert(machine);
    next.run(request).await
}
