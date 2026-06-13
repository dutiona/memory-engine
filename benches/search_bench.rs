//! Criterion benchmarks for search operations at various dataset sizes.
//!
//! These benchmarks are long-lived and intended to be run manually at each
//! release to track performance regressions and validate dispatch thresholds.
//!
//! # Usage
//!
//! ```bash
//! cargo bench                                        # run all
//! cargo bench -- --save-baseline v0.1.0              # save named baseline
//! cargo bench -- --baseline v0.1.0                   # compare to baseline
//! cargo bench -- cosine_similarity                   # run one group
//! cargo bench -- vector_search/1000                  # run one size
//! ```
//!
//! # Methodology
//!
//! - Data generation is deterministic (blake3 hash → embedding).
//! - Criterion's `b.iter()` excludes setup from measurement.
//! - `SQLite` WAL mode + OS page cache → warm-cache after first iteration.
//!   This is realistic for interactive use where the DB is already open.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use memory_engine::engine::{EngineConfig, MemoryEngine};
use memory_engine::search::cosine_similarity;
use memory_engine::search::hybrid::{SearchMode, SearchQuery};
use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::{AddFactRequest, FactType, ScopeQuery};

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
                (f32::from(byte) / 255.0).mul_add(2.0, -1.0)
            })
            .collect();
        Ok(embedding)
    }
}

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

fn setup_engine_with_dim(n: usize, dim: usize) -> MemoryEngine {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("bench.db");
    let config = EngineConfig::new(db_path, dim);
    let engine = MemoryEngine::open(&config).expect("open engine");
    let embedder = ConstEmbedder { dim };

    for i in 0..n {
        let topic = TOPICS[i % TOPICS.len()];
        let content = format!("{topic} — fact number {i}");
        engine
            .add_fact(
                &AddFactRequest {
                    content: content.clone(),
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
    // Criterion runs the benchmark function many times; the engine holds
    // open connections to the file.
    std::mem::forget(dir);
    engine
}

fn setup_engine(n: usize) -> MemoryEngine {
    setup_engine_with_dim(n, DIM)
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
                &AddFactRequest {
                    content,
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: Some(scope.into()),
                    opts: None,
                },
                &embedder,
                None,
            )
            .expect("add_fact");
    }

    std::mem::forget(dir);
    engine
}

fn query_embedding_with_dim(dim: usize) -> Vec<f32> {
    let embedder = ConstEmbedder { dim };
    embedder.embed("Rust memory safety").unwrap()
}

fn query_embedding() -> Vec<f32> {
    query_embedding_with_dim(DIM)
}

// ---------------------------------------------------------------------------
// Cosine similarity micro-benchmark (isolates inner-loop cost from DB I/O)
// ---------------------------------------------------------------------------

fn bench_cosine_similarity(c: &mut Criterion) {
    let mut group = c.benchmark_group("cosine_similarity");

    for &dim in &[128, 384, 768] {
        let a: Vec<f32> = (0..dim)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let v = (i as f32 / dim as f32).mul_add(2.0, -1.0);
                v
            })
            .collect();
        let b: Vec<f32> = (0..dim)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let v = ((i + 1) as f32 / dim as f32).mul_add(2.0, -1.0);
                v
            })
            .collect();

        group.bench_with_input(BenchmarkId::new("dim", dim), &dim, |bench, _| {
            bench.iter(|| cosine_similarity(&a, &b));
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Vector search at various dataset sizes (brute-force baseline)
// ---------------------------------------------------------------------------

fn bench_vector_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("vector_search");

    for &size in &[1_000, 10_000, 50_000, 100_000] {
        // Fewer samples for large N — each iteration is slow but stable.
        let samples = if size >= 50_000 { 10 } else { 20 };
        group.sample_size(samples);

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
                        rerank_depth: None,
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

// ---------------------------------------------------------------------------
// Vector search across embedding dimensions (measures dimension impact)
// ---------------------------------------------------------------------------

fn bench_vector_search_dims(c: &mut Criterion) {
    let mut group = c.benchmark_group("vector_search_dims");
    group.sample_size(10);

    let n = 10_000;
    for &dim in &[128, 384, 768] {
        let engine = setup_engine_with_dim(n, dim);
        let emb = query_embedding_with_dim(dim);
        group.bench_with_input(BenchmarkId::new("dim", dim), &dim, |b, _| {
            b.iter(|| {
                engine
                    .query(&SearchQuery {
                        text: None,
                        embedding: Some(emb.clone()),
                        mode: SearchMode::Vector,
                        limit: 10,
                        rerank_depth: None,
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

// ---------------------------------------------------------------------------
// FTS search
// ---------------------------------------------------------------------------

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
                        rerank_depth: None,
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

// ---------------------------------------------------------------------------
// Hybrid search (FTS + vector + RRF)
// ---------------------------------------------------------------------------

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
                        rerank_depth: None,
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

// ---------------------------------------------------------------------------
// Scoped search (unscoped vs exact vs subtree)
// ---------------------------------------------------------------------------

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
                    rerank_depth: None,
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
                    rerank_depth: None,
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
                    rerank_depth: None,
                    valid_at: None,
                    fact_type: None,
                    scope: Some(ScopeQuery::Subtree("user:alice".into())),
                })
                .unwrap();
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// HNSW search (ANN-accelerated vector search)
// ---------------------------------------------------------------------------

#[cfg(feature = "ann")]
fn setup_hnsw_engine(n: usize) -> MemoryEngine {
    use memory_engine::search::SearchConfig;
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("bench_hnsw.db");
    let mut config = EngineConfig::new(db_path, DIM);
    config.search_config = Some(SearchConfig { ann_threshold: 0 });
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
    std::mem::forget(dir);
    engine
}

#[cfg(feature = "ann")]
fn bench_hnsw_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("hnsw_search");

    for &size in &[1_000, 10_000, 50_000, 100_000] {
        let samples = if size >= 50_000 { 10 } else { 20 };
        group.sample_size(samples);

        let engine = setup_hnsw_engine(size);
        let emb = query_embedding();
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                engine
                    .query(&SearchQuery {
                        text: None,
                        embedding: Some(emb.clone()),
                        mode: SearchMode::Vector,
                        limit: 10,
                        rerank_depth: None,
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

criterion_group!(
    benches,
    bench_cosine_similarity,
    bench_vector_search,
    bench_vector_search_dims,
    bench_fts_search,
    bench_hybrid_search,
    bench_scoped_search
);

#[cfg(feature = "ann")]
criterion_group!(ann_benches, bench_hnsw_search);

#[cfg(feature = "ann")]
criterion_main!(benches, ann_benches);

#[cfg(not(feature = "ann"))]
criterion_main!(benches);
