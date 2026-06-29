//! LLM-backed dream cycle (#554): a [`DreamCycle`] whose merge decisions come from a
//! consumer-injected [`DeltaProposer`].
//!
//! This is the *pluggable consolidation backend*. It runs the same
//! retrieve-before-reflect loop as [`DefaultDreamCycle`](super::default_impl) — select
//! the undreamt window, decide what to consolidate, emit a delta-based
//! [`CycleReport`] — but delegates the "what to merge" decision to a `DeltaProposer`
//! (typically an LLM over HTTP). It keeps the engine LLM-free: the proposer returns
//! ids + summary text only, and *this* type embeds the summary (via its own
//! [`EmbeddingProvider`]) and assembles the [`CycleDelta::Synthesize`] deltas.
//!
//! Safety rails against a forgetful or hostile proposer:
//! - **Window clamp** — every proposed `source_id` is filtered to the ids actually in
//!   the fed window, so the LLM cannot act on facts outside what it was shown
//!   (`validate_report` checks existence/active, not window membership).
//! - **`processed_ids` = the whole window** — not the proposer's output — so a proposer
//!   that forgets a fact cannot leave it un-dream-marked and livelock the #209 guard.
//! - **Apply-time validation** — each `Synthesize` still passes `validate_report`
//!   (sources exist + active, embedding dim) and the one-transaction invariant.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::Utc;

use crate::engine::spawn_join_err;
use crate::error::Result;
use crate::traits::{DeltaProposer, DreamCycle, EmbeddingProvider};
use crate::types::{Fact, FactId, FactType, NewFact};

use super::context::CycleContext;
use super::report::{CycleDelta, CycleMetadata, CycleReport, IdentityOutput};

/// `method_version` stamped into every report this backend produces.
const METHOD_VERSION: &str = "llm-proposer-v1";

/// A [`DreamCycle`] that consolidates by asking a [`DeltaProposer`] what to merge,
/// then synthesizing each merge group into one fact.
///
/// Holds `Arc` handles to the proposer and the embedder (both `Send + Sync`);
/// construct one per `run_dream_cycle` call. The `Arc`s let the async `run` offload
/// each (possibly blocking HTTP) `propose`/`embed` call to the blocking pool via
/// `spawn_blocking` — keeping the executor unblocked and a `reqwest::blocking`
/// provider nested-runtime-safe. See the module docs for the safety rails.
///
/// # Injecting the backend
///
/// The proposer and embedder are consumer-supplied; the engine never embeds or calls
/// an LLM on their behalf. Wire them in, then hand the cycle to
/// [`run_dream_cycle_guarded`](crate::MemoryEngine::run_dream_cycle_guarded):
///
/// ```
/// use memory_engine::{DeltaProposer, EmbeddingProvider, LlmDreamCycle};
/// use memory_engine::error::Result;
/// use memory_engine::types::{ConsolidationProposal, Fact};
///
/// struct MyProposer; // e.g. memory-engine-embed's HttpDeltaProposer
/// impl DeltaProposer for MyProposer {
///     fn propose(&self, _window: &[Fact], _prior: &[Fact]) -> Result<ConsolidationProposal> {
///         Ok(ConsolidationProposal::default()) // nothing to merge
///     }
/// }
/// struct MyEmbedder;
/// impl EmbeddingProvider for MyEmbedder {
///     fn embed(&self, _text: &str) -> Result<Vec<f32>> { Ok(vec![0.0; 8]) }
///     fn fingerprint(&self) -> memory_engine::types::EmbeddingFingerprint {
///         memory_engine::types::EmbeddingFingerprint::new("my-model", "my-provider", 8)
///     }
/// }
///
/// use std::sync::Arc;
/// let proposer: Arc<dyn DeltaProposer> = Arc::new(MyProposer);
/// let embedder: Arc<dyn EmbeddingProvider> = Arc::new(MyEmbedder);
/// let backend = LlmDreamCycle::new(proposer, embedder);
/// // engine.run_dream_cycle_guarded(&backend).await?  — then apply_cycle_report(...).await
/// let _ = &backend;
/// ```
pub struct LlmDreamCycle {
    proposer: Arc<dyn DeltaProposer>,
    embedder: Arc<dyn EmbeddingProvider>,
}

impl LlmDreamCycle {
    /// Build an LLM consolidation backend from an injected proposer + embedder.
    #[must_use]
    pub fn new(proposer: Arc<dyn DeltaProposer>, embedder: Arc<dyn EmbeddingProvider>) -> Self {
        Self { proposer, embedder }
    }
}

#[async_trait::async_trait]
impl DreamCycle for LlmDreamCycle {
    async fn run(&self, ctx: &CycleContext<'_>) -> Result<CycleReport> {
        let window = ctx.time_window();
        let facts = ctx.dream().list_undreamt_in_period(window).await?;
        // `processed_ids` is the WHOLE window, independent of what the proposer
        // returns — a forgetful proposer cannot leave a fact un-dream-marked and
        // livelock the #209 guard.
        let processed_ids: Vec<FactId> = facts.iter().map(|f| f.id).collect();
        let by_id: HashMap<FactId, &Fact> = facts.iter().map(|f| (f.id, f)).collect();

        // Offload the proposer (a possibly-blocking HTTP round-trip) off the executor;
        // clone the owned inputs into the blocking task (no borrow crosses the boundary).
        let proposal = {
            let proposer = Arc::clone(&self.proposer);
            let window_facts = facts.clone();
            let prior = ctx.prior_wisdom().to_vec();
            tokio::task::spawn_blocking(move || proposer.propose(&window_facts, &prior))
                .await
                .map_err(spawn_join_err)??
        };

        let now = Utc::now();
        let mut deltas: Vec<CycleDelta> = Vec::new();
        // A source may be merged at most once across the whole report. Claiming each
        // source globally (not just within a group) is load-bearing: two groups naming
        // the same id would emit two `Synthesize` deltas, and the second would fail
        // apply with `AlreadyExpired`, roll back the whole report, leave nothing
        // dream-marked, and — because the proposer is deterministic at temperature 0 —
        // livelock on the same window forever (#641).
        let mut consumed: HashSet<FactId> = HashSet::new();
        for group in &proposal.merges {
            // Clamp every proposed id to the fed window (dropping out-of-window /
            // hallucinated ids) and to ids not already claimed by an earlier emitted
            // group, de-duplicating within the group. validate_report checks
            // existence/active but NOT window membership, so this clamp is what stops
            // an LLM acting on facts it was never shown.
            let mut local_seen = HashSet::new();
            let sources: Vec<FactId> = group
                .source_ids
                .iter()
                .copied()
                .filter(|id| {
                    by_id.contains_key(id) && !consumed.contains(id) && local_seen.insert(*id)
                })
                .collect();
            if sources.is_empty() {
                continue;
            }
            // The window spans all scopes, so the synthetic must inherit its sources'
            // scope rather than default to root — else a child-scope merge would leak
            // into root and widen retrieval visibility. A group whose sources straddle
            // scopes is skipped: merging across an isolation boundary is never valid.
            let scope_id = by_id[&sources[0]].scope_id;
            if sources.iter().any(|id| by_id[id].scope_id != scope_id) {
                continue;
            }
            // A merge inherits the strongest importance of its sources so consolidating
            // high-value facts does not produce a trivially-forgettable summary. Every
            // id is present in `by_id` (it passed the clamp), so indexing is safe.
            let importance = sources
                .iter()
                .map(|id| by_id[id].base_importance)
                .fold(0.0_f64, f64::max);
            // The engine stays LLM-free: the backend embeds its own summary text.
            // Offload the embed (also possibly a blocking HTTP round-trip).
            let embedding = {
                let embedder = Arc::clone(&self.embedder);
                let summary = group.summary.clone();
                tokio::task::spawn_blocking(move || embedder.embed(&summary))
                    .await
                    .map_err(spawn_join_err)??
            };
            // A consolidated pattern. The builder defaults the remaining fields
            // (empty metadata, `is_pinned = false`, `last_accessed = t_created`).
            let new_fact = NewFact::builder(group.summary.clone(), embedding, FactType::Semantic)
                .t_created(now)
                .base_importance(importance)
                .scope_id(scope_id)
                .build();
            // Claim the sources only now that the group is actually emitted, so a
            // skipped (cross-scope) group does not lock its ids away from later groups.
            consumed.extend(sources.iter().copied());
            deltas.push(CycleDelta::Synthesize { sources, new_fact });
        }

        // Mirror DefaultDreamCycle: next id off the persisted history ring.
        let cycle_id = ctx
            .prior_reports()
            .iter()
            .map(|m| m.cycle_id)
            .max()
            .map_or(0, |m| m + 1);

        Ok(CycleReport {
            deltas,
            identity: IdentityOutput::empty(),
            metadata: CycleMetadata {
                cycle_id,
                ran_at: now,
                time_window: window,
                facts_selected: facts.len(),
                method_version: METHOD_VERSION.to_owned(),
                processed_ids,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::MemoryEngine;
    use crate::error::MemoryError;
    use crate::types::{AddFactRequest, ConsolidationProposal, MergeGroup};

    const DIM: usize = 4;

    /// Returns a fixed-dimension embedding; `dim` lets a test force a mismatch.
    struct FixedEmbed(usize);
    impl EmbeddingProvider for FixedEmbed {
        fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![0.1; self.0])
        }
        fn fingerprint(&self) -> crate::types::EmbeddingFingerprint {
            crate::types::EmbeddingFingerprint::new("mock", "test", self.0)
        }
    }

    /// A proposer that returns a canned set of merge groups, ignoring its inputs.
    struct FakeProposer {
        merges: Vec<MergeGroup>,
    }
    impl DeltaProposer for FakeProposer {
        fn propose(&self, _window: &[Fact], _prior: &[Fact]) -> Result<ConsolidationProposal> {
            Ok(ConsolidationProposal {
                merges: self.merges.clone(),
            })
        }
    }

    fn engine() -> MemoryEngine {
        MemoryEngine::builder(DIM).build().unwrap()
    }

    async fn add(engine: &MemoryEngine, content: &str) -> FactId {
        add_scoped(engine, content, None).await
    }

    async fn add_scoped(engine: &MemoryEngine, content: &str, scope: Option<&str>) -> FactId {
        let req = AddFactRequest {
            content: content.into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: scope.map(str::to_owned),
            opts: None,
        };
        engine
            .add_fact(
                &req,
                std::sync::Arc::new(FixedEmbed(DIM)) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap()
    }

    async fn scope_of(engine: &MemoryEngine, id: FactId) -> i64 {
        engine.storage().get_fact(id).await.unwrap().scope_id
    }

    fn merge(source_ids: Vec<FactId>, summary: &str) -> MergeGroup {
        MergeGroup {
            source_ids,
            summary: summary.into(),
        }
    }

    fn synth_sources(report: &CycleReport) -> Vec<Vec<FactId>> {
        report
            .deltas
            .iter()
            .filter_map(|d| match d {
                CycleDelta::Synthesize { sources, .. } => Some(sources.clone()),
                _ => None,
            })
            .collect()
    }

    /// `run()` selects the undreamt window, emits one `Synthesize` per merge group, and
    /// stamps `processed_ids` with the WHOLE window (not just merged facts) so the
    /// #209 guard cannot livelock on an un-marked leftover.
    #[tokio::test]
    async fn run_emits_synthesize_with_full_window_processed_ids() {
        let engine = engine();
        let s1 = add(&engine, "fact one").await;
        let s2 = add(&engine, "fact two").await;
        let s3 = add(&engine, "unmerged fact").await;
        let proposer = FakeProposer {
            merges: vec![merge(vec![s1, s2], "merged one+two")],
        };
        let llm = LlmDreamCycle::new(
            std::sync::Arc::new(proposer) as std::sync::Arc<dyn DeltaProposer>,
            std::sync::Arc::new(FixedEmbed(DIM)) as std::sync::Arc<dyn EmbeddingProvider>,
        );

        let report = engine.run_dream_cycle(&llm).await.unwrap();

        assert_eq!(synth_sources(&report), vec![vec![s1, s2]]);
        let mut processed = report.metadata.processed_ids.clone();
        processed.sort_unstable();
        assert_eq!(
            processed,
            vec![s1, s2, s3],
            "processed_ids = the whole window"
        );
        assert_eq!(report.metadata.method_version, METHOD_VERSION);
    }

    /// End-to-end: the produced report applies cleanly, creating the supersedes edges +
    /// lineage, and invariant M holds — no active unmarked fact looks like a caller write.
    #[tokio::test]
    async fn run_then_apply_creates_edges_lineage_and_holds_invariant_m() {
        let engine = engine();
        let s1 = add(&engine, "fact one").await;
        let s2 = add(&engine, "fact two").await;
        let _s3 = add(&engine, "unmerged but processed").await;
        let proposer = FakeProposer {
            merges: vec![merge(vec![s1, s2], "merged")],
        };
        let llm = LlmDreamCycle::new(
            std::sync::Arc::new(proposer) as std::sync::Arc<dyn DeltaProposer>,
            std::sync::Arc::new(FixedEmbed(DIM)) as std::sync::Arc<dyn EmbeddingProvider>,
        );

        let report = engine.run_dream_cycle(&llm).await.unwrap();
        let res = engine.apply_cycle_report(&report).await.unwrap();
        assert_eq!(res.synthesized, 1);
        let synth = res.synthesized_fact_ids[0];

        let edges = engine
            .storage()
            .list_active_edges_by_source(synth)
            .await
            .unwrap();
        let (lineage, _p) = engine
            .storage()
            .get_lineage_by_wisdom_fact(synth)
            .await
            .unwrap();
        let lineage_sources = lineage.source_fact_ids;
        let e1 = engine.storage().get_fact(s1).await.unwrap();
        let e2 = engine.storage().get_fact(s2).await.unwrap();
        for src in [s1, s2] {
            assert!(
                edges
                    .iter()
                    .any(|e| e.relation_type == "supersedes" && e.target_fact_id == src)
            );
        }
        assert_eq!(lineage_sources, vec![s1, s2]);
        assert!(e1.t_expired.is_some() && e2.t_expired.is_some());

        let max_caller = engine.storage().max_caller_written_fact_id().await.unwrap();
        assert_eq!(
            max_caller, None,
            "invariant M: nothing looks like a caller write"
        );
    }

    /// An out-of-window source id is silently dropped (clamped), not passed through to
    /// `validate_report` (which would reject the whole report as `UnknownFact`).
    #[tokio::test]
    async fn run_clamps_out_of_window_source_ids() {
        let engine = engine();
        let s1 = add(&engine, "in window").await;
        let proposer = FakeProposer {
            merges: vec![merge(vec![s1, 999_999], "merged with a ghost")],
        };
        let llm = LlmDreamCycle::new(
            std::sync::Arc::new(proposer) as std::sync::Arc<dyn DeltaProposer>,
            std::sync::Arc::new(FixedEmbed(DIM)) as std::sync::Arc<dyn EmbeddingProvider>,
        );

        let report = engine.run_dream_cycle(&llm).await.unwrap();
        assert_eq!(synth_sources(&report), vec![vec![s1]], "ghost id dropped");
        // And it applies (the clamped single source is valid).
        engine.apply_cycle_report(&report).await.unwrap();
    }

    /// A group whose sources are ALL outside the window collapses to nothing — the group
    /// is skipped rather than emitted as an empty (and invalid) Synthesize.
    #[tokio::test]
    async fn run_skips_group_with_no_in_window_sources() {
        let engine = engine();
        let s1 = add(&engine, "in window").await;
        let proposer = FakeProposer {
            merges: vec![merge(vec![888_888, 999_999], "all ghosts")],
        };
        let llm = LlmDreamCycle::new(
            std::sync::Arc::new(proposer) as std::sync::Arc<dyn DeltaProposer>,
            std::sync::Arc::new(FixedEmbed(DIM)) as std::sync::Arc<dyn EmbeddingProvider>,
        );

        let report = engine.run_dream_cycle(&llm).await.unwrap();
        assert!(synth_sources(&report).is_empty(), "all-ghost group skipped");
        // The real window fact is still processed (dream-marked) on apply — no livelock.
        let res = engine.apply_cycle_report(&report).await.unwrap();
        assert_eq!(res.synthesized, 0);
        assert!(report.metadata.processed_ids.contains(&s1));
    }

    /// An empty proposal (the LLM found nothing to merge) yields a no-op report that
    /// still dream-marks the window.
    #[tokio::test]
    async fn run_empty_proposal_is_noop_but_marks_window() {
        let engine = engine();
        let s1 = add(&engine, "lonely fact").await;
        let proposer = FakeProposer { merges: vec![] };
        let llm = LlmDreamCycle::new(
            std::sync::Arc::new(proposer) as std::sync::Arc<dyn DeltaProposer>,
            std::sync::Arc::new(FixedEmbed(DIM)) as std::sync::Arc<dyn EmbeddingProvider>,
        );

        let report = engine.run_dream_cycle(&llm).await.unwrap();
        assert!(synth_sources(&report).is_empty());
        engine.apply_cycle_report(&report).await.unwrap();
        let max_caller = engine.storage().max_caller_written_fact_id().await.unwrap();
        assert_eq!(max_caller, None, "the window fact {s1} was dream-marked");
    }

    /// The backend has no engine handle, so it cannot validate the embedder's dimension;
    /// a mismatch surfaces at apply time (parity with `AddFact`).
    #[tokio::test]
    async fn embed_dim_mismatch_surfaces_at_apply() {
        let engine = engine();
        let s1 = add(&engine, "a").await;
        let s2 = add(&engine, "b").await;
        let proposer = FakeProposer {
            merges: vec![merge(vec![s1, s2], "merged")],
        };
        let bad_embedder = FixedEmbed(DIM + 4); // wrong dimension
        let llm = LlmDreamCycle::new(
            std::sync::Arc::new(proposer) as std::sync::Arc<dyn DeltaProposer>,
            std::sync::Arc::new(bad_embedder) as std::sync::Arc<dyn EmbeddingProvider>,
        );

        let report = engine.run_dream_cycle(&llm).await.unwrap(); // run itself can't know engine dim
        let err = engine.apply_cycle_report(&report).await.unwrap_err();
        assert!(matches!(err, MemoryError::EmbeddingDimension { .. }));
    }

    /// `cycle_id` mirrors `DefaultDreamCycle`: `max(prior_reports.cycle_id) + 1`.
    #[tokio::test]
    async fn cycle_id_increments_via_prior_reports() {
        let engine = engine();
        let s1 = add(&engine, "a").await;
        let s2 = add(&engine, "b").await;
        let proposer = FakeProposer {
            merges: vec![merge(vec![s1, s2], "merged")],
        };
        let llm = LlmDreamCycle::new(
            std::sync::Arc::new(proposer) as std::sync::Arc<dyn DeltaProposer>,
            std::sync::Arc::new(FixedEmbed(DIM)) as std::sync::Arc<dyn EmbeddingProvider>,
        );

        let first = engine.run_dream_cycle(&llm).await.unwrap();
        assert_eq!(first.metadata.cycle_id, 0);
        engine.apply_cycle_report(&first).await.unwrap();

        let second = engine.run_dream_cycle(&llm).await.unwrap();
        assert_eq!(
            second.metadata.cycle_id, 1,
            "cycle_id advances off prior_reports"
        );
    }

    /// A proposer that names the same source in two groups must not emit two
    /// Synthesize deltas for it — the second collides at apply with `AlreadyExpired`,
    /// rolling back the whole report and livelocking the deterministic LLM (#641
    /// BLOCKER, flagged by both reviewers). Cross-group dedup claims each source once.
    #[tokio::test]
    async fn run_dedups_sources_across_merge_groups() {
        let engine = engine();
        let s1 = add(&engine, "a").await;
        let s2 = add(&engine, "b").await;
        let s3 = add(&engine, "c").await;
        let proposer = FakeProposer {
            merges: vec![merge(vec![s1, s2], "g1"), merge(vec![s2, s3], "g2")],
        };
        let llm = LlmDreamCycle::new(
            std::sync::Arc::new(proposer) as std::sync::Arc<dyn DeltaProposer>,
            std::sync::Arc::new(FixedEmbed(DIM)) as std::sync::Arc<dyn EmbeddingProvider>,
        );

        let report = engine.run_dream_cycle(&llm).await.unwrap();
        // s2 is claimed by g1; g2 keeps only s3.
        assert_eq!(synth_sources(&report), vec![vec![s1, s2], vec![s3]]);
        // The report applies cleanly — no AlreadyExpired rollback.
        let res = engine.apply_cycle_report(&report).await.unwrap();
        assert_eq!(res.synthesized, 2);
    }

    /// The window spans all scopes, so a synthetic must inherit its sources' scope
    /// rather than hard-code root — else a child-scope merge leaks into root and
    /// widens retrieval visibility. A group whose sources span scopes is skipped
    /// (no cross-scope merge). (#641 HIGH.)
    #[tokio::test]
    async fn run_inherits_source_scope_and_skips_cross_scope_groups() {
        let engine = engine();
        let root1 = add_scoped(&engine, "root a", None).await;
        let root2 = add_scoped(&engine, "root b", None).await;
        let child1 = add_scoped(&engine, "child a", Some("proj")).await;
        let child2 = add_scoped(&engine, "child b", Some("proj")).await;
        let child_scope = scope_of(&engine, child1).await;
        assert_ne!(child_scope, 1, "child facts are in a non-root scope");

        let proposer = FakeProposer {
            merges: vec![
                merge(vec![root1, child1], "cross-scope"), // spans scopes → skipped
                merge(vec![child1, child2], "child merge"), // same child scope → emit
                merge(vec![root1, root2], "root merge"),   // same root scope → emit
            ],
        };
        let llm = LlmDreamCycle::new(
            std::sync::Arc::new(proposer) as std::sync::Arc<dyn DeltaProposer>,
            std::sync::Arc::new(FixedEmbed(DIM)) as std::sync::Arc<dyn EmbeddingProvider>,
        );

        let report = engine.run_dream_cycle(&llm).await.unwrap();
        assert_eq!(
            synth_sources(&report),
            vec![vec![child1, child2], vec![root1, root2]],
            "cross-scope group dropped; same-scope groups kept"
        );
        let res = engine.apply_cycle_report(&report).await.unwrap();
        assert_eq!(res.synthesized_fact_ids.len(), 2);
        assert_eq!(
            scope_of(&engine, res.synthesized_fact_ids[0]).await,
            child_scope,
            "child synthetic stays in the child scope"
        );
        assert_eq!(
            scope_of(&engine, res.synthesized_fact_ids[1]).await,
            1,
            "root synthetic stays at root"
        );
    }
}
