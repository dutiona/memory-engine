mod config;
mod depth;
mod embedding;
mod error;
mod server;
mod tools;

use std::sync::Arc;

use clap::Parser;
use memory_engine::engine::{EngineConfig, MemoryEngine};
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
    let engine_config = EngineConfig::new(mcp_config.engine.db_path.clone(), embed_dim);
    let engine =
        MemoryEngine::open(&engine_config).map_err(|e| format!("failed to open engine: {e}"))?;
    let engine = Arc::new(engine);

    // 4. Initialize embedding provider (optional)
    let embedder = mcp_config.embedding.map(|emb_config| {
        Arc::new(embedding::HttpEmbeddingProvider::new(
            emb_config.endpoint,
            emb_config.model,
            emb_config.api_key,
            emb_config.dimensions,
            emb_config.timeout_secs,
        ))
    });

    // 5. Construct and serve
    let mcp_server = server::MemoryMcpServer::new(engine, embedder, embed_dim);

    tracing::info!("memory-engine-mcp starting on stdio");
    let transport = rmcp::transport::io::stdio();
    let service = mcp_server.serve(transport).await?;
    service.waiting().await?;

    Ok(())
}

/// Load configuration from TOML file with CLI overrides.
fn load_config(cli: &Cli) -> Result<config::McpConfig, BoxError> {
    // Start with TOML file if provided
    let mut mcp_config = if let Some(config_path) = &cli.config {
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
        }
    };

    // CLI overrides
    if let Some(db_path) = &cli.db_path {
        mcp_config.engine.db_path = db_path.clone();
    }

    // Build embedding config from CLI if not in TOML
    if mcp_config.embedding.is_none() {
        if let (Some(url), Some(model)) = (&cli.embed_url, &cli.embed_model) {
            mcp_config.embedding = Some(config::EmbeddingSection {
                endpoint: url.clone(),
                model: model.clone(),
                api_key: cli.embed_api_key.clone(),
                dimensions: mcp_config.engine.embed_dim.unwrap_or(384),
                timeout_secs: 30,
            });
        }
    }

    Ok(mcp_config)
}
