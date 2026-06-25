use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::engine::MemoryEngine;
use crate::engine::cycle::{
    CycleContext, CycleMetadata, CycleOutcome, CycleReport, SkipReason, TimeWindow,
};
use crate::error::{MemoryError, MigrationError, Result};
use crate::search::hybrid::{SearchQuery, SearchResult};
use crate::traits::{
    ConsolidationConfig, ConsolidationStats, EmbeddingProvider, ForgetPolicy, PruneStats,
    SummaryGenerator,
};
use crate::types::{Fact, NewFact, PromoteRequest, PromotionResult};

// Re-import trait types used in public API signatures
pub use crate::traits::{DreamCycle, InsightStream};
pub use crate::types::Insight;

/// Config key for the #209 caller-write cursor: the highest `facts.id` of a
/// caller-written fact observed at the last guarded cycle decision. A config value,
/// not schema (no migration). See [`MemoryEngine::run_dream_cycle_guarded`].
const CALLER_WRITE_CURSOR: &str = "last_caller_write_fact_id";

/// Top-level `metadata` key marking a fact captured via the pre-compaction insight flush.
///
/// Written by the MCP `memory_flush_insights` tool and read by
/// [`MemoryEngine::list_recent_insights`](crate::MemoryEngine::list_recent_insights).
/// Defined once here so the writer (MCP crate) and the reader (core) share a single
/// literal and cannot drift. The stamped value is an object (e.g.
/// `{"flushed_at": <rfc3339>}`); readers match on key *presence with a non-null value*.
pub const INSIGHT_MARKER_KEY: &str = "insight";

/// Capability-restricted handle passed to [`DreamCycle::run`].
///
/// Exposes only the operations a `DreamCycle` consumer needs:
/// - **Read**: query, list facts, get statistics
/// - **Engine-internal batch ops**: consolidation, forgetting (delegated through
///   existing engine methods with their own lock protocol)
/// - **Promotion**: the only new write path — atomic fact + lineage insertion
///
/// This prevents consumers from calling arbitrary engine mutations (which could
/// deadlock by re-acquiring write locks) while still giving access to the
/// pipeline operations the synthesis requires.
pub struct DreamContext<'a> {
    engine: &'a MemoryEngine,
}

impl<'a> DreamContext<'a> {
    pub(crate) const fn new(engine: &'a MemoryEngine) -> Self {
        Self { engine }
    }

    /// Run a hybrid query (FTS5 + vector + graph, RRF merge).
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub async fn query(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        self.engine.query(query).await
    }

    /// List all active (non-expired) facts, optionally limited.
    ///
    /// # Errors
    ///
    /// Returns an error if the store read fails.
    pub async fn list_active_facts(&self, limit: Option<usize>) -> Result<Vec<Fact>> {
        self.engine.list_active_facts(limit).await
    }

    /// Retrieve a single fact by ID.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if no fact with this ID exists.
    pub async fn get_fact(&self, id: i64) -> Result<Fact> {
        self.engine.get_fact(id).await
    }

    /// Run engine-internal consolidation (dedup → cluster → global summaries).
    ///
    /// Delegates to `MemoryEngine::consolidate()`, which manages its own locks.
    /// `generator` produces the summary text; `embedder` projects it into the
    /// fact vector space (issue #116). Both are `Arc<dyn _>` so the engine can
    /// offload the (possibly blocking) consumer calls off the async executor.
    ///
    /// # Errors
    ///
    /// Returns an error if consolidation fails.
    pub async fn consolidate(
        &self,
        generator: Arc<dyn SummaryGenerator>,
        embedder: Arc<dyn EmbeddingProvider>,
        config: &ConsolidationConfig,
    ) -> Result<ConsolidationStats> {
        self.engine.consolidate(generator, embedder, config).await
    }

    /// Run Ebbinghaus decay + pruning on stale facts.
    ///
    /// Delegates to `MemoryEngine::forget()`, which manages its own locks.
    ///
    /// # Errors
    ///
    /// Returns an error if the forget operation fails.
    pub async fn forget(&self, policy: &ForgetPolicy) -> Result<PruneStats> {
        self.engine.forget(policy).await
    }

    /// Atomically promote a fact to wisdom with lineage tracking.
    ///
    /// Inserts the promoted fact and its lineage record in a single `SQLite`
    /// transaction below the seam. The provenance envelope is serialized into the
    /// fact's metadata under the `"promotion_provenance"` key.
    ///
    /// # Errors
    ///
    /// Returns an error if the promotion fails (dimension mismatch, DB error, etc.).
    pub async fn promote(&self, req: &PromoteRequest) -> Result<PromotionResult> {
        self.engine.promote_with_lineage(req).await
    }

    /// List active facts in `window` that have not yet been dream-cycled.
    ///
    /// This is a cycle's input-selection query: the metadata `dream_cycle` marker
    /// excludes facts a previous cycle already processed (idempotency). Root scope,
    /// all fact types.
    ///
    /// # Errors
    ///
    /// Returns an error if the store read fails.
    pub async fn list_undreamt_in_period(&self, window: TimeWindow) -> Result<Vec<Fact>> {
        self.engine
            .storage
            .list_undreamt_facts_in_period(window.start, window.end, &[], None)
            .await
    }

    /// Aggregated outcome counts for a fact (for importance rescoring).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if the fact does not exist, or a store error.
    pub async fn outcome_counts(&self, fact_id: i64) -> Result<crate::types::OutcomeCounts> {
        self.engine.get_outcome_counts(fact_id).await
    }

    /// Aggregated outcome counts for many facts in a single query (batch rescoring).
    ///
    /// The batch form of [`Self::outcome_counts`] — one `GROUP BY` scan instead of
    /// one query per fact. Facts with no outcomes (or unknown ids) are absent from
    /// the map; callers treat a missing key as [`OutcomeCounts::default`](crate::types::OutcomeCounts).
    ///
    /// # Errors
    ///
    /// Returns a store error if the query fails.
    pub async fn outcome_counts_batch(
        &self,
        fact_ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, crate::types::OutcomeCounts>> {
        self.engine.get_outcome_counts_batch(fact_ids).await
    }
}

// --- MemoryEngine integration methods ---

impl MemoryEngine {
    /// Record a high-value insight via the provided `InsightStream`.
    ///
    /// Convenience method that validates the insight and delegates to
    /// `stream.record()`. The engine does not store any state from this call —
    /// the stream implementation decides how to persist the insight.
    ///
    /// # Errors
    ///
    /// Returns an error if the stream's `record()` fails.
    pub fn record_insight(&self, insight: Insight, stream: &dyn InsightStream) -> Result<()> {
        stream.record(insight)
    }

    /// Run a `DreamCycle`, returning the **unapplied** delta-based [`CycleReport`].
    ///
    /// Builds a retrieve-before-reflect [`CycleContext`] (prior wisdom + recent
    /// cycle history + the default `[last_dream_cycle_at, now)` window), delegates
    /// to `cycle.run()`, and returns its report. The report is **not** applied —
    /// the caller inspects it (the human review gate) and applies it via
    /// [`Self::apply_cycle_report`]. Verifies write access up front since applying
    /// will require it.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine is read-only.
    /// Returns an error if context construction or the cycle's `run()` fails.
    pub async fn run_dream_cycle(&self, cycle: &dyn DreamCycle) -> Result<CycleReport> {
        self.ensure_open()?;
        // Verify write access up front (apply happens separately).
        if self.read_only {
            return Err(MemoryError::ReadOnly);
        }
        let cycle_ctx = self.build_cycle_context().await?;
        cycle.run(&cycle_ctx).await
    }

    /// Run a `DreamCycle` **only if the caller has not written facts since the last
    /// decision** (#209) — the write/consolidate-race gate for the #554 harness, where
    /// fact-writes and the cycle can fire on the same trigger.
    ///
    /// On entry, under a single write-lock acquisition, this compares
    /// [`FactStore::max_caller_written_fact_id`] against the persisted cursor
    /// `last_caller_write_fact_id`:
    ///
    /// - **New caller writes** (`max > cursor`): advance the cursor to `max` and return
    ///   [`CycleOutcome::Skipped`] — the cycle stands down this invocation; the facts
    ///   stay un-dream-cycled for a later quiet run (deferral, not drop). Only the cursor
    ///   moves — never `last_dream_cycle_at` or the cycle history.
    /// - **No new caller writes** (`max <= cursor`, or no caller facts at all): delegate
    ///   to [`Self::run_dream_cycle`] and wrap the report as [`CycleOutcome::Ran`]. A real
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
    pub async fn run_dream_cycle_guarded(&self, cycle: &dyn DreamCycle) -> Result<CycleOutcome> {
        self.ensure_open()?;
        // Cursor read + max-id read + (skip-only) advance via the port. These are
        // separate port calls rather than one lock-held critical section: per the
        // deferral contract this is benign — a caller write landing between the reads
        // is attributed to the *next* invocation (never lost, never double-processed),
        // and concurrent guarded calls are idempotent via the marker + watermark. True
        // mutual exclusion is #207.
        let cursor =
            Self::parse_caller_write_cursor(self.storage.get_config(CALLER_WRITE_CURSOR).await?)?;
        // `None` (empty / fully-excluded table) ⇒ no caller writes ⇒ run.
        let max = self.storage.max_caller_written_fact_id().await?;
        let decision = match max {
            Some(max_id) if max_id > cursor => {
                self.storage
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
        let report = self.run_dream_cycle(cycle).await?;
        // Defend invariant M against a buggy consumer `DreamCycle` (the shipped
        // DefaultDreamCycle complies): a report that selected facts but left
        // `processed_ids` empty would leave those facts unmarked → the guarded cycle
        // defers forever. Reject loudly rather than silently livelock. A legitimately
        // quiet window (facts_selected == 0) is fine.
        if report.metadata.facts_selected > 0 && report.metadata.processed_ids.is_empty() {
            return Err(MemoryError::Cycle(
                crate::error::CycleError::MalformedReport {
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
    async fn build_cycle_context(&self) -> Result<CycleContext<'_>> {
        let now = Utc::now();
        // Prior wisdom = ALL active pinned facts (port read). The dream cycle
        // genuinely wants the full pinned set as prior wisdom, so it passes
        // `usize::MAX` (no cap) — the #395 cap is a resume-tier concern only.
        let prior_wisdom = self.storage.list_pinned_facts(&[], None).await?;
        // Watermark: the default window start.
        let start = match self.storage.get_config("last_dream_cycle_at").await? {
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
        let prior_reports = match self.storage.get_config("dream_cycle_history").await? {
            Some(s) => serde_json::from_str::<Vec<CycleMetadata>>(&s)?,
            None => Vec::new(),
        };
        let time_window = TimeWindow { start, end: now };
        Ok(CycleContext::new(
            DreamContext::new(self),
            prior_wisdom,
            prior_reports,
            time_window,
        ))
    }

    /// Atomic promotion: insert promoted fact + lineage record in one savepoint.
    ///
    /// Reuses `FactStore::insert()` (the same store-level helper used by
    /// `add_fact` and `add_facts_batch`) to avoid a divergent insert pipeline.
    /// Wraps fact insert + lineage insert in a single savepoint for atomicity,
    /// delegating the insert steps to [`Self::promote_in_conn`] so the standalone
    /// path and `apply_cycle_report`'s `Promote` delta share one pipeline.
    pub(crate) async fn promote_with_lineage(
        &self,
        req: &PromoteRequest,
    ) -> Result<PromotionResult> {
        // Validate embedding dimension up-front.
        if req.embedding.len() != self.embed_dim {
            return Err(MemoryError::EmbeddingDimension {
                expected: self.embed_dim,
                actual: req.embedding.len(),
            });
        }

        // Normalize metadata to an object + inject the provenance envelope (pure
        // engine-side work; the atomic insert + identity guard live below the seam).
        // `PromotionProvenance` carries only descriptive fields — the lineage table
        // (keyed by its row PK) is authoritative for the `lineage_id →
        // source_fact_ids` mapping, so no id leaks into the stored envelope.
        let mut metadata = match req.metadata.clone() {
            serde_json::Value::Object(map) => serde_json::Value::Object(map),
            _ => serde_json::json!({}),
        };
        if let serde_json::Value::Object(ref mut map) = metadata {
            map.insert(
                "promotion_provenance".to_owned(),
                serde_json::to_value(&req.provenance)
                    .map_err(|e| MemoryError::Internal(format!("serialize provenance: {e}")))?,
            );
        }

        let now = Utc::now();
        let new_fact = NewFact {
            content: req.content.clone(),
            content_hash: String::new(), // FactStore::insert computes this via blake3
            embedding: req.embedding.clone(),
            fact_type: req.fact_type,
            t_created: now,
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            base_importance: req.importance,
            access_count: 0,
            last_accessed: now,
            metadata,
            scope_id: 1,     // placeholder — promote_atomic patches it from scope_path
            is_pinned: true, // promoted wisdom is pinned (unforgettable)
        };

        // Atomic fact + lineage insert below the seam (identity guard + scope resolution
        // + HNSW notify all internal). Returns any new scope ids to cache.
        let (result, scope_ids_to_cache) = self
            .storage
            .promote_atomic(
                &new_fact,
                req.scope.as_deref(),
                &req.source_fact_ids,
                &req.provenance,
            )
            .await?;

        // Mirror any newly-created scope chain into the in-memory tree.
        for sid in scope_ids_to_cache {
            self.cache_scope_chain(sid).await?;
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::MemoryEngine;
    use crate::engine::cycle::{CycleContext, CycleMetadata, CycleReport, IdentityOutput};
    use crate::error::MemoryError;
    use crate::types::{FactType, Insight, PromoteRequest, PromotionProvenance};

    // --- Stub implementations ---

    struct RecordingStream {
        called: std::sync::atomic::AtomicBool,
    }

    impl RecordingStream {
        fn new() -> Self {
            Self {
                called: std::sync::atomic::AtomicBool::new(false),
            }
        }

        fn was_called(&self) -> bool {
            self.called.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    impl InsightStream for RecordingStream {
        fn record(&self, _insight: Insight) -> Result<()> {
            self.called
                .store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        }
    }

    struct NoopCycle;

    #[async_trait::async_trait]
    impl DreamCycle for NoopCycle {
        async fn run(&self, ctx: &CycleContext<'_>) -> Result<CycleReport> {
            Ok(CycleReport {
                deltas: vec![],
                identity: IdentityOutput::empty(),
                metadata: CycleMetadata {
                    cycle_id: 0,
                    ran_at: Utc::now(),
                    time_window: ctx.time_window(),
                    facts_selected: 0,
                    method_version: "noop".into(),
                    processed_ids: vec![],
                },
            })
        }
    }

    fn stub_provenance() -> PromotionProvenance {
        use chrono::Utc;
        let now = Utc::now();
        PromotionProvenance {
            source_count: 3,
            session_count: 2,
            date_range_start: now,
            date_range_end: now,
            confidence: 0.85,
            method_version: "test-v1".into(),
            representative_ids: vec![1, 2, 3],
        }
    }

    /// Helper: add source facts so that lineage validation passes.
    async fn add_source_facts(engine: &MemoryEngine, ids: &[i64]) -> Vec<i64> {
        use crate::traits::EmbeddingProvider;
        use crate::types::AddFactRequest;

        struct FixedEmbed;
        impl EmbeddingProvider for FixedEmbed {
            fn embed(&self, _text: &str) -> Result<Vec<f32>> {
                Ok(vec![0.1, 0.2, 0.3, 0.4])
            }

            fn fingerprint(&self) -> crate::types::EmbeddingFingerprint {
                crate::types::EmbeddingFingerprint::new("mock", "test", 4)
            }
        }

        let mut actual_ids = Vec::new();
        for _ in ids {
            let req = AddFactRequest {
                content: "source fact".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            };
            actual_ids.push(
                engine
                    .add_fact(
                        &req,
                        std::sync::Arc::new(FixedEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                        None,
                    )
                    .await
                    .unwrap(),
            );
        }
        actual_ids
    }

    // --- Tests ---

    #[tokio::test]
    async fn record_insight_delegates_to_stream() {
        let engine = MemoryEngine::builder(4).build().unwrap();
        let stream = RecordingStream::new();
        let insight = Insight {
            content: "test insight".into(),
            fact_type: FactType::Semantic,
            importance: Some(0.8),
            metadata: None,
            scope: None,
        };

        engine.record_insight(insight, &stream).unwrap();
        assert!(
            stream.was_called(),
            "stream.record() should have been called"
        );
    }

    #[tokio::test]
    async fn run_dream_cycle_creates_context_and_delegates() {
        let engine = MemoryEngine::builder(4).build().unwrap();
        let cycle = NoopCycle;

        let report = engine.run_dream_cycle(&cycle).await.unwrap();
        assert!(report.deltas.is_empty());
        assert_eq!(report.metadata.method_version, "noop");
    }

    /// A `DreamCycle` that violates the `processed_ids` contract: claims it selected
    /// facts but returns none. Used to prove the T4b guard rejects it.
    struct SelectingButEmptyCycle;
    #[async_trait::async_trait]
    impl DreamCycle for SelectingButEmptyCycle {
        async fn run(&self, ctx: &CycleContext<'_>) -> Result<CycleReport> {
            Ok(CycleReport {
                deltas: vec![],
                identity: IdentityOutput::empty(),
                metadata: CycleMetadata {
                    cycle_id: 0,
                    ran_at: Utc::now(),
                    time_window: ctx.time_window(),
                    facts_selected: 5, // claims work...
                    method_version: "bad".into(),
                    processed_ids: vec![], // ...but marks nothing → contract violation
                },
            })
        }
    }

    async fn caller_cursor(engine: &MemoryEngine) -> Option<String> {
        engine.get_config(CALLER_WRITE_CURSOR).await.unwrap()
    }

    /// #209 (a): a caller write since the cursor ⇒ Skipped, cursor advanced to the new
    /// max, and the dream-cycle watermark is left untouched (skip ≠ run).
    #[tokio::test]
    async fn guarded_skips_and_advances_cursor_when_caller_wrote() {
        let engine = MemoryEngine::builder(4).build().unwrap();
        let ids = add_source_facts(&engine, &[0]).await; // one caller fact; cursor starts absent (=0)
        let max_id = ids[0];

        let outcome = engine.run_dream_cycle_guarded(&NoopCycle).await.unwrap();
        match outcome {
            CycleOutcome::Skipped(SkipReason::CallerWroteFacts {
                since_fact_id,
                new_max_fact_id,
            }) => {
                assert_eq!(since_fact_id, 0, "cursor was absent ⇒ 0");
                assert_eq!(new_max_fact_id, max_id);
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
        assert_eq!(
            caller_cursor(&engine).await.as_deref(),
            Some(max_id.to_string().as_str())
        );
        // Skip must NOT advance the dream-cycle watermark (only a real apply does).
        let wm = engine.get_config("last_dream_cycle_at").await.unwrap();
        assert!(wm.is_none(), "skip must not touch last_dream_cycle_at");
    }

    /// #209 (b)+(c): cold start on a populated store skips ONCE (advancing the cursor),
    /// then a second invocation with no new caller writes RUNS.
    #[tokio::test]
    async fn guarded_skip_once_then_runs_when_quiet() {
        let engine = MemoryEngine::builder(4).build().unwrap();
        add_source_facts(&engine, &[0, 0, 0]).await; // 3 pre-existing caller facts, cursor absent

        // First: caller writes detected ⇒ skip, cursor advanced.
        assert!(matches!(
            engine.run_dream_cycle_guarded(&NoopCycle).await.unwrap(),
            CycleOutcome::Skipped(_)
        ));
        let cursor_after_skip = caller_cursor(&engine).await;
        assert!(cursor_after_skip.is_some(), "skip advanced the cursor");

        // Second: no new writes since the advanced cursor ⇒ run.
        assert!(matches!(
            engine.run_dream_cycle_guarded(&NoopCycle).await.unwrap(),
            CycleOutcome::Ran(_)
        ));
        // A real run must NOT advance the cursor (the dream-marker, not the cursor,
        // removes processed facts from the signal). NoopCycle applies nothing, so the
        // cursor is exactly where the skip left it.
        assert_eq!(
            caller_cursor(&engine).await,
            cursor_after_skip,
            "a run leaves the caller-write cursor unchanged"
        );
        // Third: still quiet ⇒ runs again (steady state on a quiet store).
        assert!(matches!(
            engine.run_dream_cycle_guarded(&NoopCycle).await.unwrap(),
            CycleOutcome::Ran(_)
        ));
    }

    /// #209: an empty store (no caller facts at all) ⇒ `max_caller_written_fact_id` is
    /// `None` ⇒ the guard runs immediately (None treated as "no caller writes").
    #[tokio::test]
    async fn guarded_runs_on_empty_store() {
        let engine = MemoryEngine::builder(4).build().unwrap();
        assert!(matches!(
            engine.run_dream_cycle_guarded(&NoopCycle).await.unwrap(),
            CycleOutcome::Ran(_)
        ));
    }

    /// T4b guard: a cycle that selected facts but returned empty `processed_ids` is
    /// rejected (else those facts would never be dream-marked → perpetual skip).
    #[tokio::test]
    async fn guarded_rejects_malformed_report() {
        let engine = MemoryEngine::builder(4).build().unwrap();
        let err = engine
            .run_dream_cycle_guarded(&SelectingButEmptyCycle)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                MemoryError::Cycle(crate::error::CycleError::MalformedReport { facts_selected: 5 })
            ),
            "expected MalformedReport, got {err:?}"
        );
    }

    /// The malformed-report guard must also fire in the steady-state path: after a
    /// caller write defers the first call, the SECOND (quiet) call actually runs the
    /// cycle — and a contract-violating cycle is caught there, not just on cold start.
    #[tokio::test]
    async fn guarded_rejects_malformed_report_after_initial_skip() {
        let engine = MemoryEngine::builder(4).build().unwrap();
        add_source_facts(&engine, &[0]).await; // caller write ⇒ first call defers
        assert!(matches!(
            engine
                .run_dream_cycle_guarded(&SelectingButEmptyCycle)
                .await
                .unwrap(),
            CycleOutcome::Skipped(_)
        ));
        // Second call: quiet ⇒ runs the cycle ⇒ contract violation is caught.
        let err = engine
            .run_dream_cycle_guarded(&SelectingButEmptyCycle)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                MemoryError::Cycle(crate::error::CycleError::MalformedReport { facts_selected: 5 })
            ),
            "expected MalformedReport on the post-skip run, got {err:?}"
        );
    }

    #[tokio::test]
    async fn promote_with_lineage_rejects_wrong_dimension() {
        let engine = MemoryEngine::builder(4).build().unwrap();
        let req = PromoteRequest {
            content: "promoted wisdom".into(),
            fact_type: FactType::Semantic,
            embedding: vec![0.1; 8], // wrong dim (engine expects 4)
            importance: 0.9,
            metadata: serde_json::json!({}),
            scope: None,
            source_fact_ids: vec![1, 2, 3],
            provenance: stub_provenance(),
        };

        let err = engine.promote_with_lineage(&req).await.unwrap_err();
        assert!(
            matches!(
                err,
                MemoryError::EmbeddingDimension {
                    expected: 4,
                    actual: 8
                }
            ),
            "expected EmbeddingDimension, got: {err}"
        );
    }

    #[tokio::test]
    async fn promote_into_unstamped_store_is_rejected() {
        // `promote` is public and inserts a PRE-COMPUTED vector with no live
        // embedder, so it can be the literal first write on a fresh store. #613
        // cannot stamp the identity here, so it guards: promotion into a store with
        // no recorded `embedding_meta` is rejected (Codex review HIGH). After a real
        // embedding write establishes the identity, promotion succeeds.
        let engine = MemoryEngine::builder(4).build().unwrap();
        let req = PromoteRequest {
            content: "promoted wisdom".into(),
            fact_type: FactType::Semantic,
            embedding: vec![0.1, 0.2, 0.3, 0.4], // correct dim — dim check passes
            importance: 0.9,
            metadata: serde_json::json!({}),
            scope: None,
            source_fact_ids: vec![],
            provenance: stub_provenance(),
        };

        let err = engine.promote_with_lineage(&req).await.unwrap_err();
        assert!(
            matches!(err, MemoryError::Internal(ref m) if m.contains("no embedding identity")),
            "expected the identity guard error, got: {err:?}"
        );

        // add_source_facts embeds via add_fact, which records the identity at dim 4.
        let source_ids = add_source_facts(&engine, &[1, 2]).await;
        let ok_req = PromoteRequest {
            source_fact_ids: source_ids,
            ..req
        };
        assert!(
            engine.promote_with_lineage(&ok_req).await.is_ok(),
            "promotion succeeds once the store has an embedding identity"
        );
    }

    #[tokio::test]
    async fn promote_with_lineage_atomic_insert() {
        let engine = MemoryEngine::builder(4).build().unwrap();
        let source_ids = add_source_facts(&engine, &[1, 2, 3]).await;

        let req = PromoteRequest {
            content: "User prefers terse responses".into(),
            fact_type: FactType::Semantic,
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            importance: 0.9,
            metadata: serde_json::json!({}),
            scope: None,
            source_fact_ids: source_ids,
            provenance: stub_provenance(),
        };

        let result = engine.promote_with_lineage(&req).await.unwrap();
        assert!(result.fact_id > 0, "fact_id should be assigned");
        assert!(result.lineage_id > 0, "lineage_id should be assigned");

        // Verify fact was inserted
        let fact = engine.get_fact(result.fact_id).await.unwrap();
        assert_eq!(fact.content, "User prefers terse responses");
        assert!(fact.is_pinned, "promoted fact should be pinned");

        // Verify provenance in metadata
        let prov = fact.metadata.get("promotion_provenance");
        assert!(
            prov.is_some(),
            "metadata should contain promotion_provenance"
        );
    }

    #[tokio::test]
    async fn promote_with_lineage_preserves_existing_metadata() {
        let engine = MemoryEngine::builder(4).build().unwrap();
        let source_ids = add_source_facts(&engine, &[1, 2]).await;

        let req = PromoteRequest {
            content: "wisdom fact".into(),
            fact_type: FactType::Semantic,
            embedding: vec![1.0, 0.0, 0.0, 0.0],
            importance: 0.95,
            metadata: serde_json::json!({"existing": "data"}),
            scope: None,
            source_fact_ids: source_ids.clone(),
            provenance: stub_provenance(),
        };

        let result = engine.promote_with_lineage(&req).await.unwrap();

        // Verify existing metadata preserved alongside provenance
        let fact = engine.get_fact(result.fact_id).await.unwrap();
        assert_eq!(fact.metadata["existing"], "data");
        assert!(fact.metadata.get("promotion_provenance").is_some());

        // Verify lineage record via wisdom_fact_id lookup
        let ids = engine
            .storage()
            .get_lineage_source_fact_ids(result.fact_id)
            .await
            .unwrap();
        assert_eq!(ids, source_ids);
    }
}
