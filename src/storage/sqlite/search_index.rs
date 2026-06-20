//! `impl SearchIndex for SqliteBackend` — delegates to the verbatim `search/*`
//! free functions, returning ranked `(fact_id, score)` pairs.
//!
//! D3: `vector_search` is brute-force here (HNSW ownership + its engine-owned
//! dispatch policy move into the backend in `#631`). The vector score is widened
//! `f32 → f64` (value-exact, order-preserving) to match the trait's `f64` return.

use async_trait::async_trait;

use super::{SqliteBackend, convert};
use crate::error::Result;
use crate::search::{fts_count_expired, fts_search, vector_search};
use crate::storage::{FactFilter, SearchIndex};
use crate::types::FactType;

#[async_trait]
impl SearchIndex for SqliteBackend {
    async fn lexical_search(
        &self,
        query: &str,
        filter: &FactFilter,
        k: usize,
    ) -> Result<Vec<(i64, f64)>> {
        let (fact_type, scope_ids) = convert::search_params(filter)?;
        let query = query.to_owned();
        self.block_read(move |c| {
            Ok(
                fts_search(c, &query, k, fact_type.as_ref(), scope_ids.as_deref())?
                    .into_iter()
                    .map(|r| (r.fact_id, r.score))
                    .collect(),
            )
        })
        .await
    }

    async fn vector_search(
        &self,
        embedding: &[f32],
        filter: &FactFilter,
        k: usize,
    ) -> Result<Vec<(i64, f64)>> {
        let (fact_type, scope_ids) = convert::search_params(filter)?;
        let dim = self.embed_dim;
        let q = embedding.to_vec();
        self.block_read(move |c| {
            Ok(
                vector_search(c, &q, dim, k, fact_type.as_ref(), scope_ids.as_deref())?
                    .into_iter()
                    .map(|r| (r.fact_id, f64::from(r.score)))
                    .collect(),
            )
        })
        .await
    }

    async fn lexical_count_expired(
        &self,
        query: &str,
        fact_type: Option<&FactType>,
        scope_ids: Option<&[i64]>,
    ) -> Result<usize> {
        let query = query.to_owned();
        let fact_type = fact_type.copied();
        let scope_ids = scope_ids.map(<[i64]>::to_vec);
        self.block_read(move |c| {
            fts_count_expired(c, &query, fact_type.as_ref(), scope_ids.as_deref())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;

    use super::super::SqliteBackend;
    use crate::error::MemoryError;
    use crate::pool::ConnectionPool;
    use crate::search::{fts_count_expired, fts_search, vector_search};
    use crate::storage::{FactFilter, SearchIndex};
    use crate::store::facts::FactStore;
    use crate::store::upcaster::UpcasterRegistry;
    use crate::types::{FactType, NewFact};

    const DIM: usize = 4;

    fn fact(content: &str, embedding: [f32; DIM], expired: bool) -> NewFact {
        NewFact {
            content: content.into(),
            content_hash: String::new(),
            embedding: embedding.to_vec(),
            fact_type: FactType::Episodic,
            t_created: Utc::now(),
            t_expired: expired.then(Utc::now),
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            scope_id: 1,
            importance: 0.5,
            access_count: 0,
            last_accessed: Utc::now(),
            metadata: serde_json::json!({}),
            is_pinned: false,
        }
    }

    /// Build a pool, seed it via the write conn, and return it shared. The backend
    /// and the oracle then operate on the same in-memory DB.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the write guard is intentionally held across the seed loop"
    )]
    fn seeded(facts: &[NewFact]) -> Arc<ConnectionPool> {
        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());
        {
            let conn = pool.write();
            let store = FactStore::new(&conn, DIM);
            for f in facts {
                store.insert(f).unwrap();
            }
        }
        pool
    }

    fn backend(pool: Arc<ConnectionPool>) -> SqliteBackend {
        SqliteBackend::from_pool(pool, Arc::new(UpcasterRegistry::new()))
    }

    #[tokio::test]
    async fn lexical_parity_default_filter_value_and_order() {
        let pool = seeded(&[
            fact("Rust Rust Rust systems programming", [0.1; DIM], false),
            fact("Python data science", [0.1; DIM], false),
            fact("Rust language", [0.1; DIM], false),
        ]);
        let oracle: Vec<(i64, f64)> = {
            let c = pool.read().unwrap();
            fts_search(&c, "Rust", 10, None, None)
                .unwrap()
                .into_iter()
                .map(|r| (r.fact_id, r.score))
                .collect()
        };
        let got = backend(Arc::clone(&pool))
            .lexical_search("Rust", &FactFilter::default(), 10)
            .await
            .unwrap();
        assert_eq!(got, oracle);
        assert_eq!(got.len(), 2);
    }

    #[tokio::test]
    async fn lexical_scope_some_empty_matches_nothing_none_finds() {
        let pool = seeded(&[fact("Rust language", [0.1; DIM], false)]);
        let be = backend(Arc::clone(&pool));
        // None = no scope constraint → finds the row.
        let found = be
            .lexical_search("Rust", &FactFilter::default(), 10)
            .await
            .unwrap();
        assert_eq!(found.len(), 1);
        // Some(empty) = matches nothing (NOT normalized to "all").
        let none = be
            .lexical_search("Rust", &FactFilter::new().scope_ids(Vec::<i64>::new()), 10)
            .await
            .unwrap();
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn lexical_malformed_query_is_empty_not_error() {
        let pool = seeded(&[fact("some content", [0.1; DIM], false)]);
        let got = backend(pool)
            .lexical_search("\"unbalanced", &FactFilter::default(), 10)
            .await
            .unwrap();
        assert!(got.is_empty());
    }

    #[tokio::test]
    async fn vector_parity_distinct_scores_value_and_order() {
        let pool = seeded(&[
            fact("a", [1.0, 0.0, 0.0, 0.0], false),
            fact("b", [0.0, 1.0, 0.0, 0.0], false),
            fact("c", [0.9, 0.1, 0.0, 0.0], false),
        ]);
        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let oracle: Vec<(i64, f64)> = {
            let c = pool.read().unwrap();
            vector_search(&c, &query, DIM, 10, None, None)
                .unwrap()
                .into_iter()
                .map(|r| (r.fact_id, f64::from(r.score)))
                .collect()
        };
        let got = backend(Arc::clone(&pool))
            .vector_search(&query, &FactFilter::default(), 10)
            .await
            .unwrap();
        assert_eq!(got, oracle);
    }

    #[tokio::test]
    async fn vector_wrong_dim_is_embedding_dimension_error() {
        let pool = seeded(&[fact("a", [1.0, 0.0, 0.0, 0.0], false)]);
        let err = backend(pool)
            .vector_search(&[1.0_f32, 0.0, 0.0], &FactFilter::default(), 10)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                MemoryError::EmbeddingDimension {
                    expected: DIM,
                    actual: 3
                }
            ),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn count_expired_parity() {
        let pool = seeded(&[
            fact("Rust active", [0.1; DIM], false),
            fact("Rust expired one", [0.1; DIM], true),
            fact("Rust expired two", [0.1; DIM], true),
        ]);
        let oracle = {
            let c = pool.read().unwrap();
            fts_count_expired(&c, "Rust", None, None).unwrap()
        };
        let got = backend(Arc::clone(&pool))
            .lexical_count_expired("Rust", None, None)
            .await
            .unwrap();
        assert_eq!(got, oracle);
        assert_eq!(got, 2);
    }

    #[tokio::test]
    async fn search_rejects_unsupported_filter_dimension() {
        use crate::storage::TemporalFilter;
        let pool = seeded(&[fact("Rust", [0.1; DIM], false)]);
        let err = backend(pool)
            .lexical_search(
                "Rust",
                &FactFilter::new().temporal(TemporalFilter::IncludeExpired),
                10,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::Internal(_)), "got {err:?}");
    }
}
