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

use chrono::Utc;
use rusqlite::Connection;

use crate::engine::MemoryEngine;
use crate::error::{CycleError, MemoryError, Result};
use crate::graph::EdgeData;
use crate::store::edges::EdgeStore;
use crate::store::events::EventStore;
use crate::store::facts::FactStore;
use crate::store::schema::{get_config, set_config};
use crate::types::{EventType, NewEdge, NewEvent, PromoteRequest};

#[cfg(feature = "ann")]
use crate::search::strategy::VectorSearchStrategy;

use super::report::{
    ApplyResult, CycleDelta, CycleMetadata, CycleReport, IMPORTANCE_STEP, MAX_ADJUSTMENT,
};

/// Config key holding the RFC3339 high-water mark of the last applied cycle.
const LAST_DREAM_CYCLE_AT: &str = "last_dream_cycle_at";
/// Config key holding the bounded JSON history of recent [`CycleMetadata`].
const DREAM_CYCLE_HISTORY: &str = "dream_cycle_history";
/// How many recent cycles to retain in the history ring (for `prior_reports`).
const DREAM_CYCLE_HISTORY_MAX: usize = 8;
/// `relation_type` of the graph edge written by a [`CycleDelta::Supersede`].
const SUPERSEDES_RELATION: &str = "supersedes";

impl MemoryEngine {
    /// Validate and apply a [`CycleReport`] atomically.
    ///
    /// The deltas are validated against a pre-apply snapshot (existence, score
    /// bounds, active state); if any check fails the method returns a
    /// [`CycleError`] and the store is left **unchanged**. Otherwise every delta
    /// is applied in one transaction, the report's `processed_ids` are stamped
    /// with the dream-cycle marker, and the `last_dream_cycle_at` watermark is set
    /// to the report's `time_window.end` (so facts created during the cycle are not
    /// skipped). HNSW index notification happens after commit, outside the lock.
    ///
    /// Concurrency note: this is single-fire safe (a sequential re-run is a near
    /// no-op via the marker + watermark). Mutual exclusion against a concurrent
    /// writer is out of scope here — see #207 (distributed lock) / #209.
    ///
    /// # Errors
    ///
    /// - [`MemoryError::ReadOnly`] if the engine is read-only.
    /// - [`MemoryError::Cycle`] if any delta fails validation.
    /// - [`MemoryError::EmbeddingDimension`] if an `AddFact`/`Promote` embedding
    ///   does not match the engine dimension.
    /// - [`MemoryError::Database`] on a store failure.
    // One delta-dispatch match makes the body long but cohesive; splitting per-variant
    // helpers would scatter the single-transaction invariant across functions.
    #[allow(clippy::too_many_lines)]
    pub fn apply_cycle_report(&self, report: &CycleReport) -> Result<ApplyResult> {
        // Single lock acquisition for the whole operation (validate + apply).
        let conn = self.write_conn()?;

        // --- Validation pass (read-only, on the held connection) ---
        self.validate_report(&conn, report)?;

        // --- Apply pass (one transaction; rollback on any error) ---
        let now = Utc::now();
        let tx = conn
            .unchecked_transaction()
            .map_err(MemoryError::Database)?;
        let mut result = ApplyResult::default();
        // (fact_id, embedding) pairs to notify HNSW about after commit.
        #[cfg(feature = "ann")]
        let mut to_index: Vec<(i64, Vec<f32>)> = Vec::new();
        // Facts expired this cycle (Quarantine/Supersede) — for post-commit HNSW
        // tombstoning and in-memory graph cleanup.
        let mut expired_ids: Vec<i64> = Vec::new();
        // (new_id, old_id, edge_id) supersede edges — to mirror into the in-memory graph.
        let mut supersede_edges: Vec<(i64, i64, i64)> = Vec::new();
        // Survivor (`new_id`) facts of a Supersede — still active, so they must be
        // dream-marked (invariant M, #209) or they re-enter the next cycle / trip the
        // caller-write cursor. (`old_id` is expired, so it self-excludes.)
        let mut supersede_new_ids: Vec<i64> = Vec::new();

        for delta in &report.deltas {
            match delta {
                CycleDelta::AddFact(nf) => {
                    let id = FactStore::new(&tx, self.embed_dim).insert(nf)?;
                    result.new_fact_ids.push(id);
                    result.facts_added += 1;
                    #[cfg(feature = "ann")]
                    to_index.push((id, nf.embedding.clone()));
                }
                CycleDelta::AdjustScore {
                    fact_id,
                    adjustment,
                } => {
                    let store = FactStore::new(&tx, self.embed_dim);
                    let current = store.get(*fact_id)?.importance;
                    let new_importance = f64::from(*adjustment)
                        .mul_add(IMPORTANCE_STEP, current)
                        .clamp(0.0, 1.0);
                    store.update_importance(*fact_id, new_importance)?;
                    result.scores_adjusted += 1;
                }
                CycleDelta::Quarantine { fact_id, reason } => {
                    let store = FactStore::new(&tx, self.embed_dim);
                    store.expire(*fact_id, now)?;
                    store.merge_metadata(
                        *fact_id,
                        &serde_json::json!({
                            "quarantine": { "reason": reason, "at": now.to_rfc3339() }
                        }),
                    )?;
                    expired_ids.push(*fact_id);
                    result.quarantined += 1;
                }
                CycleDelta::Promote {
                    fact_id,
                    provenance,
                } => {
                    // Build a promotion request from the existing fact's content,
                    // then reuse the shared in-connection promotion pipeline.
                    let source = FactStore::new(&tx, self.embed_dim).get(*fact_id)?;
                    let req = PromoteRequest {
                        content: source.content,
                        fact_type: source.fact_type,
                        embedding: source.embedding.clone(),
                        importance: source.importance,
                        metadata: source.metadata,
                        scope: None,
                        source_fact_ids: vec![*fact_id],
                        provenance: provenance.clone(),
                    };
                    let promoted = self.promote_in_conn(&tx, &req)?;
                    result.promoted += 1;
                    // Invariant M (#209): record the promoted fact's id so it gets
                    // dream-marked below. Captured unconditionally (not just under
                    // `ann`) — the marker must land in every build profile.
                    result.promoted_fact_ids.push(promoted.fact_id);
                    #[cfg(feature = "ann")]
                    to_index.push((promoted.fact_id, source.embedding));
                }
                CycleDelta::TagOutcome { fact_id, outcome } => {
                    let event = NewEvent {
                        timestamp: now,
                        event_type: EventType::OutcomeSignal,
                        payload: serde_json::json!({ "fact_id": fact_id, "outcome": outcome }),
                        source: "dream_cycle".into(),
                        session_id: None,
                        scope_id: 1, // root: outcome signals are cross-scope
                        origin_node_id: "local".into(),
                        sequence_id: 0,
                        created_at: None,
                    };
                    EventStore::new(&tx, &self.upcaster_registry).insert(&event)?;
                    result.outcomes_tagged += 1;
                }
                CycleDelta::Supersede { old_id, new_id } => {
                    let store = FactStore::new(&tx, self.embed_dim);
                    let new_fact = store.get(*new_id)?;
                    store.expire(*old_id, now)?;
                    let edge_id = EdgeStore::new(&tx).insert(&NewEdge {
                        source_fact_id: *new_id,
                        target_fact_id: *old_id,
                        relation_type: SUPERSEDES_RELATION.to_owned(),
                        weight: 1.0,
                        t_created: now,
                        t_expired: None,
                        scope_id: new_fact.scope_id,
                    })?;
                    expired_ids.push(*old_id);
                    supersede_edges.push((*new_id, *old_id, edge_id));
                    supersede_new_ids.push(*new_id);
                    result.superseded += 1;
                }
            }
        }

        // Invariant M (#209): dream-mark not just the cycle's *inputs* (`processed_ids`)
        // but every fact the cycle *creates or leaves active* — AddFact synthetics,
        // promoted wisdom, and Supersede survivors. Otherwise those facts look like a
        // fresh caller write to the #209 cursor (and re-enter the next cycle's input).
        // Every id here is provably present in this transaction (fresh insert or a
        // validate_report-checked live fact), so `merge_metadata` cannot hit NotFound.
        let mut to_mark: Vec<i64> = report.metadata.processed_ids.clone();
        to_mark.extend(&result.new_fact_ids);
        to_mark.extend(&result.promoted_fact_ids);
        to_mark.extend(&supersede_new_ids);
        to_mark.sort_unstable();
        to_mark.dedup();
        if !to_mark.is_empty() {
            FactStore::new(&tx, self.embed_dim).mark_dream_cycled(
                &to_mark,
                report.metadata.cycle_id,
                now,
            )?;
        }
        set_config(
            &tx,
            LAST_DREAM_CYCLE_AT,
            &report.metadata.time_window.end.to_rfc3339(),
        )?;
        Self::append_cycle_history(&tx, &report.metadata)?;

        tx.commit().map_err(MemoryError::Database)?;
        drop(conn); // release the write lock before side-effect notifications

        // Mirror supersede edges into the in-memory graph (other edge-mutating paths
        // — conflict resolution, co-session linking — keep it in sync the same way).
        // Removing the expired fact's stale edges + adding the supersedes link matches
        // `conflict::temporal`'s post-commit pattern.
        if !supersede_edges.is_empty() {
            let mut graph = self.graph.write();
            for (new_id, old_id, edge_id) in &supersede_edges {
                graph.remove_edges_by_fact(*old_id);
                graph.add_edge(
                    *new_id,
                    *old_id,
                    EdgeData {
                        edge_id: *edge_id,
                        relation_type: SUPERSEDES_RELATION.to_owned(),
                        weight: 1.0,
                    },
                );
            }
        }

        #[cfg(feature = "ann")]
        if let Some(ref hnsw) = self.hnsw_strategy {
            for (id, emb) in to_index {
                hnsw.notify_insert(id, &emb);
            }
            // Tombstone expired (quarantined/superseded) facts so the index does not
            // waste candidate slots on rows that retrieval filters out anyway.
            for id in &expired_ids {
                hnsw.notify_expire(*id);
            }
        }
        #[cfg(not(feature = "ann"))]
        let _ = &expired_ids;

        Ok(result)
    }

    /// Read-only pre-apply validation of every delta. Returns the first failure;
    /// runs on the already-held write connection (no separate read guard, which
    /// would deadlock on an in-memory engine).
    fn validate_report(&self, conn: &Connection, report: &CycleReport) -> Result<()> {
        let store = FactStore::new(conn, self.embed_dim);

        // Maps `MemoryError::NotFound` to a typed cycle error; propagates others.
        let require_fact = |id: i64, missing: CycleError| -> Result<crate::types::Fact> {
            match store.get(id) {
                Ok(f) => Ok(f),
                Err(MemoryError::NotFound(_)) => Err(MemoryError::Cycle(missing)),
                Err(e) => Err(e),
            }
        };

        // Deltas apply sequentially, so validation must model state *evolution* across
        // the report — not just the pre-apply snapshot. We track facts expired earlier
        // in this same report so a later delta cannot re-target an already-(in-report-)
        // expired fact (which would otherwise fail mid-apply with an untyped `NotFound`,
        // or — for `AdjustScore`, whose store path has no `t_expired` guard — silently
        // mutate an expired row).
        let mut expired_in_report: std::collections::HashSet<i64> =
            std::collections::HashSet::new();
        // A fact is "expired" for validation if it was already expired in the store OR
        // expired earlier in this report. Free fn (not a closure) so it does not hold a
        // borrow on `expired_in_report` across the later `.insert()` calls.
        #[allow(
            clippy::items_after_statements,
            reason = "fn defined near its only call site for locality; hoisting above the HashSet init would obscure the intent"
        )]
        fn ensure_active(
            f: &crate::types::Fact,
            expired_in_report: &std::collections::HashSet<i64>,
        ) -> Result<()> {
            if f.t_expired.is_some() || expired_in_report.contains(&f.id) {
                return Err(MemoryError::Cycle(CycleError::AlreadyExpired(f.id)));
            }
            Ok(())
        }

        for delta in &report.deltas {
            match delta {
                CycleDelta::AddFact(nf) => {
                    if nf.embedding.len() != self.embed_dim {
                        return Err(MemoryError::EmbeddingDimension {
                            expected: self.embed_dim,
                            actual: nf.embedding.len(),
                        });
                    }
                    // Parity with the trusted `add_fact` ingest path: a report can be
                    // client-supplied (via the `memory_apply_cycle_report` MCP tool), so
                    // an `AddFact` delta must clear the same importance/payload guards.
                    // Without this, a hostile report could write an out-of-range
                    // `importance` (no column CHECK → poisons Ebbinghaus decay globally)
                    // or an oversized `content`/`metadata` that ordinary ingest forbids.
                    Self::validate_importance(Some(nf.importance))?;
                    crate::limits::check_str_size(&nf.content, "fact content")?;
                    crate::limits::check_json_size(&nf.metadata, "fact metadata")?;
                }
                CycleDelta::AdjustScore {
                    fact_id,
                    adjustment,
                } => {
                    if adjustment.abs() > MAX_ADJUSTMENT {
                        return Err(MemoryError::Cycle(CycleError::AdjustmentOutOfRange {
                            fact_id: *fact_id,
                            adjustment: *adjustment,
                        }));
                    }
                    let f = require_fact(*fact_id, CycleError::UnknownFact(*fact_id))?;
                    ensure_active(&f, &expired_in_report)?;
                }
                CycleDelta::Quarantine { fact_id, .. } => {
                    let f = require_fact(*fact_id, CycleError::UnknownFact(*fact_id))?;
                    ensure_active(&f, &expired_in_report)?;
                    expired_in_report.insert(*fact_id);
                }
                CycleDelta::Promote { fact_id, .. } => {
                    let f = require_fact(*fact_id, CycleError::UnknownFact(*fact_id))?;
                    ensure_active(&f, &expired_in_report)?; // cannot promote a fact expired earlier in the report
                }
                CycleDelta::TagOutcome { fact_id, .. } => {
                    // Outcome signals may be recorded on active OR expired facts
                    // (matching `record_outcome`), so no active-state check here.
                    require_fact(*fact_id, CycleError::UnknownFact(*fact_id))?;
                }
                CycleDelta::Supersede { old_id, new_id } => {
                    let old = require_fact(*old_id, CycleError::SupersedeMissing(*old_id))?;
                    ensure_active(&old, &expired_in_report)?;
                    let new = require_fact(*new_id, CycleError::SupersedeMissing(*new_id))?;
                    ensure_active(&new, &expired_in_report)?; // the superseding fact must itself be live
                    expired_in_report.insert(*old_id);
                }
            }
        }

        // `processed_ids` are stamped with the dream-cycle marker during apply; an
        // unknown id there would otherwise abort mid-apply with an untyped `NotFound`.
        for id in &report.metadata.processed_ids {
            require_fact(*id, CycleError::UnknownFact(*id))?;
        }
        Ok(())
    }

    /// Append a cycle's metadata to the bounded history ring (oldest dropped past
    /// [`DREAM_CYCLE_HISTORY_MAX`]). `run_dream_cycle` reads this back to populate
    /// `CycleContext::prior_reports` — the retrieve-before-reflect input.
    fn append_cycle_history(conn: &Connection, metadata: &CycleMetadata) -> Result<()> {
        let mut history: Vec<CycleMetadata> = match get_config(conn, DREAM_CYCLE_HISTORY)? {
            Some(s) => serde_json::from_str(&s)?,
            None => Vec::new(),
        };
        history.push(metadata.clone());
        let len = history.len();
        if len > DREAM_CYCLE_HISTORY_MAX {
            history.drain(0..len - DREAM_CYCLE_HISTORY_MAX);
        }
        set_config(conn, DREAM_CYCLE_HISTORY, &serde_json::to_string(&history)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::report::{CycleMetadata, IdentityOutput, TimeWindow};
    use super::*;
    use crate::traits::EmbeddingProvider;
    use crate::types::{AddFactRequest, FactType, NewFact, Outcome, PromotionProvenance};

    const DIM: usize = 4;

    struct FixedEmbed;
    impl EmbeddingProvider for FixedEmbed {
        fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![0.1, 0.2, 0.3, 0.4])
        }
    }

    fn engine() -> MemoryEngine {
        MemoryEngine::builder(DIM).build().unwrap()
    }

    fn add(engine: &MemoryEngine, content: &str) -> i64 {
        let req = AddFactRequest {
            content: content.into(),
            fact_type: FactType::Episodic,
            source_event_id: None,
            scope: None,
            opts: None,
        };
        engine.add_fact(&req, &FixedEmbed, None).unwrap()
    }

    fn importance_of(engine: &MemoryEngine, id: i64) -> f64 {
        engine
            .with_read(|conn| FactStore::new(conn, DIM).get(id))
            .unwrap()
            .importance
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
            lineage_id: 0,
        }
    }

    #[test]
    fn empty_report_is_ok_noop() {
        let engine = engine();
        let res = engine.apply_cycle_report(&report(vec![], vec![])).unwrap();
        assert_eq!(res, ApplyResult::default());
    }

    #[test]
    fn adjust_score_moves_base_importance_and_clamps() {
        let engine = engine();
        let id = add(&engine, "f"); // default importance 0.5
        engine
            .apply_cycle_report(&report(
                vec![CycleDelta::AdjustScore {
                    fact_id: id,
                    adjustment: 2,
                }],
                vec![id],
            ))
            .unwrap();
        // 0.5 + 2*0.05 = 0.6
        assert!((importance_of(&engine, id) - 0.6).abs() < 1e-9);

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
                .unwrap();
        }
        assert!((importance_of(&engine, id) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn out_of_range_adjustment_is_rejected_and_leaves_store_unchanged() {
        let engine = engine();
        let id = add(&engine, "f");
        let before = importance_of(&engine, id);
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
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Cycle(CycleError::AdjustmentOutOfRange { .. })
        ));
        assert!(
            (importance_of(&engine, id) - before).abs() < 1e-9,
            "store must be unchanged after a rejected report"
        );
        // The watermark must not have advanced either.
        engine
            .with_read(|conn| {
                assert!(crate::store::schema::get_config(conn, "last_dream_cycle_at")?.is_none());
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn unknown_fact_reference_rejects_report() {
        let engine = engine();
        let err = engine
            .apply_cycle_report(&report(
                vec![CycleDelta::AdjustScore {
                    fact_id: 999,
                    adjustment: 1,
                }],
                vec![],
            ))
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Cycle(CycleError::UnknownFact(999))
        ));
    }

    #[test]
    fn quarantine_expires_and_marks_but_row_survives() {
        let engine = engine();
        let id = add(&engine, "bad fact");
        engine
            .apply_cycle_report(&report(
                vec![CycleDelta::Quarantine {
                    fact_id: id,
                    reason: "explicit correction".into(),
                }],
                vec![id],
            ))
            .unwrap();
        let fact = engine
            .with_read(|conn| FactStore::new(conn, DIM).get(id))
            .unwrap();
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
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Cycle(CycleError::AlreadyExpired(_))
        ));
    }

    #[test]
    fn quarantine_is_distinguishable_from_forgetting_in_explain_fact() {
        use crate::inspect::{ExpiredReason, FactState};
        let engine = engine();
        let id = add(&engine, "to quarantine");
        engine
            .apply_cycle_report(&report(
                vec![CycleDelta::Quarantine {
                    fact_id: id,
                    reason: "explicit correction".into(),
                }],
                vec![id],
            ))
            .unwrap();
        let explanation = engine.explain_fact(id).unwrap();
        assert_eq!(
            explanation.state,
            FactState::Expired {
                reason: ExpiredReason::Quarantined
            },
            "explain_fact must report a quarantined fact as Quarantined, not Unknown/Forgotten"
        );
    }

    #[test]
    fn supersede_expires_old_and_creates_edge() {
        let engine = engine();
        let old = add(&engine, "old fact");
        let new = add(&engine, "new fact");
        engine
            .apply_cycle_report(&report(
                vec![CycleDelta::Supersede {
                    old_id: old,
                    new_id: new,
                }],
                vec![old, new],
            ))
            .unwrap();
        // old expired, new still active
        let (old_f, new_f) = engine
            .with_read(|conn| {
                let s = FactStore::new(conn, DIM);
                Ok((s.get(old)?, s.get(new)?))
            })
            .unwrap();
        assert!(old_f.t_expired.is_some());
        assert!(new_f.t_expired.is_none());
        // a "supersedes" edge new -> old exists
        let edges = engine
            .with_read(|conn| crate::store::edges::EdgeStore::new(conn).list_active_by_source(new))
            .unwrap();
        assert!(
            edges
                .iter()
                .any(|e| e.relation_type == "supersedes" && e.target_fact_id == old),
            "expected a supersedes edge new -> old"
        );
    }

    #[test]
    fn supersede_missing_target_rejects() {
        let engine = engine();
        let old = add(&engine, "old");
        let err = engine
            .apply_cycle_report(&report(
                vec![CycleDelta::Supersede {
                    old_id: old,
                    new_id: 12345,
                }],
                vec![],
            ))
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Cycle(CycleError::SupersedeMissing(12345))
        ));
    }

    #[test]
    fn promote_and_tagoutcome_do_not_deadlock_and_apply() {
        // Regression guard for the non-reentrant-mutex traps: Promote reuses
        // promote_in_conn and TagOutcome inserts on the shared tx — neither
        // re-acquires the write lock.
        let engine = engine();
        let id = add(&engine, "promote me");
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
            .unwrap();
        assert_eq!(res.promoted, 1);
        assert_eq!(res.outcomes_tagged, 1);
        // a pinned wisdom fact now exists
        let counts = engine.get_outcome_counts(id).unwrap();
        assert_eq!(counts.positive, 1);
    }

    #[test]
    fn add_fact_inserts_and_marks_processed() {
        let engine = engine();
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
            importance: 0.5,
            access_count: 0,
            last_accessed: "2026-06-16T00:30:00Z".parse().unwrap(),
            metadata: serde_json::json!({}),
            scope_id: 1,
            is_pinned: false,
        };
        let res = engine
            .apply_cycle_report(&report(vec![CycleDelta::AddFact(nf)], vec![]))
            .unwrap();
        assert_eq!(res.facts_added, 1);
        assert_eq!(res.new_fact_ids.len(), 1);
        // watermark advanced to the window end
        engine
            .with_read(|conn| {
                let wm = crate::store::schema::get_config(conn, "last_dream_cycle_at")?.unwrap();
                assert!(wm.starts_with("2026-06-16T01:00:00"));
                Ok(())
            })
            .unwrap();
    }

    /// Invariant M (#209): every fact a cycle *creates or leaves active* — the AddFact
    /// synthetic, the promoted wisdom fact, and a Supersede survivor — must be
    /// dream-marked in the apply transaction. Otherwise it looks like a fresh caller
    /// write to the #209 cursor and re-enters the next cycle's input. The crisp proof:
    /// after applying a report that processes every caller fact, NO active unpinned
    /// fact remains unmarked, so `max_caller_written_fact_id()` returns `None`.
    #[test]
    fn apply_dream_marks_all_cycle_outputs_invariant_m() {
        let engine = engine();
        let a = add(&engine, "source to promote");
        let old = add(&engine, "to be superseded");
        let new = add(&engine, "supersede survivor");

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
            importance: 0.5,
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
            .unwrap();
        assert_eq!(
            res.promoted_fact_ids.len(),
            1,
            "one promote → one promoted id"
        );
        assert_eq!(res.new_fact_ids.len(), 1, "one AddFact → one synthetic id");

        let is_marked = |id: i64| {
            engine
                .with_read(|conn| {
                    Ok(FactStore::new(conn, DIM)
                        .get(id)?
                        .metadata
                        .get("dream_cycle")
                        .is_some())
                })
                .unwrap()
        };
        assert!(is_marked(res.promoted_fact_ids[0]), "promoted fact marked");
        assert!(is_marked(res.new_fact_ids[0]), "AddFact synthetic marked");
        assert!(is_marked(new), "supersede survivor marked");
        assert!(is_marked(a), "promoted source (a processed input) marked");

        // The invariant in one assertion: nothing the cycle produced looks like a
        // caller write. (Pre-fix, the AddFact synthetic + supersede survivor would be
        // unmarked, so this would return Some(max(synthetic, survivor)).)
        let max_caller = engine
            .with_read(|conn| FactStore::new(conn, DIM).max_caller_written_fact_id())
            .unwrap();
        assert_eq!(
            max_caller, None,
            "no active unpinned unmarked fact may survive a full-coverage cycle apply"
        );
    }

    #[test]
    fn add_fact_wrong_dimension_rejected() {
        let engine = engine();
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
            importance: 0.5,
            access_count: 0,
            last_accessed: "2026-06-16T00:30:00Z".parse().unwrap(),
            metadata: serde_json::json!({}),
            scope_id: 1,
            is_pinned: false,
        };
        nf.content = "bad".into();
        let err = engine
            .apply_cycle_report(&report(vec![CycleDelta::AddFact(nf)], vec![]))
            .unwrap_err();
        assert!(matches!(err, MemoryError::EmbeddingDimension { .. }));
    }

    /// A client-supplied report (via `memory_apply_cycle_report`) must not bypass the
    /// importance guard the trusted `add_fact` path enforces: an out-of-range
    /// `importance` would otherwise persist (no column CHECK) and poison decay/forget
    /// ranking. The whole report is rejected and nothing is written.
    #[test]
    fn add_fact_out_of_range_importance_rejected() {
        let engine = engine();
        let nf = NewFact::builder("hostile", vec![0.1, 0.2, 0.3, 0.4], FactType::Semantic)
            .importance(5.0) // outside [0, 1]
            .scope_id(1)
            .build();
        let err = engine
            .apply_cycle_report(&report(vec![CycleDelta::AddFact(nf)], vec![]))
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Conflict(crate::error::ConflictError::PolicyParameter(_))
        ));
        // Nothing was written — validation runs before any delta is applied.
        let stats = engine.statistics().unwrap();
        assert_eq!(
            stats.facts.total, 0,
            "rejected report must not write any fact"
        );
    }

    /// Same boundary, payload-size dimension: an oversized `content` in an `AddFact`
    /// delta is rejected with the same guard `add_fact` uses (issue #572 / L10).
    #[test]
    fn add_fact_oversized_content_rejected() {
        let engine = engine();
        let huge = "x".repeat(crate::limits::MAX_PAYLOAD_BYTES + 1);
        let nf = NewFact::builder(huge, vec![0.1, 0.2, 0.3, 0.4], FactType::Semantic)
            .scope_id(1)
            .build();
        let err = engine
            .apply_cycle_report(&report(vec![CycleDelta::AddFact(nf)], vec![]))
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Conflict(crate::error::ConflictError::PayloadTooLarge { .. })
        ));
    }

    #[test]
    fn duplicate_in_report_expiry_is_rejected_with_typed_error() {
        // Two quarantines of the same fact: validation models the in-report state, so
        // the second is a typed AlreadyExpired (not a raw NotFound mid-apply), and the
        // store is left untouched (the fact stays active).
        let engine = engine();
        let id = add(&engine, "f");
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
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Cycle(CycleError::AlreadyExpired(_))
        ));
        // Untouched: still active.
        let f = engine
            .with_read(|conn| FactStore::new(conn, DIM).get(id))
            .unwrap();
        assert!(
            f.t_expired.is_none(),
            "rejected report must not expire the fact"
        );
    }

    #[test]
    fn adjust_after_quarantine_in_same_report_is_rejected() {
        // AdjustScore on a fact quarantined earlier in the SAME report must be rejected
        // in validation — otherwise update_importance (no t_expired guard) would
        // silently mutate an expired row.
        let engine = engine();
        let id = add(&engine, "f");
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
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Cycle(CycleError::AlreadyExpired(_))
        ));
    }

    #[test]
    fn unknown_processed_id_is_rejected_in_preflight() {
        // A bogus processed_id would otherwise fail mid-apply with a raw NotFound;
        // pre-flight validation rejects it with a typed CycleError and changes nothing.
        let engine = engine();
        let id = add(&engine, "f");
        let before = importance_of(&engine, id);
        let err = engine
            .apply_cycle_report(&report(
                vec![CycleDelta::AdjustScore {
                    fact_id: id,
                    adjustment: 1,
                }],
                vec![id, 999_999], // 999_999 does not exist
            ))
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Cycle(CycleError::UnknownFact(999_999))
        ));
        assert!((importance_of(&engine, id) - before).abs() < 1e-9);
    }
}
