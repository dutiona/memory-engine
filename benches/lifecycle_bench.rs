//! Criterion benchmarks for lifecycle operations (consolidate, forget, ingest).
//!
//! These benchmarks measure the core write-path primitives at various corpus
//! sizes to track performance regressions and validate scaling behavior.
//!
//! # Usage
//!
//! ```bash
//! cargo bench -- consolidation              # consolidation only
//! cargo bench -- forgetting                 # forget only
//! cargo bench -- add_fact_single            # single ingest
//! cargo bench -- add_facts_batch            # batch ingest
//! cargo bench -- --save-baseline v0.1.0     # save named baseline
//! cargo bench -- --baseline v0.1.0          # compare to baseline
//! ```
//!
//! # Methodology
//!
//! - Data generation is deterministic (blake3 hash → embedding).
//! - `ConstEmbedder` mirrors `search_bench.rs` for consistency.
//! - `ConcatSummaryGenerator` joins fact content and produces a constant
//!   embedding — no LLM cost, deterministic output.
//! - `consolidate()` and `forget()` are idempotent — `iter_with_setup`
//!   creates a fresh engine per iteration to measure actual work, not no-ops.

use chrono::{Duration, Utc};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use memory_engine::engine::{EngineConfig, MemoryEngine};
use memory_engine::traits::{
    ConsolidationConfig, EmbeddingProvider, ForgetPolicy, SummaryGenerator,
};
use memory_engine::types::{AddFactOptions, AddFactRequest, Fact, FactType};

const DIM: usize = 128;

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

/// Deterministic embedder: blake3 hash of the text projected to `dim` floats.
/// Same implementation as `search_bench.rs` for cross-benchmark consistency.
struct ConstEmbedder {
    dim: usize,
}

impl EmbeddingProvider for ConstEmbedder {
    fn embed(&self, text: &str) -> memory_engine::error::Result<Vec<f32>> {
        let hash = blake3::hash(text.as_bytes());
        let bytes = hash.as_bytes();
        let embedding: Vec<f32> = (0..self.dim)
            .map(|i| {
                let byte = bytes[i % 32];
                (f32::from(byte) / 255.0).mul_add(2.0, -1.0)
            })
            .collect();
        Ok(embedding)
    }
}

/// Trivial summary generator: concatenates fact content.
/// Produces deterministic text without any LLM dependency. Summary embedding is
/// performed by the [`ConstEmbedder`] injected into consolidation (issue #116).
struct ConcatSummaryGenerator;

impl SummaryGenerator for ConcatSummaryGenerator {
    fn summarize(&self, facts: &[Fact]) -> memory_engine::error::Result<String> {
        let summary: String = facts
            .iter()
            .map(|f| f.content.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        Ok(summary)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const TOPICS: [&str; 10] = [
    "Rust memory safety and ownership",
    "Python machine learning with PyTorch",
    "JavaScript web development frameworks",
    "Database query optimization techniques",
    "Distributed systems consensus protocols",
    "Neural network architecture design",
    "Kubernetes container orchestration",
    "Graph algorithms and data structures",
    "Cryptographic hash functions and security",
    "Real-time operating systems embedded",
];

/// Create a file-backed engine pre-populated with `n` facts.
/// Returns `(engine, _dir)` — the `_dir` handle keeps the tempdir alive.
fn setup_engine(n: usize) -> (MemoryEngine, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("bench.db");
    let config = EngineConfig::new(db_path, DIM);
    let engine = MemoryEngine::open(&config).expect("open engine");
    let embedder = ConstEmbedder { dim: DIM };

    for i in 0..n {
        let topic = TOPICS[i % TOPICS.len()];
        let content = format!("{topic} — fact number {i}");
        engine
            .add_fact(
                &AddFactRequest {
                    content,
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                &embedder,
                None,
            )
            .expect("add_fact");
    }

    (engine, dir)
}

/// Create a file-backed engine with `n` old, low-importance facts that will
/// actually be expired by `forget()`. Without this, fresh high-importance facts
/// all survive and the benchmark only measures scan overhead.
fn setup_forgettable_engine(n: usize) -> (MemoryEngine, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("bench.db");
    let config = EngineConfig::new(db_path, DIM);
    let engine = MemoryEngine::open(&config).expect("open engine");
    let embedder = ConstEmbedder { dim: DIM };

    let old = Utc::now() - Duration::days(120);
    for i in 0..n {
        let topic = TOPICS[i % TOPICS.len()];
        let content = format!("{topic} — stale fact {i}");
        engine
            .add_fact(
                &AddFactRequest {
                    content,
                    fact_type: FactType::Episodic,
                    source_event_id: None,
                    scope: None,
                    opts: Some(AddFactOptions {
                        importance: Some(0.1),
                        t_created: Some(old),
                        last_accessed: Some(old),
                        ..Default::default()
                    }),
                },
                &embedder,
                None,
            )
            .expect("add_fact");
    }

    (engine, dir)
}

fn make_requests(n: usize) -> Vec<AddFactRequest> {
    (0..n)
        .map(|i| {
            let topic = TOPICS[i % TOPICS.len()];
            AddFactRequest {
                content: format!("{topic} — batch fact {i}"),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Consolidation benchmark
// ---------------------------------------------------------------------------

fn bench_consolidation(c: &mut Criterion) {
    let mut group = c.benchmark_group("consolidation");

    for &size in &[100, 1_000, 5_000, 10_000, 50_000] {
        let samples = if size >= 10_000 { 10 } else { 20 };
        group.sample_size(samples);

        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &n| {
            b.iter_with_setup(
                || setup_engine(n),
                |(engine, _dir)| {
                    let generator = ConcatSummaryGenerator;
                    let embedder = ConstEmbedder { dim: DIM };
                    let config = ConsolidationConfig {
                        dedup_threshold: 0.95,
                        min_cluster_size: 3,
                    };
                    engine.consolidate(&generator, &embedder, &config).unwrap();
                },
            );
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Forgetting benchmark
// ---------------------------------------------------------------------------

fn bench_forgetting(c: &mut Criterion) {
    let mut group = c.benchmark_group("forgetting");

    for &size in &[100, 1_000, 5_000, 10_000, 50_000] {
        let samples = if size >= 10_000 { 10 } else { 20 };
        group.sample_size(samples);

        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &n| {
            b.iter_with_setup(
                || setup_forgettable_engine(n),
                |(engine, _dir)| {
                    let policy = ForgetPolicy {
                        min_importance: 0.3,
                        ..Default::default()
                    };
                    engine.forget(&policy).unwrap();
                },
            );
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Single-fact ingest at various corpus sizes
// ---------------------------------------------------------------------------

fn bench_add_fact_single(c: &mut Criterion) {
    let mut group = c.benchmark_group("add_fact_single");
    let embedder = ConstEmbedder { dim: DIM };

    for &corpus_size in &[0, 1_000, 10_000] {
        let samples = if corpus_size >= 10_000 { 10 } else { 20 };
        group.sample_size(samples);

        group.bench_with_input(
            BenchmarkId::new("corpus", corpus_size),
            &corpus_size,
            |b, &n| {
                b.iter_with_setup(
                    || setup_engine(n),
                    |(engine, _dir)| {
                        engine
                            .add_fact(
                                &AddFactRequest {
                                    content: "benchmark insertion probe".to_string(),
                                    fact_type: FactType::Semantic,
                                    source_event_id: None,
                                    scope: None,
                                    opts: None,
                                },
                                &embedder,
                                None,
                            )
                            .unwrap();
                    },
                );
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Batch ingest throughput
// ---------------------------------------------------------------------------

fn bench_add_facts_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("add_facts_batch");
    let embedder = ConstEmbedder { dim: DIM };

    for &batch_size in &[10, 100, 1_000] {
        let samples = if batch_size >= 1_000 { 10 } else { 20 };
        group.sample_size(samples);

        let requests = make_requests(batch_size);

        group.bench_with_input(
            BenchmarkId::new("batch", batch_size),
            &batch_size,
            |b, _| {
                b.iter_with_setup(
                    || {
                        let dir = tempfile::tempdir().expect("tempdir");
                        let db_path = dir.path().join("bench_batch.db");
                        let config = EngineConfig::new(db_path, DIM);
                        let engine = MemoryEngine::open(&config).expect("open engine");
                        (engine, dir)
                    },
                    |(engine, _dir)| {
                        engine.add_facts_batch(&requests, &embedder, None).unwrap();
                    },
                );
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_consolidation,
    bench_forgetting,
    bench_add_fact_single,
    bench_add_facts_batch
);
criterion_main!(benches);
