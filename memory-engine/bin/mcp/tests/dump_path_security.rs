//! Security regression tests for the `memory_dump_state` path guard.
//!
//! Issues #296 / #354 / #414 (one vulnerability, three lenses) + test #320:
//! the guard must keep a client-supplied dump destination inside the system
//! temp directory and must not be defeatable by a symlink at the leaf
//! (CWE-59 symlink-follow), a symlinked temp root (CWE-22), or a swap between
//! check and use (CWE-367 TOCTOU).

use std::sync::Arc;

use memory_engine::MemoryEngine;
use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::{AddFactRequest, FactType};
use memory_engine_mcp::tools;
use serde_json::{Map, Value, json};

struct FakeEmbed;
impl EmbeddingProvider for FakeEmbed {
    fn embed(&self, _text: &str) -> memory_engine::Result<Vec<f32>> {
        Ok(vec![0.1, 0.2, 0.3])
    }
    fn fingerprint(&self) -> memory_engine::EmbeddingFingerprint {
        memory_engine::EmbeddingFingerprint::new("mock", "test", 3)
    }
}

fn test_engine() -> (MemoryEngine, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let engine = MemoryEngine::builder(3)
        .path(dir.path().join("test.db"))
        .build()
        .unwrap();
    (engine, dir)
}

async fn add_test_fact(engine: &MemoryEngine, content: &str) -> i64 {
    let emb: Arc<dyn EmbeddingProvider> = Arc::new(FakeEmbed);
    engine
        .add_fact(
            &AddFactRequest {
                content: content.into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            emb,
            None,
        )
        .await
        .unwrap()
}

fn args(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

/// A client-supplied path OUTSIDE the temp directory is rejected, and the error
/// message names the temp-directory constraint (#296 / #354 CWE-22).
#[tokio::test]
async fn dump_state_rejects_path_outside_temp() {
    let (engine, _dir) = test_engine();
    add_test_fact(&engine, "fact").await;

    let result = tools::dispatch(
        "memory_dump_state",
        args(&[
            ("format", json!("json")),
            ("path", json!("/etc/clobber.json")),
        ]),
        &engine,
        None,
        None,
        3,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await;

    let err = result.expect_err("dump outside temp must be rejected");
    assert!(
        err.message.contains("temp"),
        "error must name the temp-directory constraint, got: {}",
        err.message
    );
    assert!(
        !std::path::Path::new("/etc/clobber.json").exists(),
        "the out-of-temp target must NOT have been written"
    );
}

/// A symlink LEAF whose target escapes temp must be rejected, and the escape
/// target must not be written (#414 CWE-59 symlink-follow + #367 TOCTOU).
///
/// The leaf component is itself a symlink that lives inside temp but points
/// outside it; the parent canonicalizes cleanly, so a parent-only guard would
/// wave it through and the downstream `File::create`/`VACUUM INTO` would follow
/// the link and clobber the external target.
#[cfg(unix)]
#[tokio::test]
async fn dump_state_rejects_symlink_leaf_escaping_temp() {
    let (engine, _dir) = test_engine();
    add_test_fact(&engine, "fact").await;

    // The escape target lives OUTSIDE temp.
    let escape_dir = tempfile::Builder::new()
        .prefix("dump-escape-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let escape_target = escape_dir.path().join("secret.json");

    // A scratch dir INSIDE temp holding the malicious symlink leaf.
    let in_temp = tempfile::tempdir().unwrap();
    let link = in_temp.path().join("evil.json");
    std::os::unix::fs::symlink(&escape_target, &link).unwrap();

    let result = tools::dispatch(
        "memory_dump_state",
        args(&[
            ("format", json!("json")),
            ("path", json!(link.display().to_string())),
        ]),
        &engine,
        None,
        None,
        3,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await;

    assert!(
        result.is_err(),
        "a symlink leaf pointing outside temp must be rejected"
    );
    assert!(
        !escape_target.exists(),
        "the symlink target outside temp must NOT have been written"
    );
}

/// The happy path — a fresh (non-symlink) destination inside temp — still works
/// after hardening. Guards against over-restriction (#354 CWE-22 false reject).
#[tokio::test]
async fn dump_state_accepts_fresh_path_inside_temp() {
    let (engine, dir) = test_engine();
    add_test_fact(&engine, "fact").await;

    let custom_path = dir.path().join("ok-dump.json");

    let result = tools::dispatch(
        "memory_dump_state",
        args(&[
            ("format", json!("json")),
            ("path", json!(custom_path.display().to_string())),
        ]),
        &engine,
        None,
        None,
        3,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await
    .expect("a fresh path inside temp must be accepted");

    let content = &result.content[0];
    let v: Value = match &content.raw {
        rmcp::model::RawContent::Text(t) => serde_json::from_str(&t.text).unwrap(),
        _ => panic!("expected text content"),
    };
    let reported = v["path"].as_str().unwrap();
    assert!(std::path::Path::new(reported).exists());
}
