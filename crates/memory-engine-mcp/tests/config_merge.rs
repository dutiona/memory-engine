//! Tests for CLI-over-TOML configuration merging.
//!
//! Validates field-level merge semantics:
//! - TOML provides defaults
//! - CLI flags override individual TOML fields
//! - Missing TOML sections handled gracefully
//! - `deny_unknown_fields` rejects extra config

use memory_engine_mcp::config::{
    EmbeddingOverrides, EmbeddingSection, McpConfig, ResolvedEmbedding, resolve_embedding,
};
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
fn embedding_provider_defaults_to_ollama() {
    // A legacy config without `provider` still parses; defaults to "ollama" so the
    // fingerprint reflects the historical hardcoded backend. (#618)
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
    let emb = config.embedding.unwrap();
    assert_eq!(emb.provider, "ollama");
    assert_eq!(emb.query_instruction, None);
    assert_eq!(emb.mrl_dim, None);
}

/// #1064: the TOML section and the flag-only resolver take one shared default.
#[test]
fn the_section_and_the_resolver_default_to_the_shared_provider() {
    use memory_engine_embed::HttpEmbeddingProvider;
    assert_eq!(
        section("").provider,
        HttpEmbeddingProvider::DEFAULT_PROVIDER
    );
    let overrides = EmbeddingOverrides {
        url: Some("http://cli-host:8080/embed".into()),
        model: Some("cli-model".into()),
        ..EmbeddingOverrides::default()
    };
    assert_eq!(
        resolve(&overrides, None).provider,
        HttpEmbeddingProvider::DEFAULT_PROVIDER
    );
}

#[test]
fn embedding_asymmetric_tei_qwen_config_parses() {
    // Full asymmetric TEI/Qwen config: explicit provider, query_instruction, mrl_dim. (#618)
    let toml_str = r#"
[engine]
db_path = "/tmp/test.db"
embed_dim = 256

[embedding]
endpoint = "http://localhost:8080/v1/embeddings"
model = "Qwen/Qwen3-Embedding-0.6B"
provider = "tei"
dimensions = 1024
query_instruction = "Instruct: retrieve\nQuery: "
mrl_dim = 256
"#;
    let config: McpConfig = toml::from_str(toml_str).unwrap();
    let emb = config.embedding.unwrap();
    assert_eq!(emb.provider, "tei");
    assert_eq!(emb.dimensions, 1024); // native
    assert_eq!(emb.mrl_dim, Some(256)); // stored (== engine embed_dim)
    assert_eq!(
        emb.query_instruction.as_deref(),
        Some("Instruct: retrieve\nQuery: ")
    );
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
// CLI-over-TOML field merge (`config::resolve_embedding`, the code main.rs runs)
// ---------------------------------------------------------------------------

const SHA: &str = "06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439";

fn section(extra: &str) -> EmbeddingSection {
    let toml_str = format!(
        "endpoint = \"http://toml-host:11434/v1/embeddings\"\nmodel = \"toml-model\"\ndimensions = 8\n{extra}"
    );
    toml::from_str(&toml_str).unwrap()
}

const SERVED_TOML: &str = "artifact_sha256 = \"06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439\"\npooling = \"last\"\nnormalization = \"l2\"\nmax_input_tokens = 2048\n";

fn resolve(overrides: &EmbeddingOverrides, base: Option<&EmbeddingSection>) -> ResolvedEmbedding {
    resolve_embedding(overrides, base, 8).unwrap().unwrap()
}

#[test]
fn served_keys_parse_from_the_embedding_section() {
    let r = resolve(&EmbeddingOverrides::default(), Some(&section(SERVED_TOML)));
    let served = r.served.expect("served artifact declared");
    assert_eq!(served.artifact_sha256, SHA);
    assert_eq!(served.pooling, "last");
    assert_eq!(served.normalization, "l2");
    assert_eq!(served.max_input_tokens, 2048);
}

#[test]
fn every_override_wins_over_toml_field_by_field() {
    let base = section(&format!(
        "api_key = \"toml-key\"\nprovider = \"tei\"\nquery_instruction = \"T: \"\n{SERVED_TOML}"
    ));
    let overrides = EmbeddingOverrides {
        url: Some("http://cli-host:8080/embed".into()),
        model: Some("cli-model".into()),
        api_key: Some("cli-key".into()),
        api_key_file: None,
        provider: Some("llama.cpp".into()),
        query_instruction: Some(String::new()),
        artifact_sha256: Some("0".repeat(64)),
        pooling: Some("mean".into()),
        normalization: Some("none".into()),
        max_input_tokens: Some(512),
    };
    let r = resolve(&overrides, Some(&base));
    assert_eq!(r.endpoint, "http://cli-host:8080/embed");
    assert_eq!(r.model, "cli-model");
    assert_eq!(r.api_key.as_deref(), Some("cli-key"));
    assert_eq!(r.provider, "llama.cpp");
    assert_eq!(
        r.query_instruction.as_deref(),
        Some(""),
        "the empty opt-out survives"
    );
    let served = r.served.unwrap();
    assert_eq!(served.artifact_sha256, "0".repeat(64));
    assert_eq!(
        (served.pooling.as_str(), served.normalization.as_str()),
        ("mean", "none")
    );
    assert_eq!(served.max_input_tokens, 512);

    // Unset overrides keep the TOML values.
    let r = resolve(&EmbeddingOverrides::default(), Some(&base));
    assert_eq!(r.endpoint, "http://toml-host:11434/v1/embeddings");
    assert_eq!(r.api_key.as_deref(), Some("toml-key"));
    assert_eq!(r.query_instruction.as_deref(), Some("T: "));
}

#[test]
fn a_partial_served_declaration_names_the_missing_fields() {
    let overrides = EmbeddingOverrides {
        artifact_sha256: Some(SHA.into()),
        ..EmbeddingOverrides::default()
    };
    let err = resolve_embedding(&overrides, Some(&section("")), 8)
        .err()
        .expect("partial declaration")
        .to_string();
    for field in ["pooling", "normalization", "max_input_tokens"] {
        assert!(err.contains(field), "{err}");
    }
}

#[test]
fn neither_url_nor_model_resolves_to_no_embedder() {
    assert!(
        resolve_embedding(&EmbeddingOverrides::default(), None, 8)
            .unwrap()
            .is_none()
    );
}

#[test]
fn without_toml_the_native_dim_is_the_engine_dim() {
    let overrides = EmbeddingOverrides {
        url: Some("http://cli-host:8080/embed".into()),
        model: Some("m".into()),
        ..EmbeddingOverrides::default()
    };
    let r = resolve_embedding(&overrides, None, 1024).unwrap().unwrap();
    assert_eq!(r.native_dim, 1024);
    assert_eq!(r.provider, "ollama", "the historical default provider");
    assert!(
        r.query_instruction.is_none(),
        "no override keeps the pinned default"
    );
}

#[test]
fn the_dimension_checks_run_in_the_resolver() {
    // Without MRL, `dimensions` must equal the engine dim.
    let err = resolve_embedding(&EmbeddingOverrides::default(), Some(&section("")), 16)
        .err()
        .expect("dimensions != engine dim");
    assert!(
        err.to_string().contains("must equal the engine embed_dim"),
        "{err}"
    );
    // With MRL, the target must equal the engine dim.
    let mrl = section("mrl_dim = 4\n");
    assert!(
        resolve_embedding(&EmbeddingOverrides::default(), Some(&mrl), 4)
            .unwrap()
            .is_some()
    );
    assert!(resolve_embedding(&EmbeddingOverrides::default(), Some(&mrl), 8).is_err());
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

#[test]
fn an_embedder_field_without_both_url_and_model_is_refused() {
    let cases = [
        EmbeddingOverrides {
            url: Some("http://cli-host:8080/embed".into()),
            ..EmbeddingOverrides::default()
        },
        EmbeddingOverrides {
            model: Some("m".into()),
            ..EmbeddingOverrides::default()
        },
        EmbeddingOverrides {
            provider: Some("llama.cpp".into()),
            ..EmbeddingOverrides::default()
        },
        EmbeddingOverrides {
            artifact_sha256: Some(SHA.into()),
            pooling: Some("last".into()),
            normalization: Some("l2".into()),
            max_input_tokens: Some(2048),
            ..EmbeddingOverrides::default()
        },
    ];
    for overrides in cases {
        let err = resolve_embedding(&overrides, None, 8)
            .err()
            .expect("a half-configured embedder is refused")
            .to_string();
        assert!(err.contains("url") && err.contains("model"), "{err}");
    }
}

#[test]
fn debug_output_never_prints_an_api_key() {
    let config: McpConfig = toml::from_str(
        r#"
[engine]
db_path = "/tmp/test.db"

[embedding]
endpoint = "http://localhost:11434/v1/embeddings"
model = "m"
dimensions = 8
api_key = "embed-key-sentinel"
"#,
    )
    .unwrap();
    let debug = format!("{config:?}");
    assert!(debug.contains("api_key"), "{debug}");
    assert!(!debug.contains("embed-key-sentinel"), "{debug}");
}

// --- #1101: the embedder key from a file ---

/// A key built at run time, written to a fresh file; the caller keeps the directory.
fn key_file() -> (TempDir, std::path::PathBuf, String) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("embed-key");
    let key = format!("file-key-{}", std::process::id());
    std::fs::write(&path, format!("{key}\n")).unwrap();
    (dir, path, key)
}

fn file_overrides(path: &std::path::Path) -> EmbeddingOverrides {
    EmbeddingOverrides {
        url: Some("http://cli-host:8080/embed".into()),
        model: Some("cli-model".into()),
        api_key_file: Some(path.to_path_buf()),
        ..EmbeddingOverrides::default()
    }
}

#[test]
fn the_key_file_supplies_the_api_key() {
    let (_dir, path, key) = key_file();
    let r = resolve(&file_overrides(&path), None);
    assert_eq!(r.api_key.as_deref(), Some(key.as_str()));
}

#[test]
fn a_key_file_beside_another_key_is_refused_naming_both_settings() {
    let (_dir, path, key) = key_file();
    let flag = format!("flag-key-{}", std::process::id());
    let beside_flag = EmbeddingOverrides {
        api_key: Some(flag.clone()),
        ..file_overrides(&path)
    };
    let toml_key = format!("toml-key-{}", std::process::id());
    let beside_toml = section(&format!("api_key = \"{toml_key}\"\n"));
    for (overrides, base, other) in [
        (
            &beside_flag,
            None,
            "--embed-api-key / MEMORY_MCP_EMBED_API_KEY",
        ),
        (
            &file_overrides(&path),
            Some(&beside_toml),
            "api_key in [embedding]",
        ),
    ] {
        let err = resolve_embedding(overrides, base, 8)
            .err()
            .expect("two key sources are refused")
            .to_string();
        assert!(
            err.contains("--embed-api-key-file / MEMORY_MCP_EMBED_API_KEY_FILE"),
            "{err}"
        );
        assert!(err.contains(other), "{err}");
        for secret in [&key, &flag, &toml_key] {
            assert!(!err.contains(secret.as_str()), "{err}");
        }
    }
}

#[test]
fn a_lone_key_file_is_refused_by_the_url_and_model_rule() {
    let (_dir, path, _) = key_file();
    let lone = EmbeddingOverrides {
        api_key_file: Some(path),
        ..EmbeddingOverrides::default()
    };
    let err = resolve_embedding(&lone, None, 8).err().unwrap().to_string();
    assert!(err.contains("needs both a url"), "{err}");
}
