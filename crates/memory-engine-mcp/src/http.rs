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

/// Serve MCP on `listener` until `shutdown` resolves, then flush the sidecar snapshot.
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
    let engine = Arc::clone(&server.engine);
    let in_flight = Arc::clone(&server.in_flight);
    axum::serve(listener, router(server, tokens, allowed_hosts))
        .with_graceful_shutdown(shutdown)
        .await?;
    // axum returns once connections close, but rmcp runs each call in its own task, so a
    // call whose client hung up may still be writing: wait for every call to finish. A
    // flush beside a write could pair the new DB fingerprint with old projections. The
    // fence is never released, so a call queued behind it cannot commit after the flush;
    // the process exits once `serve` returns.
    std::mem::forget(in_flight.write_owned().await);
    // Best effort: the DB is the source of truth, so a failed flush only means the next
    // open rebuilds the projections from it.
    if let Err(e) = engine.flush_snapshot().await {
        tracing::warn!("failed to flush sidecar snapshot on shutdown: {e}");
    }
    Ok(())
}

/// Validate one allowed `Host` value: a host name or `host:port`, never a wildcard.
///
/// rmcp compares entries literally, so `*` would admit nothing while its author believes
/// it admits everything; userinfo would silently reduce to the bare host.
///
/// # Errors
///
/// Returns the reason the value is refused.
pub fn parse_allowed_host(value: &str) -> Result<String, String> {
    let refused = || {
        format!(
            "an allowed host is a host name or host:port, without '*', '@' or spaces: {value:?}"
        )
    };
    if value.is_empty() || value.trim() != value || value.contains(['*', '@']) {
        return Err(refused());
    }
    axum::http::uri::Authority::try_from(value).map_err(|_| refused())?;
    Ok(value.to_owned())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_allowed_host_accepts_authorities() {
        for host in ["memory.example", "memory.example:8443", "[::1]"] {
            assert_eq!(parse_allowed_host(host).unwrap(), host);
        }
    }

    #[test]
    fn parse_allowed_host_refuses_wildcards_userinfo_and_whitespace() {
        for host in [
            "",
            "*",
            "*.example",
            "user@memory.example",
            " memory.example",
            "memory.example ",
        ] {
            assert!(parse_allowed_host(host).is_err(), "{host:?}");
        }
    }
}
