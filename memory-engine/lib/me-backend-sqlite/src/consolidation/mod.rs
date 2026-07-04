//! Phases 1 and 3 of the 3-pass consolidation pipeline — the SQL-touching halves,
//! moved below the seam (Wave 2 #816 / S2, sub-PR 2b).
//!
//! [`load_snapshot`] (phase 1, a brief read) and [`apply_plan`] (phase 3, the
//! all-or-nothing write) both take a `&rusqlite::Connection`, so they move here with
//! the SQL that produces/consumes them. Phase 2 (`compute_plan`, pure business logic
//! over `&dyn SummaryGenerator`/`&dyn EmbeddingProvider`, no `Connection`) stays in the
//! facade's `consolidation` module, which re-exports everything below
//! (`pub(crate) use me_backend_sqlite::consolidation::{load_snapshot,
//! load_snapshot_capped, apply_plan};`) so `crate::consolidation::{load_snapshot,
//! apply_plan}` keep resolving for every existing facade caller (`engine/mod.rs`,
//! `engine/consolidation.rs`, `storage/conformance/fixtures.rs`, and the facade's own
//! `consolidate`/`consolidate_with_caps` test helpers, which also need
//! `load_snapshot_capped`).
//!
//! The same split recurses one level into `apply_plan`'s three callees: each of
//! `dedup`/`cluster`/`global` splits into a pure `compute_*` (stays in the facade) and
//! an SQL-touching `apply_*` (moves here). The facade's own `dedup.rs`/`cluster.rs`/
//! `global.rs` re-export the `apply_*` half so their `#[cfg(test)]`-only
//! `local_dedup`/`cluster_fusion`/`global_integration` wrappers — which compose
//! `compute_*` + `apply_*` for that file's own unit tests — keep resolving unchanged.

mod cluster;
mod dedup;
mod global;

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use me_traits::{ConsolidationConfig, ConsolidationStats};
use me_types::error::{MigrationError, Result, StorageError};

use crate::store::facts::FactStore;
use crate::store::schema::{get_config, set_config};

pub use cluster::apply_clusters;
pub use dedup::apply_dedup;
pub use global::apply_global;
/// `Snapshot`/`ConsolidationPlan` — pure data — live in `me-types`; re-exported here so
/// `crate::consolidation::{Snapshot, ConsolidationPlan}` keep resolving intra-crate,
/// exactly like the facade's own re-export.
pub use me_types::types::consolidation::{ConsolidationPlan, Snapshot};

/// Safety cap for the O(N·M) dedup pass (`compute_dedup`, facade-side). Beyond this many
/// active facts the pass is **skipped and the consolidation watermark is NOT advanced**,
/// so the skipped facts are retried on a later run once the corpus shrinks.
const MAX_FACTS_FOR_DEDUP: usize = 50_000;

/// Safety cap for the O(N²) cluster pass (`compute_clusters`, facade-side). Beyond this
/// many active facts clustering is **silently skipped, preserving any existing cluster
/// summaries**.
const MAX_FACTS_FOR_CLUSTERING: usize = 50_000;

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

    let last = get_config(conn, "last_consolidated_at")?
        .map(|s| DateTime::parse_from_rfc3339(&s))
        .transpose()
        .map_err(|e| MigrationError::Incompatible(format!("invalid last_consolidated_at: {e}")))?
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
