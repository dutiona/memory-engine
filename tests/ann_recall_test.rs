//! Integration test: HNSW recall vs brute-force oracle.
//!
//! Verifies that HnswStrategy returns results with >= 90% average overlap
//! with the brute-force ground truth across multiple diverse queries.

#![cfg(feature = "ann")]

use std::collections::HashSet;

use memory_engine::engine::{EngineConfig, MemoryEngine};
use memory_engine::search::SearchConfig;
use memory_engine::search::hybrid::{SearchMode, SearchQuery};
use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::FactType;

const DIM: usize = 32;
const N: usize = 5_000;
const K: usize = 10;

struct Blake3Embedder;

impl EmbeddingProvider for Blake3Embedder {
    fn embed(&self, text: &str) -> memory_engine::error::Result<Vec<f32>> {
        let hash = blake3::hash(text.as_bytes());
        let bytes = hash.as_bytes();
        Ok((0..DIM)
            .map(|i| {
                let byte = bytes[i % 32];
                (f32::from(byte) / 255.0).mul_add(2.0, -1.0)
            })
            .collect())
    }
}

#[test]
fn hnsw_recall_at_k_exceeds_threshold() {
    // Build brute-force engine
    let dir_bf = tempfile::tempdir().unwrap();
    let config_bf = EngineConfig::new(dir_bf.path().join("bf.db"), DIM);
    let engine_bf = MemoryEngine::open(&config_bf).unwrap();

    // Build HNSW engine (threshold=0 -> always use HNSW)
    let dir_ann = tempfile::tempdir().unwrap();
    let mut config_ann = EngineConfig::new(dir_ann.path().join("ann.db"), DIM);
    config_ann.search_config = Some(SearchConfig {
        ann_threshold: 0,
        ..Default::default()
    });
    let engine_ann = MemoryEngine::open(&config_ann).unwrap();

    let embedder = Blake3Embedder;
    for i in 0..N {
        let content = format!("fact number {i} about topic {}", i % 50);
        engine_bf
            .add_fact(
                &content,
                FactType::Semantic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();
        engine_ann
            .add_fact(
                &content,
                FactType::Semantic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();
    }

    // Multiple diverse queries for robust recall measurement
    let queries = [
        "fact about topic 7",
        "fact about topic 42",
        "fact number 100 about topic 0",
        "completely unrelated query string",
        "fact about topic 25",
    ];

    let mut recalls = Vec::new();
    for query_text in &queries {
        let query_emb = embedder.embed(query_text).unwrap();
        let query = SearchQuery {
            text: None,
            embedding: Some(query_emb),
            mode: SearchMode::Vector,
            limit: K,
            rerank_depth: None,
            valid_at: None,
            fact_type: None,
            scope: None,
        };

        let bf_results = engine_bf.query(&query).unwrap();
        let ann_results = engine_ann.query(&query).unwrap();

        // Compare by content strings, not row IDs (separate DBs)
        let bf_contents: HashSet<&str> =
            bf_results.iter().map(|r| r.fact.content.as_str()).collect();
        let ann_contents: HashSet<&str> = ann_results
            .iter()
            .map(|r| r.fact.content.as_str())
            .collect();

        let overlap = bf_contents.intersection(&ann_contents).count();
        let recall = overlap as f64 / K as f64;
        recalls.push(recall);

        assert!(
            recall >= 0.7,
            "HNSW recall@{K} = {recall:.2} for query '{query_text}' (min 0.7). \
             BF: {bf_contents:?}, ANN: {ann_contents:?}"
        );
    }

    let avg_recall = recalls.iter().sum::<f64>() / recalls.len() as f64;
    assert!(
        avg_recall >= 0.9,
        "Average HNSW recall@{K} = {avg_recall:.2} (expected >= 0.9). \
         Per-query: {recalls:?}"
    );
}
