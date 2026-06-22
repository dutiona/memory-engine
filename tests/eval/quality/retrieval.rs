//! C1: Retrieval quality benchmarks against the golden corpus.
//!
//! Loads all 50 facts, runs all 25 queries, computes IR metrics,
//! and asserts quality gates.

use std::collections::{HashMap, HashSet};

use memory_engine::search::hybrid::MatchType;
use memory_engine::search::query::MemoryQuery;
use memory_engine::traits::EmbeddingProvider;

use crate::corpus::{CorpusQuery, golden_corpus};
use crate::helpers::{CorpusBuilder, TestEmbedder, eval_engine};
use crate::metrics::{mrr, ndcg_at_k, precision_at_k, recall_at_k};

/// Per-query metrics collected during evaluation.
struct QueryMetrics {
    precision_5: f64,
    recall_10: f64,
    mrr_val: f64,
    ndcg_10: f64,
}

/// Run a corpus query in FTS-only mode (avoids blake3 vector noise).
async fn run_fts_query(
    engine: &memory_engine::engine::MemoryEngine,
    query: &CorpusQuery,
) -> (Vec<i64>, memory_engine::search::hybrid::QueryDiagnostics) {
    let mut mq = MemoryQuery::new().text(query.text).limit(20);

    if let Some(scope) = query.scope {
        mq = mq.scope_subtree(scope);
    }

    let response = engine.execute_query(&mq).await.expect("query failed");
    let ids: Vec<i64> = response.results.iter().map(|r| r.fact.id).collect();
    (ids, response.diagnostics)
}

/// Build the relevant-ID set and grade map for a query given the corpus-to-engine ID mapping.
fn build_relevance_data(
    query: &CorpusQuery,
    fact_ids: &[i64],
) -> (HashSet<i64>, HashMap<i64, u32>) {
    let relevant: HashSet<i64> = query
        .relevance
        .iter()
        .map(|&(idx, _)| fact_ids[idx])
        .collect();

    let grades: HashMap<i64, u32> = query
        .relevance
        .iter()
        .map(|&(idx, grade)| (fact_ids[idx], grade))
        .collect();

    (relevant, grades)
}

#[tokio::test]
async fn golden_corpus_retrieval_quality_gates() {
    let engine = eval_engine();
    let corpus = golden_corpus();
    let fact_ids = CorpusBuilder::populate(&engine, &corpus)
        .await
        .expect("populate failed");

    assert_eq!(
        fact_ids.len(),
        corpus.facts.len(),
        "all corpus facts should be inserted"
    );

    // Use FTS-only mode: blake3 deterministic embeddings produce pseudo-random
    // vectors with no semantic signal, so vector search adds noise rather than
    // recall. FTS-only isolates keyword retrieval quality, which is the
    // meaningful signal for this embedder.
    let mut all_metrics: Vec<QueryMetrics> = Vec::with_capacity(corpus.queries.len());
    let mut floor_failures: Vec<String> = Vec::new();

    for (qi, query) in corpus.queries.iter().enumerate() {
        let (retrieved, diagnostics) = run_fts_query(&engine, query).await;
        let (relevant, grades) = build_relevance_data(query, &fact_ids);

        let p5 = precision_at_k(&retrieved, &relevant, 5);
        let r10 = recall_at_k(&retrieved, &relevant, 10);
        let mrr_val = mrr(&retrieved, &relevant);
        let ndcg_10 = ndcg_at_k(&retrieved, &grades, 10);

        // Per-query floor: collect failures for batch assertion
        if p5 < 0.20 {
            floor_failures.push(format!(
                "Q{qi} ({}) p@5={p5:.3} (fts={}, vec={})",
                query.description, diagnostics.fts_candidates, diagnostics.vector_candidates,
            ));
        }

        all_metrics.push(QueryMetrics {
            precision_5: p5,
            recall_10: r10,
            mrr_val,
            ndcg_10,
        });
    }

    // Per-query floor: no total failures
    assert!(
        floor_failures.is_empty(),
        "queries below precision@5 >= 0.20 floor:\n{}",
        floor_failures.join("\n"),
    );

    // Mean quality gates across all 25 queries
    let n = f64::from(u32::try_from(all_metrics.len()).unwrap());
    let mean_p5 = all_metrics.iter().map(|m| m.precision_5).sum::<f64>() / n;
    let mean_r10 = all_metrics.iter().map(|m| m.recall_10).sum::<f64>() / n;
    let mean_mrr = all_metrics.iter().map(|m| m.mrr_val).sum::<f64>() / n;
    let mean_ndcg = all_metrics.iter().map(|m| m.ndcg_10).sum::<f64>() / n;

    assert!(
        mean_p5 >= 0.60,
        "mean precision@5 = {mean_p5:.3} < 0.60 gate"
    );
    assert!(
        mean_r10 >= 0.70,
        "mean recall@10 = {mean_r10:.3} < 0.70 gate"
    );
    assert!(mean_mrr >= 0.50, "mean MRR = {mean_mrr:.3} < 0.50 gate");
    assert!(
        mean_ndcg >= 0.55,
        "mean nDCG@10 = {mean_ndcg:.3} < 0.55 gate"
    );
}

#[tokio::test]
async fn hybrid_queries_produce_vector_match_types() {
    let engine = eval_engine();
    let corpus = golden_corpus();
    let fact_ids = CorpusBuilder::populate(&engine, &corpus)
        .await
        .expect("populate failed");

    let embedder = TestEmbedder;

    for (qi, query) in corpus.queries.iter().enumerate() {
        if !query.assert_hybrid {
            continue;
        }

        let embedding = embedder.embed(query.text).expect("embedding failed");
        let mut mq = MemoryQuery::new()
            .text(query.text)
            .embedding(embedding)
            .limit(20);

        if let Some(scope) = query.scope {
            mq = mq.scope_subtree(scope);
        }

        let response = engine.execute_query(&mq).await.expect("query failed");
        let has_vector_match = response
            .results
            .iter()
            .any(|r| matches!(r.match_type, MatchType::Vector | MatchType::Both));

        assert!(
            has_vector_match,
            "Q{qi} ({desc}) flagged assert_hybrid but no result has Vector/Both match type",
            desc = query.description,
        );
    }

    // Suppress unused-variable warning when fact_ids is only used for population
    let _ = fact_ids;
}
