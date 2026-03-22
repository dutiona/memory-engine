mod config;
mod depth;
mod embedding;
mod error;
mod server;
mod tools;

use clap::Parser;
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

    let _cli = Cli::parse();

    // TODO: Phase B-D implementation
    // 1. Load config (TOML + env + CLI layering)
    // 2. Probe embed_dim from existing DB if needed
    // 3. Open MemoryEngine
    // 4. Initialize HttpEmbeddingProvider
    // 5. Construct MemoryMcpServer
    // 6. Serve over stdio

    tracing::info!("memory-engine-mcp starting");
    eprintln!("memory-engine-mcp: not yet implemented — scaffold only");
    std::process::exit(1);
}
