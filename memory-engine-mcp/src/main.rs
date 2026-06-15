use std::sync::Arc;

use clap::Parser;
use memory_engine::MemoryEngine;
use memory_engine_mcp::{config, embedding, server, summary};
use rmcp::ServiceExt;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// MCP server for memory-engine — exposes agent memory as MCP tools over stdio.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Path to TOML configuration file.
    #[arg(long, env = "MEMORY_MCP_CONFIG")]
    config: Option<std::path::PathBuf>,

    /// Path to the memory-engine SQLite database.
    #[arg(long, env = "MEMORY_MCP_DB_PATH")]
    db_path: Option<std::path::PathBuf>,

    /// Embedding endpoint URL (OpenAI-compatible).
    #[arg(long, env = "MEMORY_MCP_EMBED_URL")]
    embed_url: Option<String>,

    /// Embedding model name.
    #[arg(long, env = "MEMORY_MCP_EMBED_MODEL")]
    embed_model: Option<String>,

    /// Embedding API key (optional, for authenticated endpoints).
    #[arg(long, env = "MEMORY_MCP_EMBED_API_KEY")]
    embed_api_key: Option<String>,

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
    // MCP uses stdout for JSON-RPC — all logging goes to stderr.
    tracing_subscriber::registry()
        .with(fmt::layer().with_writer(std::io::stderr).with_ansi(false))
        .with(EnvFilter::from_default_env().add_directive(tracing::Level::WARN.into()))
        .init();

    let cli = Cli::parse();

    // 1. Load config (TOML file, with CLI overrides)
    let mcp_config = load_config(&cli)?;

    // 2. Resolve embed_dim: explicit config > DB probe
    let embed_dim = match mcp_config.engine.embed_dim {
        Some(dim) => dim,
        None => config::probe_embed_dim(&mcp_config.engine.db_path)
            .map_err(|e| format!("cannot determine embed_dim: {e}"))?,
    };

    // 3. Open MemoryEngine
    let engine = MemoryEngine::builder(embed_dim)
        .path(mcp_config.engine.db_path.clone())
        .build()
        .map_err(|e| format!("failed to open engine: {e}"))?;
    let engine = Arc::new(engine);

    // 4. Initialize embedding provider (optional)
    // Use the resolved embed_dim (from DB probe or config), not the TOML value.
    let embedder = build_embedder(&cli, &mcp_config, embed_dim)?;

    // 5. Initialize summary generator (optional — text only; embedding is done
    //    by the embedder, injected separately into consolidation per #116)
    let summary_gen = build_summary_generator(&cli, &mcp_config)?
        .map(|sg| sg as Arc<dyn memory_engine::traits::SummaryGenerator + Send + Sync>);

    // 6. Construct and serve
    let mcp_server = server::MemoryMcpServer::new(engine, embedder, summary_gen, embed_dim);

    tracing::info!("memory-engine-mcp starting on stdio");
    let transport = rmcp::transport::io::stdio();
    let service = mcp_server.serve(transport).await?;
    service.waiting().await?;

    Ok(())
}

/// Build the embedding provider from config + CLI, using the probed embed_dim.
///
/// CLI flags override TOML values (endpoint, model, api_key) when provided,
/// enabling operators to inject runtime secrets via env/CLI.
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
    let timeout = base.map_or(30, |b| b.timeout_secs);

    match (endpoint, model) {
        (Some(url), Some(mdl)) => Ok(Some(Arc::new(
            embedding::HttpEmbeddingProvider::new(url, mdl, api_key, embed_dim, timeout)
                .map_err(|e| format!("failed to create embedding provider: {e}"))?,
        ))),
        _ => Ok(None),
    }
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
            },
            embedding: None,
            summary: None,
        }
    };

    // CLI overrides
    if let Some(db_path) = &cli.db_path {
        mcp_config.engine.db_path = db_path.clone();
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
