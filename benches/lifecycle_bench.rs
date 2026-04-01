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
//! - Criterion's `b.iter()` excludes setup from measurement.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use memory_engine::engine::{EngineConfig, MemoryEngine};
use memory_engine::traits::{
    ConsolidationConfig, EmbeddingProvider, ForgetPolicy, SummaryGenerator,
};
use memory_engine::types::{AddFactRequest, Fact, FactType};

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

/// Trivial summary generator: concatenates fact content, embeds the result.
/// Produces deterministic output without any LLM dependency.
struct ConcatSummaryGenerator {
    embedder: ConstEmbedder,
}

impl SummaryGenerator for ConcatSummaryGenerator {
    fn summarize(&self, facts: &[Fact]) -> memory_engine::error::Result<String> {
        let summary: String = facts
            .iter()
            .map(|f| f.content.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        Ok(summary)
    }

    fn embed(&self, text: &str) -> memory_engine::error::Result<Vec<f32>> {
        self.embedder.embed(text)
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

fn setup_engine(n: usize) -> MemoryEngine {
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

    // Leak the tempdir so the DB file persists for the benchmark duration.
    std::mem::forget(dir);
    engine
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

    let generator = ConcatSummaryGenerator {
        embedder: ConstEmbedder { dim: DIM },
    };
    let config = ConsolidationConfig {
        dedup_threshold: 0.95,
        min_cluster_size: 3,
    };

    for &size in &[100, 1_000, 5_000, 10_000, 50_000] {
        let samples = if size >= 10_000 { 10 } else { 20 };
        group.sample_size(samples);

        let engine = setup_engine(size);
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                engine.consolidate(&generator, &config).unwrap();
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Forgetting benchmark
// ---------------------------------------------------------------------------

fn bench_forgetting(c: &mut Criterion) {
    let mut group = c.benchmark_group("forgetting");

    let policy = ForgetPolicy::default();

    for &size in &[100, 1_000, 5_000, 10_000, 50_000] {
        let samples = if size >= 10_000 { 10 } else { 20 };
        group.sample_size(samples);

        let engine = setup_engine(size);
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                engine.forget(&policy).unwrap();
            });
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

        let engine = setup_engine(corpus_size);
        let mut counter: usize = 0;

        group.bench_with_input(
            BenchmarkId::new("corpus", corpus_size),
            &corpus_size,
            |b, _| {
                b.iter(|| {
                    counter += 1;
                    let content = format!("incremental fact {counter}");
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
                        .unwrap();
                });
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
                        // Fresh engine per iteration to avoid unbounded growth.
                        let dir = tempfile::tempdir().expect("tempdir");
                        let db_path = dir.path().join("bench_batch.db");
                        let config = EngineConfig::new(db_path, DIM);
                        let engine = MemoryEngine::open(&config).expect("open engine");
                        // Leak to keep DB alive.
                        std::mem::forget(dir);
                        engine
                    },
                    |engine| {
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
