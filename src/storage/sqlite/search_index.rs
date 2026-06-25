//! `impl SearchIndex for SqliteBackend` — delegates to the verbatim `search/*`
//! free functions, returning ranked `(fact_id, score)` pairs.
//!
//! D3 (Stage B): `vector_search` now dispatches to HNSW when
//! `should_use_hnsw()` returns true, falling back to brute-force otherwise.
//! The vector score is widened `f32 → f64` (value-exact, order-preserving) to
//! match the trait's `f64` return at both paths.
//!
//! HNSW search is CPU-heavy but in-memory (no DB I/O beyond the filter check
//! per candidate). It runs synchronously inside `block_read` on a blocking
//! thread — matching the engine's sync HNSW dispatch in `query.rs` and avoiding
//! an extra `spawn_blocking` layer for the in-memory work.

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

        // HNSW dispatch (Stage B): mirrors `engine/mod.rs:should_use_hnsw()` exactly.
        // `hnsw` is stored as `Arc<HnswStrategy>` so we can clone it into the
        // `'static` `spawn_blocking` closure without unsafe code. The Arc clone is
        // cheap (pointer copy + refcount bump); no data is copied.
        #[cfg(feature = "ann")]
        if self.should_use_hnsw(&filter) {
            let fact_type = filter.fact_type;
            let scope_ids = filter.scope_ids.clone();
            let pool = std::sync::Arc::clone(&self.pool);
            // `should_use_hnsw` already confirmed `self.hnsw.is_some()`.
            let hnsw = std::sync::Arc::clone(
                self.hnsw
                    .as_ref()
                    .expect("should_use_hnsw() is true only when hnsw is Some"),
            );
            // The read guard (`conn`) must be held for the entire HNSW search because
            // per-candidate DB reads (`check_fact_filters`, `load_embedding`) all use
            // the same connection. Dropping it early would require a separate `pool.read()`
            // per candidate, which is both more expensive and semantically inconsistent.
            #[allow(
                clippy::significant_drop_tightening,
                reason = "conn guard must outlive the HNSW search; early drop would \
                          require a separate connection per candidate lookup"
            )]
            let out = tokio::task::spawn_blocking(move || {
                use crate::search::strategy::VectorSearchStrategy as _;
                let conn = pool.read()?;
                // `HnswStrategy::search` post-filters each HNSW candidate via
                // `check_fact_filters` (t_expired IS NULL + fact_type + scope_id),
                // then exact-scores via `load_embedding` — all inside the closure.
                let results =
                    hnsw.search(&conn, &q, dim, k, fact_type.as_ref(), scope_ids.as_deref())?;
                // Widen f32 → f64 at the backend boundary (value-exact, order-preserving).
                Ok(results
                    .into_iter()
                    .map(|r| (r.fact_id, f64::from(r.score)))
                    .collect::<Vec<_>>())
            })
            .await
            .map_err(super::map_join)?;
            return super::map_seam_err(out);
        }

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
            base_importance: 0.5,
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

// =============================================================================
// Stage B — HNSW dispatch tests (feature = "ann" only)
// =============================================================================

#[cfg(all(test, feature = "ann"))]
mod hnsw_tests {
    use std::sync::Arc;

    use chrono::Utc;

    use super::super::SqliteBackend;
    use crate::pool::ConnectionPool;
    use crate::search::strategy::SearchConfig;
    use crate::storage::graph::FactGraph;
    use crate::storage::{FactFilter, SearchIndex};
    use crate::store::facts::FactStore;
    use crate::store::upcaster::UpcasterRegistry;
    use crate::types::{EmbeddingFingerprint, FactType, NewFact};

    const DIM: usize = 4;

    fn fact(content: &str, embedding: [f32; DIM]) -> NewFact {
        NewFact {
            content: content.into(),
            content_hash: String::new(),
            embedding: embedding.to_vec(),
            fact_type: FactType::Semantic,
            t_created: Utc::now(),
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            scope_id: 1,
            base_importance: 0.5,
            access_count: 0,
            last_accessed: Utc::now(),
            metadata: serde_json::json!({}),
            is_pinned: false,
        }
    }

    /// Seed facts via the write connection and return the shared pool.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "write guard intentionally held across the seed loop"
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

    /// Build a backend with HNSW enabled at the given threshold.
    fn backend_with_ann(pool: Arc<ConnectionPool>, threshold: usize) -> SqliteBackend {
        SqliteBackend::from_pool(pool, Arc::new(UpcasterRegistry::new()))
            .with_search_config(SearchConfig {
                ann_threshold: threshold,
            })
            .unwrap()
    }

    /// Build a backend without any search config (always brute-force).
    fn backend_no_config(pool: Arc<ConnectionPool>) -> SqliteBackend {
        SqliteBackend::from_pool(pool, Arc::new(UpcasterRegistry::new()))
    }

    // -------------------------------------------------------------------------
    // B1 — HNSW vs brute-force recall parity on a small exact corpus
    // -------------------------------------------------------------------------

    /// With `ann_threshold = 0` (always-ANN), HNSW and brute-force must return
    /// the same top-k ids and ordering for an exact-match corpus.
    #[tokio::test]
    async fn hnsw_vs_brute_recall_parity_top2() {
        let facts = vec![
            fact("north", [1.0, 0.0, 0.0, 0.0]),
            fact("near-north", [0.9, 0.1, 0.0, 0.0]),
            fact("east", [0.0, 1.0, 0.0, 0.0]),
        ];
        let pool = seeded(&facts);

        // Brute-force oracle (no search config).
        let brute = backend_no_config(Arc::clone(&pool));
        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let oracle: Vec<(i64, f64)> = brute
            .vector_search(&query, &FactFilter::default(), 2)
            .await
            .unwrap();
        assert_eq!(oracle.len(), 2, "oracle must return 2 results");

        // HNSW backend: threshold=0 ⇒ active_count (3) >= 0 ⇒ HNSW always active.
        let hnsw_be = backend_with_ann(Arc::clone(&pool), 0);
        let hnsw_result: Vec<(i64, f64)> = hnsw_be
            .vector_search(&query, &FactFilter::default(), 2)
            .await
            .unwrap();
        assert_eq!(hnsw_result.len(), 2, "HNSW must return 2 results");

        // IDs and scores must match exactly (HNSW exact-rescores via load_embedding).
        assert_eq!(
            hnsw_result.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            oracle.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            "HNSW top-k ids must match brute-force"
        );
        for ((_, hs), (_, bs)) in hnsw_result.iter().zip(oracle.iter()) {
            assert!(
                (hs - bs).abs() < 1e-6,
                "HNSW score {hs} must equal brute-force score {bs} (f32→f64 widening)"
            );
        }
    }

    // -------------------------------------------------------------------------
    // B2 — ann_threshold boundary: below ⇒ brute-force, at/above ⇒ HNSW
    // -------------------------------------------------------------------------

    /// With 3 active facts, threshold=4 ⇒ `active_count` (3) < 4 ⇒ brute-force.
    /// Both paths must return the same results.
    #[tokio::test]
    async fn threshold_above_active_count_uses_brute_force() {
        let facts = vec![
            fact("a", [1.0, 0.0, 0.0, 0.0]),
            fact("b", [0.9, 0.1, 0.0, 0.0]),
            fact("c", [0.0, 1.0, 0.0, 0.0]),
        ];
        let pool = seeded(&facts);
        // threshold=4 > active_count(3) → should_use_hnsw() = false → brute-force.
        let be = backend_with_ann(Arc::clone(&pool), 4);
        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let got = be
            .vector_search(&query, &FactFilter::default(), 2)
            .await
            .unwrap();
        // Must still return correct results via brute-force.
        assert_eq!(got.len(), 2);
        // First result must be the [1,0,0,0] fact (exact match).
        let all_facts = {
            let c = pool.read().unwrap();
            FactStore::new(&c, DIM).list_all().unwrap()
        };
        let north_id = all_facts.iter().find(|f| f.content == "a").unwrap().id;
        assert_eq!(
            got[0].0, north_id,
            "first result must be the exact-match fact"
        );
    }

    /// With 3 facts, threshold=3 ⇒ `active_count` (3) >= 3 ⇒ HNSW activates.
    #[tokio::test]
    async fn threshold_equal_active_count_activates_hnsw() {
        let facts = vec![
            fact("a", [1.0, 0.0, 0.0, 0.0]),
            fact("b", [0.9, 0.1, 0.0, 0.0]),
            fact("c", [0.0, 1.0, 0.0, 0.0]),
        ];
        let pool = seeded(&facts);
        // threshold=3, active_count=3 → 3 >= 3 → HNSW.
        let be = backend_with_ann(Arc::clone(&pool), 3);
        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let got = be
            .vector_search(&query, &FactFilter::default(), 2)
            .await
            .unwrap();
        assert_eq!(got.len(), 2, "HNSW at threshold must return 2 results");
    }

    // -------------------------------------------------------------------------
    // B3 — search_config == None ⇒ never HNSW (always brute-force)
    // -------------------------------------------------------------------------

    /// A backend with no `search_config` must never activate HNSW, even with lots
    /// of facts. This validates the `#630` edge case.
    #[tokio::test]
    async fn no_search_config_always_brute_force() {
        let facts = vec![
            fact("a", [1.0, 0.0, 0.0, 0.0]),
            fact("b", [0.9, 0.1, 0.0, 0.0]),
            fact("c", [0.0, 1.0, 0.0, 0.0]),
        ];
        let pool = seeded(&facts);
        // No search config → should_use_hnsw() always false.
        let be = backend_no_config(Arc::clone(&pool));
        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let got = be
            .vector_search(&query, &FactFilter::default(), 2)
            .await
            .unwrap();
        // Must return correct results via brute-force.
        assert_eq!(got.len(), 2);
        let all_facts = {
            let c = pool.read().unwrap();
            FactStore::new(&c, DIM).list_all().unwrap()
        };
        let north_id = all_facts.iter().find(|f| f.content == "a").unwrap().id;
        assert_eq!(
            got[0].0, north_id,
            "no-config backend must still rank correctly"
        );
    }

    // -------------------------------------------------------------------------
    // B4 — index maintenance: insert/expire reflected in HNSW results
    // -------------------------------------------------------------------------

    /// After `insert_fact` the newly inserted fact must appear in HNSW results.
    #[tokio::test]
    async fn notify_insert_via_insert_fact_makes_fact_findable() {
        let pool = seeded(&[
            fact("a", [1.0, 0.0, 0.0, 0.0]),
            fact("b", [0.0, 1.0, 0.0, 0.0]),
        ]);
        // threshold=0 so HNSW is always active.
        let be = backend_with_ann(Arc::clone(&pool), 0);

        // Insert a new fact close to [1,0,0,0] via the FactGraph write path.
        let new_id = be
            .insert_fact(&fact("new-close", [0.99, 0.01, 0.0, 0.0]))
            .await
            .unwrap();

        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let results = be
            .vector_search(&query, &FactFilter::default(), 3)
            .await
            .unwrap();
        let found_ids: Vec<i64> = results.iter().map(|(id, _)| *id).collect();
        assert!(
            found_ids.contains(&new_id),
            "newly inserted fact must appear in HNSW results; got {found_ids:?}"
        );
    }

    /// After `expire_fact` the expired fact must NOT appear in HNSW results.
    #[tokio::test]
    async fn notify_expire_via_expire_fact_removes_fact_from_results() {
        let pool = seeded(&[
            fact("north", [1.0, 0.0, 0.0, 0.0]),
            fact("near-north", [0.9, 0.1, 0.0, 0.0]),
            fact("east", [0.0, 1.0, 0.0, 0.0]),
        ]);
        let be = backend_with_ann(Arc::clone(&pool), 0);

        // Find the id of "north".
        let north_id = be
            .list_all_facts()
            .await
            .unwrap()
            .into_iter()
            .find(|f| f.content == "north")
            .unwrap()
            .id;

        // Expire it.
        be.expire_fact(north_id, Utc::now()).await.unwrap();

        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let results = be
            .vector_search(&query, &FactFilter::default(), 3)
            .await
            .unwrap();
        assert!(
            !results.iter().any(|(id, _)| *id == north_id),
            "expired fact must not appear in HNSW results; got {results:?}"
        );
    }

    /// After `insert_fact_atomic` the inserted fact must appear in HNSW results.
    #[tokio::test]
    async fn notify_insert_via_insert_fact_atomic() {
        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());
        let be = backend_with_ann(Arc::clone(&pool), 0);
        let fp = EmbeddingFingerprint::new("test-model", "tei", DIM);

        let new_id = be
            .insert_fact_atomic(&fact("atomic", [0.95, 0.05, 0.0, 0.0]), &fp, DIM)
            .await
            .unwrap();

        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let results = be
            .vector_search(&query, &FactFilter::default(), 3)
            .await
            .unwrap();
        let found_ids: Vec<i64> = results.iter().map(|(id, _)| *id).collect();
        assert!(
            found_ids.contains(&new_id),
            "atomically inserted fact must appear in HNSW results; got {found_ids:?}"
        );
    }

    /// After `insert_facts_batch_atomic` all inserted facts appear in HNSW results.
    #[tokio::test]
    async fn notify_insert_via_insert_facts_batch_atomic() {
        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());
        let be = backend_with_ann(Arc::clone(&pool), 0);
        let fp = EmbeddingFingerprint::new("test-model", "tei", DIM);

        let batch = vec![
            fact("batch-a", [1.0, 0.0, 0.0, 0.0]),
            fact("batch-b", [0.9, 0.1, 0.0, 0.0]),
        ];
        let paths: Vec<Option<String>> = vec![None, None];
        let (ids, _) = be
            .insert_facts_batch_atomic(&batch, &paths, &fp, DIM)
            .await
            .unwrap();

        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let results = be
            .vector_search(&query, &FactFilter::default(), 5)
            .await
            .unwrap();
        let found_ids: Vec<i64> = results.iter().map(|(id, _)| *id).collect();
        for id in &ids {
            assert!(
                found_ids.contains(id),
                "batch-inserted fact {id} must appear in HNSW results; got {found_ids:?}"
            );
        }
    }

    // -------------------------------------------------------------------------
    // B5 — non-Active filter falls through to brute-force
    // -------------------------------------------------------------------------

    /// A filter with extra dimensions (pinned / metadata / ids) must use brute-force
    /// even when HNSW is otherwise active, so no result is incorrectly filtered.
    #[tokio::test]
    async fn rich_filter_falls_through_to_brute_force() {
        let mut pinned_fact = fact("pinned-north", [1.0, 0.0, 0.0, 0.0]);
        pinned_fact.is_pinned = true;
        let pool = seeded(&[pinned_fact, fact("unpinned", [0.9, 0.1, 0.0, 0.0])]);

        // HNSW active (threshold=0), but filter carries `pinned=true`.
        let be = backend_with_ann(Arc::clone(&pool), 0);
        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let got = be
            .vector_search(&query, &FactFilter::new().pinned(true), 5)
            .await
            .unwrap();
        // Must return only the pinned fact (brute-force path honours the filter).
        assert_eq!(
            got.len(),
            1,
            "pinned filter must restrict to the single pinned fact; got {got:?}"
        );
        let all_facts = {
            let c = pool.read().unwrap();
            FactStore::new(&c, DIM).list_all().unwrap()
        };
        let pinned_id = all_facts.iter().find(|f| f.is_pinned).unwrap().id;
        assert_eq!(got[0].0, pinned_id, "result must be the pinned fact");
    }

    // -------------------------------------------------------------------------
    // B6 — snapshot round-trip
    // -------------------------------------------------------------------------

    /// `hnsw_snapshot` + `load_hnsw_snapshot` must produce an index that gives the
    /// same top-k results as the original (rebuilt from the same DB data).
    #[tokio::test]
    async fn snapshot_round_trip_preserves_search_results() {
        let facts = vec![
            fact("north", [1.0, 0.0, 0.0, 0.0]),
            fact("near-north", [0.9, 0.1, 0.0, 0.0]),
            fact("east", [0.0, 1.0, 0.0, 0.0]),
        ];
        let pool = seeded(&facts);
        let cfg = SearchConfig { ann_threshold: 0 };

        // Original backend.
        let orig = backend_with_ann(Arc::clone(&pool), 0);
        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let original_results = orig
            .vector_search(&query, &FactFilter::default(), 2)
            .await
            .unwrap();

        // Take a snapshot.
        let snap = orig.hnsw_snapshot().unwrap().expect("HNSW must be active");

        // Build a fresh backend and load the snapshot.
        let mut restored =
            SqliteBackend::from_pool(Arc::clone(&pool), Arc::new(UpcasterRegistry::new()))
                .with_search_config(cfg)
                .unwrap();
        restored.load_hnsw_snapshot(&snap).unwrap();

        let restored_results = restored
            .vector_search(&query, &FactFilter::default(), 2)
            .await
            .unwrap();

        // Same ids in same order.
        assert_eq!(
            restored_results
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
            original_results
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
            "snapshot round-trip must preserve top-k result order"
        );
    }
}
