//! `impl SearchIndex for SqliteBackend` — delegates to the verbatim `search/*`
//! free functions, returning ranked `(fact_id, score)` pairs.
//!
//! D3: `vector_search` is brute-force here (HNSW ownership + its engine-owned
//! dispatch policy move into the backend in `#631`). The vector score is widened
//! `f32 → f64` (value-exact, order-preserving) to match the trait's `f64` return.

use async_trait::async_trait;

use super::{SqliteBackend, convert};
use crate::error::Result;
use crate::search::{fts_count_expired, fts_search_filtered, vector_search_filtered};
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
        // Clone the filter into the blocking closure so the rendered `FilterSql`
        // (with its boxed, non-`Send` bind params) is built and consumed entirely
        // on the pool thread — no params cross the await boundary.
        let filter = filter.clone();
        let query = query.to_owned();
        self.block_read(move |c| {
            let fsql = convert::build_filter_sql(&filter, "f.")?;
            Ok(fts_search_filtered(c, &query, k, &fsql)?
                .into_iter()
                .map(|r| (r.fact_id, r.score))
                .collect())
        })
        .await
    }

    async fn vector_search(
        &self,
        embedding: &[f32],
        filter: &FactFilter,
        k: usize,
    ) -> Result<Vec<(i64, f64)>> {
        let filter = filter.clone();
        let dim = self.embed_dim;
        let q = embedding.to_vec();
        self.block_read(move |c| {
            // Bare (un-aliased) columns: the vector scan is a single-table `FROM facts`.
            let fsql = convert::build_filter_sql(&filter, "")?;
            Ok(vector_search_filtered(c, &q, dim, k, &fsql)?
                .into_iter()
                .map(|r| (r.fact_id, f64::from(r.score)))
                .collect())
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
    #[allow(
        clippy::significant_drop_tightening,
        reason = "oracle read guard intentionally spans prepare + query_map"
    )]
    async fn lexical_include_expired_returns_active_and_expired() {
        use crate::storage::TemporalFilter;
        let pool = seeded(&[
            fact("Rust active", [0.1; DIM], false),
            fact("Rust expired", [0.1; DIM], true),
        ]);
        // Oracle: every "Rust" match regardless of expiry, BM25-ordered.
        let oracle: Vec<i64> = {
            let c = pool.read().unwrap();
            let mut stmt = c
                .prepare(
                    "SELECT f.id FROM facts_fts JOIN facts AS f ON f.id = facts_fts.rowid \
                     WHERE facts_fts MATCH ?1 ORDER BY bm25(facts_fts)",
                )
                .unwrap();
            stmt.query_map(["Rust"], |r| r.get::<_, i64>(0))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        let got = backend(Arc::clone(&pool))
            .lexical_search(
                "Rust",
                &FactFilter::new().temporal(TemporalFilter::IncludeExpired),
                10,
            )
            .await
            .unwrap();
        let got_ids: Vec<i64> = got.iter().map(|(id, _)| *id).collect();
        assert_eq!(got_ids, oracle);
        assert_eq!(
            got_ids.len(),
            2,
            "IncludeExpired must surface the expired row"
        );
    }

    /// Lexical-search ids restricted to `ids`, BM25-ordered, against a raw oracle.
    async fn lexical_ids(pool: &Arc<ConnectionPool>, filter: &FactFilter) -> Vec<i64> {
        backend(Arc::clone(pool))
            .lexical_search("Rust", filter, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    }

    /// Seed a metadata/pinned/valid-window-bearing fact (the base `fact` helper
    /// fixes those to empty/false/none).
    fn fact_meta(content: &str, metadata: serde_json::Value, pinned: bool) -> NewFact {
        let mut f = fact(content, [0.1; DIM], false);
        f.metadata = metadata;
        f.is_pinned = pinned;
        f
    }

    #[tokio::test]
    async fn lexical_ids_filter_restricts_to_listed_ids() {
        let pool = seeded(&[
            fact("Rust one", [0.1; DIM], false),
            fact("Rust two", [0.1; DIM], false),
        ]);
        let first = {
            let c = pool.read().unwrap();
            c.query_row("SELECT MIN(id) FROM facts", [], |r| r.get::<_, i64>(0))
                .unwrap()
        };
        let got = lexical_ids(&pool, &FactFilter::new().ids(vec![first])).await;
        assert_eq!(got, vec![first], "ids filter must keep only the listed id");
    }

    #[tokio::test]
    async fn lexical_pinned_filter_partitions_pinned_and_unpinned() {
        let pool = seeded(&[
            fact_meta("Rust pinned", serde_json::json!({}), true),
            fact_meta("Rust loose", serde_json::json!({}), false),
        ]);
        let pinned = lexical_ids(&pool, &FactFilter::new().pinned(true)).await;
        let unpinned = lexical_ids(&pool, &FactFilter::new().pinned(false)).await;
        assert_eq!(pinned.len(), 1, "pinned(true) keeps only the pinned row");
        assert_eq!(
            unpinned.len(),
            1,
            "pinned(false) keeps only the unpinned row"
        );
        assert_ne!(pinned[0], unpinned[0]);
    }

    #[tokio::test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "oracle read guard intentionally spans prepare + query_map"
    )]
    async fn lexical_metadata_absent_present_split_matches_store_idiom() {
        use crate::storage::MetadataPredicate;
        let pool = seeded(&[
            fact_meta("Rust marked", serde_json::json!({"dream_cycle": 1}), false),
            fact_meta("Rust unmarked", serde_json::json!({}), false),
        ]);
        // Oracle: the store's own json_type-absence idiom.
        let oracle_absent: Vec<i64> = {
            let c = pool.read().unwrap();
            let mut stmt = c
                .prepare(
                    "SELECT f.id FROM facts_fts JOIN facts AS f ON f.id = facts_fts.rowid \
                     WHERE facts_fts MATCH ?1 AND f.t_expired IS NULL \
                       AND json_type(f.metadata, '$.dream_cycle') IS NULL ORDER BY bm25(facts_fts)",
                )
                .unwrap();
            stmt.query_map(["Rust"], |r| r.get::<_, i64>(0))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        let absent = lexical_ids(
            &pool,
            &FactFilter::new().with_metadata(MetadataPredicate::KeyAbsent("dream_cycle".into())),
        )
        .await;
        let present = lexical_ids(
            &pool,
            &FactFilter::new().with_metadata(MetadataPredicate::KeyPresent("dream_cycle".into())),
        )
        .await;
        assert_eq!(
            absent, oracle_absent,
            "KeyAbsent must match the json_type oracle"
        );
        assert_eq!(absent.len(), 1);
        assert_eq!(present.len(), 1, "KeyPresent is the complement");
        assert_ne!(absent[0], present[0]);
    }

    #[tokio::test]
    async fn lexical_metadata_key_equals_binds_scalar_and_path() {
        use crate::storage::MetadataPredicate;
        // Verifies the empirically-uncertain bit: a *parameter-bound* JSON path
        // (`json_extract(metadata, ?)`) plus a scalar value bind.
        let pool = seeded(&[
            fact_meta("Rust hot", serde_json::json!({"score": 42}), false),
            fact_meta("Rust cold", serde_json::json!({"score": 7}), false),
        ]);
        let got = lexical_ids(
            &pool,
            &FactFilter::new().with_metadata(MetadataPredicate::KeyEquals(
                "score".into(),
                serde_json::json!(42),
            )),
        )
        .await;
        assert_eq!(got.len(), 1, "KeyEquals(42) keeps only the score=42 row");
    }

    #[tokio::test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "oracle read guard intentionally spans prepare + query_map"
    )]
    async fn vector_asof_matches_temporal_window_oracle() {
        use crate::storage::TemporalFilter;
        use chrono::{Duration, Utc};
        let now = Utc::now();
        // valid: [now-1h, now+1h) — visible at `now`.
        let mut in_window = fact("a", [1.0, 0.0, 0.0, 0.0], false);
        in_window.t_valid = Some(now - Duration::hours(1));
        in_window.t_invalid = Some(now + Duration::hours(1));
        // valid only in the future: [now+1h, ..) — NOT visible at `now`.
        let mut future = fact("b", [0.9, 0.1, 0.0, 0.0], false);
        future.t_valid = Some(now + Duration::hours(1));
        let pool = seeded(&[in_window, future]);
        let query = [1.0_f32, 0.0, 0.0, 0.0];
        // Oracle: the store's AsOf shape (facts.rs `list_active_at`).
        let oracle: Vec<i64> = {
            let c = pool.read().unwrap();
            let mut stmt = c
                .prepare(
                    "SELECT id FROM facts WHERE t_expired IS NULL \
                       AND (t_valid IS NULL OR t_valid <= ?1) \
                       AND (t_invalid IS NULL OR t_invalid > ?1) ORDER BY id",
                )
                .unwrap();
            stmt.query_map([now.to_rfc3339()], |r| r.get::<_, i64>(0))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        let mut got: Vec<i64> = backend(Arc::clone(&pool))
            .vector_search(
                &query,
                &FactFilter::new().temporal(TemporalFilter::AsOf(now)),
                10,
            )
            .await
            .unwrap()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        got.sort_unstable();
        assert_eq!(got, oracle, "AsOf must honor the valid-window oracle");
        assert_eq!(got.len(), 1, "only the in-window fact is visible at `now`");
    }

    #[tokio::test]
    async fn metadata_key_with_special_chars_is_a_valid_json_path() {
        use crate::storage::MetadataPredicate;
        // A hyphenated key must not break the JSON path (`$.user-id` is a SQLite
        // path syntax error — the key has to be quoted + escaped).
        let pool = seeded(&[
            fact_meta("Rust tagged", serde_json::json!({"user-id": 1}), false),
            fact_meta("Rust untagged", serde_json::json!({}), false),
        ]);
        let got = lexical_ids(
            &pool,
            &FactFilter::new().with_metadata(MetadataPredicate::KeyPresent("user-id".into())),
        )
        .await;
        assert_eq!(
            got.len(),
            1,
            "hyphenated metadata key must match, not error"
        );
    }

    #[tokio::test]
    async fn key_equals_null_matches_present_null_only() {
        use crate::storage::MetadataPredicate;
        // KeyEquals(k, null) means "k present and explicitly JSON null" — it must
        // match {"k": null} but NOT an absent key nor a non-null value.
        let pool = seeded(&[
            fact_meta("Rust isnull", serde_json::json!({"k": null}), false),
            fact_meta("Rust hasval", serde_json::json!({"k": 5}), false),
            fact_meta("Rust absent", serde_json::json!({}), false),
        ]);
        let got = lexical_ids(
            &pool,
            &FactFilter::new().with_metadata(MetadataPredicate::KeyEquals(
                "k".into(),
                serde_json::Value::Null,
            )),
        )
        .await;
        assert_eq!(
            got.len(),
            1,
            "KeyEquals(null) matches only the present-null row"
        );
    }

    #[tokio::test]
    async fn asof_excludes_expired_rows_even_inside_their_valid_window() {
        use crate::storage::TemporalFilter;
        use chrono::Utc;
        // Both facts have an open valid window (t_valid/t_invalid = None), so the
        // only thing that may exclude the expired one is the system-time guard
        // `t_expired IS NULL` — the store's `list_active_at` keeps it; AsOf must too.
        let pool = seeded(&[
            fact("Rust active", [0.1; DIM], false),
            fact("Rust expired", [0.1; DIM], true),
        ]);
        let got = lexical_ids(
            &pool,
            &FactFilter::new().temporal(TemporalFilter::AsOf(Utc::now())),
        )
        .await;
        assert_eq!(
            got.len(),
            1,
            "AsOf must NOT surface soft-deleted (expired) rows; got {got:?}"
        );
    }
}
