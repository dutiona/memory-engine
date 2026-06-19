//! Integration tests for the `consolidate` subcommand (#554), both backends.
//!
//! The LLM backend test drives a real CLI subprocess against a wiremock Ollama
//! `/api/generate` endpoint plus a wiremock embedding endpoint — the end-to-end seam
//! the efficiency×quality benchmark uses. The subprocess makes blocking HTTP calls, so
//! it runs inside `spawn_blocking` while the test's runtime serves wiremock.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIM: usize = 4;

struct FakeEmbed;
impl memory_engine::EmbeddingProvider for FakeEmbed {
    fn embed(&self, _text: &str) -> memory_engine::Result<Vec<f32>> {
        Ok(vec![0.1, 0.2, 0.3, 0.4])
    }
}

/// Create a DB with three undreamt facts (ids 1, 2, 3) at embedding dim 4.
fn create_db() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let engine = memory_engine::MemoryEngine::builder(DIM)
        .path(db_path.clone())
        .build()
        .unwrap();
    for content in ["the sky is blue", "the sky is azure", "rust is fast"] {
        engine
            .add_fact(
                &memory_engine::AddFactRequest {
                    content: content.into(),
                    fact_type: memory_engine::FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                &FakeEmbed,
                None,
            )
            .unwrap();
    }
    drop(engine);
    (dir, db_path)
}

/// Run `consolidate` and return the parsed JSON result.
fn run_consolidate(db: &Path, extra: &[&str]) -> Value {
    let mut args = vec![
        "--db",
        db.to_str().unwrap(),
        "--format",
        "json",
        "consolidate",
    ];
    args.extend_from_slice(extra);
    let out = Command::cargo_bin("memory-engine-cli")
        .unwrap()
        .args(&args)
        .output()
        .unwrap();
    assert!(out.status.success(), "consolidate failed: {out:?}");
    serde_json::from_slice(&out.stdout).expect("valid JSON on stdout")
}

#[test]
fn dream_cycle_backend_runs_on_the_second_guarded_invocation() {
    let (_dir, db) = create_db();

    // First invocation: the #209 guard sees fresh caller writes and defers.
    let first = run_consolidate(&db, &["--backend", "dream-cycle"]);
    assert_eq!(first["backend"], "dream-cycle");
    assert_eq!(first["outcome"], "skipped");

    // Second (quiet) invocation: the cycle runs and applies.
    let second = run_consolidate(&db, &["--backend", "dream-cycle"]);
    assert_eq!(second["outcome"], "ran");
    assert!(second["applied"].is_object(), "applied result present");
    assert!(
        second["llm"].is_null(),
        "dream-cycle backend reports no LLM stats"
    );
}

#[test]
fn llm_backend_requires_its_urls() {
    let (_dir, db) = create_db();
    let out = Command::cargo_bin("memory-engine-cli")
        .unwrap()
        .args([
            "--db",
            db.to_str().unwrap(),
            "consolidate",
            "--backend",
            "llm",
        ])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "llm backend without --llm-url must fail"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--llm-url"),
        "stderr names the missing flag: {stderr}"
    );
}

#[tokio::test]
async fn llm_backend_synthesizes_via_wiremock_end_to_end() {
    let (_dir, db) = create_db();
    let server = MockServer::start().await;

    // The proposer asks the model to merge facts 1 and 2.
    let generate_body = serde_json::json!({
        "model": "gemma4:26b",
        "response": r#"{"merges":[{"source_ids":[1,2],"summary":"the sky is blue/azure"}]}"#,
        "done": true,
        "eval_count": 40,
        "prompt_eval_count": 15,
    });
    Mock::given(method("POST"))
        .and(path("/api/generate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&generate_body))
        .mount(&server)
        .await;
    // The LLM backend embeds its own summary text (dim 4).
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "embedding": [0.1, 0.2, 0.3, 0.4]
        })))
        .mount(&server)
        .await;

    let llm_url = format!("{}/api/generate", server.uri());
    let embed_url = format!("{}/v1/embeddings", server.uri());
    let db_str = db.to_str().unwrap().to_owned();

    let second = tokio::task::spawn_blocking(move || {
        let extra = [
            "--backend",
            "llm",
            "--llm-url",
            &llm_url,
            "--llm-model",
            "gemma4:26b",
            "--embed-url",
            &embed_url,
            "--embed-model",
            "nomic-embed-text",
        ];
        // First invocation defers (the #209 guard) and never calls the LLM.
        let _first = run_consolidate(Path::new(&db_str), &extra);
        // Second invocation runs the LLM backend end-to-end.
        run_consolidate(Path::new(&db_str), &extra)
    })
    .await
    .expect("join");

    assert_eq!(second["backend"], "llm");
    assert_eq!(second["outcome"], "ran");
    assert_eq!(
        second["applied"]["synthesized"], 1,
        "one merge group → one Synthesize: {second}"
    );
    assert_eq!(second["llm"]["llm_calls"], 1);
    assert_eq!(second["llm"]["eval_count"], 40);
    assert_eq!(second["llm"]["prompt_eval_count"], 15);
}
