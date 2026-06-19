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

    /// Embedding provider configuration (optional — needed for `add_fact` + vector queries).
    pub embedding: Option<EmbeddingSection>,

    /// Summary generator configuration (optional — needed for consolidation).
    ///
    /// Requires `[embedding]` to also be configured, since summaries must be embedded.
    pub summary: Option<SummarySection>,
}

/// Engine / database configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineSection {
    /// Path to the memory-engine `SQLite` database.
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

/// Summary / LLM provider configuration for consolidation.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SummarySection {
    /// Chat-completions endpoint URL (e.g. `https://api.openai.com/v1/chat/completions`).
    pub endpoint: String,

    /// Model name to request (e.g. `gpt-4o-mini`).
    pub model: String,

    /// API key (optional — for authenticated endpoints).
    pub api_key: Option<String>,

    /// HTTP timeout in seconds. Default: 120 (LLM inference is slower than embedding).
    #[serde(default = "default_summary_timeout")]
    pub timeout_secs: u64,
}

const fn default_timeout() -> u64 {
    30
}

const fn default_summary_timeout() -> u64 {
    120
}

/// Probe the embedding dimension from an existing database.
///
/// Reads the persisted identity from the `embedding_meta` config row (#613,
/// ADR 0015) and extracts its `dim`, falling back to the legacy bare `embed_dim`
/// key for pre-#613 databases. Mirrors `peek_embed_dim_from_db` in
/// `memory-engine-cli/src/db.rs`.
///
/// # Errors
///
/// Returns an error if the database doesn't exist, has no recorded embedding
/// identity yet (nothing embedded), or the stored value is malformed.
pub fn probe_embed_dim(db_path: &Path) -> Result<usize, String> {
    use rusqlite::OptionalExtension;

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

    // Preferred: the embedding_meta identity tuple records `dim` (#613).
    let meta_raw: Option<String> = conn
        .query_row(
            "SELECT value FROM config WHERE key = 'embedding_meta'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("config query failed: {e}"))?;
    if let Some(raw) = meta_raw {
        let meta: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| format!("corrupt embedding_meta in config table: {e}"))?;
        let dim = meta
            .get("dim")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "embedding_meta is missing a numeric 'dim' field".to_owned())?;
        return usize::try_from(dim).map_err(|_| "embedding_meta 'dim' out of range".to_owned());
    }

    // Legacy fallback: a pre-#613 database carried a bare `embed_dim` key.
    let legacy: Option<String> = conn
        .query_row(
            "SELECT value FROM config WHERE key = 'embed_dim'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("config query failed: {e}"))?;
    if let Some(dim_str) = legacy {
        return dim_str
            .parse::<usize>()
            .map_err(|e| format!("invalid embed_dim value '{dim_str}': {e}"));
    }

    Err("database has no embedding identity yet (nothing has been embedded)".to_owned())
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
        assert!(config.summary.is_none());
    }

    #[test]
    fn deserialize_config_with_summary() {
        let toml_str = r#"
[engine]
db_path = "/tmp/test.db"

[embedding]
endpoint = "http://localhost:11434/v1/embeddings"
model = "nomic-embed-text"
dimensions = 384

[summary]
endpoint = "http://localhost:11434/v1/chat/completions"
model = "llama3"
"#;
        let config: McpConfig = toml::from_str(toml_str).unwrap();
        let summary = config.summary.unwrap();
        assert_eq!(summary.model, "llama3");
        assert_eq!(summary.timeout_secs, 120); // default
        assert!(summary.api_key.is_none());
    }

    #[test]
    fn probe_nonexistent_db() {
        let result = probe_embed_dim(Path::new("/tmp/nonexistent_memory_engine_test.db"));
        assert!(result.is_err());
    }
}
