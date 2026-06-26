//! `SearchIndex` contract bodies.
//!
//! Rank-free: this battery asserts only what holds across rankers (malformed→empty,
//! wrong-dim→error, expired-count, and positive *membership*). Score/order *parity*
//! is backend-native and stays per-backend golden in `src/storage/sqlite/`.

use super::factory::ConformanceBackend;
use super::fixtures::{DIM, new_fact, seed_facts};
use crate::error::MemoryError;
use crate::storage::FactFilter;

/// A malformed lexical query yields an empty result, NOT an error (the FTS-syntax
/// swallow). A backend that errors here breaks the contract.
pub async fn malformed_query_yields_empty<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    seed_facts(&be, &[new_fact("some content here")]).await;
    let got = be
        .lexical_search("\"unbalanced", &FactFilter::default(), 10)
        .await
        .expect("a malformed query must be Ok(empty), not Err");
    assert!(
        got.is_empty(),
        "[{}] a malformed lexical query must yield an empty result",
        f.name()
    );
}

/// A wrong-length embedding is an `EmbeddingDimension` error, NOT an empty result.
pub async fn vector_wrong_dim_yields_embedding_dimension<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let err = be
        .vector_search(&[0.1_f32; DIM + 1], &FactFilter::default(), 10)
        .await
        .expect_err("a wrong-dim embedding must be rejected");
    assert!(
        matches!(err, MemoryError::EmbeddingDimension { .. }),
        "[{}] a wrong-dim embedding must be EmbeddingDimension (not empty), got {err:?}",
        f.name()
    );
}

/// `lexical_count_expired` counts only the **expired** matches (its own
/// `t_expired IS NOT NULL` predicate — a port method, not a parity concern).
pub async fn lexical_count_expired_counts_only_expired<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let ids = seed_facts(&be, &[new_fact("zebra alpha"), new_fact("zebra beta")]).await;
    be.expire_fact(ids[1], chrono::Utc::now())
        .await
        .expect("expire one match");
    let n = be
        .lexical_count_expired("zebra", None, None)
        .await
        .expect("count expired");
    assert_eq!(
        n,
        1,
        "[{}] lexical_count_expired must count only the one expired match",
        f.name()
    );
}

/// `lexical_search` returns the matching fact (rank-free **membership**, never a
/// score/order assertion).
pub async fn lexical_returns_matching_fact<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let id = seed_facts(&be, &[new_fact("xylophone uniqueterm")]).await[0];
    let hits = be
        .lexical_search("xylophone", &FactFilter::default(), 10)
        .await
        .expect("lexical_search");
    assert!(
        hits.iter().any(|(fid, _)| *fid == id),
        "[{}] lexical_search must return the matching fact (membership)",
        f.name()
    );
}

/// `vector_search` returns the seeded fact (rank-free **membership**).
pub async fn vector_returns_seeded_fact<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let id = seed_facts(&be, &[new_fact("vector me")]).await[0];
    let hits = be
        .vector_search(&[0.1_f32; DIM], &FactFilter::default(), 10)
        .await
        .expect("vector_search");
    assert!(
        hits.iter().any(|(fid, _)| *fid == id),
        "[{}] vector_search must return the seeded fact (membership)",
        f.name()
    );
}
