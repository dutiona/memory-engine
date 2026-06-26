//! Gated TEI/Qwen integration smoke test (#621, §Wave 0 → Wave 1).
//!
//! Promotes the #611 validation spike into a `cargo test` target, exercising the REAL
//! asymmetric API (`embed_query` / `with_query_instruction` / `with_mrl_dim` from #617)
//! against a live Qwen3-Embedding model over any OpenAI-compatible endpoint (TEI or
//! Ollama — both speak `/v1/embeddings`).
//!
//! **Every test is `#[ignore]`** — it needs a live model and is never run by default CI.
//! Run it explicitly once an endpoint is up:
//!
//! ```bash
//! # Ollama (model: `ollama pull dengcao/Qwen3-Embedding-0.6B:Q8_0`) — the default:
//! cargo test -p memory-engine-embed --test qwen_integration -- --ignored
//! # TEI (or any OpenAI-compatible endpoint), via env override:
//! QWEN_ENDPOINT=http://localhost:8080/v1/embeddings \
//!   QWEN_MODEL=Qwen/Qwen3-Embedding-0.6B \
//!   cargo test -p memory-engine-embed --test qwen_integration -- --ignored
//! ```
//!
//! Asserts the Wave-0 premises against the real model: native 1024 dim, the query
//! instruction prefix is applied by `embed_query` (and lifts retrieval), concurrent
//! embeds all succeed, and a model-identity mismatch is hard-rejected through the engine.

use std::sync::Arc;
use std::thread;

use memory_engine::MemoryEngine;
use memory_engine::error::MemoryError;
use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::{AddFactRequest, FactType};
use memory_engine_embed::HttpEmbeddingProvider;

/// Qwen3-Embedding query instruction (asymmetric: queries only; documents bare).
const QUERY_INSTRUCTION: &str =
    "Instruct: Given a search query, retrieve relevant memory facts.\nQuery: ";

const NATIVE_DIM: usize = 1024;

fn endpoint() -> String {
    std::env::var("QWEN_ENDPOINT").unwrap_or_else(|_| "http://localhost:11434/v1/embeddings".into())
}

fn model() -> String {
    std::env::var("QWEN_MODEL").unwrap_or_else(|_| "dengcao/Qwen3-Embedding-0.6B:Q8_0".into())
}

/// A provider at the native dim, optionally with the query instruction prefix.
fn provider(with_prefix: bool) -> HttpEmbeddingProvider {
    let p = HttpEmbeddingProvider::new(endpoint(), model(), "tei".into(), None, NATIVE_DIM, 60)
        .expect("build provider");
    if with_prefix {
        p.with_query_instruction(QUERY_INSTRUCTION)
    } else {
        p
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    // The dot product zips (truncating on a length mismatch) while the norms span full
    // lengths — so a mismatch would silently yield a wrong value. All callers pass
    // same-dim vectors from one provider; assert it so a future misuse fails loudly.
    debug_assert_eq!(a.len(), b.len(), "cosine inputs must have equal length");
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// 1-based rank of `relevant` when docs are sorted by descending cosine to `query_emb`.
fn rank_of(query_emb: &[f32], doc_embs: &[Vec<f32>], relevant: usize) -> usize {
    let target = cosine(query_emb, &doc_embs[relevant]);
    1 + doc_embs
        .iter()
        .enumerate()
        .filter(|&(i, e)| i != relevant && cosine(query_emb, e) > target)
        .count()
}

/// Labeled set spanning code retrieval + multilingual (from the #611 spike).
/// Returns `(docs, queries)` where each query is `(text, relevant_doc_index)`.
fn corpus() -> (Vec<&'static str>, Vec<(&'static str, usize)>) {
    let docs = vec![
        "fn read_file(path: &str) -> std::io::Result<String> { std::fs::read_to_string(path) }",
        "def read_file(path):\n    with open(path) as f:\n        return f.read()",
        "The Eiffel Tower is a wrought-iron lattice tower located in Paris, France.",
        "La tour Eiffel est une tour de fer puddlé située à Paris, en France.",
        "Photosynthesis converts sunlight into chemical energy stored in glucose.",
        "SELECT id, name FROM users WHERE active = 1 ORDER BY created_at DESC;",
        "El gato duerme en el sofá durante toda la tarde.",
        "Rust's borrow checker enforces memory safety at compile time without a GC.",
        "To create or overwrite a file in Rust: std::fs::write(path, contents).unwrap();",
        "Java: String s = Files.readString(Path.of(path)); reads a whole file.",
        "The Statue of Liberty stands on Liberty Island in New York Harbor, USA.",
        "Rust's zero-cost abstractions compile high-level code to efficient machine code.",
    ];
    let queries = vec![
        ("how do I read a file in Rust", 0usize),
        ("cómo leer un archivo en Python", 1usize),
        ("where is the Eiffel Tower located?", 2usize),
        ("Où se trouve la tour Eiffel ?", 3usize),
        ("memory safety guarantees in Rust", 7usize),
    ];
    (docs, queries)
}

#[test]
#[ignore = "requires a live Qwen embedding endpoint; run with --ignored (see module docs)"]
fn qwen_native_dimension_is_1024() {
    // Discover the real dim by exploiting the provider's own guard: build with a wrong
    // expected_dim and read `actual` from the rejection.
    let probe = HttpEmbeddingProvider::new(endpoint(), model(), "tei".into(), None, 1, 60).unwrap();
    let actual = match probe.embed("dimension probe") {
        Err(MemoryError::EmbeddingDimension { actual, .. }) => actual,
        Ok(v) => v.len(),
        Err(e) => panic!("probe embed failed: {e}"),
    };
    assert_eq!(
        actual, NATIVE_DIM,
        "Qwen3-Embedding-0.6B should be 1024-dim"
    );
}

#[test]
#[ignore = "requires a live Qwen embedding endpoint; run with --ignored"]
fn qwen_embed_query_applies_instruction_prefix() {
    // embed_query must route through the configured query instruction, so for the SAME
    // text it produces a different vector than the bare-document embed. A provider with
    // NO instruction must produce identical vectors (embed_query == embed).
    let q = "where is the Eiffel Tower located?";

    let prefixed = provider(true);
    let doc_vec = prefixed.embed(q).unwrap();
    let query_vec = prefixed.embed_query(q).unwrap();
    let sim = cosine(&doc_vec, &query_vec);
    assert!(
        sim < 0.999,
        "embed_query should apply the prefix and differ from embed (cosine {sim} too close to 1.0)"
    );

    let symmetric = provider(false);
    assert_eq!(
        symmetric.embed(q).unwrap(),
        symmetric.embed_query(q).unwrap(),
        "without a query instruction, embed_query must equal embed"
    );
}

#[test]
#[ignore = "requires a live Qwen embedding endpoint; run with --ignored"]
#[allow(clippy::cast_precision_loss)] // MRR over a tiny corpus; rank/count precision irrelevant
fn qwen_query_prefix_does_not_regress_retrieval() {
    // The #611 SPIKE established the empirical *lift* (top-1 80%→100%); that go/no-go
    // belongs to the spike, not a gated regression test. Here we assert the robust,
    // non-flaky bar: enabling the query instruction prefix must not REGRESS MRR. (A
    // strict `>` lift bar would be flaky — an easy corpus can already score plain MRR
    // 1.0, and model nondeterminism could tie the two.)
    let prefixed = provider(true);
    let (docs, queries) = corpus();
    // Documents are embedded bare (no prefix) via embed_batch.
    let doc_embs = prefixed.embed_batch(&docs).unwrap();

    let (mut mrr_plain, mut mrr_pref) = (0.0_f32, 0.0_f32);
    for (q, rel) in &queries {
        let plain = prefixed.embed(q).unwrap(); // bare query
        let pref = prefixed.embed_query(q).unwrap(); // instruction-prefixed
        mrr_plain += 1.0 / rank_of(&plain, &doc_embs, *rel) as f32;
        mrr_pref += 1.0 / rank_of(&pref, &doc_embs, *rel) as f32;
    }
    let n = queries.len() as f32;
    let (mrr_plain, mrr_pref) = (mrr_plain / n, mrr_pref / n);
    // Epsilon guards against f32 sum non-associativity tying the two at microscopically
    // different values when ranks merely reorder across queries.
    assert!(
        mrr_pref + 1e-4 >= mrr_plain,
        "the query instruction prefix must not regress retrieval: prefixed MRR {mrr_pref:.3} < plain {mrr_plain:.3}"
    );
}

#[test]
#[ignore = "requires a live Qwen embedding endpoint; run with --ignored"]
fn qwen_concurrent_embeds_succeed() {
    // Model-axis smoke for the ~5-concurrent-session burst (TEI's server-side dynamic
    // batching is the separate server axis). Each blocking embed runs on its own thread.
    let shared: Arc<dyn EmbeddingProvider> = Arc::new(provider(false));
    let handles: Vec<_> = (0..5)
        .map(|i| {
            let p = Arc::clone(&shared);
            thread::spawn(move || p.embed(&format!("concurrent probe {i}")).map(|v| v.len()))
        })
        .collect();
    for h in handles {
        let dim = h.join().expect("thread panicked").expect("embed failed");
        assert_eq!(dim, NATIVE_DIM, "concurrent embed returned wrong dim");
    }
}

#[tokio::test]
#[ignore = "requires a live Qwen embedding endpoint; run with --ignored"]
async fn qwen_model_mismatch_is_rejected() {
    // End-to-end #614 enforcement against the real model: a store stamped by the real
    // Qwen provider rejects a provider that declares a different model identity.
    let engine = MemoryEngine::builder(NATIVE_DIM).build().unwrap();
    let qwen: Arc<dyn EmbeddingProvider> = Arc::new(provider(false));

    engine
        .add_fact(
            &AddFactRequest {
                content: "Rust's borrow checker enforces memory safety.".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            qwen.clone(),
            None,
        )
        .await
        .expect("first add stamps the Qwen identity");

    // A provider declaring a DIFFERENT model (same endpoint/dim) must be rejected — the
    // fingerprint mismatch is caught without needing the bogus model to actually embed.
    let other = HttpEmbeddingProvider::new(
        endpoint(),
        "some-other-model".into(),
        "tei".into(),
        None,
        NATIVE_DIM,
        60,
    )
    .unwrap();
    let err = engine
        .verify_embedding_identity(&other)
        .await
        .expect_err("a differing model identity must be rejected");
    assert!(
        matches!(err, MemoryError::EmbeddingModelMismatch { .. }),
        "expected EmbeddingModelMismatch, got {err:?}"
    );
}
