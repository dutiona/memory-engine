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
use crate::store::lineage::LineageStore;
use crate::store::schema::{get_config, set_config};
use crate::types::{
    EventType, NewEdge, NewEvent, NewLineageRecord, PromoteRequest, PromotionProvenance,
};

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

        // #613 — `AddFact` / `Synthesize` insert PRE-COMPUTED-embedding facts with no
        // live `EmbeddingProvider`, so they cannot stamp the store's identity. Reject
        // them against an un-stamped store (a `Promote` delta self-guards in
        // `promote_in_conn`). Normally a no-op: a populated store applying a cycle
        // already has a recorded identity.
        if report
            .deltas
            .iter()
            .any(|d| matches!(d, CycleDelta::AddFact(_) | CycleDelta::Synthesize { .. }))
        {
            crate::store::embedding_meta::require_present(&tx)?;
        }

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
        // Synthetic merge facts created by a Synthesize — like Supersede survivors they
        // are active cycle *outputs* and must be dream-marked (invariant M). Their
        // expired sources self-exclude.
        let mut synthesize_new_ids: Vec<i64> = Vec::new();

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
                CycleDelta::Synthesize { sources, new_fact } => {
                    // 1. Insert the synthetic summary (id not knowable until now — the
                    //    reason this can't be AddFact + Supersede in one report).
                    let store = FactStore::new(&tx, self.embed_dim);
                    let synth_id = store.insert(new_fact)?;
                    // 2. Expire each source and link synthetic -> source with a
                    //    "supersedes" edge (mirrors the Supersede arm). The edge is
                    //    scoped to the synthetic, the surviving fact. While here, track
                    //    the sources' creation span for the provenance date range.
                    let mut range_start = new_fact.t_created;
                    let mut range_end = new_fact.t_created;
                    for (i, src) in sources.iter().enumerate() {
                        let src_fact = store.get(*src)?;
                        if i == 0 {
                            range_start = src_fact.t_created;
                            range_end = src_fact.t_created;
                        } else {
                            range_start = range_start.min(src_fact.t_created);
                            range_end = range_end.max(src_fact.t_created);
                        }
                        store.expire(*src, now)?;
                        let edge_id = EdgeStore::new(&tx).insert(&NewEdge {
                            source_fact_id: synth_id,
                            target_fact_id: *src,
                            relation_type: SUPERSEDES_RELATION.to_owned(),
                            weight: 1.0,
                            t_created: now,
                            t_expired: None,
                            scope_id: new_fact.scope_id,
                        })?;
                        expired_ids.push(*src);
                        supersede_edges.push((synth_id, *src, edge_id));
                    }
                    // 3. One lineage row maps the synthetic to all its sources, so the
                    //    knowledge-base can trace the merge (parity with promotion's
                    //    provenance chain). The date range spans the **sources'** real
                    //    creation times (not the cycle instant), so downstream provenance
                    //    consumers see the period the merge actually covers.
                    //    `representative_ids` is capped at the first few per its
                    //    quick-review contract; lineage holds the full set.
                    let provenance = PromotionProvenance {
                        source_count: u32::try_from(sources.len()).unwrap_or(u32::MAX),
                        session_count: 0,
                        date_range_start: range_start,
                        date_range_end: range_end,
                        confidence: 1.0,
                        method_version: "synthesize-v1".to_owned(),
                        representative_ids: sources.iter().take(5).copied().collect(),
                        lineage_id: 0,
                    };
                    LineageStore::new(&tx).insert(
                        &NewLineageRecord {
                            wisdom_fact_id: synth_id,
                            source_fact_ids: sources.clone(),
                        },
                        &provenance,
                    )?;
                    // 4. Bookkeeping: the synthetic is an active output → dream-mark it
                    //    (invariant M); report it for observability.
                    synthesize_new_ids.push(synth_id);
                    result.synthesized_fact_ids.push(synth_id);
                    result.synthesized += 1;
                    #[cfg(feature = "ann")]
                    to_index.push((synth_id, new_fact.embedding.clone()));
                }
            }
        }

        // Invariant M (#209): dream-mark not just the cycle's *inputs* (`processed_ids`)
        // but every fact the cycle *creates or leaves active* — AddFact synthetics,
        // promoted wisdom, Supersede survivors, and Synthesize merge facts. Otherwise
        // those facts look like a
        // fresh caller write to the #209 cursor (and re-enter the next cycle's input).
        // Every id here is provably present in this transaction (fresh insert or a
        // validate_report-checked live fact), so `merge_metadata` cannot hit NotFound.
        let mut to_mark: Vec<i64> = report.metadata.processed_ids.clone();
        to_mark.extend(&result.new_fact_ids);
        to_mark.extend(&result.promoted_fact_ids);
        to_mark.extend(&supersede_new_ids);
        to_mark.extend(&synthesize_new_ids);
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

    /// Validate a freshly-constructed [`NewFact`] carried by an `AddFact` or
    /// `Synthesize` delta against the same guards the trusted `add_fact` ingest path
    /// enforces. A report can be client-supplied (via `memory_apply_cycle_report`) or
    /// LLM-derived (an `LlmDreamCycle` synthesizing a merge summary, #554), so an
    /// out-of-range `importance` (no column CHECK → poisons Ebbinghaus decay globally)
    /// or an oversized `content`/`metadata` must be rejected pre-apply, not persisted.
    fn validate_new_fact(&self, nf: &crate::types::NewFact) -> Result<()> {
        if nf.embedding.len() != self.embed_dim {
            return Err(MemoryError::EmbeddingDimension {
                expected: self.embed_dim,
                actual: nf.embedding.len(),
            });
        }
        Self::validate_importance(Some(nf.importance))?;
        crate::limits::check_str_size(&nf.content, "fact content")?;
        crate::limits::check_json_size(&nf.metadata, "fact metadata")?;
        Ok(())
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
                CycleDelta::AddFact(nf) => self.validate_new_fact(nf)?,
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
                CycleDelta::Synthesize { sources, new_fact } => {
                    self.validate_new_fact(new_fact)?;
                    if sources.is_empty() {
                        return Err(MemoryError::Cycle(CycleError::SynthesizeNoSources));
                    }
                    // Every source must exist and still be active; expiring an
                    // already-expired (or earlier-in-report-expired) source would
                    // clobber its original `t_expired`. Record each as expired so a
                    // later delta cannot re-target it.
                    for src in sources {
                        let f = require_fact(*src, CycleError::UnknownFact(*src))?;
                        ensure_active(&f, &expired_in_report)?;
                        expired_in_report.insert(*src);
                    }
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

        fn fingerprint(&self) -> crate::types::EmbeddingFingerprint {
            crate::types::EmbeddingFingerprint::new("mock", "test", 4)
        }
    }

    fn engine() -> MemoryEngine {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        // Stamp the embedding identity so apply tests can apply pre-computed-vector
        // deltas (AddFact/Synthesize) — #613 requires a recorded identity for those.
        // This mirrors production, where facts (and thus the identity) exist before a
        // cycle runs. Same fingerprint FixedEmbed records, so a later `add()` is a no-op.
        engine
            .set_config(
                "embedding_meta",
                &serde_json::to_string(&crate::types::EmbeddingFingerprint::new(
                    "mock", "test", DIM,
                ))
                .unwrap(),
            )
            .unwrap();
        engine
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

    #[test]
    fn apply_add_fact_into_unstamped_store_is_rejected() {
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
            importance: 0.5,
            access_count: 0,
            last_accessed: "2026-06-16T00:30:00Z".parse().unwrap(),
            metadata: serde_json::json!({}),
            scope_id: 1,
            is_pinned: false,
        };
        let err = engine
            .apply_cycle_report(&report(vec![CycleDelta::AddFact(nf)], vec![]))
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
            importance: 0.5,
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
    #[test]
    fn synthesize_inserts_summary_expires_sources_and_creates_edges_and_lineage() {
        let engine = engine();
        let s1 = add(&engine, "source one");
        let s2 = add(&engine, "source two");
        let res = engine
            .apply_cycle_report(&report(
                vec![CycleDelta::Synthesize {
                    sources: vec![s1, s2],
                    new_fact: synthetic("merged summary"),
                }],
                vec![s1, s2],
            ))
            .unwrap();

        assert_eq!(res.synthesized, 1, "one Synthesize delta applied");
        assert_eq!(
            res.synthesized_fact_ids.len(),
            1,
            "one synthetic id reported"
        );
        let synth_id = res.synthesized_fact_ids[0];

        // The synthetic exists and is active; both sources are expired.
        let (synth, f1, f2) = engine
            .with_read(|conn| {
                let s = FactStore::new(conn, DIM);
                Ok((s.get(synth_id)?, s.get(s1)?, s.get(s2)?))
            })
            .unwrap();
        assert_eq!(synth.content, "merged summary");
        assert!(synth.t_expired.is_none(), "synthetic is active");
        assert!(f1.t_expired.is_some(), "source one expired");
        assert!(f2.t_expired.is_some(), "source two expired");

        // A "supersedes" edge synthetic -> src exists for each source.
        let edges = engine
            .with_read(|conn| {
                crate::store::edges::EdgeStore::new(conn).list_active_by_source(synth_id)
            })
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
            .with_read(|conn| {
                crate::store::lineage::LineageStore::new(conn).get_by_wisdom_fact(synth_id)
            })
            .unwrap();
        assert_eq!(lineage.wisdom_fact_id, synth_id);
        assert_eq!(lineage.source_fact_ids, vec![s1, s2]);
    }

    /// Invariant M for Synthesize: the synthetic is dream-marked and its sources are
    /// expired, so neither re-enters the next cycle's input and the synthetic does not
    /// look like a fresh caller write to the #209 cursor.
    #[test]
    fn synthesize_outputs_dream_marked_and_excluded_next_cycle_invariant_m() {
        let engine = engine();
        let s1 = add(&engine, "src a");
        let s2 = add(&engine, "src b");
        let res = engine
            .apply_cycle_report(&report(
                vec![CycleDelta::Synthesize {
                    sources: vec![s1, s2],
                    new_fact: synthetic("merged"),
                }],
                vec![s1, s2],
            ))
            .unwrap();
        let synth_id = res.synthesized_fact_ids[0];

        // The synthetic carries the dream_cycle marker.
        let marked = engine
            .with_read(|conn| {
                Ok(FactStore::new(conn, DIM)
                    .get(synth_id)?
                    .metadata
                    .get("dream_cycle")
                    .is_some())
            })
            .unwrap();
        assert!(marked, "synthetic merge fact must be dream-marked");

        // The crisp invariant: nothing the cycle produced looks like a caller write
        // (synthetic marked; both sources expired, so not active-unpinned-unmarked).
        let max_caller = engine
            .with_read(|conn| FactStore::new(conn, DIM).max_caller_written_fact_id())
            .unwrap();
        assert_eq!(
            max_caller, None,
            "synthetic must not look like a caller write; sources are expired"
        );

        // The in-window synthetic is not re-selected as undreamt input next cycle.
        let w = meta(vec![]).time_window;
        let undreamt = engine
            .with_read(|conn| {
                FactStore::new(conn, DIM).list_undreamt_in_period(w.start, w.end, &[], None)
            })
            .unwrap();
        assert!(
            !undreamt.iter().any(|f| f.id == synth_id),
            "synthetic must be excluded from the next cycle's input"
        );
    }

    /// A `Synthesize` with no sources is degenerate (use `AddFact`): rejected pre-apply,
    /// nothing written.
    #[test]
    fn synthesize_requires_at_least_one_source() {
        let engine = engine();
        let err = engine
            .apply_cycle_report(&report(
                vec![CycleDelta::Synthesize {
                    sources: vec![],
                    new_fact: synthetic("orphan"),
                }],
                vec![],
            ))
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Cycle(CycleError::SynthesizeNoSources)
        ));
        assert_eq!(
            engine.statistics().unwrap().facts.total,
            0,
            "rejected report must not insert the synthetic"
        );
    }

    /// A nonexistent source id is a typed `UnknownFact` (not a raw mid-apply `NotFound`),
    /// and the whole report is rejected: the valid sibling source stays active.
    #[test]
    fn synthesize_missing_source_rejected() {
        let engine = engine();
        let s1 = add(&engine, "real source");
        let err = engine
            .apply_cycle_report(&report(
                vec![CycleDelta::Synthesize {
                    sources: vec![s1, 999_999],
                    new_fact: synthetic("merged"),
                }],
                vec![s1],
            ))
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Cycle(CycleError::UnknownFact(999_999))
        ));
        let f = engine
            .with_read(|conn| FactStore::new(conn, DIM).get(s1))
            .unwrap();
        assert!(
            f.t_expired.is_none(),
            "rejected report must not expire the valid source"
        );
    }

    /// Merging an already-expired source is rejected (it would clobber the source's
    /// original `t_expired`).
    #[test]
    fn synthesize_expired_source_rejected() {
        let engine = engine();
        let s1 = add(&engine, "expire me first");
        engine
            .apply_cycle_report(&report(
                vec![CycleDelta::Quarantine {
                    fact_id: s1,
                    reason: "x".into(),
                }],
                vec![s1],
            ))
            .unwrap();
        let err = engine
            .apply_cycle_report(&report(
                vec![CycleDelta::Synthesize {
                    sources: vec![s1],
                    new_fact: synthetic("merged"),
                }],
                vec![],
            ))
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Cycle(CycleError::AlreadyExpired(_))
        ));
    }

    /// A wrong-dimension synthetic embedding is rejected pre-apply (parity with
    /// `AddFact`), so no source is expired.
    #[test]
    fn synthesize_wrong_dimension_rejected() {
        let engine = engine();
        let s1 = add(&engine, "source");
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
            .unwrap_err();
        assert!(matches!(err, MemoryError::EmbeddingDimension { .. }));
        let f = engine
            .with_read(|conn| FactStore::new(conn, DIM).get(s1))
            .unwrap();
        assert!(
            f.t_expired.is_none(),
            "rejected report must not expire the source"
        );
    }

    /// The lineage provenance date range must span the SOURCES' real creation times,
    /// not the synthetic's creation instant — else downstream consumers think the merge
    /// covers only the cycle moment (#641, Codex). Proof: a synthetic stamped with a
    /// far-future `t_created` still yields a provenance range bounded by its sources.
    #[test]
    fn synthesize_lineage_provenance_spans_sources_not_synthetic_instant() {
        let engine = engine();
        let s1 = add(&engine, "src one"); // t_created ~ now
        let s2 = add(&engine, "src two");
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
            .unwrap();
        let synth = res.synthesized_fact_ids[0];
        let (_lineage, prov) = engine
            .with_read(|conn| {
                crate::store::lineage::LineageStore::new(conn).get_by_wisdom_fact(synth)
            })
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
    #[test]
    fn synthesize_duplicate_source_across_deltas_is_rejected() {
        let engine = engine();
        let s1 = add(&engine, "a");
        let s2 = add(&engine, "b");
        let s3 = add(&engine, "c");
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
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Cycle(CycleError::AlreadyExpired(_))
        ));
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

    /// Invariant M (#209): every fact a cycle *creates or leaves active* — the `AddFact`
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
