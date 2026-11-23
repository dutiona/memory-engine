use std::sync::Arc;

use clap::Parser;
use memory_engine::MemoryEngine;
use memory_engine_mcp::{auth, config, embedding, home, http, server, tools};
use tokio::signal::unix::{SignalKind, signal};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// MCP server for memory-engine — exposes agent memory as MCP tools over streamable HTTP.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Address to serve MCP on, e.g. `127.0.0.1:<port>`.
    #[arg(long, env = "MEMORY_MCP_LISTEN")]
    listen: std::net::SocketAddr,

    /// The token-hash file: one `<machine> <sha256-hex>` line per machine (ADR-0020 §4).
    #[arg(long, env = "MEMORY_MCP_TOKEN_FILE")]
    token_file: std::path::PathBuf,

    /// A `Host` value to serve besides loopback (repeatable; the variable takes a comma
    /// list without spaces). An entry without a port admits any port.
    #[arg(
        long = "allowed-host",
        env = "MEMORY_MCP_ALLOWED_HOSTS",
        value_delimiter = ',',
        value_parser = http::parse_allowed_host
    )]
    allowed_hosts: Vec<String>,

    /// The owner whose user level (`user:<user>`) this server reads and files (ADR-0020 §7).
    #[arg(long, env = "MEMORY_MCP_USER", value_parser = home::User::parse)]
    user: home::User,

    /// Path to TOML configuration file.
    #[arg(long, env = "MEMORY_MCP_CONFIG")]
    config: Option<std::path::PathBuf>,

    /// Path to the memory-engine `SQLite` database.
    #[arg(long, env = "MEMORY_MCP_DB_PATH")]
    db_path: Option<std::path::PathBuf>,

    /// Directory for the pre-migration backup; overrides `engine.backup_dir`.
    #[arg(long, env = "MEMORY_MCP_BACKUP_DIR")]
    backup_dir: Option<std::path::PathBuf>,

    /// The engine's stored embedding dimension; overrides `engine.embed_dim`. Needed to
    /// start on a blank store without a TOML file.
    #[arg(long, env = "MEMORY_MCP_EMBED_DIM")]
    embed_dim: Option<usize>,

    /// Embedder settings, each overriding its `[embedding]` key.
    #[command(flatten)]
    embedding: config::EmbeddingOverrides,
}

fn main() -> Result<(), BoxError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run());
    // Dropping the runtime would wait for every blocking call, an embed the drain abandoned
    // included, until the provider's timeout. The flush ran inside `serve` and no task can
    // write past its fence, so nothing left is waited for.
    runtime.shutdown_background();
    result
}

async fn run() -> Result<(), BoxError> {
    tracing_subscriber::registry()
        .with(fmt::layer().with_writer(std::io::stderr).with_ansi(false))
        .with(log_filter(&std::env::var("RUST_LOG").unwrap_or_default()))
        .init();

    let cli = Cli::parse();

    // Before anything slow: a stop that arrives during startup or once the port is open
    // must take the draining path, never the signal's default kill.
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;
    let shutdown = async move {
        tokio::select! {
            _ = sigterm.recv() => {}
            _ = sigint.recv() => {}
        }
    };

    // 0. The token file first: a bad one must never get as far as migrating the store.
    let tokens = auth::TokenTable::load(&cli.token_file).map_err(|e| e.to_string())?;
    // Every model-tool answer starts with a call ID drawn from the OS RNG (ADR-0020 §8). One
    // draw now, before anything opens, so a served call never meets an uninitialised RNG.
    tools::new_call_id().map_err(|e| format!("the OS random number generator failed: {e}"))?;

    // 1. Load config (TOML file, with CLI overrides)
    let mcp_config = load_config(&cli)?;

    // 2. Resolve embed_dim: explicit config > what the store records, read at its own schema
    //    version without migrating it
    let embed_dim = match mcp_config.engine.embed_dim {
        Some(dim) => dim,
        None => memory_engine::read_recorded_embedding(&mcp_config.engine.db_path)
            .map_err(|e| format!("cannot determine embed_dim: {e}"))?
            .dim()
            .ok_or("cannot determine embed_dim: the database records no dimension yet (nothing has been stored)")?,
    };

    // 3. Resolve the embedder configuration and build the provider before the store opens,
    //    so a configuration refusal creates no database and migrates nothing. The builder
    //    makes a blocking reqwest client, which in debug builds creates and drops a runtime of
    //    its own: that panics unless this thread leaves the async context first.
    let resolved =
        config::resolve_embedding(&cli.embedding, mcp_config.embedding.as_ref(), embed_dim)
            .map_err(|e| format!("invalid embedding configuration: {e}"))?;
    let embedder = tokio::task::block_in_place(|| build_embedder(resolved))?;

    // 4. Open MemoryEngine
    // The token file's machines are the identifiers the engine keeps as written in the
    // records and memories it files for them, where `erase --machine` finds them (#1136).
    let mut builder = MemoryEngine::builder(embed_dim)
        .machines(tokens.machines().map(|m| m.as_str().to_owned()))
        .path(mcp_config.engine.db_path.clone());
    if let Some(dir) = &mcp_config.engine.backup_dir {
        // Checked now: `VACUUM INTO` would only fail at the next pending migration.
        if !dir.is_dir() {
            return Err(format!(
                "the backup directory is not a directory (engine.backup_dir, --backup-dir or MEMORY_MCP_BACKUP_DIR): {}",
                dir.display()
            ).into());
        }
        builder = builder.backup_dir(dir.clone());
    }
    // 5. The embedding-identity check (#614, #1065). The query path embeds at this layer and
    //    hands the engine a pre-computed vector, so the engine can't fingerprint-check per
    //    query: the open refuses a store whose recorded identity differs from the configured
    //    provider's, rather than serve wrong-vector-space results. It reads the store
    //    read-only before anything migrates it, so a refused start leaves the file as it was.
    if let Some(provider) = embedder.as_deref() {
        builder =
            builder.declared_identity(memory_engine::EmbeddingProvider::fingerprint(provider));
    }
    // The store lock (ADR-0020 §8), held until `run` returns: no maintenance command and no
    // second server writes beside this one. Taken after every configuration check and before
    // the open, so the identity is read under it and a refusal still writes nothing.
    let _store_lock =
        memory_engine::StoreLock::acquire(&mcp_config.engine.db_path).map_err(|e| match e {
            memory_engine::StoreLockError::Held { .. } => {
                format!("{e}: stop the other server or maintenance command first")
            }
            memory_engine::StoreLockError::Io { .. } => e.to_string(),
        })?;
    let engine = builder.build().map_err(|e| match e {
        memory_engine::MemoryError::EmbeddingModelMismatch { .. }
        | memory_engine::MemoryError::EmbeddingDimension { .. } => {
            format!("embedding identity check failed: {e}")
        }
        e => format!("failed to open engine: {e}"),
    })?;
    let engine = Arc::new(engine);

    // 6. Construct and serve. The author denylist is read once: the backfill route requires
    //    it, live capture does not use it, so the server starts without it and the route
    //    refuses every upload.
    let mut mcp_server = server::MemoryMcpServer::new(engine, embedder, cli.user.clone());
    if let Some(literals) = load_denylist() {
        mcp_server = mcp_server.with_denylist(literals);
    }

    let listener = tokio::net::TcpListener::bind(cli.listen).await?;
    tracing::info!("memory-engine-mcp listening on {}", listener.local_addr()?);
    http::serve(
        listener,
        mcp_server,
        tokens,
        cli.allowed_hosts.clone(),
        shutdown,
    )
    .await?;

    Ok(())
}

/// The log filter for `RUST_LOG` (#1149): a bare level applies to every crate, a crate-only
/// directive raises only that crate while every other crate stays at WARN, and no `RUST_LOG`
/// gives WARN. The `warn` goes first: a later bare level replaces it, while a crate directive
/// sits beside it. `with_default_directive` would not work: tracing-subscriber adds its default
/// only when the filter holds no directive at all.
fn log_filter(rust_log: &str) -> EnvFilter {
    EnvFilter::builder().parse_lossy(format!("warn,{rust_log}"))
}

/// The author denylist of `ME_REDACT_DENYLIST_FILE` (#1026): its literals when the variable is
/// set and the file readable, an empty or comments-only file meaning an explicit "none"; `None`
/// when the variable is unset or the file unreadable, logged as which.
fn load_denylist() -> Option<Vec<String>> {
    use memory_engine::bootstrap::{DENYLIST_ENV_VAR, load_secret_denylist};
    if std::env::var_os(DENYLIST_ENV_VAR).is_none() {
        tracing::warn!("{DENYLIST_ENV_VAR} is unset: backfill uploads answer 503");
        return None;
    }
    match load_secret_denylist() {
        Ok(literals) => {
            tracing::info!(
                literals = literals.len(),
                "author denylist loaded for backfill uploads"
            );
            Some(literals)
        }
        Err(error) => {
            tracing::warn!(%error, "the author denylist is unreadable: backfill uploads answer 503");
            None
        }
    }
}

/// Recognized embedding backends. `provider` is free-form (it feeds the fingerprint),
/// but a value outside this set warns at startup to catch typos.
const KNOWN_PROVIDERS: [&str; 4] = ["llama.cpp", "ollama", "tei", "openai"];

/// Build the embedding provider from the resolved configuration (`None` = no embedder).
fn build_embedder(
    resolved: Option<config::ResolvedEmbedding>,
) -> Result<Option<Arc<embedding::HttpEmbeddingProvider>>, BoxError> {
    let Some(r) = resolved else {
        return Ok(None);
    };
    // `provider` is stamped verbatim into the persisted fingerprint (#614), like the
    // equally free-form `model`. We don't hard-restrict it — custom OpenAI-compatible
    // backends are valid — but warn on an unrecognized value to catch typos before they
    // bake a bogus identity into a fresh store.
    if !KNOWN_PROVIDERS.contains(&r.provider.as_str()) {
        tracing::warn!(
            provider = %r.provider,
            "embedding provider is not one of {KNOWN_PROVIDERS:?}; it is stamped into the \
             persisted embedding fingerprint as-is — check for a typo"
        );
    }

    let mut provider = embedding::HttpEmbeddingProvider::new(
        r.endpoint,
        r.model,
        r.provider,
        r.api_key,
        r.native_dim,
        r.timeout_secs,
    )
    .map_err(|e| format!("failed to create embedding provider: {e}"))?;
    if let Some(instruction) = r.query_instruction {
        provider = provider.with_query_instruction(instruction);
    }
    if let Some(target) = r.mrl_dim {
        provider = provider
            .with_mrl_dim(target)
            .map_err(|e| format!("invalid embedding.mrl_dim: {e}"))?;
    }
    if let Some(served) = r.served {
        provider = provider.with_served_artifact(served);
    }
    Ok(Some(Arc::new(provider)))
}

/// Load configuration from TOML file with CLI overrides.
fn load_config(cli: &Cli) -> Result<config::McpConfig, BoxError> {
    // Start with TOML file if provided
    let mut mcp_config = if let Some(config_path) = &cli.config {
        if !config_path.is_file() {
            return Err(format!("config path is not a file: {}", config_path.display()).into());
        }
        let content = std::fs::read_to_string(config_path)
            .map_err(|e| format!("cannot read config file {}: {e}", config_path.display()))?;
        toml::from_str::<config::McpConfig>(&content)
            .map_err(|e| format!("invalid config file: {e}"))?
    } else {
        // Minimal config — db_path must come from CLI
        let db_path = cli
            .db_path
            .clone()
            .ok_or("either --config or --db-path is required")?;
        config::McpConfig {
            engine: config::EngineSection {
                db_path,
                embed_dim: None,
                backup_dir: None,
            },
            embedding: None,
        }
    };

    // CLI overrides
    if let Some(db_path) = &cli.db_path {
        mcp_config.engine.db_path.clone_from(db_path);
    }
    if let Some(backup_dir) = &cli.backup_dir {
        mcp_config.engine.backup_dir = Some(backup_dir.clone());
    }
    if let Some(dim) = cli.embed_dim {
        mcp_config.engine.embed_dim = Some(dim);
    }

    // The embedder settings are merged by `config::resolve_embedding`, once the engine
    // dimension is known.

    Ok(mcp_config)
}

#[cfg(test)]
mod tests {
    use super::log_filter;
    use tracing_subscriber::prelude::*;

    /// Each case's targets at WARN, INFO and DEBUG: `memory_engine_mcp`, then any other crate.
    #[test]
    fn log_filter_follows_the_three_rust_log_cases() {
        for (rust_log, own, other) in [
            ("info", [true, true, false], [true, true, false]),
            (
                "memory_engine_mcp=debug",
                [true, true, true],
                [true, false, false],
            ),
            ("", [true, false, false], [true, false, false]),
        ] {
            let subscriber = tracing_subscriber::registry().with(log_filter(rust_log));
            tracing::subscriber::with_default(subscriber, || {
                let seen_own = [
                    tracing::enabled!(target: "memory_engine_mcp", tracing::Level::WARN),
                    tracing::enabled!(target: "memory_engine_mcp", tracing::Level::INFO),
                    tracing::enabled!(target: "memory_engine_mcp", tracing::Level::DEBUG),
                ];
                let seen_other = [
                    tracing::enabled!(target: "other_crate", tracing::Level::WARN),
                    tracing::enabled!(target: "other_crate", tracing::Level::INFO),
                    tracing::enabled!(target: "other_crate", tracing::Level::DEBUG),
                ];
                assert_eq!(
                    (seen_own, seen_other),
                    (own, other),
                    "RUST_LOG={rust_log:?}"
                );
            });
        }
    }
}
