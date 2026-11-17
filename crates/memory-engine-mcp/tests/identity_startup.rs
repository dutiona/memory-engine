//! The server refuses to start, before serving anything, when its configured embedding
//! identity differs from the one the store recorded (ADR 0015 §3, #1017). Each refusal is
//! observed on the built binary: it exits non-zero before `/mcp` ever answered, names the
//! failed check on stderr, and leaves the store's record unchanged.

mod common;

use std::path::Path;
use std::sync::Arc;

use common::{ServerProcess, write_token_file};
use memory_engine::traits::EmbeddingProvider;
use memory_engine::{AddFactRequest, EmbeddingFingerprint, FactType, MemoryEngine, ServedArtifact};

const DIM: usize = 1024;
const SHA: &str = "06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439";
/// Never contacted: the identity check compares fingerprints, it does not embed.
const ENDPOINT: &str = "http://127.0.0.1:9/v1/embeddings";

fn pinned_served() -> ServedArtifact {
    ServedArtifact {
        artifact_sha256: SHA.into(),
        pooling: "last".into(),
        normalization: "l2".into(),
        max_input_tokens: 2048,
    }
}

/// The ADR 0015 §5 identity.
fn base() -> EmbeddingFingerprint {
    EmbeddingFingerprint::new("Qwen/Qwen3-Embedding-0.6B", "llama.cpp", DIM)
        .with_served(pinned_served())
}

struct Stub(EmbeddingFingerprint);

impl EmbeddingProvider for Stub {
    fn embed(&self, _text: &str) -> memory_engine::Result<Vec<f32>> {
        Ok(vec![0.5; self.0.dim])
    }
    fn fingerprint(&self) -> EmbeddingFingerprint {
        self.0.clone()
    }
}

/// Stamp `dir/memory.db` with `fp` through the real first-write path (one `add_fact`).
fn stamp_store(dir: &Path, fp: &EmbeddingFingerprint) {
    let db = dir.join("memory.db");
    let fp = fp.clone();
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(async move {
            let engine = MemoryEngine::builder(fp.dim).path(db).build().unwrap();
            engine
                .add_fact(
                    &AddFactRequest {
                        content: "a stamped fact".into(),
                        fact_type: FactType::Semantic,
                        source_event_id: None,
                        scope: None,
                        opts: None,
                    },
                    Arc::new(Stub(fp)) as Arc<dyn EmbeddingProvider>,
                    None,
                )
                .await
                .unwrap();
        });
}

/// The store's recorded identity columns, read without the engine (no migration).
fn recorded(dir: &Path) -> Vec<String> {
    let conn = rusqlite::Connection::open_with_flags(
        dir.join("memory.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    conn.query_row(
        "SELECT model, provider, dim, matryoshka_base_dim, element_type, artifact_sha256, \
         pooling, normalization, max_input_tokens FROM embedding_spaces WHERE status = 'active'",
        [],
        |r| {
            (0..9)
                .map(|i| {
                    r.get::<_, rusqlite::types::Value>(i)
                        .map(|v| format!("{v:?}"))
                })
                .collect()
        },
    )
    .unwrap()
}

/// The TOML for `dir` declaring `fp` (served keys only when `fp` has an artifact).
fn write_config(dir: &Path, fp: &EmbeddingFingerprint) {
    let mut embedding = format!(
        "[embedding]\nendpoint = '{ENDPOINT}'\nmodel = '{}'\nprovider = '{}'\ndimensions = {}\n",
        fp.model, fp.provider, fp.dim
    );
    if let Some(s) = &fp.served {
        use std::fmt::Write;
        write!(
            embedding,
            "artifact_sha256 = '{}'\npooling = '{}'\nnormalization = '{}'\nmax_input_tokens = {}\n",
            s.artifact_sha256, s.pooling, s.normalization, s.max_input_tokens
        )
        .unwrap();
    }
    common::write_config(dir, "");
    let file = dir.join("config.toml");
    let engine = std::fs::read_to_string(&file).unwrap().replace(
        &format!("embed_dim = {}", common::DIM),
        &format!("embed_dim = {DIM}"),
    );
    std::fs::write(&file, format!("{engine}{embedding}")).unwrap();
}

/// The log line `main` writes right after binding its listener. Every spawn here enables it,
/// so its absence proves the listener was never bound, not merely that a poll missed it.
const LISTENING: &str = "listening on";

fn with_log<'a>(envs: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    let mut all = vec![("RUST_LOG", "memory_engine_mcp=info")];
    all.extend_from_slice(envs);
    all
}

/// The binary with `--config` for `dir`, logging at info.
fn spawn(dir: &Path, envs: &[(&str, &str)]) -> ServerProcess {
    ServerProcess::spawn(dir, &[], &with_log(envs))
}

/// The binary without `--config` (an environment-only deployment), logging at info.
fn spawn_bare(dir: &Path, envs: &[(&str, &str)]) -> ServerProcess {
    ServerProcess::spawn_bare(
        dir,
        &write_token_file(dir, &common::MACHINES),
        &[],
        &with_log(envs),
    )
}

/// Assert the server exits non-zero having never bound its listener, naming `marker`.
fn assert_refused(server: &mut ServerProcess, case: &str, marker: &str) {
    let status = server.wait_ready().expect_err(&format!(
        "{case}: the server served; stderr:\n{}",
        server.stderr()
    ));
    assert!(!status.success(), "{case}: {status}");
    let stderr = server.stderr();
    assert!(
        stderr.contains(marker),
        "{case}: expected {marker:?}; stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains(LISTENING),
        "{case}: the listener was bound before the refusal; stderr:\n{stderr}"
    );
}

/// Assert the server comes up and stops cleanly.
fn assert_serves(server: &mut ServerProcess, case: &str) {
    assert!(
        server.wait_ready().is_ok(),
        "{case}: stderr:\n{}",
        server.stderr()
    );
    let stderr = server.stderr();
    assert!(
        stderr.contains(LISTENING),
        "{case}: the listening line is logged, so its absence on a refusal means something; {stderr}"
    );
    assert!(
        !stderr.contains("is not one of"),
        "{case}: llama.cpp is known; {stderr}"
    );
    assert!(
        server.terminate().success(),
        "{case}: stderr:\n{}",
        server.stderr()
    );
}

/// A store stamped with `stamped`, served under a configuration declaring `configured`.
fn refuses(
    case: &str,
    stamped: &EmbeddingFingerprint,
    configured: &EmbeddingFingerprint,
    marker: &str,
) {
    let dir = tempfile::tempdir().unwrap();
    stamp_store(dir.path(), stamped);
    let before = recorded(dir.path());
    write_config(dir.path(), configured);
    let mut server = spawn(dir.path(), &[]);
    assert_refused(&mut server, case, marker);
    assert_eq!(
        recorded(dir.path()),
        before,
        "{case}: the store's record is unchanged"
    );
}

#[test]
fn refuses_to_start_when_any_one_identity_field_differs() {
    let identity = "embedding identity check failed";
    let with = |f: fn(&mut EmbeddingFingerprint)| {
        let mut fp = base();
        f(&mut fp);
        fp
    };
    let served_with = |f: fn(&mut ServedArtifact)| {
        let mut s = pinned_served();
        f(&mut s);
        base().with_served(s)
    };
    let cases: [(&str, EmbeddingFingerprint, &str); 9] = [
        (
            "model",
            with(|fp| fp.model = "Qwen/Qwen3-Embedding-4B".into()),
            identity,
        ),
        (
            "provider",
            with(|fp| fp.provider = "ollama".into()),
            identity,
        ),
        (
            "matryoshka_base_dim",
            with(|fp| fp.matryoshka_base_dim = Some(2 * DIM)),
            identity,
        ),
        (
            "element_type",
            with(|fp| fp.element_type = "int8".into()),
            identity,
        ),
        (
            "artifact_sha256",
            served_with(|s| s.artifact_sha256 = "0".repeat(64)),
            identity,
        ),
        (
            "pooling",
            served_with(|s| s.pooling = "mean".into()),
            identity,
        ),
        (
            "normalization",
            served_with(|s| s.normalization = "none".into()),
            identity,
        ),
        (
            "max_input_tokens",
            served_with(|s| s.max_input_tokens = 8192),
            identity,
        ),
        // A store of another dimension is refused at open, still before serving.
        ("dim", with(|fp| fp.dim = 512), "failed to open engine"),
    ];
    for (case, stamped, marker) in cases {
        refuses(case, &stamped, &base(), marker);
    }
}

#[test]
fn refuses_a_store_recorded_without_a_served_artifact_under_a_served_config() {
    let unserved = EmbeddingFingerprint::new("Qwen/Qwen3-Embedding-0.6B", "llama.cpp", DIM);
    refuses(
        "unserved store",
        &unserved,
        &base(),
        "embedding identity check failed",
    );
}

#[test]
fn refuses_a_served_store_under_a_config_without_the_served_keys() {
    let unserved = EmbeddingFingerprint::new("Qwen/Qwen3-Embedding-0.6B", "llama.cpp", DIM);
    refuses(
        "served store",
        &base(),
        &unserved,
        "embedding identity check failed",
    );
}

#[test]
fn an_environment_override_reaches_the_identity_check() {
    let dir = tempfile::tempdir().unwrap();
    stamp_store(dir.path(), &base());
    let before = recorded(dir.path());
    write_config(dir.path(), &base());
    let other = "1".repeat(64);
    let mut server = spawn(
        dir.path(),
        &[("MEMORY_MCP_EMBED_ARTIFACT_SHA256", other.as_str())],
    );
    assert_refused(
        &mut server,
        "env override",
        "embedding identity check failed",
    );
    assert_eq!(recorded(dir.path()), before);
}

#[test]
fn a_partial_served_declaration_is_refused_before_the_store_opens() {
    let dir = tempfile::tempdir().unwrap();
    let unserved = EmbeddingFingerprint::new("Qwen/Qwen3-Embedding-0.6B", "llama.cpp", DIM);
    write_config(dir.path(), &unserved);
    let mut server = spawn(dir.path(), &[("MEMORY_MCP_EMBED_ARTIFACT_SHA256", SHA)]);
    assert_refused(
        &mut server,
        "partial declaration",
        "invalid embedding configuration",
    );
    let stderr = server.stderr();
    for field in ["pooling", "normalization", "max_input_tokens"] {
        assert!(stderr.contains(field), "{stderr}");
    }
    assert!(
        !dir.path().join("memory.db").exists(),
        "no database is created"
    );
}

#[test]
fn serves_when_the_store_matches_the_pinned_identity() {
    let dir = tempfile::tempdir().unwrap();
    stamp_store(dir.path(), &base());
    write_config(dir.path(), &base());
    let mut server = spawn(dir.path(), &[]);
    assert_serves(&mut server, "pinned identity");
}

#[test]
fn serves_an_unserved_store_under_an_unserved_config() {
    let dir = tempfile::tempdir().unwrap();
    let unserved = EmbeddingFingerprint::new("Qwen/Qwen3-Embedding-0.6B", "llama.cpp", DIM);
    stamp_store(dir.path(), &unserved);
    write_config(dir.path(), &unserved);
    let mut server = spawn(dir.path(), &[]);
    assert_serves(&mut server, "unserved store");
}

/// The environment of a deployment configured without a TOML file.
fn deployment_env(db: &str) -> Vec<(&'static str, String)> {
    vec![
        ("MEMORY_MCP_DB_PATH", db.to_owned()),
        ("MEMORY_MCP_EMBED_DIM", DIM.to_string()),
        ("MEMORY_MCP_EMBED_URL", ENDPOINT.to_owned()),
        (
            "MEMORY_MCP_EMBED_MODEL",
            "Qwen/Qwen3-Embedding-0.6B".to_owned(),
        ),
        ("MEMORY_MCP_EMBED_PROVIDER", "llama.cpp".to_owned()),
        ("MEMORY_MCP_EMBED_ARTIFACT_SHA256", SHA.to_owned()),
        ("MEMORY_MCP_EMBED_POOLING", "last".to_owned()),
        ("MEMORY_MCP_EMBED_NORMALIZATION", "l2".to_owned()),
        ("MEMORY_MCP_EMBED_MAX_INPUT_TOKENS", "2048".to_owned()),
    ]
}

fn spawn_from_environment(dir: &Path) -> ServerProcess {
    let db = dir.join("memory.db");
    let env = deployment_env(db.to_str().unwrap());
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    spawn_bare(dir, &env)
}

#[test]
fn an_environment_only_deployment_serves_its_stamped_store() {
    // The environment's served artifact reaches the identity check: a stamped store with
    // the pinned identity is accepted only if all four values arrived.
    let dir = tempfile::tempdir().unwrap();
    stamp_store(dir.path(), &base());
    let mut server = spawn_from_environment(dir.path());
    assert_serves(&mut server, "environment, stamped store");
}

#[test]
fn an_environment_only_deployment_starts_on_a_blank_store() {
    // MEMORY_MCP_EMBED_DIM lets a blank store open without a TOML file.
    let dir = tempfile::tempdir().unwrap();
    let mut server = spawn_from_environment(dir.path());
    assert_serves(&mut server, "environment, blank store");
}

/// A blank store and an environment-only configuration (with its dimension, so the exit
/// cannot come from `cannot determine embed_dim`) carrying `envs`.
fn refuses_from_environment(case: &str, envs: &[(&str, &str)]) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("memory.db");
    let mut env = vec![
        ("MEMORY_MCP_DB_PATH", db.to_str().unwrap()),
        ("MEMORY_MCP_EMBED_DIM", "1024"),
    ];
    env.extend_from_slice(envs);
    let mut server = spawn_bare(dir.path(), &env);
    assert_refused(&mut server, case, "invalid embedding configuration");
    assert!(server.stderr().contains("url"), "{}", server.stderr());
    assert!(!db.exists(), "{case}: no database is created");
}

#[test]
fn refuses_a_half_configured_embedder() {
    refuses_from_environment(
        "model without url",
        &[("MEMORY_MCP_EMBED_MODEL", "Qwen/Qwen3-Embedding-0.6B")],
    );
}

#[test]
fn refuses_embedder_fields_without_an_embedder() {
    refuses_from_environment(
        "served fields only",
        &[
            ("MEMORY_MCP_EMBED_ARTIFACT_SHA256", SHA),
            ("MEMORY_MCP_EMBED_POOLING", "last"),
            ("MEMORY_MCP_EMBED_NORMALIZATION", "l2"),
            ("MEMORY_MCP_EMBED_MAX_INPUT_TOKENS", "2048"),
        ],
    );
}
