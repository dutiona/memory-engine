use std::path::{Path, PathBuf};

use memory_engine::ServedArtifact;
use memory_engine::error::{ConflictError, MemoryError, Result as EngineResult};
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

    /// An existing directory where a schema migration first writes `<db>.v<N>.bak`.
    /// Unset means a migration runs without a backup.
    pub backup_dir: Option<PathBuf>,
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

    /// Operator-declared serving backend (e.g. `"llama.cpp"`, `"ollama"`, `"tei"`, `"openai"`). Cannot
    /// be sniffed from the endpoint (Ollama and TEI both speak `/v1/embeddings`); it feeds
    /// the [`EmbeddingFingerprint`](memory_engine::EmbeddingFingerprint) identity, so it
    /// must reflect the real backend. Free-form (like `model`); unrecognized values warn
    /// at startup but are accepted. Defaults to `"ollama"` for back-compat.
    #[serde(default = "default_provider")]
    pub provider: String,

    /// Expected embedding dimensions. This is the **native** dimension the model emits
    /// and the provider validates raw responses against. With Matryoshka truncation
    /// ([`mrl_dim`](Self::mrl_dim)), the engine stores the *truncated* `mrl_dim` while
    /// this stays the native (pre-truncation) length.
    pub dimensions: usize,

    /// Query-only instruction prefix, applied by the query path (`embed_query`); the
    /// document path (`add_fact`/`embed`) is never prefixed. Absent = ME's pinned instruction
    /// (ADR 0015 §6); `""` = no prefix, for a symmetric model.
    #[serde(default)]
    pub query_instruction: Option<String>,

    /// Optional Matryoshka (MRL) truncation target. When set, each returned vector is
    /// sliced to `mrl_dim` and L2-renormalized. Must equal the engine `embed_dim` (the
    /// engine stores post-truncation vectors) and be `<= dimensions`. `None` = no
    /// truncation (store the native vector).
    #[serde(default)]
    pub mrl_dim: Option<usize>,

    /// HTTP timeout in seconds. Default: 30.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,

    /// Served artifact (ADR 0015 §1): SHA-256 of the model file the provider serves, as 64
    /// lowercase hex digits. The four served keys are all set or all absent.
    #[serde(default)]
    pub artifact_sha256: Option<String>,

    /// Served artifact: the pooling the server applies, e.g. `last`.
    #[serde(default)]
    pub pooling: Option<String>,

    /// Served artifact: the normalization of each output vector, e.g. `l2`.
    #[serde(default)]
    pub normalization: Option<String>,

    /// Served artifact: the maximum input length, in tokens, the server embeds.
    #[serde(default)]
    pub max_input_tokens: Option<usize>,
}

/// The embedder settings that flags and environment variables override, field by field,
/// over `[embedding]`.
///
/// Deliberately no `Debug`: it holds the embedding API key in clear text.
#[derive(clap::Args, Clone, Default)]
pub struct EmbeddingOverrides {
    /// Embedding endpoint URL (OpenAI-compatible).
    #[arg(long = "embed-url", env = "MEMORY_MCP_EMBED_URL")]
    pub url: Option<String>,

    /// Embedding model name.
    #[arg(long = "embed-model", env = "MEMORY_MCP_EMBED_MODEL")]
    pub model: Option<String>,

    /// Embedding API key (optional, for authenticated endpoints).
    #[arg(long = "embed-api-key", env = "MEMORY_MCP_EMBED_API_KEY")]
    pub api_key: Option<String>,

    /// Embedding serving backend (e.g. `llama.cpp`, `ollama`, `tei`, `openai`) —
    /// operator-declared, feeds the fingerprint. Free-form; unrecognized values warn.
    #[arg(long = "embed-provider", env = "MEMORY_MCP_EMBED_PROVIDER")]
    pub provider: Option<String>,

    /// Query instruction prefix; `''` for a symmetric model (default: ME's pinned one).
    #[arg(
        long = "embed-query-instruction",
        env = "MEMORY_MCP_EMBED_QUERY_INSTRUCTION"
    )]
    pub query_instruction: Option<String>,

    /// Served artifact: SHA-256 of the served model file (64 lowercase hex digits).
    #[arg(
        long = "embed-artifact-sha256",
        env = "MEMORY_MCP_EMBED_ARTIFACT_SHA256"
    )]
    pub artifact_sha256: Option<String>,

    /// Served artifact: pooling, e.g. `last`.
    #[arg(long = "embed-pooling", env = "MEMORY_MCP_EMBED_POOLING")]
    pub pooling: Option<String>,

    /// Served artifact: normalization, e.g. `l2`.
    #[arg(long = "embed-normalization", env = "MEMORY_MCP_EMBED_NORMALIZATION")]
    pub normalization: Option<String>,

    /// Served artifact: maximum input length in tokens.
    #[arg(
        long = "embed-max-input-tokens",
        env = "MEMORY_MCP_EMBED_MAX_INPUT_TOKENS"
    )]
    pub max_input_tokens: Option<usize>,
}

/// The embedder configuration after the CLI-over-TOML merge and its checks, ready to build
/// a provider from.
///
/// Deliberately no `Debug`: it holds the embedding API key in clear text.
#[derive(Clone)]
pub struct ResolvedEmbedding {
    /// Endpoint URL.
    pub endpoint: String,
    /// Model name, stamped into the fingerprint.
    pub model: String,
    /// API key, if any.
    pub api_key: Option<String>,
    /// Serving backend, stamped into the fingerprint (default `ollama`).
    pub provider: String,
    /// The native dimension the model emits.
    pub native_dim: usize,
    /// The query instruction override; `None` keeps the provider's pinned default.
    pub query_instruction: Option<String>,
    /// The Matryoshka truncation target, equal to the engine dimension when set.
    pub mrl_dim: Option<usize>,
    /// HTTP timeout in seconds.
    pub timeout_secs: u64,
    /// The declared served artifact (ADR 0015 §1), validated.
    pub served: Option<ServedArtifact>,
}

/// Merge the embedder settings — each field from its override, else from `[embedding]` —
/// and check them against the engine dimension, before any store is opened.
///
/// Returns `Ok(None)` when no embedder setting is configured at all. Without an
/// `[embedding]` section the native dimension is `engine_dim`.
///
/// # Errors
///
/// [`MemoryError::Conflict`] (`PolicyParameter`) when any embedder setting is given
/// without both a URL and a model, when the served artifact is partial or
/// invalid ([`ServedArtifact::from_parts`]), when `mrl_dim` differs from `engine_dim`, or
/// when, without MRL, the native dimension differs from `engine_dim`.
///
/// # Examples
///
/// ```
/// use memory_engine_mcp::config::{EmbeddingOverrides, resolve_embedding};
///
/// let overrides = EmbeddingOverrides {
///     url: Some("http://127.0.0.1:8080/v1/embeddings".into()),
///     model: Some("Qwen/Qwen3-Embedding-0.6B".into()),
///     provider: Some("llama.cpp".into()),
///     ..EmbeddingOverrides::default()
/// };
/// let resolved = resolve_embedding(&overrides, None, 1024).unwrap().unwrap();
/// assert_eq!(resolved.native_dim, 1024);
/// assert!(resolved.served.is_none());
/// ```
pub fn resolve_embedding(
    overrides: &EmbeddingOverrides,
    section: Option<&EmbeddingSection>,
    engine_dim: usize,
) -> EngineResult<Option<ResolvedEmbedding>> {
    let invalid = |msg: String| MemoryError::Conflict(ConflictError::PolicyParameter(msg));
    let pick = |cli: &Option<String>, toml: Option<String>| cli.clone().or(toml);
    let url = pick(&overrides.url, section.map(|b| b.endpoint.clone()));
    let model = pick(&overrides.model, section.map(|b| b.model.clone()));
    let api_key = pick(&overrides.api_key, section.and_then(|b| b.api_key.clone()));
    let provider = pick(&overrides.provider, section.map(|b| b.provider.clone()));
    let query_instruction = pick(
        &overrides.query_instruction,
        section.and_then(|b| b.query_instruction.clone()),
    );
    let served = ServedArtifact::from_parts(
        pick(
            &overrides.artifact_sha256,
            section.and_then(|b| b.artifact_sha256.clone()),
        ),
        pick(&overrides.pooling, section.and_then(|b| b.pooling.clone())),
        pick(
            &overrides.normalization,
            section.and_then(|b| b.normalization.clone()),
        ),
        overrides
            .max_input_tokens
            .or_else(|| section.and_then(|b| b.max_input_tokens)),
    )?;

    let (endpoint, model) = match (url, model) {
        (Some(endpoint), Some(model)) => (endpoint, model),
        (None, None)
            if api_key.is_none()
                && provider.is_none()
                && query_instruction.is_none()
                && served.is_none() =>
        {
            return Ok(None);
        }
        // Anything else would start with no embedder, and so no identity check, while the
        // operator believes one is configured.
        _ => {
            return Err(invalid(
                "the embedder needs both a url (--embed-url / MEMORY_MCP_EMBED_URL) and a \
                 model (--embed-model / MEMORY_MCP_EMBED_MODEL); other embedder settings were \
                 given without them"
                    .into(),
            ));
        }
    };

    // `native_dim` is what the provider validates raw responses against: the configured
    // `dimensions`, or the engine dim on the flag/env-only path with no `[embedding]`.
    let native_dim = section.map_or(engine_dim, |b| b.dimensions);
    let mrl_dim = section.and_then(|b| b.mrl_dim);
    match mrl_dim {
        // The engine stores post-truncation vectors, so the MRL target must equal it.
        Some(target) if target != engine_dim => {
            return Err(invalid(format!(
                "embedding.mrl_dim ({target}) must equal the engine embed_dim ({engine_dim}): \
                 the engine stores post-truncation vectors"
            )));
        }
        None if native_dim != engine_dim => {
            return Err(invalid(format!(
                "embedding.dimensions ({native_dim}) must equal the engine embed_dim \
                 ({engine_dim}) when MRL is disabled (native and stored dimensions are \
                 identical without truncation)"
            )));
        }
        _ => {}
    }

    Ok(Some(ResolvedEmbedding {
        endpoint,
        model,
        api_key,
        provider: provider.unwrap_or_else(default_provider),
        native_dim,
        query_instruction,
        mrl_dim,
        timeout_secs: section.map_or_else(default_timeout, |b| b.timeout_secs),
        served,
    }))
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

fn default_provider() -> String {
    "ollama".to_string()
}

const fn default_timeout() -> u64 {
    30
}

const fn default_summary_timeout() -> u64 {
    120
}

/// Probe the embedding dimension from an existing database.
///
/// Reads `dim` from the active `embedding_spaces` row (v13+, #622), falling back to the
/// legacy `embedding_meta` config row (v12, #613) and then to the bare `embed_dim` key of
/// pre-#613 databases. Opens the file read-only. Mirrors `peek_embed_dim_from_db` in
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

    // Preferred (v13+): the embedding_spaces registry's active row records `dim` (#622).
    // Guard on the table existing so an un-migrated v12 DB (no such table) falls through to
    // the legacy config paths below rather than erroring.
    let has_registry: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='embedding_spaces'",
            [],
            |_| Ok(true),
        )
        .optional()
        .map_err(|e| format!("schema query failed: {e}"))?
        .unwrap_or(false);
    if has_registry {
        let dim: Option<i64> = conn
            .query_row(
                "SELECT dim FROM embedding_spaces WHERE status = 'active'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("embedding_spaces query failed: {e}"))?;
        if let Some(dim) = dim {
            return usize::try_from(dim)
                .map_err(|_| "embedding_spaces 'dim' out of range".to_owned());
        }
    }

    // Legacy fallback: the pre-#622 embedding_meta config row records `dim` (#613).
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
backup_dir = "/tmp/backups"

[embedding]
endpoint = "http://localhost:11434/v1/embeddings"
model = "nomic-embed-text"
dimensions = 384
"#;
        let config: McpConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.engine.embed_dim, Some(384));
        assert_eq!(
            config.engine.backup_dir,
            Some(PathBuf::from("/tmp/backups"))
        );
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

    #[test]
    fn probe_reads_dim_from_embedding_spaces_registry() {
        // #622: a v13 store records the identity (incl. dim) in the embedding_spaces
        // table, not the config row. The probe must read it from there.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("probe.db");
        // Build creates the v13 schema (empty registry); the temporary engine drops here.
        memory_engine::MemoryEngine::builder(8)
            .path(path.clone())
            .build()
            .unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO embedding_spaces (name, model, provider, dim, status)
             VALUES ('default', 'm', 'tei', 8, 'active')",
            [],
        )
        .unwrap();
        drop(conn);
        assert_eq!(probe_embed_dim(&path).unwrap(), 8);
    }
}
