use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Top-level MCP server configuration.
///
/// Loaded from TOML file, with CLI args and env vars as overrides.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpConfig {
    /// Engine database configuration.
    pub engine: EngineSection,

    /// Embedding provider configuration (optional — needed for add_fact + vector queries).
    pub embedding: Option<EmbeddingSection>,
}

/// Engine / database configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineSection {
    /// Path to the memory-engine SQLite database.
    pub db_path: PathBuf,

    /// Embedding dimension. If omitted, probed from existing database.
    pub embed_dim: Option<usize>,
}

/// Embedding provider configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingSection {
    /// OpenAI-compatible embedding endpoint URL (e.g. `http://localhost:11434/v1/embeddings`).
    pub endpoint: String,

    /// Model name to request (e.g. `nomic-embed-text`).
    pub model: String,

    /// API key (optional — for authenticated endpoints).
    pub api_key: Option<String>,

    /// Expected embedding dimensions. Used to validate responses.
    pub dimensions: usize,

    /// HTTP timeout in seconds. Default: 30.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

const fn default_timeout() -> u64 {
    30
}

/// Probe `embed_dim` from an existing database by reading the config table.
///
/// Mirrors the pattern from `memory-engine-cli/src/db.rs`.
///
/// # Errors
///
/// Returns an error if the database doesn't exist or has no `embed_dim` config.
pub fn probe_embed_dim(db_path: &Path) -> Result<usize, String> {
    if !db_path.is_file() {
        return Err(format!(
            "database path is not a file or does not exist: {}",
            db_path.display()
        ));
    }
    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("cannot open database: {e}"))?;

    let dim_str: String = conn
        .query_row(
            "SELECT value FROM config WHERE key = 'embed_dim'",
            [],
            |row| row.get(0),
        )
        .map_err(|_| "database has no embed_dim in config table".to_owned())?;

    dim_str
        .parse::<usize>()
        .map_err(|e| format!("invalid embed_dim value '{dim_str}': {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_full_config() {
        let toml_str = r#"
[engine]
db_path = "/tmp/test.db"
embed_dim = 384

[embedding]
endpoint = "http://localhost:11434/v1/embeddings"
model = "nomic-embed-text"
dimensions = 384
"#;
        let config: McpConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.engine.embed_dim, Some(384));
        assert!(config.embedding.is_some());
        let emb = config.embedding.unwrap();
        assert_eq!(emb.model, "nomic-embed-text");
        assert_eq!(emb.timeout_secs, 30); // default
    }

    #[test]
    fn deserialize_minimal_config() {
        let toml_str = r#"
[engine]
db_path = "/tmp/test.db"
"#;
        let config: McpConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.engine.embed_dim, None);
        assert!(config.embedding.is_none());
    }

    #[test]
    fn probe_nonexistent_db() {
        let result = probe_embed_dim(Path::new("/tmp/nonexistent_memory_engine_test.db"));
        assert!(result.is_err());
    }
}
