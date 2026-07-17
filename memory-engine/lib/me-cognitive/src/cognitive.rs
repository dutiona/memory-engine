//! Dream-cycle orchestration: [`run_dream_cycle`] / [`run_dream_cycle_guarded`] /
//! [`build_cycle_context`], extracted verbatim from the facade's
//! `MemoryEngine::{run_dream_cycle, run_dream_cycle_guarded, build_cycle_context}`
//! (Wave 2 #816 / S5, sub-PR 2, closes #981) as free functions over
//! [`MemoryCtx`] + a `&dyn DreamCtx` capability handle.
//!
//! The facade keeps thin delegates with their own rustdoc
//! (`memory-engine/src/engine/cognitive.rs`); this module owns no facade type. **No
//! unit tests live here** — mirroring `me_consolidate::consolidate` (also an
//! orchestration-only free function with no direct unit tests in that crate): the
//! logic is a thin sequencing of already-tested pieces (`build_cycle_context`'s reads,
//! `DreamCycle::run`, the cursor arithmetic), and its *integration* — the facade
//! wiring `MemoryEngine::run_dream_cycle`/`run_dream_cycle_guarded` delegate to this —
//! is proven by the facade's own `engine/cognitive.rs` test module, unchanged by this
//! carve.

use chrono::{DateTime, Utc};

use me_traits::{DreamCtx, DreamCycle};
use me_types::error::{MemoryError, MigrationError, Result};
use me_types::types::cycle_report::{CycleMetadata, CycleOutcome, CycleReport, SkipReason};

use me_storage::MemoryCtx;

use crate::context::CycleContext;

/// Config key for the #209 caller-write cursor: the highest `facts.id` of a
/// caller-written fact observed at the last guarded cycle decision.
///
/// A config value, not schema (no migration). See [`run_dream_cycle_guarded`].
///
/// `pub` (not crate-private) solely so the facade's own `engine/cognitive.rs` test
/// module — which still constructs a real `MemoryEngine` and asserts on this exact
/// config key — can reach it via the `crate::cognitive` alias instead of duplicating
/// the literal (a duplicated literal is a silent-drift hazard: a typo in either copy
/// breaks the #209 guard test without a compile error). Not flat-re-exported into the
/// facade's own public API.
pub const CALLER_WRITE_CURSOR: &str = "last_caller_write_fact_id";

/// Top-level `metadata` key marking a fact captured via the pre-compaction insight flush.
///
/// Written by the MCP `memory_flush_insights` tool and read by the facade's
/// `MemoryEngine::list_recent_insights`. Defined once here so the writer (MCP crate)
/// and the reader (facade) share a single literal and cannot drift. The stamped value
/// is an object (e.g. `{"flushed_at": <rfc3339>}`); readers match on key *presence
/// with a non-null value*.
pub const INSIGHT_MARKER_KEY: &str = "insight";

/// Run a `DreamCycle`, returning the **unapplied** delta-based [`CycleReport`].
///
/// Builds a retrieve-before-reflect [`CycleContext`] (prior wisdom + recent cycle
/// history + the default `[last_dream_cycle_at, now)` window), delegates to
/// `cycle.run()`, and returns its report. The report is **not** applied — the caller
/// inspects it (the human review gate) and applies it via
/// [`crate::apply_cycle_report`]. Verifies write access up front since applying will
/// require it.
///
/// # Errors
///
/// Returns `MemoryError::ReadOnly` if the engine is read-only.
/// Returns an error if context construction or the cycle's `run()` fails.
pub async fn run_dream_cycle<'a>(
    ctx: MemoryCtx<'a>,
    dream: &'a dyn DreamCtx,
    cycle: &dyn DreamCycle,
) -> Result<CycleReport> {
    ctx.ensure_open()?;
    // Verify write access up front (apply happens separately).
    if ctx.read_only {
        return Err(MemoryError::ReadOnly);
    }
    let cycle_ctx = build_cycle_context(ctx, dream).await?;
    cycle.run(&cycle_ctx).await
}

/// Run a `DreamCycle` **only if the caller has not written facts since the last
/// decision** (#209).
///
/// This is the write/consolidate-race gate for the #554 harness, where fact-writes
/// and the cycle can fire on the same trigger.
///
/// On entry, under a single write-lock acquisition, this compares
/// `FactStore::max_caller_written_fact_id` against the persisted cursor
/// `last_caller_write_fact_id`:
///
/// - **New caller writes** (`max > cursor`): advance the cursor to `max` and return
///   [`CycleOutcome::Skipped`] — the cycle stands down this invocation; the facts
///   stay un-dream-cycled for a later quiet run (deferral, not drop). Only the cursor
///   moves — never `last_dream_cycle_at` or the cycle history.
/// - **No new caller writes** (`max <= cursor`, or no caller facts at all): delegate
///   to [`run_dream_cycle`] and wrap the report as [`CycleOutcome::Ran`]. A real
///   run does not advance the cursor; the `dream_cycle` marker (invariant M) is what
///   removes processed facts from the signal, so a quiet re-run runs again only when
///   genuinely new caller writes arrive.
///
/// **Concurrency:** the cursor read+advance is atomic w.r.t. other writers (the
/// write lock), but the lock is released before the cycle runs (so a consumer cycle's
/// work does not serialize all writers). A write landing during the run is attributed
/// to the *next* invocation — never lost, never double-processed. This is deferral,
/// **not** mutual exclusion; concurrent guarded calls can both run (idempotent via the
/// marker + watermark). True mutual exclusion is #207.
///
/// # Errors
///
/// Returns `MemoryError::ReadOnly` if the engine is read-only, or a store/cycle error.
#[must_use = "the CycleOutcome carries the skip/run decision — a dropped Skipped silently loses the deferral"]
pub async fn run_dream_cycle_guarded<'a>(
    ctx: MemoryCtx<'a>,
    dream: &'a dyn DreamCtx,
    cycle: &dyn DreamCycle,
) -> Result<CycleOutcome> {
    ctx.ensure_open()?;
    // Cursor read + max-id read + (skip-only) advance via the port. These are
    // separate port calls rather than one lock-held critical section: per the
    // deferral contract this is benign — a caller write landing between the reads
    // is attributed to the *next* invocation (never lost, never double-processed),
    // and concurrent guarded calls are idempotent via the marker + watermark. True
    // mutual exclusion is #207.
    let cursor = parse_caller_write_cursor(ctx.storage.get_config(CALLER_WRITE_CURSOR).await?)?;
    // `None` (empty / fully-excluded table) ⇒ no caller writes ⇒ run.
    let max = ctx.storage.max_caller_written_fact_id().await?;
    let decision = match max {
        Some(max_id) if max_id > cursor => {
            ctx.storage
                .set_config(CALLER_WRITE_CURSOR, &max_id.to_string())
                .await?;
            Some(SkipReason::CallerWroteFacts {
                since_fact_id: cursor,
                new_max_fact_id: max_id,
            })
        }
        _ => None,
    };

    // No new caller writes — run.
    if let Some(reason) = decision {
        return Ok(CycleOutcome::Skipped(reason));
    }
    let report = run_dream_cycle(ctx, dream, cycle).await?;
    // Defend invariant M against a buggy consumer `DreamCycle` (the shipped
    // DefaultDreamCycle complies): a report that selected facts but left
    // `processed_ids` empty would leave those facts unmarked → the guarded cycle
    // defers forever. Reject loudly rather than silently livelock. A legitimately
    // quiet window (facts_selected == 0) is fine.
    if report.metadata.facts_selected > 0 && report.metadata.processed_ids.is_empty() {
        return Err(MemoryError::Cycle(
            me_types::error::CycleError::MalformedReport {
                facts_selected: report.metadata.facts_selected,
            },
        ));
    }
    Ok(CycleOutcome::Ran(report))
}

/// Parse the #209 caller-write cursor (`last_caller_write_fact_id`) from its
/// config string; absent ⇒ `0`. A config key, not schema — no migration.
fn parse_caller_write_cursor(raw: Option<String>) -> Result<i64> {
    raw.map_or(Ok(0), |s| {
        s.parse::<i64>().map_err(|e| {
            MemoryError::Migration(MigrationError::Incompatible(format!(
                "invalid {CALLER_WRITE_CURSOR}: {e}"
            )))
        })
    })
}

/// Build the retrieve-before-reflect context for a cycle: prior wisdom (active
/// pinned facts), the recent cycle-metadata history, and the default time
/// window `[last_dream_cycle_at, now)`.
async fn build_cycle_context<'a>(
    ctx: MemoryCtx<'a>,
    dream: &'a dyn DreamCtx,
) -> Result<CycleContext<'a>> {
    let now = Utc::now();
    // Prior wisdom = ALL active pinned facts (port read). The dream cycle
    // genuinely wants the full pinned set as prior wisdom, so it passes
    // `usize::MAX` (no cap) — the #395 cap is a resume-tier concern only.
    let prior_wisdom = ctx.storage.list_pinned_facts(&[], None).await?;
    // Watermark: the default window start.
    let start = match ctx.storage.get_config("last_dream_cycle_at").await? {
        Some(s) => DateTime::parse_from_rfc3339(&s)
            .map_err(|e| {
                MemoryError::Migration(MigrationError::Incompatible(format!(
                    "invalid last_dream_cycle_at: {e}"
                )))
            })?
            .with_timezone(&Utc),
        None => DateTime::from_timestamp(0, 0).expect("unix epoch is a valid timestamp"),
    };
    // Recent cycle-metadata history ring.
    let prior_reports = match ctx.storage.get_config("dream_cycle_history").await? {
        Some(s) => serde_json::from_str::<Vec<CycleMetadata>>(&s)?,
        None => Vec::new(),
    };
    let time_window = me_types::types::cycle_report::TimeWindow { start, end: now };
    Ok(CycleContext::new(
        dream,
        prior_wisdom,
        prior_reports,
        time_window,
    ))
}
