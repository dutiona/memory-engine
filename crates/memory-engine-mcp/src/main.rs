use std::sync::Arc;

use clap::Parser;
use memory_engine::MemoryEngine;
use memory_engine_mcp::{auth, config, embedding, http, server, summary};
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

    /// Path to TOML configuration file.
    #[arg(long, env = "MEMORY_MCP_CONFIG")]
    config: Option<std::path::PathBuf>,

    /// Path to the memory-engine `SQLite` database.
    #[arg(long, env = "MEMORY_MCP_DB_PATH")]
    db_path: Option<std::path::PathBuf>,

    /// Directory for the pre-migration backup; overrides `engine.backup_dir`.
    #[arg(long, env = "MEMORY_MCP_BACKUP_DIR")]
    backup_dir: Option<std::path::PathBuf>,

    /// Embedding endpoint URL (OpenAI-compatible).
    #[arg(long, env = "MEMORY_MCP_EMBED_URL")]
    embed_url: Option<String>,

    /// Embedding model name.
    #[arg(long, env = "MEMORY_MCP_EMBED_MODEL")]
    embed_model: Option<String>,

    /// Embedding API key (optional, for authenticated endpoints).
    #[arg(long, env = "MEMORY_MCP_EMBED_API_KEY")]
    embed_api_key: Option<String>,

    /// Embedding serving backend (e.g. `ollama`, `tei`, `openai`) — operator-declared,
    /// feeds the fingerprint. Free-form; unrecognized values warn but are accepted.
    #[arg(long, env = "MEMORY_MCP_EMBED_PROVIDER")]
    embed_provider: Option<String>,

    /// Summary / chat-completions endpoint URL (for consolidation).
    #[arg(long, env = "MEMORY_MCP_SUMMARY_URL")]
    summary_url: Option<String>,

    /// Summary model name.
    #[arg(long, env = "MEMORY_MCP_SUMMARY_MODEL")]
    summary_model: Option<String>,

    /// Summary API key (optional, for authenticated endpoints).
    #[arg(long, env = "MEMORY_MCP_SUMMARY_API_KEY")]
    summary_api_key: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    tracing_subscriber::registry()
        .with(fmt::layer().with_writer(std::io::stderr).with_ansi(false))
        .with(EnvFilter::from_default_env().add_directive(tracing::Level::WARN.into()))
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

    // 1. Load config (TOML file, with CLI overrides)
    let mcp_config = load_config(&cli)?;

    // 2. Resolve embed_dim: explicit config > DB probe
    let embed_dim = match mcp_config.engine.embed_dim {
        Some(dim) => dim,
        None => config::probe_embed_dim(&mcp_config.engine.db_path)
            .map_err(|e| format!("cannot determine embed_dim: {e}"))?,
    };

    // 3. Open MemoryEngine
    let mut builder = MemoryEngine::builder(embed_dim).path(mcp_config.engine.db_path.clone());
    if let Some(dir) = &mcp_config.engine.backup_dir {
        // Checked now: `VACUUM INTO` would only fail at the next pending migration.
        if !dir.is_dir() {
            return Err(format!("engine.backup_dir is not a directory: {}", dir.display()).into());
        }
        builder = builder.backup_dir(dir.clone());
    }
    let engine = builder
        .build()
        .map_err(|e| format!("failed to open engine: {e}"))?;
    let engine = Arc::new(engine);

    // 4. Initialize embedding provider (optional)
    // Use the resolved embed_dim (from DB probe or config), not the TOML value.
    // Both builders make a blocking reqwest client, which in debug builds creates and drops
    // a runtime of its own: that panics unless this thread leaves the async context first.
    let embedder = tokio::task::block_in_place(|| build_embedder(&cli, &mcp_config, embed_dim))?;

    // 4b. Eager embedding-identity check (#614, §Design.2). The query path embeds at
    // this layer and hands the engine a pre-computed vector, so the engine can't
    // fingerprint-check per query — verify once at startup that the configured provider
    // matches the store's recorded identity, and refuse to serve on a mismatch rather
    // than silently returning wrong-vector-space query results.
    if let Some(provider) = embedder.as_deref() {
        engine
            .verify_embedding_identity(provider)
            .await
            .map_err(|e| format!("embedding identity check failed: {e}"))?;
    }

    // 5. Initialize summary generator (optional — text only; embedding is done
    //    by the embedder, injected separately into consolidation per #116)
    let summary_gen = tokio::task::block_in_place(|| build_summary_generator(&cli, &mcp_config))?
        .map(|sg| sg as Arc<dyn memory_engine::traits::SummaryGenerator + Send + Sync>);

    // 6. Construct and serve
    let mcp_server = server::MemoryMcpServer::new(engine, embedder, summary_gen, embed_dim);

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

/// Build the embedding provider from config + CLI, using the probed `embed_dim`.
///
/// CLI flags override TOML values (endpoint, model, `api_key`) when provided,
/// enabling operators to inject runtime secrets via env/CLI.
/// Recognized embedding backends. `provider` is free-form (it feeds the fingerprint),
/// but a value outside this set warns at startup to catch typos.
const KNOWN_PROVIDERS: [&str; 3] = ["ollama", "tei", "openai"];

fn build_embedder(
    cli: &Cli,
    mcp_config: &config::McpConfig,
    embed_dim: usize,
) -> Result<Option<Arc<embedding::HttpEmbeddingProvider>>, BoxError> {
    // Merge TOML + CLI: CLI overrides individual fields
    let base = mcp_config.embedding.as_ref();

    let endpoint = cli
        .embed_url
        .clone()
        .or_else(|| base.map(|b| b.endpoint.clone()));
    let model = cli
        .embed_model
        .clone()
        .or_else(|| base.map(|b| b.model.clone()));
    let api_key = cli
        .embed_api_key
        .clone()
        .or_else(|| base.and_then(|b| b.api_key.clone()));
    // `provider` feeds the fingerprint (#614); source it from CLI > TOML, defaulting to
    // "ollama" (also EmbeddingSection's serde default) for the CLI-only / legacy path.
    let provider = cli
        .embed_provider
        .clone()
        .or_else(|| base.map(|b| b.provider.clone()))
        .unwrap_or_else(|| "ollama".to_string());
    let timeout = base.map_or(30, |b| b.timeout_secs);
    let query_instruction = base.and_then(|b| b.query_instruction.clone());
    let mrl_dim = base.and_then(|b| b.mrl_dim);

    let (Some(url), Some(mdl)) = (endpoint, model) else {
        return Ok(None);
    };

    // `native_dim` is the dimension the model emits — what the provider validates the
    // raw HTTP response against. With MRL the provider truncates to `mrl_dim`, so the
    // native dim comes from the config's `dimensions`; without MRL native == stored ==
    // `embed_dim`. Always read `dimensions` (falling back to `embed_dim` only on the
    // CLI-only path with no `[embedding]` section) so a `dimensions != embed_dim`
    // misconfig surfaces at startup instead of failing cryptically on the first embed.
    let native_dim = base.map_or(embed_dim, |b| b.dimensions);

    // `provider` is stamped verbatim into the persisted fingerprint (#614), like the
    // equally free-form `model`. We don't hard-restrict it — custom OpenAI-compatible
    // backends are valid — but warn on an unrecognized value to catch typos before they
    // bake a bogus identity into a fresh store.
    if !KNOWN_PROVIDERS.contains(&provider.as_str()) {
        tracing::warn!(
            provider = %provider,
            "embedding provider is not one of {KNOWN_PROVIDERS:?}; it is stamped into the \
             persisted embedding fingerprint as-is — check for a typo"
        );
    }

    let mut provider =
        embedding::HttpEmbeddingProvider::new(url, mdl, provider, api_key, native_dim, timeout)
            .map_err(|e| format!("failed to create embedding provider: {e}"))?;

    if let Some(instruction) = query_instruction {
        provider = provider.with_query_instruction(instruction);
    }
    if let Some(target) = mrl_dim {
        // The engine stores post-truncation vectors, so the MRL target MUST equal the
        // engine's `embed_dim`; otherwise the engine would reject every truncated vector.
        // Fail loudly here at startup rather than on the first embed call.
        if target != embed_dim {
            return Err(format!(
                "embedding.mrl_dim ({target}) must equal the engine embed_dim ({embed_dim}): \
                 the engine stores post-truncation vectors"
            )
            .into());
        }
        provider = provider
            .with_mrl_dim(target)
            .map_err(|e| format!("invalid embedding.mrl_dim: {e}"))?;
    } else if native_dim != embed_dim {
        // No truncation: the native dim the provider validates against must equal the
        // engine's stored dim. An unequal pair is a misconfiguration — surface it now.
        return Err(format!(
            "embedding.dimensions ({native_dim}) must equal the engine embed_dim ({embed_dim}) \
             when MRL is disabled (native and stored dimensions are identical without truncation)"
        )
        .into());
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
            summary: None,
        }
    };

    // CLI overrides
    if let Some(db_path) = &cli.db_path {
        mcp_config.engine.db_path.clone_from(db_path);
    }
    if let Some(backup_dir) = &cli.backup_dir {
        mcp_config.engine.backup_dir = Some(backup_dir.clone());
    }

    // NOTE: Embedding config from CLI is handled in build_embedder(),
    // which runs after embed_dim probe to avoid dimension mismatch.

    Ok(mcp_config)
}

/// Build the summary generator from config + CLI.
///
/// The generator produces summary text only; embedding of those summaries is
/// performed by the separately-configured embedder at consolidation time
/// (issue #116). The `memory_consolidate` tool therefore needs *both* a summary
/// generator and an embedder — that requirement is enforced in the tool handler.
fn build_summary_generator(
    cli: &Cli,
    mcp_config: &config::McpConfig,
) -> Result<Option<Arc<summary::HttpSummaryGenerator>>, BoxError> {
    let base = mcp_config.summary.as_ref();

    let endpoint = cli
        .summary_url
        .clone()
        .or_else(|| base.map(|b| b.endpoint.clone()));
    let model = cli
        .summary_model
        .clone()
        .or_else(|| base.map(|b| b.model.clone()));
    let api_key = cli
        .summary_api_key
        .clone()
        .or_else(|| base.and_then(|b| b.api_key.clone()));
    let timeout = base.map_or(120, |b| b.timeout_secs);

    match (endpoint, model) {
        (Some(url), Some(mdl)) => Ok(Some(Arc::new(
            summary::HttpSummaryGenerator::new(url, mdl, api_key, timeout)
                .map_err(|e| format!("failed to create summary generator: {e}"))?,
        ))),
        _ => Ok(None),
    }
}
