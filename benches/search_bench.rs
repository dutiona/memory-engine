//! Criterion benchmarks for search operations at various dataset sizes.
//!
//! Run with: `cargo bench`
//! Save baseline: `cargo bench -- --save-baseline phase3`

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use memory_engine::engine::{EngineConfig, MemoryEngine};
use memory_engine::search::hybrid::{SearchMode, SearchQuery};
use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::{FactType, ScopeQuery};

const DIM: usize = 128;

struct ConstEmbedder {
    dim: usize,
}

impl EmbeddingProvider for ConstEmbedder {
    fn embed(&self, text: &str) -> memory_engine::error::Result<Vec<f32>> {
        // Deterministic embedding based on text hash for reproducible benchmarks.
        let hash = blake3::hash(text.as_bytes());
        let bytes = hash.as_bytes();
        let embedding: Vec<f32> = (0..self.dim)
            .map(|i| {
                let byte = bytes[i % 32];
                (f32::from(byte) / 255.0) * 2.0 - 1.0
            })
            .collect();
        Ok(embedding)
    }
}

fn setup_engine(n: usize) -> MemoryEngine {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("bench.db");
    let config = EngineConfig::new(db_path, DIM);
    let engine = MemoryEngine::open(&config).expect("open engine");
    let embedder = ConstEmbedder { dim: DIM };

    let topics = [
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

    for i in 0..n {
        let topic = topics[i % topics.len()];
        let content = format!("{topic} — fact number {i}");
        engine
            .add_fact(&content, FactType::Semantic, None, &embedder, None, None)
            .expect("add_fact");
    }

    // Leak the tempdir so the DB file persists for the benchmark duration.
    // Criterion runs the benchmark function many times; the engine holds
    // open connections to the file.
    std::mem::forget(dir);
    engine
}

fn setup_scoped_engine(n: usize) -> MemoryEngine {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("bench_scoped.db");
    let config = EngineConfig::new(db_path, DIM);
    let engine = MemoryEngine::open(&config).expect("open engine");
    let embedder = ConstEmbedder { dim: DIM };

    let scopes = [
        "user:alice/project:alpha",
        "user:alice/project:beta",
        "user:bob/project:gamma",
    ];

    for i in 0..n {
        let scope = scopes[i % scopes.len()];
        let content = format!("scoped fact number {i} in {scope}");
        engine
            .add_fact(
                &content,
                FactType::Semantic,
                None,
                &embedder,
                Some(scope),
                None,
            )
            .expect("add_fact");
    }

    std::mem::forget(dir);
    engine
}

fn query_embedding() -> Vec<f32> {
    let embedder = ConstEmbedder { dim: DIM };
    embedder.embed("Rust memory safety").unwrap()
}

fn bench_vector_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("vector_search");
    group.sample_size(20);

    for &size in &[1_000, 10_000] {
        let engine = setup_engine(size);
        let emb = query_embedding();
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                engine
                    .query(&SearchQuery {
                        text: None,
                        embedding: Some(emb.clone()),
                        mode: SearchMode::Vector,
                        limit: 10,
                        valid_at: None,
                        fact_type: None,
                        scope: None,
                    })
                    .unwrap();
            });
        });
    }
    group.finish();
}

fn bench_fts_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("fts_search");
    group.sample_size(20);

    for &size in &[1_000, 10_000] {
        let engine = setup_engine(size);
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                engine
                    .query(&SearchQuery {
                        text: Some("Rust memory safety".into()),
                        embedding: None,
                        mode: SearchMode::Fts,
                        limit: 10,
                        valid_at: None,
                        fact_type: None,
                        scope: None,
                    })
                    .unwrap();
            });
        });
    }
    group.finish();
}

fn bench_hybrid_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("hybrid_search");
    group.sample_size(20);

    for &size in &[1_000, 10_000] {
        let engine = setup_engine(size);
        let emb = query_embedding();
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                engine
                    .query(&SearchQuery {
                        text: Some("Rust memory".into()),
                        embedding: Some(emb.clone()),
                        mode: SearchMode::Hybrid,
                        limit: 10,
                        valid_at: None,
                        fact_type: None,
                        scope: None,
                    })
                    .unwrap();
            });
        });
    }
    group.finish();
}

fn bench_scoped_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("scoped_search");
    group.sample_size(20);

    let size = 10_000;
    let engine = setup_scoped_engine(size);
    let emb = query_embedding();

    group.bench_function("unscoped", |b| {
        b.iter(|| {
            engine
                .query(&SearchQuery {
                    text: Some("scoped fact".into()),
                    embedding: Some(emb.clone()),
                    mode: SearchMode::Hybrid,
                    limit: 10,
                    valid_at: None,
                    fact_type: None,
                    scope: None,
                })
                .unwrap();
        });
    });

    group.bench_function("scoped_exact", |b| {
        b.iter(|| {
            engine
                .query(&SearchQuery {
                    text: Some("scoped fact".into()),
                    embedding: Some(emb.clone()),
                    mode: SearchMode::Hybrid,
                    limit: 10,
                    valid_at: None,
                    fact_type: None,
                    scope: Some(ScopeQuery::Exact("user:alice/project:alpha".into())),
                })
                .unwrap();
        });
    });

    group.bench_function("scoped_subtree", |b| {
        b.iter(|| {
            engine
                .query(&SearchQuery {
                    text: Some("scoped fact".into()),
                    embedding: Some(emb.clone()),
                    mode: SearchMode::Hybrid,
                    limit: 10,
                    valid_at: None,
                    fact_type: None,
                    scope: Some(ScopeQuery::Subtree("user:alice".into())),
                })
                .unwrap();
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_vector_search,
    bench_fts_search,
    bench_hybrid_search,
    bench_scoped_search
);
criterion_main!(benches);
