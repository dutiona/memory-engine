//! Three-pass consolidation pipeline: dedup, cluster fusion, global integration.
//!
//! **Lock-free compute, atomic apply (#409).** The pipeline is split into three phases
//! so the engine can release its single write lock across the unbounded consumer
//! `summarize`/`embed` IO:
//!
//! 1. [`load_snapshot`] — a brief read: count + `last_consolidated_at` + the active set,
//!    loaded once (#389) and short-circuited entirely when over both safety caps (#659).
//! 2. [`compute_plan`] — **no `Connection`, no lock**: the dedup decision, the cluster
//!    summaries, and the global summary are all computed (the consumer IO lives here) and
//!    returned as a [`ConsolidationPlan`] of pure data.
//! 3. [`apply_plan`] — all writes in a **single transaction**, preserving atomicity
//!    exactly as before (D3): a consumer failure aborts in phase 2 before any write, and
//!    a write failure rolls the whole transaction back.
//!
//! A single-connection `consolidate` entry composes all three on one connection for the
//! unit tests; the engine calls the phases separately so it can drop the lock between the
//! read and the compute.

mod cluster;
mod dedup;
mod global;

use crate::error::StorageError;
use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::error::Result;
use crate::store::facts::FactStore;
use crate::store::schema::{get_config, set_config};
use crate::traits::{
    ConsolidationConfig, ConsolidationStats, EmbeddingProvider, SummarizableContent,
    SummaryGenerator,
};
use crate::types::Fact;

/// Safety cap for the O(N·M) dedup pass (`compute_dedup`). Beyond this many active facts
/// the pass is **skipped and the consolidation watermark is NOT advanced**, so the
/// skipped facts are retried on a later run once the corpus shrinks
/// (`DedupComputed::skipped`).
const MAX_FACTS_FOR_DEDUP: usize = 50_000;

/// Safety cap for the O(N²) cluster pass (`compute_clusters`). Beyond this many active facts
/// clustering is **silently skipped, preserving any existing cluster summaries**
/// (the cap is checked before they would be deleted).
///
/// Deliberate policy difference from [`MAX_FACTS_FOR_DEDUP`]: a dedup skip blocks
/// the watermark so the deferred work is retried, whereas a cluster skip is a
/// no-op that simply keeps the prior summaries until the corpus is tractable
/// again. The two caps share a value today but are named and documented
/// separately so the policies cannot drift silently (#345).
const MAX_FACTS_FOR_CLUSTERING: usize = 50_000;

/// Summarize a slice of items and embed the resulting summary text, validating
/// the embedding dimension. Shared by cluster fusion and global integration so
/// the summarize → embed → dimension-check sequence cannot diverge (issue #116:
/// embedding now flows through the injected `EmbeddingProvider`).
///
/// # Errors
///
/// Propagates `SummaryGenerator` / `EmbeddingProvider` errors; returns
/// `MemoryError::EmbeddingDimension` when the embedding length != `embed_dim`.
fn summarize_and_embed(
    generator: &dyn SummaryGenerator,
    embedder: &dyn EmbeddingProvider,
    items: &[SummarizableContent<'_>],
    embed_dim: usize,
) -> Result<(String, Vec<f32>)> {
    let text = generator.summarize(items)?;
    let embedding = embedder.embed(&text)?;
    if embedding.len() != embed_dim {
        return Err(crate::error::MemoryError::EmbeddingDimension {
            expected: embed_dim,
            actual: embedding.len(),
        });
    }
    Ok((text, embedding))
}

/// `Snapshot` and `ConsolidationPlan` — the read-phase snapshot and the fully-computed
/// plan — moved to `me-types` (Wave 2 #816 E.4b Phase B) as pure data; re-exported here
/// so `crate::consolidation::{Snapshot, ConsolidationPlan}` keep resolving.
pub use me_types::types::consolidation::{ConsolidationPlan, Snapshot};

/// Phase 1 — load the read snapshot under a brief lock (engine: production caps).
///
/// # Errors
///
/// Returns `MemoryError::Conflict` if `config` fails validation, `MemoryError::Migration`
/// if `last_consolidated_at` cannot be parsed, or `MemoryError::Storage` on read failure.
pub fn load_snapshot(
    conn: &Connection,
    embed_dim: usize,
    config: &ConsolidationConfig,
) -> Result<Snapshot> {
    load_snapshot_capped(
        conn,
        embed_dim,
        config,
        MAX_FACTS_FOR_DEDUP,
        MAX_FACTS_FOR_CLUSTERING,
    )
}

/// Cap-injecting core of [`load_snapshot`]; tests pass small caps to exercise the skip
/// paths without a 50 000-fact corpus.
fn load_snapshot_capped(
    conn: &Connection,
    embed_dim: usize,
    config: &ConsolidationConfig,
    max_dedup_facts: usize,
    max_cluster_facts: usize,
) -> Result<Snapshot> {
    // Validate up front, before any read — mirrors `prune()` rejecting an invalid
    // `ForgetPolicy` at the forget entry point. In the cap-injecting core so the test
    // cap-path validates too.
    config.validate()?;

    let last = get_config(conn, "last_consolidated_at")?
        .map(|s| DateTime::parse_from_rfc3339(&s))
        .transpose()
        .map_err(|e| {
            crate::error::MigrationError::Incompatible(format!("invalid last_consolidated_at: {e}"))
        })?
        .map(|dt| dt.with_timezone(&Utc));
    let now = Utc::now();

    let fact_store = FactStore::new(conn, embed_dim);
    let active_count = fact_store.count_active()?;

    // #659: if the corpus is over BOTH caps, the dedup pass would skip AND the cluster
    // pass would skip — so neither needs the materialized active set. Short-circuit the
    // expensive `list_active` load (which deserializes every embedding BLOB) and return
    // a no-op marker instead. A genuinely empty store (count 0) is NOT over the caps, so
    // it still loads (and consolidates to a watermark-advancing no-op).
    if active_count > max_dedup_facts && active_count > max_cluster_facts {
        return Ok(Snapshot {
            active_facts: Vec::new(),
            last,
            now,
            over_both_caps: true,
        });
    }

    // #389: load the active set ONCE and share it across the dedup and cluster passes,
    // instead of each pass re-querying the store (and re-deserializing every embedding
    // BLOB — ~147 MB for 50k×768-dim, previously paid twice).
    let active_facts = fact_store.list_active(None)?;
    Ok(Snapshot {
        active_facts,
        last,
        now,
        over_both_caps: false,
    })
}

/// Phase 2 — compute the plan **without any lock or store access** (engine: production
/// caps). The consumer `summarize`/`embed` IO happens here, off the write lock (#409).
///
/// # Errors
///
/// Propagates errors from the `SummaryGenerator` or `EmbeddingProvider`, or
/// `MemoryError::EmbeddingDimension` on a mismatched embedding length. A failure here
/// aborts before any write, so the store is untouched (atomicity, D3).
pub fn compute_plan(
    snapshot: &Snapshot,
    generator: &dyn SummaryGenerator,
    embedder: &dyn EmbeddingProvider,
    embed_dim: usize,
    config: &ConsolidationConfig,
) -> Result<ConsolidationPlan> {
    compute_plan_capped(
        snapshot,
        generator,
        embedder,
        embed_dim,
        config,
        MAX_FACTS_FOR_DEDUP,
        MAX_FACTS_FOR_CLUSTERING,
    )
}

/// Cap-injecting core of [`compute_plan`].
fn compute_plan_capped(
    snapshot: &Snapshot,
    generator: &dyn SummaryGenerator,
    embedder: &dyn EmbeddingProvider,
    embed_dim: usize,
    config: &ConsolidationConfig,
    max_dedup_facts: usize,
    max_cluster_facts: usize,
) -> Result<ConsolidationPlan> {
    // Over both caps (#659): a complete no-op — dedup skipped (so the watermark is held)
    // and clustering skipped (so existing summaries are preserved). No consumer IO.
    if snapshot.over_both_caps {
        return Ok(ConsolidationPlan {
            dedup: dedup::DedupComputed::skipped(),
            cluster_ran: false,
            cluster_summaries: Vec::new(),
            global_summary: None,
            embedding_fingerprint: None,
            now: snapshot.now,
        });
    }

    // Pass 1 — dedup (pure): expirations + importance inheritances as data (#272/#264).
    let dedup = dedup::compute_dedup(
        &snapshot.active_facts,
        config.dedup_threshold,
        max_dedup_facts,
        snapshot.last,
    );

    // Survivors = the loaded active set minus the dedup expirations (no re-query, borrowed
    // so no clone — #679/#389). They carry the PRE-dedup `importance`/`importance_score`;
    // inert today, since clustering reads only `id`/`content`/`embedding`/`scope_id`.
    let expired_set: std::collections::HashSet<i64> =
        dedup.expirations.iter().map(|e| e.loser).collect();
    let survivors: Vec<&Fact> = snapshot
        .active_facts
        .iter()
        .filter(|f| !expired_set.contains(&f.id))
        .collect();

    // Pass 2 — cluster (consumer IO, no store): summarize/embed each qualifying cluster.
    // The single run-level `now` flows through so all summaries share a created_at (#495).
    let params = cluster::ClusterParams {
        embed_dim,
        min_cluster_size: config.min_cluster_size,
        cluster_threshold: config.cluster_threshold,
        max_facts: max_cluster_facts,
        now: snapshot.now,
    };
    let cluster = cluster::compute_clusters(&survivors, generator, embedder, &params)?;

    // Pass 3 — global (consumer IO, no store): summarize THIS run's in-memory cluster
    // summaries. Gated on `cluster.ran`: when clustering is skipped over the cap, the
    // existing cluster/global summaries are left untouched, so global never re-summarizes
    // stale clusters it could not refresh.
    let global_summary = if cluster.ran {
        global::compute_global(
            &cluster.summaries,
            generator,
            embedder,
            embed_dim,
            snapshot.now,
        )?
    } else {
        None
    };

    // Stamp the embedding identity only if a summary vector was actually produced (#643);
    // capture the fingerprint now so `apply_plan` needs no embedder.
    let stamp = !cluster.summaries.is_empty() || global_summary.is_some();
    let embedding_fingerprint = stamp.then(|| embedder.fingerprint());

    Ok(ConsolidationPlan {
        dedup,
        cluster_ran: cluster.ran,
        cluster_summaries: cluster.summaries,
        global_summary,
        embedding_fingerprint,
        now: snapshot.now,
    })
}

/// Phase 3 — apply the plan in a **single transaction** (#409, D3 atomicity).
///
/// All-or-nothing: a failure here rolls back every write; a consumer failure has already
/// aborted in [`compute_plan`], before this transaction opens. Tolerant of a fact
/// concurrently expired between the snapshot and this apply (see [`dedup::apply_dedup`]).
///
/// Returns the stats plus the ids **actually** expired by this call — which may be fewer
/// than the plan proposed if a concurrent writer expired a survivor (then its loser is
/// kept) or a loser (then it is not counted). The engine drives `notify_expire` and the
/// graph rebuild off this real set, not the stale plan.
///
/// # Errors
///
/// Returns `MemoryError::Storage` on SQL failure, or `MemoryError::Serialization` on a
/// summary serialization failure.
pub fn apply_plan(
    conn: &Connection,
    plan: &ConsolidationPlan,
    embed_dim: usize,
) -> Result<(ConsolidationStats, Vec<i64>)> {
    let tx = conn
        .unchecked_transaction()
        .map_err(StorageError::backend)?;

    // Dedup writes: importance inheritances + expirations (concurrency-tolerant, #409).
    // Returns the ids actually expired plus whether a survivor disappeared in the gap.
    let applied = dedup::apply_dedup(&tx, embed_dim, &plan.dedup, plan.now)?;

    // Summary writes are gated on TWO conditions:
    //  - clustering actually ran (#345): over the cap we must not delete existing summaries
    //    without replacements;
    //  - the dedup applied without a survivor disappearing in the read→write gap (#409): if
    //    a concurrent writer expired a survivor, a planned loser was kept, so the plan's
    //    summaries — clustered over the survivors *without* that loser — are stale. Skip
    //    them and let the next consolidation rebuild from the corrected active set.
    // Cluster + global move together so global never re-summarizes stale clusters.
    let wrote_summaries = plan.cluster_ran && !applied.survivor_lost;
    if wrote_summaries {
        cluster::apply_clusters(&tx, embed_dim, &plan.cluster_summaries)?;
        global::apply_global(&tx, embed_dim, plan.global_summary.as_ref())?;

        // Record the embedding identity on first vector write only (#613/#643, ADR 0015 §2),
        // atomically inside `tx` with the summaries it describes. A vector-less run leaves
        // the store unstamped, so a later real first write with a different embedder
        // establishes the true identity instead of inheriting a stale one (the
        // #614-enforcement landmine).
        if let Some(fingerprint) = &plan.embedding_fingerprint {
            crate::store::embedding_meta::record_if_absent(&tx, fingerprint, embed_dim)?;
        }
    }

    // Advance the watermark only if dedup actually ran (#439/#306). When skipped, facts
    // ingested during the over-cap period must be retried on the next run. A survivor loss
    // is a divergence, not a skip: the dedup that *did* apply is committed, so the
    // watermark advances and the kept loser is reconsidered next run.
    if !plan.dedup.skipped {
        set_config(&tx, "last_consolidated_at", &plan.now.to_rfc3339())?;
    }

    tx.commit().map_err(StorageError::backend)?;

    let stats = ConsolidationStats {
        duplicates_removed: applied.expired.len(),
        clusters_created: if wrote_summaries {
            plan.cluster_summaries.len()
        } else {
            0
        },
        global_summaries: usize::from(wrote_summaries && plan.global_summary.is_some()),
    };
    Ok((stats, applied.expired))
}

/// Orchestrate all 3 consolidation passes on a single connection (load → compute →
/// apply). The convenience entry for non-engine callers and the unit tests; the engine
/// instead calls the three phases separately so it can drop its write lock across the
/// lock-free [`compute_plan`] (#409).
///
/// Reads `last_consolidated_at` to scope dedup and updates it after a successful run.
/// `generator` produces the summary text; `embedder` projects it into the fact vector
/// space (#116). Returns the stats and the ids expired (so the engine can update vector
/// indexes).
///
/// # Errors
///
/// Returns `MemoryError::Conflict` if `config` fails validation
/// ([`ConsolidationConfig::validate`]), `MemoryError::Migration` if `last_consolidated_at`
/// cannot be parsed, or propagates errors from any pass, the `SummaryGenerator`, or the
/// `EmbeddingProvider`.
#[cfg(test)]
fn consolidate(
    conn: &Connection,
    generator: &dyn SummaryGenerator,
    embedder: &dyn EmbeddingProvider,
    embed_dim: usize,
    config: &ConsolidationConfig,
) -> Result<(ConsolidationStats, Vec<i64>)> {
    consolidate_with_caps(
        conn,
        generator,
        embedder,
        embed_dim,
        config,
        MAX_FACTS_FOR_DEDUP,
        MAX_FACTS_FOR_CLUSTERING,
    )
}

/// Cap-injecting core of [`consolidate`]; tests pass small caps to exercise the dedup-skip
/// / watermark-suppression (#439/#306) and the over-both-caps load short-circuit (#659)
/// without a 50 000-fact corpus.
///
/// # Errors
///
/// Same as `consolidate`.
#[cfg(test)]
fn consolidate_with_caps(
    conn: &Connection,
    generator: &dyn SummaryGenerator,
    embedder: &dyn EmbeddingProvider,
    embed_dim: usize,
    config: &ConsolidationConfig,
    max_dedup_facts: usize,
    max_cluster_facts: usize,
) -> Result<(ConsolidationStats, Vec<i64>)> {
    let snapshot =
        load_snapshot_capped(conn, embed_dim, config, max_dedup_facts, max_cluster_facts)?;
    let plan = compute_plan_capped(
        &snapshot,
        generator,
        embedder,
        embed_dim,
        config,
        max_dedup_facts,
        max_cluster_facts,
    )?;
    apply_plan(conn, &plan, embed_dim)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    use crate::store::facts::FactStore;
    use crate::store::schema::{init_schema, open_memory};
    use crate::store::summaries::SummaryStore;
    use crate::types::{ConsolidationLevel, FactType, NewFact, NewSummary};

    const DIM: usize = 4;

    /// Mock generator that concatenates fact contents. Always succeeds.
    struct MockGenerator;

    impl SummaryGenerator for MockGenerator {
        fn summarize(&self, items: &[SummarizableContent<'_>]) -> Result<String> {
            Ok(items.iter().map(|i| i.text).collect::<Vec<_>>().join(" + "))
        }
    }

    /// Mock generator that always fails — used to force the cluster pass to error
    /// so the whole transaction must roll back.
    struct FailingGenerator;

    impl SummaryGenerator for FailingGenerator {
        fn summarize(&self, _items: &[SummarizableContent<'_>]) -> Result<String> {
            Err(crate::error::MemoryError::Internal("summarize boom".into()))
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
                base_importance: importance,
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

        let (stats, expired) = consolidate(
            &conn,
            &MockGenerator,
            &crate::test_utils::MockEmbedder::new(DIM),
            DIM,
            &ConsolidationConfig::default(),
        )
        .unwrap();

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
    fn consolidate_rejects_invalid_config_before_mutating() {
        // The entry point validates its config up front, before touching the store
        // — mirroring how `prune()` rejects an invalid `ForgetPolicy`.
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();

        // Two near-duplicates the dedup pass *would* collapse at threshold 0.90.
        insert_fact(&conn, "alpha", vec![1.0, 0.0, 0.0, 0.0], 0.9);
        insert_fact(&conn, "alpha prime", vec![0.99, 0.01, 0.0, 0.0], 0.5);

        // `dedup_threshold` is valid but `min_cluster_size` is not, so the error
        // must come from validation rather than a pass. If validation did NOT run
        // first, the valid threshold would have expired one near-duplicate.
        // `ConsolidationConfig` is `#[non_exhaustive]` and now lives in the `me-traits`
        // crate, so it can no longer be struct-literal'd here — build it via the builder.
        let bad = ConsolidationConfig::builder()
            .dedup_threshold(0.90)
            .min_cluster_size(0)
            .build();
        let err = consolidate(
            &conn,
            &MockGenerator,
            &crate::test_utils::MockEmbedder::new(DIM),
            DIM,
            &bad,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("min_cluster_size"),
            "error should name the offending parameter, got: {err}"
        );

        // Nothing expired: validation aborted before the dedup pass ran.
        let active = FactStore::new(&conn, DIM).list_active(None).unwrap();
        assert_eq!(
            active.len(),
            2,
            "no fact should be expired when the config is rejected"
        );
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

        let (stats, expired) = consolidate(
            &conn,
            &MockGenerator,
            &crate::test_utils::MockEmbedder::new(DIM),
            DIM,
            &ConsolidationConfig::default(),
        )
        .unwrap();

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
    fn dedup_only_consolidation_does_not_stamp_identity() {
        // #643: a consolidation pass that writes NO summary vector (here a dedup-only
        // run — three near-duplicates collapse to a lone survivor that cannot form a
        // cluster) must not record the embedding identity. Stamping on a vector-less
        // run lets a later real first write with a *different* embedder inherit the
        // stale identity — precisely the #614-era staleness this deferral averts.
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        seed_cluster(&conn);

        let (stats, _) = consolidate(
            &conn,
            &MockGenerator,
            &crate::test_utils::MockEmbedder::new(DIM),
            DIM,
            &ConsolidationConfig::default(),
        )
        .unwrap();
        // No cluster/global summary is produced, so no summary vector is written.
        assert_eq!(stats.clusters_created, 0);
        assert_eq!(stats.global_summaries, 0);

        assert!(
            crate::store::embedding_meta::load(&conn).unwrap().is_none(),
            "a vector-less consolidation must not stamp the embedding identity"
        );

        // The harm averted: a later real first writer with a DIFFERENT embedder now
        // wins, instead of inheriting the no-op run's identity under #614 enforcement.
        let other = crate::types::EmbeddingFingerprint::new("other-model", "other-provider", DIM);
        let recorded = crate::store::embedding_meta::record_if_absent(&conn, &other, DIM).unwrap();
        assert_eq!(
            recorded, other,
            "the first real writer wins after a no-op consolidation"
        );
    }

    #[test]
    fn summary_writing_consolidation_stamps_identity() {
        // Mirror of the above: a pass that DOES write summary vectors records the
        // identity atomically, inside the same transaction.
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        // A single-linkage chain (see `cluster_and_global_summaries_created`) that
        // forms one cluster and one global summary without any near-duplicate expiry.
        insert_fact(&conn, "a", vec![1.0, 0.0, 0.0, 0.0], 0.5);
        insert_fact(&conn, "b", vec![0.8829, 0.4695, 0.0, 0.0], 0.5);
        insert_fact(&conn, "c", vec![0.5592, 0.829, 0.0, 0.0], 0.5);

        let (stats, _) = consolidate(
            &conn,
            &MockGenerator,
            &crate::test_utils::MockEmbedder::new(DIM),
            DIM,
            &ConsolidationConfig::default(),
        )
        .unwrap();
        assert!(
            stats.clusters_created > 0 || stats.global_summaries > 0,
            "this fixture must write at least one summary vector"
        );

        assert_eq!(
            crate::store::embedding_meta::load(&conn).unwrap(),
            Some(crate::test_utils::MockEmbedder::new(DIM).fingerprint()),
            "a summary-writing consolidation records the embedder's fingerprint"
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
        consolidate(
            &conn,
            &MockGenerator,
            &crate::test_utils::MockEmbedder::new(DIM),
            DIM,
            &ConsolidationConfig::default(),
        )
        .unwrap();
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

        let (stats, expired) = consolidate(
            &conn,
            &MockGenerator,
            &crate::test_utils::MockEmbedder::new(DIM),
            DIM,
            &ConsolidationConfig::default(),
        )
        .unwrap();

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

        let err = consolidate(
            &conn,
            &MockGenerator,
            &crate::test_utils::MockEmbedder::new(DIM),
            DIM,
            &ConsolidationConfig::default(),
        )
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
            &crate::test_utils::MockEmbedder::new(DIM),
            DIM,
            &ConsolidationConfig::default(),
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

        let (stats, expired) = consolidate(
            &conn,
            &MockGenerator,
            &crate::test_utils::MockEmbedder::new(DIM),
            DIM,
            &ConsolidationConfig::default(),
        )
        .unwrap();

        assert_eq!(stats.duplicates_removed, 0);
        assert_eq!(stats.clusters_created, 0);
        assert_eq!(stats.global_summaries, 0);
        assert!(expired.is_empty());

        // An empty run is still a successful run: the watermark advances.
        assert!(get_config(&conn, "last_consolidated_at").unwrap().is_some());
    }

    /// #439 / #306: when dedup is skipped (corpus over the cap), the orchestrator
    /// must NOT advance `last_consolidated_at`, so the over-cap facts are retried on
    /// the next run. Driven through `consolidate_with_caps` with a tiny dedup cap so
    /// the skip path is reachable without a 50 000-fact corpus. The cluster pass
    /// (separate, much larger cap) still runs — only the dedup-driven watermark
    /// advance is suppressed.
    #[test]
    fn watermark_not_advanced_when_dedup_skipped() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        seed_cluster(&conn); // 3 near-duplicates

        assert!(
            get_config(&conn, "last_consolidated_at").unwrap().is_none(),
            "precondition: no watermark before the run"
        );

        // Dedup cap of 1 vs a 3-fact corpus → dedup is skipped. The cluster cap stays
        // large (`MAX_FACTS_FOR_CLUSTERING`) so only the dedup pass skips, not clustering.
        let (stats, expired) = consolidate_with_caps(
            &conn,
            &MockGenerator,
            &crate::test_utils::MockEmbedder::new(DIM),
            DIM,
            &ConsolidationConfig::default(),
            1,
            MAX_FACTS_FOR_CLUSTERING,
        )
        .unwrap();

        // Skipped dedup contributes no removals and expires nothing...
        assert_eq!(stats.duplicates_removed, 0, "skipped dedup removes nothing");
        assert!(expired.is_empty());

        // ...but only the dedup-driven watermark advance is suppressed: the rest of
        // the pipeline proceeds, so the 3 surviving near-duplicates still cluster
        // and globalize.
        assert_eq!(
            stats.clusters_created, 1,
            "cluster pass still runs when dedup is skipped"
        );
        assert_eq!(
            stats.global_summaries, 1,
            "global pass still runs when dedup is skipped"
        );
        assert_eq!(
            FactStore::new(&conn, DIM).list_active(None).unwrap().len(),
            3,
            "no fact is expired when dedup is skipped"
        );

        // ...and crucially the watermark is held so the skipped facts are retried.
        assert!(
            get_config(&conn, "last_consolidated_at").unwrap().is_none(),
            "watermark must NOT advance when dedup is skipped (over cap)"
        );
    }

    /// #659: when the corpus is over BOTH caps, [`load_snapshot_capped`] short-circuits
    /// the expensive `list_active` load and [`compute_plan_capped`] returns a complete
    /// no-op plan — nothing expired, no summaries written, watermark held. Driven with
    /// both caps at 1 vs a 3-fact corpus so the over-both-caps branch is reachable without
    /// a 50 000-fact corpus.
    #[test]
    fn over_both_caps_skips_load_and_is_noop() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        seed_cluster(&conn); // 3 near-duplicates that would normally dedup + cluster

        // Sanity: the snapshot took the short-circuit (no facts materialized).
        let snapshot =
            load_snapshot_capped(&conn, DIM, &ConsolidationConfig::default(), 1, 1).unwrap();
        assert!(
            snapshot.over_both_caps,
            "over both caps must short-circuit the load"
        );
        assert!(
            snapshot.active_facts.is_empty(),
            "the active set must not be materialized when over both caps"
        );

        let (stats, expired) = consolidate_with_caps(
            &conn,
            &MockGenerator,
            &crate::test_utils::MockEmbedder::new(DIM),
            DIM,
            &ConsolidationConfig::default(),
            1,
            1,
        )
        .unwrap();

        assert_eq!(stats.duplicates_removed, 0);
        assert_eq!(stats.clusters_created, 0);
        assert_eq!(stats.global_summaries, 0);
        assert!(expired.is_empty());

        // Nothing expired; both passes skipped, so the watermark is held for retry.
        assert_eq!(
            FactStore::new(&conn, DIM).list_active(None).unwrap().len(),
            3,
            "no fact is expired when over both caps"
        );
        assert!(
            get_config(&conn, "last_consolidated_at").unwrap().is_none(),
            "watermark must NOT advance when over both caps"
        );
    }

    /// #409 read→write gap (loser case): the engine releases the write lock between the
    /// snapshot and the apply, so another writer may expire the *loser* a dedup merge was
    /// about to expire. [`apply_plan`] must tolerate that — `expire` returns `NotFound`,
    /// which is a no-op (the end state already holds), not a failure, and is not counted
    /// as expired by this run.
    #[test]
    fn apply_plan_tolerates_concurrently_expired_loser() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        // Two near-duplicates → dedup plans to expire the lower-importance one (drop).
        insert_fact(&conn, "keep", vec![1.0, 0.0, 0.0, 0.0], 0.9);
        insert_fact(&conn, "drop", vec![0.99, 0.01, 0.0, 0.0], 0.5);

        let config = ConsolidationConfig::default();
        let snapshot = load_snapshot(&conn, DIM, &config).unwrap();
        let plan = compute_plan(
            &snapshot,
            &MockGenerator,
            &crate::test_utils::MockEmbedder::new(DIM),
            DIM,
            &config,
        )
        .unwrap();
        assert_eq!(
            plan.dedup.expirations.len(),
            1,
            "dedup must plan exactly one expiry"
        );
        let loser = plan.dedup.expirations[0].loser;

        // Simulate a concurrent writer expiring the loser between snapshot and apply.
        FactStore::new(&conn, DIM)
            .expire(loser, Utc::now())
            .unwrap();

        // apply_plan must NOT error; the loser is already gone, so this run expires
        // nothing (it is not double-counted) and only the survivor remains.
        let (stats, expired) = apply_plan(&conn, &plan, DIM).unwrap();
        assert_eq!(
            stats.duplicates_removed, 0,
            "the loser was already expired concurrently; this run expired nothing"
        );
        assert!(expired.is_empty());
        let active = FactStore::new(&conn, DIM).list_active(None).unwrap();
        assert_eq!(active.len(), 1, "exactly the survivor remains");
        assert_eq!(active[0].content, "keep");
    }

    /// #409 read→write gap (survivor case): if the *survivor* a dedup merge folds a loser
    /// into is concurrently expired in the snapshot→apply gap, the merge decision is void.
    /// [`apply_plan`] must then KEEP the loser as the group's representative rather than
    /// expiring it too and orphaning the duplicate cluster.
    #[test]
    fn apply_plan_keeps_loser_when_survivor_concurrently_expired() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        // keep (higher importance) is the survivor; drop (lower) is the planned loser.
        insert_fact(&conn, "keep", vec![1.0, 0.0, 0.0, 0.0], 0.9);
        insert_fact(&conn, "drop", vec![0.99, 0.01, 0.0, 0.0], 0.5);

        let config = ConsolidationConfig::default();
        let snapshot = load_snapshot(&conn, DIM, &config).unwrap();
        let plan = compute_plan(
            &snapshot,
            &MockGenerator,
            &crate::test_utils::MockEmbedder::new(DIM),
            DIM,
            &config,
        )
        .unwrap();
        assert_eq!(plan.dedup.expirations.len(), 1);
        let survivor = plan.dedup.expirations[0].survivor;
        let loser = plan.dedup.expirations[0].loser;

        // Concurrently expire the SURVIVOR between snapshot and apply.
        FactStore::new(&conn, DIM)
            .expire(survivor, Utc::now())
            .unwrap();

        let (stats, expired) = apply_plan(&conn, &plan, DIM).unwrap();
        assert_eq!(
            stats.duplicates_removed, 0,
            "survivor gone → the merge is void, so the loser is NOT expired"
        );
        assert!(expired.is_empty());

        // The loser survives as the group's representative — the cluster is not orphaned.
        let active = FactStore::new(&conn, DIM).list_active(None).unwrap();
        assert_eq!(
            active.len(),
            1,
            "the loser is kept (survivor was expired elsewhere)"
        );
        assert_eq!(active[0].id, loser);
        assert_eq!(active[0].content, "drop");
    }

    /// #409 read→write gap (codex review): when a survivor disappears, the plan's
    /// cluster/global summaries were computed over the survivors WITHOUT the now-kept
    /// loser, so they are stale. `apply_plan` must NOT write them — it leaves the existing
    /// summaries in place for the next consolidation to rebuild. Seeds a cluster summary,
    /// forces a survivor loss, and asserts the seeded summary survives (a normal run would
    /// clear it).
    #[test]
    fn apply_plan_skips_stale_summary_writes_when_survivor_lost() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();

        // A pre-existing cluster summary from an earlier run.
        let summary_store = SummaryStore::new(&conn, DIM);
        summary_store
            .insert(&NewSummary {
                content: "prior cluster".into(),
                embedding: vec![0.25; DIM],
                level: ConsolidationLevel::Cluster,
                source_fact_ids: vec![1, 2],
                scope_id: 1,
                created_at: Utc::now(),
            })
            .unwrap();

        // Two near-duplicates → dedup plans to expire `drop`, survivor `keep`.
        insert_fact(&conn, "keep", vec![1.0, 0.0, 0.0, 0.0], 0.9);
        insert_fact(&conn, "drop", vec![0.99, 0.01, 0.0, 0.0], 0.5);

        let config = ConsolidationConfig::default();
        let snapshot = load_snapshot(&conn, DIM, &config).unwrap();
        let plan = compute_plan(
            &snapshot,
            &MockGenerator,
            &crate::test_utils::MockEmbedder::new(DIM),
            DIM,
            &config,
        )
        .unwrap();
        let survivor = plan.dedup.expirations[0].survivor;

        // Concurrently expire the survivor → the plan's summary view is now stale.
        FactStore::new(&conn, DIM)
            .expire(survivor, Utc::now())
            .unwrap();

        let (stats, _) = apply_plan(&conn, &plan, DIM).unwrap();
        assert_eq!(
            stats.clusters_created, 0,
            "no summaries are (re)written when a survivor was lost"
        );

        // The seeded cluster summary is preserved — a normal (non-lost) run would have
        // cleared it via apply_clusters' delete_by_level.
        let clusters = SummaryStore::new(&conn, DIM)
            .list_by_level(&ConsolidationLevel::Cluster)
            .unwrap();
        assert_eq!(
            clusters.len(),
            1,
            "stale-plan summary writes must be skipped"
        );
        assert_eq!(clusters[0].content, "prior cluster");
    }
}
