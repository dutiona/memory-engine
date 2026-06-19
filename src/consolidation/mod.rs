//! Three-pass consolidation pipeline: dedup, cluster fusion, global integration.
//!
//! All passes run atomically in a single `SQLite` transaction.

mod cluster;
mod dedup;
mod global;

pub use cluster::cluster_fusion;
pub use dedup::local_dedup;
pub use global::global_integration;

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::error::Result;
use crate::store::schema::{get_config, set_config};
use crate::traits::{ConsolidationConfig, ConsolidationStats, EmbeddingProvider, SummaryGenerator};
use crate::types::Fact;

/// Summarize a slice of facts and embed the resulting summary text, validating
/// the embedding dimension. Shared by cluster fusion and global integration so
/// the summarize → embed → dimension-check sequence cannot diverge (issue #116:
/// embedding now flows through the injected `EmbeddingProvider`).
///
/// # Errors
///
/// Propagates `SummaryGenerator` / `EmbeddingProvider` errors; returns
/// `MemoryError::EmbeddingDimension` when the embedding length != `embed_dim`.
pub fn summarize_and_embed(
    generator: &dyn SummaryGenerator,
    embedder: &dyn EmbeddingProvider,
    facts: &[Fact],
    embed_dim: usize,
) -> Result<(String, Vec<f32>)> {
    let text = generator.summarize(facts)?;
    let embedding = embedder.embed(&text)?;
    if embedding.len() != embed_dim {
        return Err(crate::error::MemoryError::EmbeddingDimension {
            expected: embed_dim,
            actual: embedding.len(),
        });
    }
    Ok((text, embedding))
}

/// Orchestrate all 3 consolidation passes atomically.
///
/// 1. Local dedup — expire near-duplicate facts
/// 2. Cluster fusion — group related facts, generate cluster summaries
/// 3. Global integration — summarize all clusters into one global summary
///
/// All passes run within a single transaction. On any failure (including
/// `SummaryGenerator` or `EmbeddingProvider` errors), the entire consolidation
/// is rolled back.
///
/// Reads `last_consolidated_at` from config to scope dedup.
/// Updates `last_consolidated_at` after successful completion.
///
/// `generator` produces the summary text; `embedder` projects that text into
/// the fact vector space (issue #116 — embedding is no longer duplicated on the
/// generator trait).
///
/// # Errors
///
/// Propagates errors from any pass, the `SummaryGenerator`, or the
/// `EmbeddingProvider`.
/// Returns `MemoryError::Migration` if `last_consolidated_at` in config cannot be parsed.
pub fn consolidate(
    conn: &Connection,
    generator: &dyn SummaryGenerator,
    embedder: &dyn EmbeddingProvider,
    embed_dim: usize,
    config: &ConsolidationConfig,
) -> Result<(ConsolidationStats, Vec<i64>)> {
    let last = get_config(conn, "last_consolidated_at")?
        .map(|s| DateTime::parse_from_rfc3339(&s))
        .transpose()
        .map_err(|e| {
            crate::error::MigrationError::Incompatible(format!("invalid last_consolidated_at: {e}"))
        })?
        .map(|dt| dt.with_timezone(&Utc));
    let now = Utc::now();

    let tx = conn.unchecked_transaction()?;

    // Record the embedding identity on first write (#613, ADR 0015 §2), inside the
    // transaction so it commits atomically with any summary vectors. Consolidation
    // embeds summary text via `embedder` into the same vector space, so it is a
    // legitimate first-write path; `record_if_absent` is a no-op once an identity
    // exists (the usual case — facts were ingested before consolidation runs).
    crate::store::embedding_meta::record_if_absent(&tx, &embedder.fingerprint(), embed_dim)?;

    let (duplicates_removed, expired_ids) =
        local_dedup(&tx, embed_dim, config.dedup_threshold, last, now)?;

    // usize::MAX is a sentinel from local_dedup meaning "skipped due to safety cap".
    let dedup_skipped = duplicates_removed == usize::MAX;
    let duplicates_removed = if dedup_skipped { 0 } else { duplicates_removed };

    let clusters_created =
        cluster_fusion(&tx, generator, embedder, embed_dim, config.min_cluster_size)?;
    let global_summaries = global_integration(&tx, generator, embedder, embed_dim)?;

    // Only advance the watermark if dedup actually ran. When skipped, facts
    // ingested during the over-cap period must be retried on the next run.
    if !dedup_skipped {
        set_config(&tx, "last_consolidated_at", &now.to_rfc3339())?;
    }

    tx.commit()?;

    Ok((
        ConsolidationStats {
            duplicates_removed,
            clusters_created,
            global_summaries,
        },
        expired_ids,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    use crate::store::facts::FactStore;
    use crate::store::schema::{init_schema, open_memory};
    use crate::store::summaries::SummaryStore;
    use crate::types::{ConsolidationLevel, FactType, NewFact};

    const DIM: usize = 4;

    /// Mock generator that concatenates fact contents. Always succeeds.
    struct MockGenerator;

    impl SummaryGenerator for MockGenerator {
        fn summarize(&self, facts: &[Fact]) -> Result<String> {
            Ok(facts
                .iter()
                .map(|f| f.content.as_str())
                .collect::<Vec<_>>()
                .join(" + "))
        }
    }

    /// Mock generator that always fails — used to force the cluster pass to error
    /// so the whole transaction must roll back.
    struct FailingGenerator;

    impl SummaryGenerator for FailingGenerator {
        fn summarize(&self, _facts: &[Fact]) -> Result<String> {
            Err(crate::error::MemoryError::Internal("summarize boom".into()))
        }
    }

    /// Mock embedder returning a fixed-dimension constant vector.
    struct MockEmbedder;

    impl EmbeddingProvider for MockEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![0.5; DIM])
        }

        fn fingerprint(&self) -> crate::types::EmbeddingFingerprint {
            crate::types::EmbeddingFingerprint::new("mock", "test", DIM)
        }
    }

    fn default_config() -> ConsolidationConfig {
        ConsolidationConfig {
            dedup_threshold: 0.90,
            min_cluster_size: 2,
        }
    }

    /// Insert an active fact, returning its id.
    fn insert_fact(conn: &Connection, content: &str, embedding: Vec<f32>, importance: f64) -> i64 {
        let store = FactStore::new(conn, DIM);
        store
            .insert(&NewFact {
                content: content.into(),
                content_hash: String::new(),
                embedding,
                fact_type: FactType::Semantic,
                t_created: Utc::now(),
                t_expired: None,
                t_valid: None,
                t_invalid: None,
                source_event_id: None,
                scope_id: 1,
                importance,
                access_count: 0,
                last_accessed: Utc::now(),
                metadata: serde_json::json!({}),
                is_pinned: false,
            })
            .unwrap()
    }

    /// Three near-identical facts → one dedup, one cluster, one global summary.
    fn seed_cluster(conn: &Connection) {
        insert_fact(conn, "alpha", vec![1.0, 0.0, 0.0, 0.0], 0.9);
        insert_fact(conn, "beta", vec![0.99, 0.01, 0.0, 0.0], 0.5);
        insert_fact(conn, "gamma", vec![0.98, 0.02, 0.0, 0.0], 0.7);
    }

    #[test]
    fn three_pass_pipeline_runs_atomically() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        seed_cluster(&conn);

        let (stats, expired) =
            consolidate(&conn, &MockGenerator, &MockEmbedder, DIM, &default_config()).unwrap();

        // Dedup pass: near-duplicates above 0.90 collapse. With 3 facts all > 0.90
        // similar, two get expired down to a single survivor.
        assert_eq!(stats.duplicates_removed, 2);
        assert_eq!(expired.len(), 2);

        // After dedup only one active fact remains, so it cannot form a cluster of
        // size >= 2; cluster + global therefore produce nothing.
        assert_eq!(stats.clusters_created, 0);
        assert_eq!(stats.global_summaries, 0);

        let active = FactStore::new(&conn, DIM).list_active(None).unwrap();
        assert_eq!(active.len(), 1);
    }

    #[test]
    fn cluster_and_global_summaries_created() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();

        // Three facts forming a single-linkage chain whose adjacent cosine
        // similarities (~0.883) sit BETWEEN the 0.85 cluster threshold and the
        // 0.90 dedup threshold, so none is a near-duplicate (no expiry) yet all
        // three link into one cluster. Unit vectors → cosine == dot product.
        // a-b = b-c = cos(28°) ≈ 0.883; a-c = cos(56°) ≈ 0.559.
        insert_fact(&conn, "a", vec![1.0, 0.0, 0.0, 0.0], 0.5);
        insert_fact(&conn, "b", vec![0.8829, 0.4695, 0.0, 0.0], 0.5);
        insert_fact(&conn, "c", vec![0.5592, 0.829, 0.0, 0.0], 0.5);

        let (stats, expired) =
            consolidate(&conn, &MockGenerator, &MockEmbedder, DIM, &default_config()).unwrap();

        assert_eq!(stats.duplicates_removed, 0, "no near-duplicates expected");
        assert!(expired.is_empty());
        assert_eq!(stats.clusters_created, 1);
        assert_eq!(stats.global_summaries, 1);

        let store = SummaryStore::new(&conn, DIM);
        assert_eq!(
            store
                .list_by_level(&ConsolidationLevel::Cluster)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .list_by_level(&ConsolidationLevel::Global)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn watermark_written_after_success() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        seed_cluster(&conn);

        // No watermark before the first run.
        assert!(get_config(&conn, "last_consolidated_at").unwrap().is_none());

        let before = Utc::now();
        consolidate(&conn, &MockGenerator, &MockEmbedder, DIM, &default_config()).unwrap();
        let after = Utc::now();

        let raw = get_config(&conn, "last_consolidated_at")
            .unwrap()
            .expect("watermark must be written after a successful run");
        let watermark = DateTime::parse_from_rfc3339(&raw)
            .unwrap()
            .with_timezone(&Utc);
        assert!(
            watermark >= before && watermark <= after,
            "watermark {watermark} not within [{before}, {after}]"
        );
    }

    #[test]
    fn watermark_read_scopes_dedup_to_new_facts() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();

        // Seed a watermark in the future so EVERY existing fact predates it.
        let future = Utc::now() + TimeDelta::days(1);
        set_config(&conn, "last_consolidated_at", &future.to_rfc3339()).unwrap();

        // Two near-duplicate facts, both created "now" (before the watermark).
        insert_fact(&conn, "old A", vec![1.0, 0.0, 0.0, 0.0], 0.5);
        insert_fact(&conn, "old B", vec![0.99, 0.01, 0.0, 0.0], 0.3);

        let (stats, expired) =
            consolidate(&conn, &MockGenerator, &MockEmbedder, DIM, &default_config()).unwrap();

        // Because both facts predate the watermark, dedup's "new facts" set is
        // empty and nothing is removed — proving the watermark is read and applied.
        assert_eq!(stats.duplicates_removed, 0);
        assert!(expired.is_empty());
        assert_eq!(
            FactStore::new(&conn, DIM).list_active(None).unwrap().len(),
            2
        );
    }

    #[test]
    fn invalid_watermark_in_config_errors() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        set_config(&conn, "last_consolidated_at", "not-a-timestamp").unwrap();

        let err = consolidate(&conn, &MockGenerator, &MockEmbedder, DIM, &default_config())
            .expect_err("a malformed watermark must surface as an error");
        assert!(
            matches!(err, crate::error::MemoryError::Migration(_)),
            "expected Migration error, got {err:?}"
        );
    }

    #[test]
    fn failing_pass_rolls_back_entire_transaction() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();

        // Three facts: a near-duplicate pair (dedup expires "dup B") plus a third
        // that still clusters with the survivor so the cluster pass actually
        // invokes the (failing) generator.
        //   dup A vs dup B: cos ≈ 1.0  → above 0.90 dedup threshold → B expires.
        //   dup A vs near C: cos ≈ 0.883 → below dedup, above 0.85 cluster → links.
        insert_fact(&conn, "dup A", vec![1.0, 0.0, 0.0, 0.0], 0.9);
        insert_fact(&conn, "dup B", vec![0.999, 0.001, 0.0, 0.0], 0.5);
        insert_fact(&conn, "near C", vec![0.8829, 0.4695, 0.0, 0.0], 0.5);

        let active_before = FactStore::new(&conn, DIM).list_active(None).unwrap().len();
        assert_eq!(active_before, 3);

        // The cluster pass calls FailingGenerator → error → whole tx rolls back.
        let err = consolidate(
            &conn,
            &FailingGenerator,
            &MockEmbedder,
            DIM,
            &default_config(),
        )
        .expect_err("a failing pass must abort consolidation");
        assert!(
            matches!(err, crate::error::MemoryError::Internal(_)),
            "expected Internal error from the generator, got {err:?}"
        );

        // ROLLBACK invariants: dedup expirations are undone, no summaries persist,
        // and the watermark is NOT advanced.
        let active_after = FactStore::new(&conn, DIM).list_active(None).unwrap();
        assert_eq!(
            active_after.len(),
            3,
            "dedup expirations must be rolled back on a later-pass failure"
        );

        let store = SummaryStore::new(&conn, DIM);
        assert!(
            store
                .list_by_level(&ConsolidationLevel::Cluster)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .list_by_level(&ConsolidationLevel::Global)
                .unwrap()
                .is_empty()
        );

        assert!(
            get_config(&conn, "last_consolidated_at").unwrap().is_none(),
            "watermark must not advance when consolidation rolls back"
        );
    }

    #[test]
    fn empty_engine_consolidates_to_noop() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();

        let (stats, expired) =
            consolidate(&conn, &MockGenerator, &MockEmbedder, DIM, &default_config()).unwrap();

        assert_eq!(stats.duplicates_removed, 0);
        assert_eq!(stats.clusters_created, 0);
        assert_eq!(stats.global_summaries, 0);
        assert!(expired.is_empty());

        // An empty run is still a successful run: the watermark advances.
        assert!(get_config(&conn, "last_consolidated_at").unwrap().is_some());
    }
}
