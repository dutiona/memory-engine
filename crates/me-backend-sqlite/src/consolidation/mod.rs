//! Phases 1 and 3 of the 3-pass consolidation pipeline — the SQL-touching halves,
//! moved below the seam (Wave 2 #816 / S2, sub-PR 2b).
//!
//! [`load_snapshot`] (phase 1, a brief read) and [`apply_plan`] (phase 3, the
//! all-or-nothing write) both take a `&rusqlite::Connection`, so they live here with
//! the SQL that produces/consumes them. Phase 2 (`compute_plan`, pure business logic
//! over `&dyn SummaryGenerator`/`&dyn EmbeddingProvider`, no `Connection`) lives in
//! `me-consolidate`, which reaches these halves through the `ConsolidationStore` port.
//!
//! The same split recurses one level into `apply_plan`'s three callees: each of
//! `dedup`/`cluster`/`global` splits into a pure `compute_*` (in `me-consolidate`) and
//! an SQL-touching `apply_*` (here).

mod cluster;
mod dedup;
mod global;

use chrono::Utc;
use rusqlite::Connection;

use me_traits::{ConsolidationConfig, ConsolidationStats};
use me_types::error::{Result, StorageError};

use crate::store::facts::{self, FactStore};
use crate::store::schema::set_config;

pub use cluster::apply_clusters;
pub use dedup::apply_dedup;
pub use global::apply_global;
/// `Snapshot`/`ConsolidationPlan` — pure data — live in `me-types`; re-exported here so
/// `crate::consolidation::{Snapshot, ConsolidationPlan}` keep resolving intra-crate,
/// exactly like the facade's own re-export.
pub use me_types::types::consolidation::{ConsolidationPlan, Snapshot};
// The dedup/cluster safety caps `load_snapshot` short-circuits on are single-sourced
// in L0 `me_types::limits` (#983) so this below-the-seam load half and the compute half
// (`me-consolidate`) cannot drift apart. Kept a private `use` — these are
// `load_snapshot`'s internal defaults, not part of this crate's re-export surface.
use me_types::limits::{MAX_FACTS_FOR_CLUSTERING, MAX_FACTS_FOR_DEDUP};

/// Phase 1 — load the read snapshot under a brief lock (engine: production caps).
///
/// # Errors
///
/// Returns `MemoryError::Conflict` if `config` fails validation, `MemoryError::Migration`
/// if the dedup watermark or the consolidation generation cannot be parsed, or
/// `MemoryError::Storage` on read failure.
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

/// Cap-injecting core of [`load_snapshot`].
///
/// The facade's own tests pass small caps to exercise the skip paths without a
/// 50 000-fact corpus (hence `pub`, not private — the facade's `consolidate_with_caps`
/// test helper calls this directly).
///
/// # Errors
///
/// Same as [`load_snapshot`].
pub fn load_snapshot_capped(
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

    // The generation, the watermark and the highest id are read before anything else, so a
    // fact committed during the load is either listed or above the next watermark (#1080).
    let generation = facts::read_consolidation_generation(conn)?;
    let watermark = facts::read_dedup_watermark(conn)?;
    let fact_store = FactStore::new(conn, embed_dim);
    let max_fact_id = fact_store.max_id()?;
    let now = Utc::now();

    // Dedup, cluster and global read only the facts with a vector (#1021); one without,
    // active or not, is recorded so the watermark holds until it has one.
    let active_count = fact_store.count_active_with_vector()?;
    let skipped_vectorless = fact_store.any_without_vector()?;

    // #659: if the corpus is over BOTH caps, the dedup pass would skip AND the cluster
    // pass would skip — so neither needs the materialized active set. Short-circuit the
    // expensive active-set load (which deserializes every embedding BLOB) and return
    // a no-op marker instead. A genuinely empty store (count 0) is NOT over the caps, so
    // it still loads (and consolidates to a watermark-advancing no-op).
    if active_count > max_dedup_facts && active_count > max_cluster_facts {
        return Ok(Snapshot {
            active_facts: Vec::new(),
            watermark,
            max_fact_id,
            generation,
            now,
            over_both_caps: true,
            skipped_vectorless,
        });
    }

    // #389: load the active set ONCE and share it across the dedup and cluster passes,
    // instead of each pass re-querying the store (and re-deserializing every embedding
    // BLOB — ~147 MB for 50k×768-dim, previously paid twice).
    let active_facts = fact_store.list_active_with_vector()?;
    Ok(Snapshot {
        active_facts,
        watermark,
        max_fact_id,
        generation,
        now,
        over_both_caps: false,
        skipped_vectorless,
    })
}

/// Phase 3 — apply the plan in a **single transaction** (#409, D3 atomicity).
///
/// All-or-nothing: a failure here rolls back every write; a consumer failure has already
/// aborted in `compute_plan` (facade-side), before this transaction opens. Tolerant of a
/// fact concurrently expired between the snapshot and this apply (see [`dedup::apply_dedup`]).
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

    // A write since the load made a fact eligible again or protected one (#1080): the plan
    // is stale, so nothing is written and the next run recomputes.
    if facts::read_consolidation_generation(&tx)? != plan.generation {
        let none = ConsolidationStats {
            duplicates_removed: 0,
            clusters_created: 0,
            global_summaries: 0,
        };
        return Ok((none, Vec::new()));
    }

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

    // Advance the watermark only if dedup actually ran (#439/#306): over the cap, the facts
    // above it are retried next run. Nor when a survivor was lost in the #409 gap: its kept
    // loser sits below the plan's highest id and must be compared again. Nor while a fact had
    // no vector at the load (#1021), active or not: it is compared as new at the first run
    // after it gets one. A fact inserted after the load has a larger id than `max_fact_id`,
    // so it needs no check here.
    if !plan.dedup.skipped && !applied.survivor_lost && !plan.skipped_vectorless {
        let next =
            facts::read_dedup_watermark(&tx)?.map_or(plan.max_fact_id, |m| m.max(plan.max_fact_id));
        set_config(&tx, facts::DEDUP_WATERMARK, &next.to_string())?;
        tx.execute("DELETE FROM config WHERE key = 'last_consolidated_at'", [])
            .map_err(StorageError::backend)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{seed_without_vector, setup_memory_db};
    use me_types::types::consolidation::DedupComputed;

    const DIM: usize = 4;

    fn insert_embedded(conn: &Connection, content: &str) -> i64 {
        FactStore::new(conn, DIM)
            .insert(&me_types::test_util::new_fact(
                content,
                vec![1.0, 0.0, 0.0, 0.0],
            ))
            .unwrap()
    }

    /// The plan `compute_plan` makes for a snapshot with nothing to merge or cluster.
    fn idle_plan(snapshot: &Snapshot) -> ConsolidationPlan {
        ConsolidationPlan {
            dedup: DedupComputed {
                skipped: false,
                expirations: Vec::new(),
                importance_updates: Vec::new(),
                importance_score_updates: Vec::new(),
            },
            cluster_ran: false,
            cluster_summaries: Vec::new(),
            global_summary: None,
            embedding_fingerprint: None,
            now: snapshot.now,
            max_fact_id: snapshot.max_fact_id,
            generation: snapshot.generation,
            skipped_vectorless: snapshot.skipped_vectorless,
        }
    }

    fn watermark(conn: &Connection) -> Option<i64> {
        facts::read_dedup_watermark(conn).unwrap()
    }

    fn run(conn: &Connection) {
        let snapshot = load_snapshot(conn, DIM, &ConsolidationConfig::default()).unwrap();
        apply_plan(conn, &idle_plan(&snapshot), DIM).unwrap();
    }

    fn fill(conn: &Connection, id: i64) {
        conn.execute(
            "UPDATE facts SET embedding = ?1 WHERE id = ?2",
            rusqlite::params![crate::store::serialize_embedding(&[0.0, 1.0, 0.0, 0.0]), id],
        )
        .unwrap();
        conn.execute("DELETE FROM embed_queue WHERE fact_id = ?1", [id])
            .unwrap();
    }

    /// A plan that folds `loser` into `survivor`, over `snapshot`.
    fn merge_plan(snapshot: &Snapshot, loser: i64, survivor: i64) -> ConsolidationPlan {
        let mut plan = idle_plan(snapshot);
        plan.dedup.expirations = vec![me_types::types::consolidation::Expiry { loser, survivor }];
        plan
    }

    fn active(conn: &Connection, id: i64) -> bool {
        FactStore::new(conn, DIM)
            .get(id)
            .unwrap()
            .t_expired
            .is_none()
    }

    #[test]
    fn a_changed_generation_discards_the_plan() {
        let conn = setup_memory_db();
        let survivor = insert_embedded(&conn, "cats purr");
        let loser = insert_embedded(&conn, "cats purr too");
        let snapshot = load_snapshot(&conn, DIM, &ConsolidationConfig::default()).unwrap();
        // An unforget, an unpin, a pin or a reconstruction between load and apply.
        facts::bump_consolidation_generation(&conn).unwrap();
        let (stats, expired) =
            apply_plan(&conn, &merge_plan(&snapshot, loser, survivor), DIM).unwrap();
        assert_eq!((stats.duplicates_removed, expired.len()), (0, 0));
        assert!(active(&conn, loser), "the stale plan expires nothing");
        assert_eq!(watermark(&conn), None, "nor moves the watermark");

        let snapshot = load_snapshot(&conn, DIM, &ConsolidationConfig::default()).unwrap();
        apply_plan(&conn, &merge_plan(&snapshot, loser, survivor), DIM).unwrap();
        assert!(!active(&conn, loser), "a fresh plan applies");
        assert_eq!(watermark(&conn), Some(loser));
    }

    /// An unforget, an unpin or a reconstruction rewinds through the same call, which also
    /// discards a plan in flight.
    #[test]
    fn a_rewind_between_load_and_apply_discards_the_plan() {
        let conn = setup_memory_db();
        let survivor = insert_embedded(&conn, "cats purr");
        let loser = insert_embedded(&conn, "cats purr too");
        let snapshot = load_snapshot(&conn, DIM, &ConsolidationConfig::default()).unwrap();
        facts::rewind_dedup_watermark(&conn, survivor).unwrap();
        apply_plan(&conn, &merge_plan(&snapshot, loser, survivor), DIM).unwrap();
        assert!(active(&conn, loser), "the stale plan expires nothing");
    }

    #[test]
    fn pinning_a_planned_loser_between_load_and_apply_discards_the_plan() {
        let conn = setup_memory_db();
        let survivor = insert_embedded(&conn, "cats purr");
        let loser = insert_embedded(&conn, "cats purr too");
        let snapshot = load_snapshot(&conn, DIM, &ConsolidationConfig::default()).unwrap();
        FactStore::new(&conn, DIM).set_pinned(loser, true).unwrap();
        apply_plan(&conn, &merge_plan(&snapshot, loser, survivor), DIM).unwrap();
        assert!(active(&conn, loser), "a pinned fact is never merged away");
    }

    #[test]
    fn a_lost_survivor_holds_the_watermark() {
        let conn = setup_memory_db();
        let survivor = insert_embedded(&conn, "cats purr");
        let loser = insert_embedded(&conn, "cats purr too");
        let snapshot = load_snapshot(&conn, DIM, &ConsolidationConfig::default()).unwrap();
        FactStore::new(&conn, DIM)
            .expire(survivor, Utc::now())
            .unwrap();
        apply_plan(&conn, &merge_plan(&snapshot, loser, survivor), DIM).unwrap();
        assert!(active(&conn, loser), "the loser of a lost survivor is kept");
        assert_eq!(watermark(&conn), None, "and compared again next run");
    }

    #[test]
    fn the_first_run_drops_the_timestamp_watermark() {
        let conn = setup_memory_db();
        set_config(&conn, "last_consolidated_at", &Utc::now().to_rfc3339()).unwrap();
        let id = insert_embedded(&conn, "embedded");
        run(&conn);
        assert_eq!(watermark(&conn), Some(id));
        assert!(
            crate::store::schema::get_config(&conn, "last_consolidated_at")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn the_watermark_holds_while_a_fact_without_a_vector_waits() {
        let conn = setup_memory_db();
        insert_embedded(&conn, "embedded");
        run(&conn);
        let mark = watermark(&conn);
        assert!(
            mark.is_some(),
            "no fact without a vector: the watermark advances"
        );

        let waiting = seed_without_vector(&conn, "waiting");
        run(&conn);
        assert_eq!(watermark(&conn), mark, "a waiting fact holds it");

        conn.execute(
            "UPDATE embed_queue SET attempts = 1, failed_at = enqueued_at, last_error = 'HTTP 400'
              WHERE fact_id = ?1",
            [waiting],
        )
        .unwrap();
        run(&conn);
        assert_eq!(watermark(&conn), mark, "a failed fact holds it");

        let snapshot = load_snapshot(&conn, DIM, &ConsolidationConfig::default()).unwrap();
        fill(&conn, waiting);
        apply_plan(&conn, &idle_plan(&snapshot), DIM).unwrap();
        assert_eq!(
            watermark(&conn),
            mark,
            "one embedded between load and apply holds it"
        );

        let snapshot = load_snapshot(&conn, DIM, &ConsolidationConfig::default()).unwrap();
        let late = seed_without_vector(&conn, "late");
        apply_plan(&conn, &idle_plan(&snapshot), DIM).unwrap();
        assert_eq!(
            watermark(&conn),
            Some(snapshot.max_fact_id),
            "one inserted between load and apply sits above the advanced watermark"
        );
        assert!(late > snapshot.max_fact_id);

        fill(&conn, late);
        run(&conn);
        assert_eq!(
            watermark(&conn),
            Some(late),
            "every fact has a vector: it advances to the highest id"
        );
    }

    #[test]
    fn load_snapshot_skips_a_fact_without_a_vector() {
        let conn = setup_memory_db();
        let embedded = insert_embedded(&conn, "embedded");
        seed_without_vector(&conn, "waiting");
        let config = ConsolidationConfig::default();

        let snapshot = load_snapshot(&conn, DIM, &config).unwrap();
        let ids: Vec<i64> = snapshot.active_facts.iter().map(|f| f.id).collect();
        assert_eq!(ids, [embedded]);
        assert!(snapshot.skipped_vectorless);

        // The caps count only facts with a vector: one fits caps of one.
        let capped = load_snapshot_capped(&conn, DIM, &config, 1, 1).unwrap();
        assert!(!capped.over_both_caps);
        assert_eq!(capped.active_facts.len(), 1);
    }
}
