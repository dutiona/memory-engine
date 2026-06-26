use std::collections::BTreeMap;
use std::path::Path;

use chrono::Utc;
use rusqlite::Connection;

use super::types::{
    EdgeStats, EngineStatistics, EventStats, FactStats, ScopeStats, StorageStats, SummaryStats,
};
use crate::error::Result;
use crate::store::summaries::str_to_level;

/// Compute aggregate statistics from the database.
///
/// # Errors
///
/// Returns [`MemoryError::Database`] on SQL failure.
pub fn compute_statistics(conn: &Connection, db_path: Option<&Path>) -> Result<EngineStatistics> {
    let now = Utc::now();
    let now_str = now.to_rfc3339();

    // Fact counts — one conditional-aggregation scan instead of five (#394).
    // `SUM(<predicate>)` over an empty table is NULL, hence the COALESCE(…, 0).
    let (total, active, expired, pinned, due): (i64, i64, i64, i64, i64) = conn.query_row(
        "SELECT \
            COUNT(*), \
            COALESCE(SUM(t_expired IS NULL), 0), \
            COALESCE(SUM(t_expired IS NOT NULL), 0), \
            COALESCE(SUM(is_pinned = 1 AND t_expired IS NULL), 0), \
            COALESCE(SUM(t_expired IS NULL AND t_valid IS NOT NULL AND t_valid <= ?1 AND (t_invalid IS NULL OR t_invalid > ?1)), 0) \
         FROM facts",
        [&now_str],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;

    // Edge counts — one conditional-aggregation scan instead of three (#394).
    let (edge_total, edge_active, edge_expired): (i64, i64, i64) = conn.query_row(
        "SELECT \
            COUNT(*), \
            COALESCE(SUM(t_expired IS NULL), 0), \
            COALESCE(SUM(t_expired IS NOT NULL), 0) \
         FROM edges",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;

    // Summary counts by level — the GROUP BY already partitions the table, so its
    // counts sum to the total; deriving `summary_total` from them (rather than a
    // separate COUNT(*)) keeps the histogram and the total provably consistent.
    // An unrecognised level (corrupted row, or a new variant that missed an
    // encoding update) now errors via `str_to_level` instead of being silently
    // dropped — which previously broke the sum(by_level) == summary_total
    // invariant with no signal (#337).
    let mut by_level = BTreeMap::new();
    let mut summary_total: i64 = 0;
    {
        let mut stmt = conn.prepare("SELECT level, COUNT(*) FROM summaries GROUP BY level")?;
        let rows = stmt.query_map([], |row| {
            let level = str_to_level(&row.get::<_, String>(0)?)?;
            let count: i64 = row.get(1)?;
            Ok((level, count))
        })?;
        for row in rows {
            let (level, count) = row?;
            summary_total += count;
            by_level.insert(level, count);
        }
    }

    // Scope counts
    let scope_total: i64 = conn.query_row("SELECT COUNT(*) FROM scopes", [], |r| r.get(0))?;
    let scope_max_depth: i64 =
        conn.query_row("SELECT COALESCE(MAX(depth), 0) FROM scopes", [], |r| {
            r.get(0)
        })?;

    // Event count
    let event_total: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;

    // Storage stats
    let page_count: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
    let page_size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    let main_db_bytes = page_count.checked_mul(page_size).ok_or_else(|| {
        crate::error::MemoryError::Internal(format!(
            "storage size overflow: page_count {page_count} * page_size {page_size}"
        ))
    })?;

    Ok(EngineStatistics {
        facts: FactStats {
            total,
            active,
            expired,
            pinned,
            due,
        },
        edges: EdgeStats {
            total: edge_total,
            active: edge_active,
            expired: edge_expired,
        },
        summaries: SummaryStats {
            total: summary_total,
            by_level,
        },
        scopes: ScopeStats {
            total: scope_total,
            max_depth: scope_max_depth,
        },
        events: EventStats { total: event_total },
        storage: StorageStats {
            page_count,
            page_size,
            main_db_bytes,
            file_path: db_path.map(|p| p.to_string_lossy().into_owned()),
        },
    })
}

#[cfg(test)]
mod tests {
    use crate::engine::MemoryEngine;
    use crate::traits::EmbeddingProvider;
    use crate::types::{AddFactRequest, FactType};

    const DIM: usize = 4;

    struct FakeEmbed;
    impl EmbeddingProvider for FakeEmbed {
        fn embed(&self, _text: &str) -> crate::error::Result<Vec<f32>> {
            Ok(vec![0.1, 0.2, 0.3, 0.4])
        }

        fn fingerprint(&self) -> crate::types::EmbeddingFingerprint {
            crate::types::EmbeddingFingerprint::new("mock", "test", 4)
        }
    }

    #[tokio::test]
    async fn empty_engine_statistics() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let stats = engine.statistics().await.unwrap();
        assert_eq!(stats.facts.total, 0);
        assert_eq!(stats.facts.active, 0);
        assert_eq!(stats.facts.expired, 0);
        assert_eq!(stats.facts.pinned, 0);
        assert_eq!(stats.facts.due, 0);
        assert_eq!(stats.edges.total, 0);
        assert_eq!(stats.summaries.total, 0);
        // Root scope always exists
        assert!(stats.scopes.total >= 1);
        assert_eq!(stats.events.total, 0);
        assert!(stats.storage.page_count > 0);
        assert!(stats.storage.page_size > 0);
        assert!(stats.storage.main_db_bytes > 0);
        assert!(stats.storage.file_path.is_none());
    }

    #[tokio::test]
    async fn statistics_with_facts() {
        use crate::types::AddFactOptions;

        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "fact one".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "fact two".into(),
                    fact_type: FactType::Episodic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();
        // Add a pinned fact
        let pin_opts = AddFactOptions {
            pinned: Some(true),
            ..Default::default()
        };
        engine
            .add_fact(
                &AddFactRequest {
                    content: "pinned fact".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: Some(pin_opts),
                },
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();

        let stats = engine.statistics().await.unwrap();
        assert_eq!(stats.facts.total, 3);
        assert_eq!(stats.facts.active, 3);
        assert_eq!(stats.facts.expired, 0);
        assert_eq!(stats.facts.pinned, 1);
    }

    #[tokio::test]
    async fn snapshot_empty_engine_statistics() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let stats = engine.statistics().await.unwrap();
        insta::assert_yaml_snapshot!(stats, {
            ".storage.page_count" => "[page_count]",
            ".storage.page_size" => "[page_size]",
            ".storage.main_db_bytes" => "[db_bytes]",
        });
    }

    #[tokio::test]
    async fn snapshot_populated_statistics() {
        use crate::types::AddFactOptions;

        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "fact one".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "fact two".into(),
                    fact_type: FactType::Episodic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();
        let pin_opts = AddFactOptions {
            pinned: Some(true),
            ..Default::default()
        };
        engine
            .add_fact(
                &AddFactRequest {
                    content: "pinned fact".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: Some(pin_opts),
                },
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();

        let stats = engine.statistics().await.unwrap();
        insta::assert_yaml_snapshot!(stats, {
            ".storage.page_count" => "[page_count]",
            ".storage.page_size" => "[page_size]",
            ".storage.main_db_bytes" => "[db_bytes]",
        });
    }

    // --- by_level aggregation path (#462) ---------------------------------
    //
    // These drive `compute_statistics` directly against a raw connection so the
    // GROUP-BY-level branch is exercised deterministically, independent of the
    // consolidation pass's clustering thresholds. Summaries are inserted via the
    // real `SummaryStore`, so the persisted `level` encoding and the
    // `str_to_level` round-trip are both covered.

    use crate::store::edges::EdgeStore;
    use crate::store::schema::{init_schema, open_memory};
    use crate::store::summaries::SummaryStore;
    use crate::types::{ConsolidationLevel, NewEdge, NewSummary};

    /// Insert a minimal valid `facts` row and return its id. Bi-temporal columns
    /// (`t_expired`, `t_valid`, `t_invalid`) are bound verbatim so the
    /// `due`-predicate boundaries can be pinned deterministically.
    fn insert_raw_fact(
        conn: &rusqlite::Connection,
        content: &str,
        t_expired: Option<&str>,
        t_valid: Option<&str>,
        t_invalid: Option<&str>,
    ) -> i64 {
        conn.execute(
            "INSERT INTO facts \
                (content, content_hash, embedding, fact_type, t_created, t_expired, \
                 t_valid, t_invalid, importance, access_count, last_accessed, metadata) \
             VALUES (?1, ?2, X'00000000', 'episodic', '2020-01-01T00:00:00+00:00', \
                     ?3, ?4, ?5, 0.5, 0, '2020-01-01T00:00:00+00:00', '{}')",
            rusqlite::params![content, content, t_expired, t_valid, t_invalid],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn level_fixture(level: ConsolidationLevel, n: i64) -> NewSummary {
        NewSummary {
            content: format!("summary {level} #{n}"),
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            level,
            source_fact_ids: vec![n],
            created_at: chrono::Utc::now(),
            scope_id: 1,
        }
    }

    #[test]
    fn by_level_counts_round_trip_and_sum_to_total() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        let store = SummaryStore::new(&conn, DIM);
        // 2 local, 1 cluster, 3 global.
        store
            .insert(&level_fixture(ConsolidationLevel::Local, 1))
            .unwrap();
        store
            .insert(&level_fixture(ConsolidationLevel::Local, 2))
            .unwrap();
        store
            .insert(&level_fixture(ConsolidationLevel::Cluster, 3))
            .unwrap();
        store
            .insert(&level_fixture(ConsolidationLevel::Global, 4))
            .unwrap();
        store
            .insert(&level_fixture(ConsolidationLevel::Global, 5))
            .unwrap();
        store
            .insert(&level_fixture(ConsolidationLevel::Global, 6))
            .unwrap();

        let stats = super::compute_statistics(&conn, None).unwrap();

        assert_eq!(stats.summaries.total, 6);
        assert_eq!(
            stats.summaries.by_level.get(&ConsolidationLevel::Local),
            Some(&2)
        );
        assert_eq!(
            stats.summaries.by_level.get(&ConsolidationLevel::Cluster),
            Some(&1)
        );
        assert_eq!(
            stats.summaries.by_level.get(&ConsolidationLevel::Global),
            Some(&3)
        );
        // The histogram must account for every summary — no silent drops (#337).
        let sum: i64 = stats.summaries.by_level.values().sum();
        assert_eq!(sum, stats.summaries.total);
    }

    #[test]
    fn by_level_empty_when_no_summaries() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        let stats = super::compute_statistics(&conn, None).unwrap();
        assert_eq!(stats.summaries.total, 0);
        assert!(stats.summaries.by_level.is_empty());
    }

    #[test]
    fn unknown_level_errors_instead_of_being_dropped() {
        // Regression guard for #337: an unrecognised stored level must surface as
        // an error, not be silently discarded (which would make
        // sum(by_level) < summaries.total with no signal).
        //
        // The `summaries.level` column carries a `CHECK(level IN
        // ('local','cluster','global'))` constraint, so a bad level cannot land
        // through a normal write — a useful defense-in-depth, but it also means a
        // corrupt-row scenario only arises from outside that guard (a future
        // variant whose encoding was added to the enum but not to the CHECK / the
        // parser, or direct file tampering). We reproduce it by toggling
        // `ignore_check_constraints` for the one INSERT, then asserting the read
        // path refuses to silently drop the row.
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        let store = SummaryStore::new(&conn, DIM);
        store
            .insert(&level_fixture(ConsolidationLevel::Local, 1))
            .unwrap();
        // Bypass the level CHECK constraint to inject a corrupted level.
        conn.pragma_update(None, "ignore_check_constraints", true)
            .unwrap();
        conn.execute(
            "INSERT INTO summaries (content, embedding, level, source_fact_ids, created_at, scope_id)
             VALUES ('corrupt', X'00000000', 'bogus', '[]', '2026-01-01T00:00:00+00:00', 1)",
            [],
        )
        .unwrap();
        conn.pragma_update(None, "ignore_check_constraints", false)
            .unwrap();

        let err = super::compute_statistics(&conn, None).unwrap_err();
        assert!(
            matches!(err, crate::error::MemoryError::Database(_)),
            "expected a Database error from the unknown level, got: {err:?}"
        );
        // Pin the *cause*, not just the variant: the #337 guard must surface the
        // `str_to_level` failure ("unknown consolidation level: bogus"), so a
        // future refactor that swallows the parse error into a generic Database
        // failure (or routes through a different, non-level SQL path) is caught.
        let msg = err.to_string();
        assert!(
            msg.contains("unknown consolidation level"),
            "error must name the unparseable level as its cause, got: {msg}"
        );
    }

    // --- edges conditional-aggregation path (#394) ------------------------
    //
    // Every other test leaves the `edges` table empty, so the edges aggregate
    // (total / active / expired) had zero positive coverage: a tuple-order swap
    // in the row mapper, or a flipped active/expired predicate, would still pass.
    // This pins all three counts to *pairwise-distinct* nonzero values
    // (total=3, active=2, expired=1) — crucially active != expired, so an
    // active<->expired tuple swap or an IS NULL / IS NOT NULL predicate flip is
    // observable (it could not be if both were 1). Any such mutation fails at
    // least one assertion.

    #[test]
    fn edges_total_active_expired_are_distinct_and_correctly_partitioned() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();

        // Two facts to satisfy the edges FK constraints.
        let f1 = insert_raw_fact(&conn, "edge fact 1", None, None, None);
        let f2 = insert_raw_fact(&conn, "edge fact 2", None, None, None);

        let store = EdgeStore::new(&conn);
        let active_edge = |rel: &str| NewEdge {
            source_fact_id: f1,
            target_fact_id: f2,
            relation_type: rel.to_owned(),
            weight: 1.0,
            t_created: chrono::Utc::now(),
            t_expired: None,
            scope_id: 1,
        };
        // Two active edges (t_expired IS NULL) ...
        store.insert(&active_edge("active_a")).unwrap();
        store.insert(&active_edge("active_b")).unwrap();
        // ... and one expired edge (t_expired IS NOT NULL).
        store
            .insert(&NewEdge {
                source_fact_id: f2,
                target_fact_id: f1,
                relation_type: "expired_rel".into(),
                weight: 1.0,
                t_created: chrono::Utc::now(),
                t_expired: Some(chrono::Utc::now()),
                scope_id: 1,
            })
            .unwrap();

        let stats = super::compute_statistics(&conn, None).unwrap();

        // Pairwise-distinct values defeat a tuple-order swap (active<->expired)
        // in the row mapper and a flipped IS NULL / IS NOT NULL predicate: each
        // count is the only one that holds its value.
        assert_eq!(stats.edges.total, 3, "all three edges counted");
        assert_eq!(stats.edges.active, 2, "exactly the t_expired IS NULL edges");
        assert_eq!(
            stats.edges.expired, 1,
            "exactly the t_expired IS NOT NULL edge"
        );
        assert_eq!(
            stats.edges.active + stats.edges.expired,
            stats.edges.total,
            "active + expired partitions total"
        );
    }

    // --- facts `due` predicate (#394) -------------------------------------
    //
    // `due` is the most complex refactored predicate (the only one binding ?1
    // and reading the t_valid/t_invalid window). Until now it was only ever
    // observed as 0, so the whole window logic was unverified. This pins every
    // boundary in one statistics call. Crucially, TWO facts are due while only
    // ONE has a future t_valid: that asymmetry makes the `t_valid <= ?1`
    // comparison observable — flipping it to `>=` would count the single future
    // fact (1) instead of the two past ones (2), so the count itself changes,
    // not merely *which* rows match.

    #[test]
    fn due_predicate_counts_only_facts_in_the_valid_window() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();

        // Due (1): active, t_valid in the past, t_invalid NULL (open-ended window).
        insert_raw_fact(
            &conn,
            "due now",
            None,
            Some("2000-01-01T00:00:00+00:00"),
            None,
        );
        // Due (2): active, t_valid in the past, t_invalid in the future (still in
        // window). A second due fact so the due count (2) differs from the count
        // of future-t_valid facts (1) — pins the direction of the `<= ?1` bound.
        insert_raw_fact(
            &conn,
            "due, closes later",
            None,
            Some("2000-01-01T00:00:00+00:00"),
            Some("2999-01-01T00:00:00+00:00"),
        );
        // Not due: t_valid in the far future (window not yet open).
        insert_raw_fact(
            &conn,
            "future valid",
            None,
            Some("2999-01-01T00:00:00+00:00"),
            None,
        );
        // Not due: invalidated in the past (window already closed).
        insert_raw_fact(
            &conn,
            "invalidated",
            None,
            Some("2000-01-01T00:00:00+00:00"),
            Some("2001-01-01T00:00:00+00:00"),
        );
        // Not due: in-window by time but expired (soft-deleted facts never due).
        insert_raw_fact(
            &conn,
            "expired but in window",
            Some("2010-01-01T00:00:00+00:00"),
            Some("2000-01-01T00:00:00+00:00"),
            None,
        );
        // Not due: no t_valid at all (the predicate requires t_valid IS NOT NULL).
        insert_raw_fact(&conn, "no valid time", None, None, None);

        let stats = super::compute_statistics(&conn, None).unwrap();

        assert_eq!(stats.facts.total, 6);
        assert_eq!(
            stats.facts.due, 2,
            "exactly the two active, past-t_valid, non-invalidated facts are due"
        );
    }
}
