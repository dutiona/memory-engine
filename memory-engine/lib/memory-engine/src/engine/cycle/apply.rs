//! Transactional application of a [`CycleReport`] (R7).
//!
//! [`MemoryEngine::apply_cycle_report`] validates the entire delta log against a
//! read-only snapshot, then applies every delta in a **single transaction** so a
//! malformed delta leaves the store byte-identical. It is an inherent method (not a
//! free function) because it needs the engine's upcaster registry (for outcome
//! events) and the HNSW handle (for post-commit index notification).
//!
//! Lock discipline: one `write_conn()` acquisition for the whole operation.
//! `Promote` reuses [`MemoryEngine::promote_in_conn`] and `TagOutcome` inserts the
//! outcome event directly on the shared transaction — neither re-acquires the lock,
//! which would self-deadlock on the non-reentrant connection mutex.

use crate::engine::MemoryEngine;
use crate::error::Result;
use crate::graph::EdgeData;
use crate::types::RelationType;

use super::report::{ApplyResult, CycleReport};

impl MemoryEngine {
    /// Validate and apply a [`CycleReport`] atomically.
    ///
    /// The full validation + delta dispatch + dream-marking + watermark/history update
    /// is one transaction below the seam ([`ConsolidationStore::apply_cycle_deltas_atomic`](crate::storage::ConsolidationStore::apply_cycle_deltas_atomic)),
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
    /// - [`MemoryError::ReadOnly`](crate::error::MemoryError::ReadOnly) if the engine is read-only.
    /// - [`MemoryError::Cycle`](crate::error::MemoryError::Cycle) if any delta fails validation.
    /// - [`MemoryError::EmbeddingDimension`](crate::error::MemoryError::EmbeddingDimension) if an
    ///   `AddFact`/`Promote`/`Synthesize` embedding does not match the engine dimension.
    /// - [`MemoryError::Storage`](crate::error::MemoryError::Storage) on a backend failure.
    pub async fn apply_cycle_report(&self, report: &CycleReport) -> Result<ApplyResult> {
        self.ensure_open()?;
        let (result, supersede_edges, _expired_ids, _to_index) = self
            .storage
            .apply_cycle_deltas_atomic(report, self.embed_dim, &self.upcaster_registry)
            .await?;

        // Mirror supersede edges into the in-memory graph. The port read is already
        // awaited, so the `parking_lot` write guard below is not held across any
        // `.await` (keeps the future `Send`). Matches the post-commit edge-sync pattern
        // used by conflict resolution + co-session linking.
        if !supersede_edges.is_empty() {
            let mut graph = self.graph.write();
            for &(new_id, old_id, edge_id) in &supersede_edges {
                graph.remove_edges_by_fact(old_id);
                graph.add_edge(
                    new_id,
                    old_id,
                    EdgeData {
                        edge_id,
                        relation_type: RelationType::Supersedes,
                        weight: 1.0,
                    },
                );
            }
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::super::report::{CycleMetadata, IdentityOutput, TimeWindow};
    use super::*;
    use crate::engine::cycle::CycleDelta;
    use crate::error::CycleError;
    use crate::error::MemoryError;
    use crate::traits::EmbeddingProvider;
    use crate::types::{AddFactRequest, FactType, NewFact, Outcome, PromotionProvenance};

    const DIM: usize = 4;

    async fn engine() -> MemoryEngine {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        // Stamp the embedding identity so apply tests can apply pre-computed-vector
        // deltas (AddFact/Synthesize) — #613 requires a recorded identity for those.
        // This mirrors production, where facts (and thus the identity) exist before a
        // cycle runs. Same fingerprint MockEmbedder::fixed4() records, so a later
        // `add()` is a no-op.
        engine
            .storage()
            .store_embedding_fingerprint(&crate::types::EmbeddingFingerprint::new(
                "mock", "test", DIM,
            ))
            .await
            .unwrap();
        engine
    }

    async fn add(engine: &MemoryEngine, content: &str) -> i64 {
        let req = AddFactRequest {
            content: content.into(),
            fact_type: FactType::Episodic,
            source_event_id: None,
            scope: None,
            opts: None,
        };
        engine
            .add_fact(
                &req,
                std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                    as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap()
    }

    async fn importance_of(engine: &MemoryEngine, id: i64) -> f64 {
        engine.storage().get_fact(id).await.unwrap().base_importance
    }

    fn meta(processed: Vec<i64>) -> CycleMetadata {
        let start = "2026-06-16T00:00:00Z".parse().unwrap();
        CycleMetadata {
            cycle_id: 1,
            ran_at: start,
            time_window: TimeWindow {
                start,
                end: "2026-06-16T01:00:00Z".parse().unwrap(),
            },
            facts_selected: processed.len(),
            method_version: "test".into(),
            processed_ids: processed,
        }
    }

    fn report(deltas: Vec<CycleDelta>, processed: Vec<i64>) -> CycleReport {
        CycleReport {
            deltas,
            identity: IdentityOutput::empty(),
            metadata: meta(processed),
        }
    }

    #[tokio::test]
    async fn apply_add_fact_into_unstamped_store_is_rejected() {
        // #613 guard: AddFact carries a pre-computed vector with no live embedder, so
        // it cannot stamp identity. Applying it to a store with no recorded identity is
        // rejected. Uses a raw builder, bypassing the identity-seeding `engine()` helper.
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let nf = NewFact {
            content: "derived pattern".into(),
            content_hash: String::new(),
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            fact_type: FactType::Semantic,
            t_created: "2026-06-16T00:30:00Z".parse().unwrap(),
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            base_importance: 0.5,
            access_count: 0,
            last_accessed: "2026-06-16T00:30:00Z".parse().unwrap(),
            metadata: serde_json::json!({}),
            scope_id: 1,
            is_pinned: false,
        };
        let err = engine
            .apply_cycle_report(&report(vec![CycleDelta::AddFact(nf)], vec![]))
            .await
            .unwrap_err();
        assert!(
            matches!(err, MemoryError::Internal(ref m) if m.contains("no embedding identity")),
            "expected the identity guard error, got: {err:?}"
        );
    }

    fn stub_provenance() -> PromotionProvenance {
        let now = "2026-06-16T00:00:00Z".parse().unwrap();
        PromotionProvenance {
            source_count: 1,
            session_count: 1,
            date_range_start: now,
            date_range_end: now,
            confidence: 0.9,
            method_version: "test".into(),
            representative_ids: vec![],
        }
    }

    #[tokio::test]
    async fn empty_report_is_ok_noop() {
        let engine = engine().await;
        let res = engine
            .apply_cycle_report(&report(vec![], vec![]))
            .await
            .unwrap();
        assert_eq!(res, ApplyResult::default());
    }

    #[tokio::test]
    async fn adjust_score_moves_base_importance_and_clamps() {
        let engine = engine().await;
        let id = add(&engine, "f").await; // default importance 0.5
        engine
            .apply_cycle_report(&report(
                vec![CycleDelta::AdjustScore {
                    fact_id: id,
                    adjustment: 2,
                }],
                vec![id],
            ))
            .await
            .unwrap();
        // 0.5 + 2*0.05 = 0.6
        assert!((importance_of(&engine, id).await - 0.6).abs() < 1e-9);

        // Repeated negative clamps at floor without underflow.
        for _ in 0..20 {
            engine
                .apply_cycle_report(&report(
                    vec![CycleDelta::AdjustScore {
                        fact_id: id,
                        adjustment: -2,
                    }],
                    vec![],
                ))
                .await
                .unwrap();
        }
        assert!((importance_of(&engine, id).await - 0.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn out_of_range_adjustment_is_rejected_and_leaves_store_unchanged() {
        let engine = engine().await;
        let id = add(&engine, "f").await;
        let before = importance_of(&engine, id).await;
        // A valid delta followed by an out-of-range one: validation rejects the
        // whole report up-front, so the valid delta must NOT have applied.
        let err = engine
            .apply_cycle_report(&report(
                vec![
                    CycleDelta::AdjustScore {
                        fact_id: id,
                        adjustment: 2,
                    },
                    CycleDelta::AdjustScore {
                        fact_id: id,
                        adjustment: 5, // > ±2
                    },
                ],
                vec![id],
            ))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Cycle(CycleError::AdjustmentOutOfRange { .. })
        ));
        assert!(
            (importance_of(&engine, id).await - before).abs() < 1e-9,
            "store must be unchanged after a rejected report"
        );
        // The watermark must not have advanced either.
        assert!(
            engine
                .storage()
                .get_config("last_dream_cycle_at")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn unknown_fact_reference_rejects_report() {
        let engine = engine().await;
        let err = engine
            .apply_cycle_report(&report(
                vec![CycleDelta::AdjustScore {
                    fact_id: 999,
                    adjustment: 1,
                }],
                vec![],
            ))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Cycle(CycleError::UnknownFact(999))
        ));
    }

    #[tokio::test]
    async fn quarantine_expires_and_marks_but_row_survives() {
        let engine = engine().await;
        let id = add(&engine, "bad fact").await;
        engine
            .apply_cycle_report(&report(
                vec![CycleDelta::Quarantine {
                    fact_id: id,
                    reason: "explicit correction".into(),
                }],
                vec![id],
            ))
            .await
            .unwrap();
        let fact = engine.storage().get_fact(id).await.unwrap();
        assert!(fact.t_expired.is_some(), "quarantine soft-expires the fact");
        assert_eq!(fact.metadata["quarantine"]["reason"], "explicit correction");
        // Quarantining an already-expired fact is rejected.
        let err = engine
            .apply_cycle_report(&report(
                vec![CycleDelta::Quarantine {
                    fact_id: id,
                    reason: "again".into(),
                }],
                vec![],
            ))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Cycle(CycleError::AlreadyExpired(_))
        ));
    }

    #[tokio::test]
    async fn quarantine_is_distinguishable_from_forgetting_in_explain_fact() {
        use crate::inspect::{ExpiredReason, FactState};
        let engine = engine().await;
        let id = add(&engine, "to quarantine").await;
        engine
            .apply_cycle_report(&report(
                vec![CycleDelta::Quarantine {
                    fact_id: id,
                    reason: "explicit correction".into(),
                }],
                vec![id],
            ))
            .await
            .unwrap();
        let explanation = engine.explain_fact(id).await.unwrap();
        assert_eq!(
            explanation.state,
            FactState::Expired {
                reason: ExpiredReason::Quarantined
            },
            "explain_fact must report a quarantined fact as Quarantined, not Unknown/Forgotten"
        );
    }

    #[tokio::test]
    async fn supersede_expires_old_and_creates_edge() {
        let engine = engine().await;
        let old = add(&engine, "old fact").await;
        let new = add(&engine, "new fact").await;
        engine
            .apply_cycle_report(&report(
                vec![CycleDelta::Supersede {
                    old_id: old,
                    new_id: new,
                }],
                vec![old, new],
            ))
            .await
            .unwrap();
        // old expired, new still active
        let old_f = engine.storage().get_fact(old).await.unwrap();
        let new_f = engine.storage().get_fact(new).await.unwrap();
        assert!(old_f.t_expired.is_some());
        assert!(new_f.t_expired.is_none());
        // a "supersedes" edge new -> old exists
        let edges = engine
            .storage()
            .list_active_edges_by_source(new)
            .await
            .unwrap();
        assert!(
            edges
                .iter()
                .any(|e| e.relation_type == "supersedes" && e.target_fact_id == old),
            "expected a supersedes edge new -> old"
        );
    }

    /// A `NewFact` with `t_created` inside the cycle window, used to assemble a
    /// `Synthesize` delta. Helper to keep the merge tests focused on behavior.
    fn synthetic(content: &str) -> NewFact {
        NewFact {
            content: content.into(),
            content_hash: String::new(),
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            fact_type: FactType::Semantic,
            t_created: "2026-06-16T00:30:00Z".parse().unwrap(),
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            base_importance: 0.5,
            access_count: 0,
            last_accessed: "2026-06-16T00:30:00Z".parse().unwrap(),
            metadata: serde_json::json!({}),
            scope_id: 1,
            is_pinned: false,
        }
    }

    /// The A1b merge primitive (happy path): a `Synthesize` report inserts the summary
    /// fact, expires **every** source, creates a `"supersedes"` edge `synthetic → src`
    /// for each, and records one `lineage` row mapping the synthetic to all sources.
    #[tokio::test]
    async fn synthesize_inserts_summary_expires_sources_and_creates_edges_and_lineage() {
        let engine = engine().await;
        let s1 = add(&engine, "source one").await;
        let s2 = add(&engine, "source two").await;
        let res = engine
            .apply_cycle_report(&report(
                vec![CycleDelta::Synthesize {
                    sources: vec![s1, s2],
                    new_fact: synthetic("merged summary"),
                }],
                vec![s1, s2],
            ))
            .await
            .unwrap();

        assert_eq!(res.synthesized, 1, "one Synthesize delta applied");
        assert_eq!(
            res.synthesized_fact_ids.len(),
            1,
            "one synthetic id reported"
        );
        let synth_id = res.synthesized_fact_ids[0];

        // The synthetic exists and is active; both sources are expired.
        let synth = engine.storage().get_fact(synth_id).await.unwrap();
        let f1 = engine.storage().get_fact(s1).await.unwrap();
        let f2 = engine.storage().get_fact(s2).await.unwrap();
        assert_eq!(synth.content, "merged summary");
        assert!(synth.t_expired.is_none(), "synthetic is active");
        assert!(f1.t_expired.is_some(), "source one expired");
        assert!(f2.t_expired.is_some(), "source two expired");

        // A "supersedes" edge synthetic -> src exists for each source.
        let edges = engine
            .storage()
            .list_active_edges_by_source(synth_id)
            .await
            .unwrap();
        for src in [s1, s2] {
            assert!(
                edges
                    .iter()
                    .any(|e| e.relation_type == "supersedes" && e.target_fact_id == src),
                "expected a supersedes edge synthetic -> {src}"
            );
        }

        // One lineage row maps the synthetic to all its sources (errors if absent).
        let (lineage, _prov) = engine
            .storage()
            .get_lineage_by_wisdom_fact(synth_id)
            .await
            .unwrap();
        assert_eq!(lineage.wisdom_fact_id, synth_id);
        assert_eq!(lineage.source_fact_ids, vec![s1, s2]);
    }

    /// Invariant M for Synthesize: the synthetic is dream-marked and its sources are
    /// expired, so neither re-enters the next cycle's input and the synthetic does not
    /// look like a fresh caller write to the #209 cursor.
    #[tokio::test]
    async fn synthesize_outputs_dream_marked_and_excluded_next_cycle_invariant_m() {
        let engine = engine().await;
        let s1 = add(&engine, "src a").await;
        let s2 = add(&engine, "src b").await;
        let res = engine
            .apply_cycle_report(&report(
                vec![CycleDelta::Synthesize {
                    sources: vec![s1, s2],
                    new_fact: synthetic("merged"),
                }],
                vec![s1, s2],
            ))
            .await
            .unwrap();
        let synth_id = res.synthesized_fact_ids[0];

        // The synthetic carries the dream_cycle marker.
        let marked = engine
            .storage()
            .get_fact(synth_id)
            .await
            .unwrap()
            .metadata
            .get("dream_cycle")
            .is_some();
        assert!(marked, "synthetic merge fact must be dream-marked");

        // The crisp invariant: nothing the cycle produced looks like a caller write
        // (synthetic marked; both sources expired, so not active-unpinned-unmarked).
        let max_caller = engine.storage().max_caller_written_fact_id().await.unwrap();
        assert_eq!(
            max_caller, None,
            "synthetic must not look like a caller write; sources are expired"
        );

        // The in-window synthetic is not re-selected as undreamt input next cycle.
        let w = meta(vec![]).time_window;
        let undreamt = engine
            .storage()
            .list_undreamt_facts_in_period(w.start, w.end, &[], None)
            .await
            .unwrap();
        assert!(
            !undreamt.iter().any(|f| f.id == synth_id),
            "synthetic must be excluded from the next cycle's input"
        );
    }

    /// A `Synthesize` with no sources is degenerate (use `AddFact`): rejected pre-apply,
    /// nothing written.
    #[tokio::test]
    async fn synthesize_requires_at_least_one_source() {
        let engine = engine().await;
        let err = engine
            .apply_cycle_report(&report(
                vec![CycleDelta::Synthesize {
                    sources: vec![],
                    new_fact: synthetic("orphan"),
                }],
                vec![],
            ))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Cycle(CycleError::SynthesizeNoSources)
        ));
        assert_eq!(
            engine.statistics().await.unwrap().facts.total,
            0,
            "rejected report must not insert the synthetic"
        );
    }

    /// A nonexistent source id is a typed `UnknownFact` (not a raw mid-apply `NotFound`),
    /// and the whole report is rejected: the valid sibling source stays active.
    #[tokio::test]
    async fn synthesize_missing_source_rejected() {
        let engine = engine().await;
        let s1 = add(&engine, "real source").await;
        let err = engine
            .apply_cycle_report(&report(
                vec![CycleDelta::Synthesize {
                    sources: vec![s1, 999_999],
                    new_fact: synthetic("merged"),
                }],
                vec![s1],
            ))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Cycle(CycleError::UnknownFact(999_999))
        ));
        let f = engine.storage().get_fact(s1).await.unwrap();
        assert!(
            f.t_expired.is_none(),
            "rejected report must not expire the valid source"
        );
    }

    /// Merging an already-expired source is rejected (it would clobber the source's
    /// original `t_expired`).
    #[tokio::test]
    async fn synthesize_expired_source_rejected() {
        let engine = engine().await;
        let s1 = add(&engine, "expire me first").await;
        engine
            .apply_cycle_report(&report(
                vec![CycleDelta::Quarantine {
                    fact_id: s1,
                    reason: "x".into(),
                }],
                vec![s1],
            ))
            .await
            .unwrap();
        let err = engine
            .apply_cycle_report(&report(
                vec![CycleDelta::Synthesize {
                    sources: vec![s1],
                    new_fact: synthetic("merged"),
                }],
                vec![],
            ))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Cycle(CycleError::AlreadyExpired(_))
        ));
    }

    /// A wrong-dimension synthetic embedding is rejected pre-apply (parity with
    /// `AddFact`), so no source is expired.
    #[tokio::test]
    async fn synthesize_wrong_dimension_rejected() {
        let engine = engine().await;
        let s1 = add(&engine, "source").await;
        let mut nf = synthetic("merged");
        nf.embedding = vec![0.1; 8]; // != DIM
        let err = engine
            .apply_cycle_report(&report(
                vec![CycleDelta::Synthesize {
                    sources: vec![s1],
                    new_fact: nf,
                }],
                vec![s1],
            ))
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::EmbeddingDimension { .. }));
        let f = engine.storage().get_fact(s1).await.unwrap();
        assert!(
            f.t_expired.is_none(),
            "rejected report must not expire the source"
        );
    }

    /// The lineage provenance date range must span the SOURCES' real creation times,
    /// not the synthetic's creation instant — else downstream consumers think the merge
    /// covers only the cycle moment (#641, Codex). Proof: a synthetic stamped with a
    /// far-future `t_created` still yields a provenance range bounded by its sources.
    #[tokio::test]
    async fn synthesize_lineage_provenance_spans_sources_not_synthetic_instant() {
        let engine = engine().await;
        let s1 = add(&engine, "src one").await; // t_created ~ now
        let s2 = add(&engine, "src two").await;
        let mut nf = synthetic("merged");
        nf.t_created = "2099-01-01T00:00:00Z".parse().unwrap(); // sentinel far from sources
        let res = engine
            .apply_cycle_report(&report(
                vec![CycleDelta::Synthesize {
                    sources: vec![s1, s2],
                    new_fact: nf,
                }],
                vec![s1, s2],
            ))
            .await
            .unwrap();
        let synth = res.synthesized_fact_ids[0];
        let (_lineage, prov) = engine
            .storage()
            .get_lineage_by_wisdom_fact(synth)
            .await
            .unwrap();
        let sentinel = "2099-01-01T00:00:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        assert!(
            prov.date_range_end < sentinel,
            "provenance range must come from the sources, not the synthetic instant"
        );
        assert!(prov.date_range_start <= prov.date_range_end);
    }

    /// Two `Synthesize` deltas naming the same source are rejected at apply: the first
    /// expires it, the second sees it (in-report) expired → `AlreadyExpired`, rolling the
    /// whole report back. This is the invariant that makes `LlmDreamCycle`'s cross-group
    /// dedup load-bearing (#641).
    #[tokio::test]
    async fn synthesize_duplicate_source_across_deltas_is_rejected() {
        let engine = engine().await;
        let s1 = add(&engine, "a").await;
        let s2 = add(&engine, "b").await;
        let s3 = add(&engine, "c").await;
        let err = engine
            .apply_cycle_report(&report(
                vec![
                    CycleDelta::Synthesize {
                        sources: vec![s1, s2],
                        new_fact: synthetic("g1"),
                    },
                    CycleDelta::Synthesize {
                        sources: vec![s2, s3], // s2 reused
                        new_fact: synthetic("g2"),
                    },
                ],
                vec![s1, s2, s3],
            ))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Cycle(CycleError::AlreadyExpired(_))
        ));
    }

    #[tokio::test]
    async fn supersede_missing_target_rejects() {
        let engine = engine().await;
        let old = add(&engine, "old").await;
        let err = engine
            .apply_cycle_report(&report(
                vec![CycleDelta::Supersede {
                    old_id: old,
                    new_id: 12345,
                }],
                vec![],
            ))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Cycle(CycleError::SupersedeMissing(12345))
        ));
    }

    #[tokio::test]
    async fn promote_and_tagoutcome_do_not_deadlock_and_apply() {
        // Regression guard for the non-reentrant-mutex traps: Promote reuses
        // promote_in_conn and TagOutcome inserts on the shared tx — neither
        // re-acquires the write lock.
        let engine = engine().await;
        let id = add(&engine, "promote me").await;
        let res = engine
            .apply_cycle_report(&report(
                vec![
                    CycleDelta::Promote {
                        fact_id: id,
                        provenance: stub_provenance(),
                    },
                    CycleDelta::TagOutcome {
                        fact_id: id,
                        outcome: Outcome::Positive,
                    },
                ],
                vec![id],
            ))
            .await
            .unwrap();
        assert_eq!(res.promoted, 1);
        assert_eq!(res.outcomes_tagged, 1);
        // a pinned wisdom fact now exists
        let counts = engine.get_outcome_counts(id).await.unwrap();
        assert_eq!(counts.positive, 1);
    }

    #[tokio::test]
    async fn add_fact_inserts_and_marks_processed() {
        let engine = engine().await;
        let nf = NewFact {
            content: "derived pattern".into(),
            content_hash: String::new(),
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            fact_type: FactType::Semantic,
            t_created: "2026-06-16T00:30:00Z".parse().unwrap(),
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            base_importance: 0.5,
            access_count: 0,
            last_accessed: "2026-06-16T00:30:00Z".parse().unwrap(),
            metadata: serde_json::json!({}),
            scope_id: 1,
            is_pinned: false,
        };
        let res = engine
            .apply_cycle_report(&report(vec![CycleDelta::AddFact(nf)], vec![]))
            .await
            .unwrap();
        assert_eq!(res.facts_added, 1);
        assert_eq!(res.new_fact_ids.len(), 1);
        // watermark advanced to the window end
        let wm = engine
            .storage()
            .get_config("last_dream_cycle_at")
            .await
            .unwrap()
            .unwrap();
        assert!(wm.starts_with("2026-06-16T01:00:00"));
    }

    /// Invariant M (#209): every fact a cycle *creates or leaves active* — the `AddFact`
    /// synthetic, the promoted wisdom fact, and a Supersede survivor — must be
    /// dream-marked in the apply transaction. Otherwise it looks like a fresh caller
    /// write to the #209 cursor and re-enters the next cycle's input. The crisp proof:
    /// after applying a report that processes every caller fact, NO active unpinned
    /// fact remains unmarked, so `max_caller_written_fact_id()` returns `None`.
    #[tokio::test]
    async fn apply_dream_marks_all_cycle_outputs_invariant_m() {
        let engine = engine().await;
        let a = add(&engine, "source to promote").await;
        let old = add(&engine, "to be superseded").await;
        let new = add(&engine, "supersede survivor").await;

        let nf = NewFact {
            content: "derived pattern".into(),
            content_hash: String::new(),
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            fact_type: FactType::Semantic,
            t_created: "2026-06-16T00:30:00Z".parse().unwrap(),
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            base_importance: 0.5,
            access_count: 0,
            last_accessed: "2026-06-16T00:30:00Z".parse().unwrap(),
            metadata: serde_json::json!({}),
            scope_id: 1,
            is_pinned: false,
        };
        let res = engine
            .apply_cycle_report(&report(
                vec![
                    CycleDelta::Promote {
                        fact_id: a,
                        provenance: stub_provenance(),
                    },
                    CycleDelta::AddFact(nf),
                    CycleDelta::Supersede {
                        old_id: old,
                        new_id: new,
                    },
                ],
                // processed_ids deliberately EXCLUDES `new` (the supersede survivor): the
                // survivor is a cycle *output*, not a processed input, so its marking must
                // come solely from the invariant-M `supersede_new_ids` union — not from
                // being listed here. This isolates the Supersede-escape BLOCKER.
                vec![a, old],
            ))
            .await
            .unwrap();
        assert_eq!(
            res.promoted_fact_ids.len(),
            1,
            "one promote → one promoted id"
        );
        assert_eq!(res.new_fact_ids.len(), 1, "one AddFact → one synthetic id");

        let is_marked = async |id: i64| {
            engine
                .storage()
                .get_fact(id)
                .await
                .unwrap()
                .metadata
                .get("dream_cycle")
                .is_some()
        };
        assert!(
            is_marked(res.promoted_fact_ids[0]).await,
            "promoted fact marked"
        );
        assert!(
            is_marked(res.new_fact_ids[0]).await,
            "AddFact synthetic marked"
        );
        assert!(is_marked(new).await, "supersede survivor marked");
        assert!(
            is_marked(a).await,
            "promoted source (a processed input) marked"
        );

        // The invariant in one assertion: nothing the cycle produced looks like a
        // caller write. (Pre-fix, the AddFact synthetic + supersede survivor would be
        // unmarked, so this would return Some(max(synthetic, survivor)).)
        let max_caller = engine.storage().max_caller_written_fact_id().await.unwrap();
        assert_eq!(
            max_caller, None,
            "no active unpinned unmarked fact may survive a full-coverage cycle apply"
        );
    }

    #[tokio::test]
    async fn add_fact_wrong_dimension_rejected() {
        let engine = engine().await;
        let mut nf = NewFact {
            content: "bad".into(),
            content_hash: String::new(),
            embedding: vec![0.1; 8], // wrong dim
            fact_type: FactType::Semantic,
            t_created: "2026-06-16T00:30:00Z".parse().unwrap(),
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            base_importance: 0.5,
            access_count: 0,
            last_accessed: "2026-06-16T00:30:00Z".parse().unwrap(),
            metadata: serde_json::json!({}),
            scope_id: 1,
            is_pinned: false,
        };
        nf.content = "bad".into();
        let err = engine
            .apply_cycle_report(&report(vec![CycleDelta::AddFact(nf)], vec![]))
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::EmbeddingDimension { .. }));
    }

    /// A client-supplied report (via `memory_apply_cycle_report`) must not bypass the
    /// importance guard the trusted `add_fact` path enforces: an out-of-range
    /// `importance` would otherwise persist (no column CHECK) and poison decay/forget
    /// ranking. The whole report is rejected and nothing is written.
    #[tokio::test]
    async fn add_fact_out_of_range_importance_rejected() {
        let engine = engine().await;
        let nf = NewFact::builder("hostile", vec![0.1, 0.2, 0.3, 0.4], FactType::Semantic)
            .base_importance(5.0) // outside [0, 1]
            .scope_id(1)
            .build();
        let err = engine
            .apply_cycle_report(&report(vec![CycleDelta::AddFact(nf)], vec![]))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Conflict(crate::error::ConflictError::PolicyParameter(_))
        ));
        // Nothing was written — validation runs before any delta is applied.
        let stats = engine.statistics().await.unwrap();
        assert_eq!(
            stats.facts.total, 0,
            "rejected report must not write any fact"
        );
    }

    /// Same boundary, payload-size dimension: an oversized `content` in an `AddFact`
    /// delta is rejected with the same guard `add_fact` uses (issue #572 / L10).
    #[tokio::test]
    async fn add_fact_oversized_content_rejected() {
        let engine = engine().await;
        let huge = "x".repeat(crate::limits::MAX_PAYLOAD_BYTES + 1);
        let nf = NewFact::builder(huge, vec![0.1, 0.2, 0.3, 0.4], FactType::Semantic)
            .scope_id(1)
            .build();
        let err = engine
            .apply_cycle_report(&report(vec![CycleDelta::AddFact(nf)], vec![]))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Conflict(crate::error::ConflictError::PayloadTooLarge { .. })
        ));
    }

    #[tokio::test]
    async fn duplicate_in_report_expiry_is_rejected_with_typed_error() {
        // Two quarantines of the same fact: validation models the in-report state, so
        // the second is a typed AlreadyExpired (not a raw NotFound mid-apply), and the
        // store is left untouched (the fact stays active).
        let engine = engine().await;
        let id = add(&engine, "f").await;
        let err = engine
            .apply_cycle_report(&report(
                vec![
                    CycleDelta::Quarantine {
                        fact_id: id,
                        reason: "first".into(),
                    },
                    CycleDelta::Quarantine {
                        fact_id: id,
                        reason: "second".into(),
                    },
                ],
                vec![id],
            ))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Cycle(CycleError::AlreadyExpired(_))
        ));
        // Untouched: still active.
        let f = engine.storage().get_fact(id).await.unwrap();
        assert!(
            f.t_expired.is_none(),
            "rejected report must not expire the fact"
        );
    }

    #[tokio::test]
    async fn adjust_after_quarantine_in_same_report_is_rejected() {
        // AdjustScore on a fact quarantined earlier in the SAME report must be rejected
        // in validation — otherwise update_base_importance (no t_expired guard) would
        // silently mutate an expired row.
        let engine = engine().await;
        let id = add(&engine, "f").await;
        let err = engine
            .apply_cycle_report(&report(
                vec![
                    CycleDelta::Quarantine {
                        fact_id: id,
                        reason: "x".into(),
                    },
                    CycleDelta::AdjustScore {
                        fact_id: id,
                        adjustment: 2,
                    },
                ],
                vec![id],
            ))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Cycle(CycleError::AlreadyExpired(_))
        ));
    }

    #[tokio::test]
    async fn unknown_processed_id_is_rejected_in_preflight() {
        // A bogus processed_id would otherwise fail mid-apply with a raw NotFound;
        // pre-flight validation rejects it with a typed CycleError and changes nothing.
        let engine = engine().await;
        let id = add(&engine, "f").await;
        let before = importance_of(&engine, id).await;
        let err = engine
            .apply_cycle_report(&report(
                vec![CycleDelta::AdjustScore {
                    fact_id: id,
                    adjustment: 1,
                }],
                vec![id, 999_999], // 999_999 does not exist
            ))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Cycle(CycleError::UnknownFact(999_999))
        ));
        assert!((importance_of(&engine, id).await - before).abs() < 1e-9);
    }
}
