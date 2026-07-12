//! Consolidate primitive: three-pass dedup → cluster → global summarization over
//! `MemoryCtx`.
//!
//! Extracted from the facade in Wave 2 #816 / S4, sub-PR 4 — the final S4 sub-PR
//! (closes #940). Two layers, mirroring the pre-carve split exactly:
//!
//! - **The pipeline** (`pipeline`, plus its `dedup`/`cluster`/`global` submodules) —
//!   production-pure business logic. Every backend-touching half
//!   (`load_snapshot`/`apply_plan`/`apply_dedup`/`apply_clusters`/`apply_global`, all
//!   `rusqlite::Connection`-based) moved to `me-backend-sqlite` in the Wave 2 #816 / S2
//!   backend carve (sub-PR 2b); this crate's production surface never touches SQL.
//!   Those backend imports, and the single-connection `consolidate`/
//!   `consolidate_with_caps` test wrappers that compose the pure `compute_*` functions
//!   with the backend's `apply_*`, are `#[cfg(test)]`-gated and reach
//!   `me-backend-sqlite` through a `[dev-dependencies]` edge only — not a production
//!   one. This is **compiler-enforced**, not a convention: dev-dependencies are not
//!   linked into the lib target, so un-`cfg(test)`-ing any `me_backend_sqlite` import
//!   here fails `cargo build -p me-consolidate` outright. (`cargo tree -p me-consolidate
//!   --edges normal` shows the same thing, but that is a manual check, not a CI gate.)
//! - **The orchestration** ([`consolidate`], this file) — `MemoryEngine::consolidate`'s
//!   body, extracted as a free function over [`MemoryCtx`] + the in-memory graph. See
//!   its own doc for the full read → compute → write contract (#409).
//!
//! # Scope (a maintainer decision — do not extend without re-reading #940)
//!
//! The Phase-5 cognitive/dream layer (`engine/cognitive.rs`, `engine/cycle/*`) stays in
//! the facade. `DreamContext` holds `engine: &'a MemoryEngine` (and `CycleContext`
//! wraps it), so moving them would force this crate to name `MemoryEngine` — an
//! illegal L3→L4 back-edge. Carving them needs a public API break (inverting the
//! public `DreamContext` into a `me-traits` capability trait) that is deliberately out
//! of scope here.
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod pipeline;

use std::sync::Arc;

use parking_lot::RwLock;

use me_index::MemoryGraph;
use me_storage::MemoryCtx;
use me_traits::{ConsolidationConfig, ConsolidationStats, EmbeddingProvider, SummaryGenerator};
use me_types::error::{MemoryError, Result};

/// Map a `tokio::task::spawn_blocking` join failure (a panic or cancellation in the
/// offloaded consumer `summarize`/`embed` call) to a `MemoryError`. Private copy of the
/// facade's `engine::spawn_join_err` (`pub(super)`, used by other engine modules too —
/// not moved), mirroring every other carved primitive's own copy.
#[allow(
    clippy::needless_pass_by_value,
    reason = "used as map_err(spawn_join_err) fn pointer"
)]
fn spawn_join_err(e: tokio::task::JoinError) -> MemoryError {
    MemoryError::Internal(format!("offloaded task failed: {e}"))
}

/// Run three-pass consolidation: local dedup, cluster fusion, global integration.
///
/// `generator` produces the summary text for each cluster and the global pass;
/// `embedder` projects that text into the fact vector space (issue #116 — embedding
/// is no longer duplicated on the `SummaryGenerator` trait).
///
/// # Lock discipline (#409)
///
/// Structured as **read → compute → write** so the engine's single write lock is
/// NOT held across the consumer `summarize`/`embed` calls (unbounded network IO):
///
/// 1. **Read** (brief lock): snapshot the active set + watermark.
/// 2. **Compute** (no lock): run dedup/cluster/global, including all consumer IO,
///    against the snapshot — producing a `ConsolidationPlan` of pure data.
/// 3. **Write** (brief lock, one transaction): apply the plan atomically, then
///    rebuild the graph if dedup expired anything.
///
/// Atomicity is preserved (a consumer failure aborts in the compute phase, before any
/// write; the apply is all-or-nothing), but other engine writers — and, for an
/// in-memory pool, readers — are no longer starved for the IO duration. Because the
/// lock is released across the compute phase, a concurrent writer (`prune`, conflict
/// resolution) may expire a planned-for fact in the gap; the apply phase tolerates
/// that. This method is **not** designed to be invoked concurrently *with itself* —
/// the per-phase lock no longer serializes whole runs, so two overlapping runs are a
/// benign last-writer-wins on derived summaries, not a supported pattern.
///
/// # Errors
///
/// Returns `MemoryError::EmbeddingReopenRequired` if the handle is fenced (#742) — this
/// is the **first** check, and this function is its sole enforcement point (the facade
/// delegate has no pre-flight of its own).
///
/// Returns `MemoryError::ReadOnly` if the engine was opened read-only — surfaced *below
/// the seam*, when the write phase reaches the pool's `try_write()` guard, not as a
/// pre-flight (see the note at the fence check).
///
/// Propagates errors from any consolidation pass, the `SummaryGenerator`, or the
/// `EmbeddingProvider`.
pub async fn consolidate(
    ctx: MemoryCtx<'_>,
    graph: &RwLock<MemoryGraph>,
    generator: Arc<dyn SummaryGenerator>,
    embedder: Arc<dyn EmbeddingProvider>,
    config: &ConsolidationConfig,
) -> Result<ConsolidationStats> {
    // The #742 reopen-fence check, matching every other carved primitive (`me-query`,
    // `me-ingest`, `me-resolve`, `me-archive` all open with this).
    //
    // ⚠️ This is the **sole** enforcement point of the fence for `consolidate`. Unlike
    // `me-archive` — whose facade delegate keeps its own pre-flight, because the
    // file-backed check must run before `archive_dir` can even be resolved — this
    // primitive's facade delegate (`engine/consolidation.rs`) is a bare one-line forward
    // with NO pre-flight of its own. So this is not belt-and-braces: drop this line and
    // the fence is simply gone. Base enforced it inside `MemoryEngine::consolidate`; the
    // carve moved it here, and here is where it lives now.
    //
    // Base's pre-carve `MemoryEngine::consolidate` called ONLY `self.ensure_open()`
    // (the #742 read-fence) — never an explicit read-only pre-check. `ReadOnly` is
    // caught below the seam instead (`apply_plan`'s write → `block_write` →
    // `try_write()` → `MemoryError::ReadOnly`), so `ctx.ensure_writable()` is
    // deliberately NOT added here either: adding it would be a behavior change (a
    // read-only run would now fail before phase 1's read instead of during phase 3's
    // write), not a faithful extraction. That gate is tracked systemically across all
    // write primitives in #972.
    ctx.ensure_open()?;
    // Phase 1 — READ: snapshot the active set + watermark below the seam.
    let snapshot = ctx
        .storage
        .load_consolidation_snapshot(config.clone())
        .await?;

    // Phase 2 — COMPUTE (no lock): dedup decision + cluster/global summaries,
    // including the consumer `summarize`/`embed` IO. Offloaded to the blocking
    // pool so the (possibly blocking HTTP) consumer calls never park the async
    // executor (#409) — and a `reqwest::blocking` provider stays nested-runtime-safe.
    let plan = {
        let config = config.clone();
        let embed_dim = ctx.embed_dim;
        tokio::task::spawn_blocking(move || {
            pipeline::compute_plan(&snapshot, &*generator, &*embedder, embed_dim, &config)
        })
        .await
        .map_err(spawn_join_err)??
    };

    // Phase 3 — WRITE: apply atomically below the seam. `apply_plan` returns the
    // ids it *actually* expired (a concurrent writer may have pre-empted some in
    // the read→compute gap) and fires the HNSW `notify_expire` internally (Stage
    // B), so the engine rebuilds its in-memory graph off the real change set only.
    let (stats, expired_ids) = ctx.storage.apply_plan(plan).await?;
    if !expired_ids.is_empty() {
        // Rebuild the in-memory graph from the active edge set (port read first,
        // then take the write guard — no guard held across `.await`).
        let edges = ctx.storage.list_active_edges().await?;
        *graph.write() = MemoryGraph::from_active_edges(&edges);
    }

    Ok(stats)
}
