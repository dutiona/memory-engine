//! `MemoryEngine`'s Phase-5a cognitive-pipeline surface.
//!
//! The dream-cycle *subsystem* (`CycleContext`, `DefaultDreamCycle`, `LlmDreamCycle`,
//! `apply_cycle_report`'s validate-then-apply body, the `run_dream_cycle`/
//! `run_dream_cycle_guarded` orchestration) was carved into the [`me_cognitive`] crate
//! in Wave 2 #816 / S5 (closes #981) — see that crate's root doc for the full history
//! (ADR 0014 decision #3 → S1 regression → S5 restoration). What stays here:
//!
//! - **`EngineDreamCtx`** — a *private* borrow-newtype over `&MemoryEngine` carrying the
//!   9 capability delegates of [`me_traits::DreamCtx`]. Deliberately **not**
//!   `impl DreamCtx for MemoryEngine`: five of the trait's method names collide with
//!   inherent `MemoryEngine` methods, and a direct impl would silently turn any future
//!   rename of one into unbounded recursion. See the type's own doc — the newtype makes
//!   that failure *unrepresentable* rather than merely discouraged.
//! - **`promote_with_lineage`** — caches newly-resolved scope ids into the engine's
//!   in-memory `ScopeTree`, an engine-owned cache `MemoryCtx` does not carry (ADR 0018
//!   decision #3: `scope_tree` is a loose per-primitive parameter, not universal).
//! - **Four thin delegates** (`record_insight`, `run_dream_cycle`,
//!   `run_dream_cycle_guarded`, `apply_cycle_report`) forwarding to
//!   [`me_cognitive`] via the `crate::cognitive` alias (the `pool`/`store`/`graph`/
//!   `scope`/`archive`/`forgetting` convention).

use std::sync::Arc;

use chrono::Utc;

use crate::engine::MemoryEngine;
use crate::error::{MemoryError, Result};
use crate::forgetting::{ForgetPolicy, PruneStats};
use crate::traits::{
    ConsolidationConfig, ConsolidationStats, DreamCtx, EmbeddingProvider, SummaryGenerator,
};
use crate::types::search::{SearchQuery, SearchResult};
use crate::types::{Fact, NewFact, OutcomeCounts, PromoteRequest, PromotionResult};

// Re-import trait/type names used in public API signatures — unchanged re-exports.
pub use crate::traits::{DreamCycle, InsightStream};
pub use crate::types::Insight;

/// `MemoryEngine`'s `DreamCtx` implementation — the capability bag restored to the
/// trait layer by Wave 2 #816 / S5 (closes #981; see [`me_cognitive`]'s crate doc for
/// the full ADR 0014 → S1-regression → S5-restoration history).
///
/// # ⚠️ The recursion trap
///
/// **Seven of these nine method names collide with existing inherent `MemoryEngine`
/// methods** (`query`, `consolidate`, `forget`, `get_fact`, `list_active_facts`,
/// `outcome_counts`, and — nominally, though `MemoryEngine` carries no inherent of
/// that exact name today — `list_undreamt_in_period`). Inside this `impl` block,
/// writing `self.query(q).await` resolves to the **inherent** method today (Rust
/// prefers inherent over trait) — correct *now*, but becomes **silent infinite
/// recursion → stack overflow** the instant that inherent method is ever renamed or
/// removed. Every body below therefore uses a **fully-qualified**
/// `Self::query(self, q).await` so the compiler can never re-resolve it to this trait
/// method — verified empirically that `Self::` resolves identically to the literal
/// type name here (`Self` is the concrete `MemoryEngine`, not the trait; the
/// inherent-over-trait priority applies the same to either spelling). `Self::`, not
/// `MemoryEngine::`, is required by `clippy::use_self` (part of the workspace's
/// `-D warnings` pedantic gate) — both are equally safe against the trap; only the
/// spelling differs. The two names that do **not** collide (`promote` →
/// `promote_with_lineage`, `outcome_counts_batch` → `get_outcome_counts_batch`) are
/// qualified anyway, for uniformity.
/// The engine's [`DreamCtx`] adapter — a private borrow-newtype over `&MemoryEngine`.
///
/// # Why this is a newtype and **not** `impl DreamCtx for MemoryEngine`
///
/// Five of `DreamCtx`'s nine methods share a name with an inherent `MemoryEngine`
/// method (`query`, `list_active_facts`, `get_fact`, `consolidate`, `forget`). Had
/// `MemoryEngine` implemented `DreamCtx` directly, each impl body would have to call
/// the same-named inherent method on `self` — and Rust resolves inherent-before-trait,
/// so it works *only for as long as the inherent method keeps its name*. Rename or
/// remove one, and the call silently re-resolves to **the trait method being defined**:
/// unbounded recursion, stack overflow, in the **consumer's** process.
///
/// That failure is invisible to every gate this repo has. Verified empirically:
/// `rustc`'s `unconditional_recursion` lint **does not fire through `#[async_trait]`**
/// (the recursive call sits inside the desugared `Box::pin(async move { … })`, not in
/// the fn's own CFG), so the workspace's `-D warnings` gate compiles it clean. No
/// qualification syntax helps either — `Self::query(self, q)` and `self.query(q)` share
/// the *same* resolution order; qualifying is cosmetic, not protective.
///
/// So the invariant is made **structural instead of conventional**: `MemoryEngine` does
/// not implement `DreamCtx` at all. Inside the impl below, `self.0` is a plain
/// `&MemoryEngine` with **no `DreamCtx` impl in scope for it**, so `self.0.query(q)` can
/// only ever resolve to the inherent method. Rename that method and this stops compiling
/// (`E0599: no method named 'query'`) — a hard error at the exact call site, not a
/// silent runtime catastrophe. Recursion here is not discouraged; it is unrepresentable.
///
/// (This is, in effect, the old `pub` `DreamContext` restored to its proper form: its
/// *narrowing* job was always correct — what was wrong was that it was public,
/// unreachable, and stranded at L4. As a private adapter implementing an L0.5 trait, it
/// is exactly what it should have been.)
// NB: `pub` (not `pub(crate)`) only to satisfy `clippy::redundant_pub_crate` — the
// enclosing `engine::cognitive` module is private and this type is never re-exported
// from `lib.rs`, so it is **not** part of the public API. The `reexports_are_accessible`
// probe + the S5-3 reachable-path ratchet (#941) are what actually hold that line.
pub struct EngineDreamCtx<'a>(pub(crate) &'a MemoryEngine);

#[async_trait::async_trait]
impl DreamCtx for EngineDreamCtx<'_> {
    async fn query(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        self.0.query(query).await
    }

    async fn list_active_facts(&self, limit: Option<usize>) -> Result<Vec<Fact>> {
        self.0.list_active_facts(limit).await
    }

    async fn get_fact(&self, id: i64) -> Result<Fact> {
        self.0.get_fact(id).await
    }

    async fn consolidate(
        &self,
        generator: Arc<dyn SummaryGenerator>,
        embedder: Arc<dyn EmbeddingProvider>,
        config: &ConsolidationConfig,
    ) -> Result<ConsolidationStats> {
        self.0.consolidate(generator, embedder, config).await
    }

    async fn forget(&self, policy: &ForgetPolicy) -> Result<PruneStats> {
        self.0.forget(policy).await
    }

    async fn promote(&self, req: &PromoteRequest) -> Result<PromotionResult> {
        self.0.promote_with_lineage(req).await
    }

    async fn list_undreamt_in_period(
        &self,
        window: crate::cognitive::TimeWindow,
    ) -> Result<Vec<Fact>> {
        // Mirrors the pre-carve `DreamContext::list_undreamt_in_period` body exactly:
        // there is no inherent `MemoryEngine` method of this name to delegate to.
        self.0
            .storage
            .list_undreamt_facts_in_period(window.start, window.end, &[], None)
            .await
    }

    async fn outcome_counts(&self, fact_id: i64) -> Result<OutcomeCounts> {
        self.0.get_outcome_counts(fact_id).await
    }

    async fn outcome_counts_batch(
        &self,
        fact_ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, OutcomeCounts>> {
        self.0.get_outcome_counts_batch(fact_ids).await
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

    /// Run a `DreamCycle`, returning the **unapplied** delta-based [`CycleReport`](crate::cognitive::CycleReport).
    ///
    /// Builds a retrieve-before-reflect [`CycleContext`](crate::cognitive::CycleContext)
    /// (prior wisdom + recent cycle history + the default `[last_dream_cycle_at, now)`
    /// window), delegates
    /// to `cycle.run()`, and returns its report. The report is **not** applied —
    /// the caller inspects it (the human review gate) and applies it via
    /// [`Self::apply_cycle_report`]. Verifies write access up front since applying
    /// will require it.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine is read-only.
    /// Returns an error if context construction or the cycle's `run()` fails.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_engine::{
    ///     AddFactRequest, DefaultDreamCycle, EmbeddingFingerprint, EmbeddingProvider, FactType,
    ///     MemoryEngine, MemoryError,
    /// };
    ///
    /// // A trivial embedder (the consumer normally injects a real one).
    /// struct Embed;
    /// impl EmbeddingProvider for Embed {
    ///     fn embed(&self, _text: &str) -> Result<Vec<f32>, MemoryError> {
    ///         Ok(vec![1.0, 0.0])
    ///     }
    ///     fn fingerprint(&self) -> EmbeddingFingerprint {
    ///         EmbeddingFingerprint::new("mock", "test", 2)
    ///     }
    /// }
    ///
    /// // The engine API is async (#631); a consumer binary uses `#[tokio::main]`.
    /// tokio::runtime::Runtime::new().unwrap().block_on(async {
    ///     let engine = MemoryEngine::builder(2).build()?;
    ///     let embedder: std::sync::Arc<dyn EmbeddingProvider> = std::sync::Arc::new(Embed);
    ///     for i in 0..3 {
    ///         let req = AddFactRequest {
    ///             content: format!("recurring pattern {i}"),
    ///             fact_type: FactType::Semantic,
    ///             source_event_id: None,
    ///             scope: None,
    ///             opts: None,
    ///         };
    ///         engine.add_fact(&req, embedder.clone(), None).await?;
    ///     }
    ///
    ///     // Produce an unapplied report (the review gate), then apply it atomically.
    ///     let report = engine.run_dream_cycle(&DefaultDreamCycle::with_defaults()).await?;
    ///     assert_eq!(report.metadata.facts_selected, 3);
    ///     let applied = engine.apply_cycle_report(&report).await?;
    ///     assert_eq!(applied.promoted, 1); // the three-fact cluster promotes one representative
    ///     Ok::<(), MemoryError>(())
    /// })
    /// .unwrap();
    /// ```
    pub async fn run_dream_cycle(
        &self,
        cycle: &dyn DreamCycle,
    ) -> Result<crate::cognitive::CycleReport> {
        crate::cognitive::run_dream_cycle(self.mem_ctx(), &EngineDreamCtx(self), cycle).await
    }

    /// Run a `DreamCycle` **only if the caller has not written facts since the last
    /// decision** (#209) — the write/consolidate-race gate for the #554 harness, where
    /// fact-writes and the cycle can fire on the same trigger.
    ///
    /// On entry, under a single write-lock acquisition, this compares
    /// `FactStore::max_caller_written_fact_id` against the persisted cursor
    /// `last_caller_write_fact_id`:
    ///
    /// - **New caller writes** (`max > cursor`): advance the cursor to `max` and return
    ///   [`CycleOutcome::Skipped`](crate::cognitive::CycleOutcome::Skipped) — the cycle
    ///   stands down this invocation; the facts stay un-dream-cycled for a later quiet
    ///   run (deferral, not drop). Only the cursor moves — never `last_dream_cycle_at`
    ///   or the cycle history.
    /// - **No new caller writes** (`max <= cursor`, or no caller facts at all): delegate
    ///   to [`Self::run_dream_cycle`] and wrap the report as
    ///   [`CycleOutcome::Ran`](crate::cognitive::CycleOutcome::Ran). A real run does not
    ///   advance the cursor; the `dream_cycle` marker (invariant M) is what removes
    ///   processed facts from the signal, so a quiet re-run runs again only when
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
    pub async fn run_dream_cycle_guarded(
        &self,
        cycle: &dyn DreamCycle,
    ) -> Result<crate::cognitive::CycleOutcome> {
        crate::cognitive::run_dream_cycle_guarded(self.mem_ctx(), &EngineDreamCtx(self), cycle)
            .await
    }

    /// Validate and apply a [`CycleReport`](crate::cognitive::CycleReport) atomically.
    ///
    /// The full validation + delta dispatch + dream-marking + watermark/history update
    /// is one transaction below the seam
    /// ([`ConsolidationStore::apply_cycle_deltas_atomic`](crate::storage::ConsolidationStore::apply_cycle_deltas_atomic)),
    /// which also fires the post-commit HNSW notify (Stage B). If any delta fails
    /// validation the store is left **unchanged**. The engine consumes only the returned
    /// supersede edges, mirroring them into its in-memory graph.
    ///
    /// Concurrency note: this is single-fire safe (a sequential re-run is a near no-op
    /// via the marker + watermark). Mutual exclusion against a concurrent writer is out
    /// of scope here — see #207 / #209.
    ///
    /// # Errors
    ///
    /// - [`MemoryError::ReadOnly`] if the engine is read-only.
    /// - [`MemoryError::Cycle`](crate::error::MemoryError::Cycle) if any delta fails validation.
    /// - [`MemoryError::EmbeddingDimension`] if an
    ///   `AddFact`/`Promote`/`Synthesize` embedding does not match the engine dimension.
    /// - [`MemoryError::Storage`](crate::error::MemoryError::Storage) on a backend failure.
    pub async fn apply_cycle_report(
        &self,
        report: &crate::cognitive::CycleReport,
    ) -> Result<crate::cognitive::ApplyResult> {
        crate::cognitive::apply_cycle_report(
            self.mem_ctx(),
            &self.graph,
            &self.upcaster_registry,
            report,
        )
        .await
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
    use crate::cognitive::{
        CALLER_WRITE_CURSOR, CycleDelta, CycleMetadata, CycleOutcome, CycleReport, IdentityOutput,
        SkipReason, TimeWindow,
    };
    use crate::engine::MemoryEngine;
    use crate::error::MemoryError;
    use crate::traits::CycleCtx;
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
        async fn run(&self, ctx: &dyn CycleCtx) -> Result<CycleReport> {
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

    /// A cycle that exercises the **restored capability bag** through `&dyn CycleCtx`.
    ///
    /// This is the regression test for the S1 defect (#981). ADR-0014 decision #3 gave a
    /// `DreamCycle` the engine capability bag; S1 re-typed `run` to `&dyn CycleCtx` and
    /// silently severed it, leaving 7 of 9 methods unreachable — and **no test noticed**,
    /// because none had ever called them through the contract. Restoring the bag without a
    /// test that consumes it would repeat exactly that mistake.
    ///
    /// Every call below goes through the `CycleCtx: DreamCtx` supertrait — i.e. through the
    /// path a real consumer's cycle takes. It also proves the `EngineDreamCtx` adapter does
    /// not recurse: a same-name resolution bug would blow the stack here, not return data.
    struct CapabilityProbeCycle {
        seen_active: std::sync::atomic::AtomicUsize,
        seen_fact_id: std::sync::atomic::AtomicI64,
        reached_outcome_counts: std::sync::atomic::AtomicBool,
    }

    #[async_trait::async_trait]
    impl DreamCycle for CapabilityProbeCycle {
        async fn run(&self, ctx: &dyn CycleCtx) -> Result<CycleReport> {
            use std::sync::atomic::Ordering;

            // --- inherited from DreamCtx via the supertrait (unreachable before #981) ---
            let active = ctx.list_active_facts(None).await?;
            self.seen_active.store(active.len(), Ordering::SeqCst);

            if let Some(first) = active.first() {
                let fetched = ctx.get_fact(first.id).await?;
                self.seen_fact_id.store(fetched.id, Ordering::SeqCst);

                // A fresh fact carries no outcome signals; what matters is that the call
                // resolves through the trait and *returns* — rather than blowing the stack.
                let _counts = ctx.outcome_counts(first.id).await?;
                self.reached_outcome_counts.store(true, Ordering::SeqCst);
            }

            Ok(CycleReport {
                deltas: vec![],
                identity: IdentityOutput::empty(),
                metadata: CycleMetadata {
                    cycle_id: 0,
                    ran_at: Utc::now(),
                    time_window: ctx.time_window(),
                    facts_selected: 0,
                    method_version: "capability-probe".into(),
                    processed_ids: vec![],
                },
            })
        }
    }

    /// #981 regression: a `DreamCycle` can reach the engine capability bag through
    /// `&dyn CycleCtx`. This is the contract ADR-0014 decision #3 promised and S1 broke.
    #[tokio::test]
    async fn cycle_can_reach_the_capability_bag_through_cyclectx() {
        use std::sync::atomic::Ordering;

        let engine = MemoryEngine::builder(4).build().unwrap();
        let ids = add_source_facts(&engine, &[1, 2]).await;
        assert!(!ids.is_empty(), "fixture must insert at least one fact");

        let probe = CapabilityProbeCycle {
            seen_active: std::sync::atomic::AtomicUsize::new(0),
            seen_fact_id: std::sync::atomic::AtomicI64::new(0),
            reached_outcome_counts: std::sync::atomic::AtomicBool::new(false),
        };
        engine.run_dream_cycle(&probe).await.unwrap();

        // The cycle actually saw the store through the trait — not an empty stub.
        assert_eq!(
            probe.seen_active.load(Ordering::SeqCst),
            ids.len(),
            "list_active_facts must reach the real store through DreamCtx"
        );
        assert!(
            ids.contains(&probe.seen_fact_id.load(Ordering::SeqCst)),
            "get_fact must return a real fact through DreamCtx"
        );
        assert!(
            probe.reached_outcome_counts.load(Ordering::SeqCst),
            "outcome_counts must have been reached through DreamCtx"
        );
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
        use crate::types::AddFactRequest;

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
                        std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                            as std::sync::Arc<dyn crate::traits::EmbeddingProvider>,
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
        async fn run(&self, ctx: &dyn CycleCtx) -> Result<CycleReport> {
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

    /// Moved from the (now-carved) `me-cognitive::apply` test suite: this proves
    /// `apply_cycle_report`'s `Quarantine` delta interacts correctly with
    /// `explain_fact` — a facade-only module (`engine::inspect`), so this
    /// cross-module assertion cannot live in `me-cognitive` itself.
    #[tokio::test]
    async fn quarantine_is_distinguishable_from_forgetting_in_explain_fact() {
        use crate::inspect::{ExpiredReason, FactState};
        let engine = MemoryEngine::builder(4).build().unwrap();
        engine
            .storage()
            .store_embedding_fingerprint(&crate::types::EmbeddingFingerprint::new(
                "mock", "test", 4,
            ))
            .await
            .unwrap();
        let id = add_source_facts(&engine, &[0]).await[0];
        let report = CycleReport {
            deltas: vec![CycleDelta::Quarantine {
                fact_id: id,
                reason: "explicit correction".into(),
            }],
            identity: IdentityOutput::empty(),
            metadata: CycleMetadata {
                cycle_id: 1,
                ran_at: Utc::now(),
                time_window: TimeWindow {
                    start: Utc::now(),
                    end: Utc::now(),
                },
                facts_selected: 1,
                method_version: "test".into(),
                processed_ids: vec![id],
            },
        };
        engine.apply_cycle_report(&report).await.unwrap();
        let explanation = engine.explain_fact(id).await.unwrap();
        assert_eq!(
            explanation.state,
            FactState::Expired {
                reason: ExpiredReason::Quarantined
            },
            "explain_fact must report a quarantined fact as Quarantined, not Unknown/Forgotten"
        );
    }
}
