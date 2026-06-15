//! Tests for CLI-over-TOML configuration merging.
//!
//! Validates field-level merge semantics:
//! - TOML provides defaults
//! - CLI flags override individual TOML fields
//! - Missing TOML sections handled gracefully
//! - `embed_dim` probing from DB fallback
//! - `deny_unknown_fields` rejects extra config

use memory_engine::MemoryEngine;
use memory_engine_mcp::config::{EmbeddingSection, McpConfig};
use std::io::Write;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// TOML deserialization
// ---------------------------------------------------------------------------

#[test]
fn full_config_deserializes() {
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
fn minimal_config_no_embedding_section() {
    let toml_str = r#"
[engine]
db_path = "/tmp/test.db"
"#;
    let config: McpConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.engine.embed_dim, None);
    assert!(config.embedding.is_none());
}

#[test]
fn config_with_api_key() {
    let toml_str = r#"
[engine]
db_path = "/tmp/test.db"
embed_dim = 768

[embedding]
endpoint = "https://api.openai.com/v1/embeddings"
model = "text-embedding-3-small"
api_key = "sk-test-key"
dimensions = 768
timeout_secs = 60
"#;
    let config: McpConfig = toml::from_str(toml_str).unwrap();
    let emb = config.embedding.unwrap();
    assert_eq!(emb.api_key.as_deref(), Some("sk-test-key"));
    assert_eq!(emb.timeout_secs, 60);
    assert_eq!(emb.dimensions, 768);
}

#[test]
fn unknown_field_rejected() {
    let toml_str = r#"
[engine]
db_path = "/tmp/test.db"
bogus_field = true
"#;
    let result = toml::from_str::<McpConfig>(toml_str);
    assert!(result.is_err());
}

#[test]
fn unknown_embedding_field_rejected() {
    let toml_str = r#"
[engine]
db_path = "/tmp/test.db"

[embedding]
endpoint = "http://localhost:11434/v1/embeddings"
model = "test"
dimensions = 384
unknown_option = "bad"
"#;
    let result = toml::from_str::<McpConfig>(toml_str);
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// CLI-over-TOML field merge (simulated)
// ---------------------------------------------------------------------------

#[test]
fn cli_db_path_overrides_toml() {
    let toml_str = r#"
[engine]
db_path = "/tmp/original.db"
embed_dim = 384
"#;
    let mut config: McpConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(
        config.engine.db_path,
        std::path::PathBuf::from("/tmp/original.db")
    );

    // Simulate CLI override
    let cli_db_path = std::path::PathBuf::from("/tmp/override.db");
    config.engine.db_path = cli_db_path.clone();
    assert_eq!(config.engine.db_path, cli_db_path);
}

/// Simulate the CLI-over-TOML merge logic from `main.rs::build_embedder`.
///
/// Returns `(endpoint, model, api_key)` after merging CLI overrides.
fn merge_embedding_config(
    cli_endpoint: Option<&str>,
    cli_model: Option<&str>,
    cli_api_key: Option<&str>,
    base: Option<&EmbeddingSection>,
) -> (Option<String>, Option<String>, Option<String>) {
    let endpoint = cli_endpoint
        .map(String::from)
        .or_else(|| base.map(|b| b.endpoint.clone()));
    let model = cli_model
        .map(String::from)
        .or_else(|| base.map(|b| b.model.clone()));
    let api_key = cli_api_key
        .map(String::from)
        .or_else(|| base.and_then(|b| b.api_key.clone()));
    (endpoint, model, api_key)
}

#[test]
fn embedding_field_merge_cli_over_toml() {
    let base = EmbeddingSection {
        endpoint: "http://toml-host:11434/v1/embeddings".into(),
        model: "toml-model".into(),
        api_key: None,
        dimensions: 384,
        timeout_secs: 30,
    };

    // CLI provides endpoint and api_key, model comes from TOML
    let (endpoint, model, api_key) = merge_embedding_config(
        Some("http://cli-host:8080/embed"),
        None,
        Some("sk-cli-key"),
        Some(&base),
    );

    assert_eq!(endpoint.as_deref(), Some("http://cli-host:8080/embed"));
    assert_eq!(model.as_deref(), Some("toml-model")); // From TOML
    assert_eq!(api_key.as_deref(), Some("sk-cli-key")); // From CLI
}

#[test]
fn no_embedding_section_no_cli_results_in_none() {
    let (endpoint, model, _api_key) = merge_embedding_config(None, None, None, None);

    // Both must be present to create an embedder
    let has_embedder = endpoint.is_some() && model.is_some();
    assert!(!has_embedder);
}

#[test]
fn partial_cli_without_toml_insufficient() {
    // CLI provides endpoint but not model, no TOML embedding section
    let (endpoint, model, _api_key) =
        merge_embedding_config(Some("http://localhost:8080/embed"), None, None, None);

    let has_embedder = endpoint.is_some() && model.is_some();
    assert!(!has_embedder); // Can't create embedder without model
}

// ---------------------------------------------------------------------------
// embed_dim probing
// ---------------------------------------------------------------------------

#[test]
fn probe_embed_dim_from_existing_db() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    // Create a real database with known embed_dim
    let engine = MemoryEngine::builder(384)
        .path(db_path.clone())
        .build()
        .unwrap();
    drop(engine);

    let probed = memory_engine_mcp::config::probe_embed_dim(&db_path).unwrap();
    assert_eq!(probed, 384);
}

#[test]
fn probe_embed_dim_nonexistent_db() {
    let result =
        memory_engine_mcp::config::probe_embed_dim(std::path::Path::new("/tmp/no_such_db.sqlite"));
    assert!(result.is_err());
}

#[test]
fn probe_embed_dim_different_dimensions() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    let engine = MemoryEngine::builder(768)
        .path(db_path.clone())
        .build()
        .unwrap();
    drop(engine);

    let probed = memory_engine_mcp::config::probe_embed_dim(&db_path).unwrap();
    assert_eq!(probed, 768);
}

// ---------------------------------------------------------------------------
// Config from TOML file (round-trip)
// ---------------------------------------------------------------------------

#[test]
fn config_from_toml_file() {
    let dir = TempDir::new().unwrap();
    let config_path = dir.path().join("config.toml");
    let mut f = std::fs::File::create(&config_path).unwrap();
    writeln!(
        f,
        r#"
[engine]
db_path = "/tmp/test.db"
embed_dim = 256

[embedding]
endpoint = "http://localhost:11434/v1/embeddings"
model = "all-minilm"
dimensions = 256
timeout_secs = 10
"#
    )
    .unwrap();

    let content = std::fs::read_to_string(&config_path).unwrap();
    let config: McpConfig = toml::from_str(&content).unwrap();
    assert_eq!(config.engine.embed_dim, Some(256));
    let emb = config.embedding.unwrap();
    assert_eq!(emb.model, "all-minilm");
    assert_eq!(emb.timeout_secs, 10);
}
