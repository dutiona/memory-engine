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

use chrono::Utc;

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
/// Holds borrowed references to the proposer and the embedder (both `Send + Sync`);
/// construct one per `run_dream_cycle` call. See the [module docs](self) for the
/// safety rails.
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
/// }
///
/// let proposer = MyProposer;
/// let embedder = MyEmbedder;
/// let backend = LlmDreamCycle::new(&proposer, &embedder);
/// // engine.run_dream_cycle_guarded(&backend)?  — then apply_cycle_report(...)
/// let _ = &backend;
/// ```
pub struct LlmDreamCycle<'a> {
    proposer: &'a dyn DeltaProposer,
    embedder: &'a dyn EmbeddingProvider,
}

impl<'a> LlmDreamCycle<'a> {
    /// Build an LLM consolidation backend from an injected proposer + embedder.
    #[must_use]
    pub const fn new(proposer: &'a dyn DeltaProposer, embedder: &'a dyn EmbeddingProvider) -> Self {
        Self { proposer, embedder }
    }
}

impl DreamCycle for LlmDreamCycle<'_> {
    fn run(&self, ctx: &CycleContext) -> Result<CycleReport> {
        let window = ctx.time_window();
        let facts = ctx.dream().list_undreamt_in_period(window)?;
        // `processed_ids` is the WHOLE window, independent of what the proposer
        // returns — a forgetful proposer cannot leave a fact un-dream-marked and
        // livelock the #209 guard.
        let processed_ids: Vec<FactId> = facts.iter().map(|f| f.id).collect();
        let by_id: HashMap<FactId, &Fact> = facts.iter().map(|f| (f.id, f)).collect();

        let proposal = self.proposer.propose(&facts, ctx.prior_wisdom())?;

        let now = Utc::now();
        let mut deltas: Vec<CycleDelta> = Vec::new();
        for group in &proposal.merges {
            // Clamp every proposed id to the fed window (dropping out-of-window /
            // hallucinated ids) and de-duplicate, preserving first-seen order. The
            // engine's validate_report checks existence/active but NOT window
            // membership, so this clamp is what stops an LLM acting on facts it was
            // never shown. A group left with no in-window source is skipped (emitting
            // an empty Synthesize would fail validation).
            let mut seen = HashSet::new();
            let sources: Vec<FactId> = group
                .source_ids
                .iter()
                .copied()
                .filter(|id| by_id.contains_key(id) && seen.insert(*id))
                .collect();
            if sources.is_empty() {
                continue;
            }
            // A merge inherits the strongest importance of its sources so consolidating
            // high-value facts does not produce a trivially-forgettable summary. Every
            // id is present in `by_id` (it passed the clamp), so indexing is safe.
            let importance = sources
                .iter()
                .map(|id| by_id[id].importance)
                .fold(0.0_f64, f64::max);
            // The engine stays LLM-free: the backend embeds its own summary text.
            let embedding = self.embedder.embed(&group.summary)?;
            let new_fact = NewFact {
                content: group.summary.clone(),
                content_hash: String::new(), // FactStore::insert computes blake3
                embedding,
                fact_type: FactType::Semantic, // a consolidated pattern
                t_created: now,
                t_expired: None,
                t_valid: None,
                t_invalid: None,
                source_event_id: None,
                importance,
                access_count: 0,
                last_accessed: now,
                metadata: serde_json::json!({}),
                scope_id: 1, // root scope (v1)
                is_pinned: false,
            };
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
    use crate::store::edges::EdgeStore;
    use crate::store::facts::FactStore;
    use crate::store::lineage::LineageStore;
    use crate::types::{AddFactRequest, ConsolidationProposal, MergeGroup};

    const DIM: usize = 4;

    /// Returns a fixed-dimension embedding; `dim` lets a test force a mismatch.
    struct FixedEmbed(usize);
    impl EmbeddingProvider for FixedEmbed {
        fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![0.1; self.0])
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

    fn add(engine: &MemoryEngine, content: &str) -> FactId {
        let req = AddFactRequest {
            content: content.into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        };
        engine.add_fact(&req, &FixedEmbed(DIM), None).unwrap()
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
    #[test]
    fn run_emits_synthesize_with_full_window_processed_ids() {
        let engine = engine();
        let s1 = add(&engine, "fact one");
        let s2 = add(&engine, "fact two");
        let s3 = add(&engine, "unmerged fact");
        let proposer = FakeProposer {
            merges: vec![merge(vec![s1, s2], "merged one+two")],
        };
        let llm = LlmDreamCycle::new(&proposer, &FixedEmbed(DIM));

        let report = engine.run_dream_cycle(&llm).unwrap();

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
    #[test]
    fn run_then_apply_creates_edges_lineage_and_holds_invariant_m() {
        let engine = engine();
        let s1 = add(&engine, "fact one");
        let s2 = add(&engine, "fact two");
        let _s3 = add(&engine, "unmerged but processed");
        let proposer = FakeProposer {
            merges: vec![merge(vec![s1, s2], "merged")],
        };
        let llm = LlmDreamCycle::new(&proposer, &FixedEmbed(DIM));

        let report = engine.run_dream_cycle(&llm).unwrap();
        let res = engine.apply_cycle_report(&report).unwrap();
        assert_eq!(res.synthesized, 1);
        let synth = res.synthesized_fact_ids[0];

        let (edges, lineage_sources, e1, e2) = engine
            .with_read(|conn| {
                let edges = EdgeStore::new(conn).list_active_by_source(synth)?;
                let (lineage, _p) = LineageStore::new(conn).get_by_wisdom_fact(synth)?;
                let s = FactStore::new(conn, DIM);
                Ok((edges, lineage.source_fact_ids, s.get(s1)?, s.get(s2)?))
            })
            .unwrap();
        for src in [s1, s2] {
            assert!(
                edges
                    .iter()
                    .any(|e| e.relation_type == "supersedes" && e.target_fact_id == src)
            );
        }
        assert_eq!(lineage_sources, vec![s1, s2]);
        assert!(e1.t_expired.is_some() && e2.t_expired.is_some());

        let max_caller = engine
            .with_read(|conn| FactStore::new(conn, DIM).max_caller_written_fact_id())
            .unwrap();
        assert_eq!(
            max_caller, None,
            "invariant M: nothing looks like a caller write"
        );
    }

    /// An out-of-window source id is silently dropped (clamped), not passed through to
    /// `validate_report` (which would reject the whole report as `UnknownFact`).
    #[test]
    fn run_clamps_out_of_window_source_ids() {
        let engine = engine();
        let s1 = add(&engine, "in window");
        let proposer = FakeProposer {
            merges: vec![merge(vec![s1, 999_999], "merged with a ghost")],
        };
        let llm = LlmDreamCycle::new(&proposer, &FixedEmbed(DIM));

        let report = engine.run_dream_cycle(&llm).unwrap();
        assert_eq!(synth_sources(&report), vec![vec![s1]], "ghost id dropped");
        // And it applies (the clamped single source is valid).
        engine.apply_cycle_report(&report).unwrap();
    }

    /// A group whose sources are ALL outside the window collapses to nothing — the group
    /// is skipped rather than emitted as an empty (and invalid) Synthesize.
    #[test]
    fn run_skips_group_with_no_in_window_sources() {
        let engine = engine();
        let s1 = add(&engine, "in window");
        let proposer = FakeProposer {
            merges: vec![merge(vec![888_888, 999_999], "all ghosts")],
        };
        let llm = LlmDreamCycle::new(&proposer, &FixedEmbed(DIM));

        let report = engine.run_dream_cycle(&llm).unwrap();
        assert!(synth_sources(&report).is_empty(), "all-ghost group skipped");
        // The real window fact is still processed (dream-marked) on apply — no livelock.
        let res = engine.apply_cycle_report(&report).unwrap();
        assert_eq!(res.synthesized, 0);
        assert!(report.metadata.processed_ids.contains(&s1));
    }

    /// An empty proposal (the LLM found nothing to merge) yields a no-op report that
    /// still dream-marks the window.
    #[test]
    fn run_empty_proposal_is_noop_but_marks_window() {
        let engine = engine();
        let s1 = add(&engine, "lonely fact");
        let proposer = FakeProposer { merges: vec![] };
        let llm = LlmDreamCycle::new(&proposer, &FixedEmbed(DIM));

        let report = engine.run_dream_cycle(&llm).unwrap();
        assert!(synth_sources(&report).is_empty());
        engine.apply_cycle_report(&report).unwrap();
        let max_caller = engine
            .with_read(|conn| FactStore::new(conn, DIM).max_caller_written_fact_id())
            .unwrap();
        assert_eq!(max_caller, None, "the window fact {s1} was dream-marked");
    }

    /// The backend has no engine handle, so it cannot validate the embedder's dimension;
    /// a mismatch surfaces at apply time (parity with `AddFact`).
    #[test]
    fn embed_dim_mismatch_surfaces_at_apply() {
        let engine = engine();
        let s1 = add(&engine, "a");
        let s2 = add(&engine, "b");
        let proposer = FakeProposer {
            merges: vec![merge(vec![s1, s2], "merged")],
        };
        let bad_embedder = FixedEmbed(DIM + 4); // wrong dimension
        let llm = LlmDreamCycle::new(&proposer, &bad_embedder);

        let report = engine.run_dream_cycle(&llm).unwrap(); // run itself can't know engine dim
        let err = engine.apply_cycle_report(&report).unwrap_err();
        assert!(matches!(err, MemoryError::EmbeddingDimension { .. }));
    }

    /// `cycle_id` mirrors `DefaultDreamCycle`: `max(prior_reports.cycle_id) + 1`.
    #[test]
    fn cycle_id_increments_via_prior_reports() {
        let engine = engine();
        let s1 = add(&engine, "a");
        let s2 = add(&engine, "b");
        let proposer = FakeProposer {
            merges: vec![merge(vec![s1, s2], "merged")],
        };
        let llm = LlmDreamCycle::new(&proposer, &FixedEmbed(DIM));

        let first = engine.run_dream_cycle(&llm).unwrap();
        assert_eq!(first.metadata.cycle_id, 0);
        engine.apply_cycle_report(&first).unwrap();

        let second = engine.run_dream_cycle(&llm).unwrap();
        assert_eq!(
            second.metadata.cycle_id, 1,
            "cycle_id advances off prior_reports"
        );
    }
}
