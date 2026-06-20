//! `impl ConsolidationStore for SqliteBackend` — delegates to [`SummaryStore`]
//! and [`LineageStore`], the concrete SQL owners of the `summaries` and `lineage`
//! tables.
//!
//! **Conn selection rule:**
//! - `insert_*` / `delete_*` → [`super::SqliteBackend::block_write`].
//! - `get_*` / `list_*` / `has_*` / `for_each_*` →
//!   [`super::SqliteBackend::block_read`] / [`super::SqliteBackend::for_each_streamed`].
//!
//! Summary methods that (de)serialize embeddings capture `self.embed_dim` as a
//! `let` binding outside the closure — the `'static` closures cannot borrow `self`.

use async_trait::async_trait;

use super::{SqliteBackend, stream_consumer_dropped};
use crate::error::Result;
use crate::storage::consolidation::ConsolidationStore;
use crate::store::lineage::LineageStore;
use crate::store::summaries::SummaryStore;
use crate::types::{
    ConsolidationLevel, LineageRecord, LineageSnapshotEntry, NewLineageRecord, NewSummary,
    PromotionProvenance, Summary,
};

#[async_trait]
impl ConsolidationStore for SqliteBackend {
    // -------------------------------------------------------------------------
    // summaries
    // -------------------------------------------------------------------------

    // WRITE
    async fn insert_summary(&self, summary: &NewSummary) -> Result<i64> {
        let summary = summary.clone();
        let dim = self.embed_dim;
        self.block_write(move |c| SummaryStore::new(c, dim).insert(&summary))
            .await
    }

    // READ
    async fn get_summary(&self, id: i64) -> Result<Summary> {
        let dim = self.embed_dim;
        self.block_read(move |c| SummaryStore::new(c, dim).get(id))
            .await
    }

    // READ
    async fn list_summaries_by_level(&self, level: &ConsolidationLevel) -> Result<Vec<Summary>> {
        let level = level.clone();
        let dim = self.embed_dim;
        self.block_read(move |c| SummaryStore::new(c, dim).list_by_level(&level))
            .await
    }

    // READ
    async fn list_all_summaries(&self) -> Result<Vec<Summary>> {
        let dim = self.embed_dim;
        self.block_read(move |c| SummaryStore::new(c, dim).list_all())
            .await
    }

    // READ (streaming)
    async fn for_each_summary(
        &self,
        f: &mut (dyn FnMut(Summary) -> Result<()> + Send),
    ) -> Result<()> {
        let dim = self.embed_dim;
        self.for_each_streamed(
            move |conn, tx| {
                SummaryStore::new(conn, dim).for_each(|summary| {
                    tx.blocking_send(summary)
                        .map_err(|_| stream_consumer_dropped())
                })
            },
            f,
        )
        .await
    }

    // WRITE
    async fn delete_summaries_by_level(&self, level: &ConsolidationLevel) -> Result<usize> {
        let level = level.clone();
        let dim = self.embed_dim;
        self.block_write(move |c| SummaryStore::new(c, dim).delete_by_level(&level))
            .await
    }

    // -------------------------------------------------------------------------
    // lineage
    // -------------------------------------------------------------------------

    // WRITE
    async fn insert_lineage(
        &self,
        record: &NewLineageRecord,
        provenance: &PromotionProvenance,
    ) -> Result<i64> {
        let record = record.clone();
        let provenance = provenance.clone();
        self.block_write(move |c| LineageStore::new(c).insert(&record, &provenance))
            .await
    }

    // WRITE
    async fn insert_lineage_raw(&self, entry: &LineageSnapshotEntry) -> Result<()> {
        let entry = entry.clone();
        self.block_write(move |c| LineageStore::new(c).insert_raw(&entry))
            .await
    }

    // READ
    async fn get_lineage_by_wisdom_fact(
        &self,
        wisdom_fact_id: i64,
    ) -> Result<(LineageRecord, PromotionProvenance)> {
        self.block_read(move |c| LineageStore::new(c).get_by_wisdom_fact(wisdom_fact_id))
            .await
    }

    // READ
    async fn get_lineage_source_fact_ids(&self, wisdom_fact_id: i64) -> Result<Vec<i64>> {
        self.block_read(move |c| LineageStore::new(c).get_source_fact_ids(wisdom_fact_id))
            .await
    }

    // WRITE
    async fn delete_lineage(&self, wisdom_fact_id: i64) -> Result<bool> {
        self.block_write(move |c| LineageStore::new(c).delete(wisdom_fact_id))
            .await
    }

    // READ
    async fn has_lineage(&self, wisdom_fact_id: i64) -> Result<bool> {
        self.block_read(move |c| LineageStore::new(c).has_lineage(wisdom_fact_id))
            .await
    }

    // READ (streaming)
    async fn for_each_lineage(
        &self,
        f: &mut (dyn FnMut(LineageSnapshotEntry) -> Result<()> + Send),
    ) -> Result<()> {
        self.for_each_streamed(
            move |conn, tx| {
                LineageStore::new(conn).for_each(|entry| {
                    tx.blocking_send(entry)
                        .map_err(|_| stream_consumer_dropped())
                })
            },
            f,
        )
        .await
    }

    // -------------------------------------------------------------------------
    // Stage A atomic port method
    // -------------------------------------------------------------------------

    // ATOMIC WRITE — validate + apply a CycleReport in one transaction.
    //
    // validate_report runs on the held write connection (avoiding the in-memory
    // self-deadlock a separate read guard would cause) — verbatim from plan §6.6.
    //
    // Promote blocker: `promote_in_conn` calls `ensure_scope_with_conn` which
    // writes to `self.scope_tree` (engine-owned in-memory). Full push-down is
    // blocked until Stage E. For now: Promote uses scope_id=1 (root) always,
    // matching the current `apply_cycle_report` behavior exactly (req.scope is
    // always None in the cycle path).
    #[allow(clippy::too_many_lines)]
    async fn apply_cycle_deltas_atomic(
        &self,
        report: &crate::engine::cycle::CycleReport,
        embed_dim: usize,
        upcaster_registry: &crate::store::upcaster::UpcasterRegistry,
    ) -> Result<(
        crate::engine::cycle::ApplyResult,
        Vec<(i64, i64, i64)>,
        Vec<i64>,
        Vec<(i64, Vec<f32>)>,
    )> {
        use chrono::Utc;

        use crate::engine::cycle::{ApplyResult, CycleDelta, IMPORTANCE_STEP};
        use crate::error::{CycleError, MemoryError};
        use crate::store::edges::EdgeStore;
        use crate::store::events::EventStore;
        use crate::store::facts::FactStore;
        use crate::store::lineage::LineageStore;
        use crate::store::schema::{get_config, set_config};
        use crate::types::{EventType, NewEdge, NewEvent, NewLineageRecord, PromotionProvenance};

        // Config keys — local copies of the private consts in engine/cycle/apply.rs.
        const LAST_DREAM_CYCLE_AT: &str = "last_dream_cycle_at";
        const SUPERSEDES_RELATION: &str = "supersedes";
        const DREAM_CYCLE_HISTORY: &str = "dream_cycle_history";
        const DREAM_CYCLE_HISTORY_MAX: usize = 8;

        let report = report.clone();
        let upcaster_registry = upcaster_registry.clone();

        self.block_write(move |conn| {
            // --- Validation pass (read-only, on the held connection) ---
            // Verbatim from validate_report in apply.rs:364-462
            {
                fn ensure_active(
                    f: &crate::types::Fact,
                    expired_in_report: &std::collections::HashSet<i64>,
                ) -> Result<()> {
                    if f.t_expired.is_some() || expired_in_report.contains(&f.id) {
                        return Err(MemoryError::Cycle(CycleError::AlreadyExpired(f.id)));
                    }
                    Ok(())
                }

                let store = FactStore::new(conn, embed_dim);
                let require_fact = |id: i64, missing: CycleError| -> Result<crate::types::Fact> {
                    match store.get(id) {
                        Ok(f) => Ok(f),
                        Err(MemoryError::NotFound(_)) => Err(MemoryError::Cycle(missing)),
                        Err(e) => Err(e),
                    }
                };

                let mut expired_in_report: std::collections::HashSet<i64> =
                    std::collections::HashSet::new();

                for delta in &report.deltas {
                    match delta {
                        CycleDelta::AddFact(nf) => {
                            // validate_new_fact equivalent
                            if nf.embedding.len() != embed_dim {
                                return Err(MemoryError::EmbeddingDimension {
                                    expected: embed_dim,
                                    actual: nf.embedding.len(),
                                });
                            }
                            // validate_importance
                            if !nf.importance.is_finite() || !(0.0..=1.0).contains(&nf.importance) {
                                return Err(MemoryError::Conflict(
                                    crate::error::ConflictError::PolicyParameter(format!(
                                        "importance must be in [0, 1], got {}",
                                        nf.importance
                                    )),
                                ));
                            }
                            crate::limits::check_str_size(&nf.content, "fact content")?;
                            crate::limits::check_json_size(&nf.metadata, "fact metadata")?;
                        }
                        CycleDelta::AdjustScore {
                            fact_id,
                            adjustment,
                        } => {
                            use crate::engine::cycle::MAX_ADJUSTMENT;
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
                            ensure_active(&f, &expired_in_report)?;
                        }
                        CycleDelta::TagOutcome { fact_id, .. } => {
                            require_fact(*fact_id, CycleError::UnknownFact(*fact_id))?;
                        }
                        CycleDelta::Supersede { old_id, new_id } => {
                            let old = require_fact(*old_id, CycleError::SupersedeMissing(*old_id))?;
                            ensure_active(&old, &expired_in_report)?;
                            let new = require_fact(*new_id, CycleError::SupersedeMissing(*new_id))?;
                            ensure_active(&new, &expired_in_report)?;
                            expired_in_report.insert(*old_id);
                        }
                        CycleDelta::Synthesize { sources, new_fact } => {
                            // validate_new_fact equivalent
                            if new_fact.embedding.len() != embed_dim {
                                return Err(MemoryError::EmbeddingDimension {
                                    expected: embed_dim,
                                    actual: new_fact.embedding.len(),
                                });
                            }
                            if !new_fact.importance.is_finite()
                                || !(0.0..=1.0).contains(&new_fact.importance)
                            {
                                return Err(MemoryError::Conflict(
                                    crate::error::ConflictError::PolicyParameter(format!(
                                        "importance must be in [0, 1], got {}",
                                        new_fact.importance
                                    )),
                                ));
                            }
                            crate::limits::check_str_size(&new_fact.content, "fact content")?;
                            crate::limits::check_json_size(&new_fact.metadata, "fact metadata")?;
                            if sources.is_empty() {
                                return Err(MemoryError::Cycle(CycleError::SynthesizeNoSources));
                            }
                            for src in sources {
                                let f = require_fact(*src, CycleError::UnknownFact(*src))?;
                                ensure_active(&f, &expired_in_report)?;
                                expired_in_report.insert(*src);
                            }
                        }
                    }
                }
                for id in &report.metadata.processed_ids {
                    require_fact(*id, CycleError::UnknownFact(*id))?;
                }
            }

            // --- Apply pass (one transaction) ---
            let now = Utc::now();

            // #613 guard: AddFact / Synthesize carry pre-computed embeddings with no
            // live provider — reject against an un-stamped store.
            if report
                .deltas
                .iter()
                .any(|d| matches!(d, CycleDelta::AddFact(_) | CycleDelta::Synthesize { .. }))
            {
                crate::store::embedding_meta::require_present(conn)?;
            }

            let tx = conn
                .unchecked_transaction()
                .map_err(MemoryError::Database)?;

            let mut result = ApplyResult::default();
            let mut to_index: Vec<(i64, Vec<f32>)> = Vec::new();
            let mut expired_ids: Vec<i64> = Vec::new();
            let mut supersede_edges: Vec<(i64, i64, i64)> = Vec::new();
            let mut supersede_new_ids: Vec<i64> = Vec::new();
            let mut synthesize_new_ids: Vec<i64> = Vec::new();

            for delta in &report.deltas {
                match delta {
                    CycleDelta::AddFact(nf) => {
                        let id = FactStore::new(&tx, embed_dim).insert(nf)?;
                        result.new_fact_ids.push(id);
                        result.facts_added += 1;
                        to_index.push((id, nf.embedding.clone()));
                    }
                    CycleDelta::AdjustScore {
                        fact_id,
                        adjustment,
                    } => {
                        let store = FactStore::new(&tx, embed_dim);
                        let current = store.get(*fact_id)?.importance;
                        let new_importance = f64::from(*adjustment)
                            .mul_add(IMPORTANCE_STEP, current)
                            .clamp(0.0, 1.0);
                        store.update_importance(*fact_id, new_importance)?;
                        result.scores_adjusted += 1;
                    }
                    CycleDelta::Quarantine { fact_id, reason } => {
                        let store = FactStore::new(&tx, embed_dim);
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
                        // Promote blocker: promote_in_conn calls ensure_scope_with_conn
                        // which writes scope_tree (engine-owned in-memory state). In Stage
                        // A we handle the DB side verbatim; scope_id=1 (root) is used
                        // unconditionally, matching the current cycle apply path exactly
                        // (Promote in apply_cycle_report always passes scope: None → 1).
                        let source = FactStore::new(&tx, embed_dim).get(*fact_id)?;

                        // #613/#615 — promotion identity guard
                        crate::store::embedding_meta::require_present(&tx)?;

                        // Inject provenance into metadata
                        let mut metadata = match source.metadata.clone() {
                            serde_json::Value::Object(map) => serde_json::Value::Object(map),
                            _ => serde_json::json!({}),
                        };
                        if let serde_json::Value::Object(ref mut map) = metadata {
                            map.insert(
                                "promotion_provenance".to_owned(),
                                serde_json::to_value(provenance).map_err(|e| {
                                    MemoryError::Internal(format!("serialize provenance: {e}"))
                                })?,
                            );
                        }
                        let prov_clone = provenance.clone();
                        let promote_fact = crate::types::NewFact {
                            content: source.content,
                            content_hash: String::new(),
                            embedding: source.embedding.clone(),
                            fact_type: source.fact_type,
                            t_created: now,
                            t_expired: None,
                            t_valid: None,
                            t_invalid: None,
                            source_event_id: None,
                            importance: source.importance,
                            access_count: 0,
                            last_accessed: now,
                            metadata,
                            scope_id: 1, // root — see Promote blocker note above
                            is_pinned: true,
                        };
                        let promoted_id = FactStore::new(&tx, embed_dim).insert(&promote_fact)?;
                        LineageStore::new(&tx).insert(
                            &NewLineageRecord {
                                wisdom_fact_id: promoted_id,
                                source_fact_ids: vec![*fact_id],
                            },
                            &prov_clone,
                        )?;
                        result.promoted += 1;
                        result.promoted_fact_ids.push(promoted_id);
                        to_index.push((promoted_id, source.embedding));
                    }
                    CycleDelta::TagOutcome { fact_id, outcome } => {
                        let event = NewEvent {
                            timestamp: now,
                            event_type: EventType::OutcomeSignal,
                            payload: serde_json::json!({
                                "fact_id": fact_id,
                                "outcome": outcome
                            }),
                            source: "dream_cycle".into(),
                            session_id: None,
                            scope_id: 1,
                            origin_node_id: "local".into(),
                            sequence_id: 0,
                            created_at: None,
                        };
                        EventStore::new(&tx, &upcaster_registry).insert(&event)?;
                        result.outcomes_tagged += 1;
                    }
                    CycleDelta::Supersede { old_id, new_id } => {
                        let store = FactStore::new(&tx, embed_dim);
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
                        let store = FactStore::new(&tx, embed_dim);
                        let synth_id = store.insert(new_fact)?;
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
                        synthesize_new_ids.push(synth_id);
                        result.synthesized_fact_ids.push(synth_id);
                        result.synthesized += 1;
                        to_index.push((synth_id, new_fact.embedding.clone()));
                    }
                }
            }

            // Invariant M (#209): dream-mark every fact the cycle creates or leaves active.
            let mut to_mark: Vec<i64> = report.metadata.processed_ids.clone();
            to_mark.extend(&result.new_fact_ids);
            to_mark.extend(&result.promoted_fact_ids);
            to_mark.extend(&supersede_new_ids);
            to_mark.extend(&synthesize_new_ids);
            to_mark.sort_unstable();
            to_mark.dedup();
            if !to_mark.is_empty() {
                FactStore::new(&tx, embed_dim).mark_dream_cycled(
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

            // append_cycle_history verbatim from apply.rs:467-479
            {
                let mut history: Vec<crate::engine::cycle::CycleMetadata> =
                    match get_config(&tx, DREAM_CYCLE_HISTORY)? {
                        Some(s) => serde_json::from_str(&s)?,
                        None => Vec::new(),
                    };
                history.push(report.metadata);
                let len = history.len();
                if len > DREAM_CYCLE_HISTORY_MAX {
                    history.drain(0..len - DREAM_CYCLE_HISTORY_MAX);
                }
                set_config(&tx, DREAM_CYCLE_HISTORY, &serde_json::to_string(&history)?)?;
            }

            tx.commit().map_err(MemoryError::Database)?;
            Ok((result, supersede_edges, expired_ids, to_index))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;

    use super::super::SqliteBackend;
    use crate::error::MemoryError;
    use crate::pool::ConnectionPool;
    use crate::storage::consolidation::ConsolidationStore;
    use crate::store::facts::FactStore;
    use crate::store::upcaster::UpcasterRegistry;
    use crate::types::{
        ConsolidationLevel, FactType, LineageSnapshotEntry, NewFact, NewLineageRecord, NewSummary,
        PromotionProvenance,
    };

    const DIM: usize = 4;

    fn backend() -> SqliteBackend {
        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());
        SqliteBackend::from_pool(pool, Arc::new(UpcasterRegistry::new()))
    }

    fn make_summary(level: ConsolidationLevel) -> NewSummary {
        NewSummary {
            content: "test summary".into(),
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            level,
            source_fact_ids: vec![1, 2],
            created_at: Utc::now(),
            scope_id: 1,
        }
    }

    fn test_provenance() -> PromotionProvenance {
        PromotionProvenance {
            source_count: 2,
            session_count: 1,
            date_range_start: chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            date_range_end: chrono::DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            confidence: 0.9,
            method_version: "dreamcycle-v1".into(),
            representative_ids: vec![1, 2],
            lineage_id: 0,
        }
    }

    /// Seed two facts so FK constraints on lineage are satisfied.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "write guard held across seed loop"
    )]
    fn seeded_with_facts() -> SqliteBackend {
        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());
        {
            let conn = pool.write();
            let store = FactStore::new(&conn, DIM);
            for i in 0..2 {
                store
                    .insert(&NewFact {
                        content: format!("source fact {i}"),
                        content_hash: format!("h{i}"),
                        embedding: vec![0.1, 0.2, 0.3, 0.4],
                        fact_type: FactType::Semantic,
                        t_created: Utc::now(),
                        t_expired: None,
                        t_valid: None,
                        t_invalid: None,
                        source_event_id: None,
                        scope_id: 1,
                        importance: 0.5,
                        access_count: 0,
                        last_accessed: Utc::now(),
                        metadata: serde_json::json!({}),
                        is_pinned: false,
                    })
                    .unwrap();
            }
        }
        SqliteBackend::from_pool(pool, Arc::new(UpcasterRegistry::new()))
    }

    // -------------------------------------------------------------------------
    // summaries
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn summary_insert_then_list_by_level() {
        let be = backend();
        let id = be
            .insert_summary(&make_summary(ConsolidationLevel::Cluster))
            .await
            .unwrap();
        assert!(id > 0);

        let list = be
            .list_summaries_by_level(&ConsolidationLevel::Cluster)
            .await
            .unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id);
        assert_eq!(list[0].level, ConsolidationLevel::Cluster);

        // Other level returns empty.
        let empty = be
            .list_summaries_by_level(&ConsolidationLevel::Global)
            .await
            .unwrap();
        assert!(empty.is_empty());
    }

    #[tokio::test]
    async fn summary_get_round_trip() {
        let be = backend();
        let s = make_summary(ConsolidationLevel::Local);
        let id = be.insert_summary(&s).await.unwrap();
        let got = be.get_summary(id).await.unwrap();
        assert_eq!(got.content, s.content);
        assert_eq!(got.embedding, s.embedding);
        assert_eq!(got.level, ConsolidationLevel::Local);
    }

    #[tokio::test]
    async fn summary_list_all_and_delete_by_level() {
        let be = backend();
        be.insert_summary(&make_summary(ConsolidationLevel::Local))
            .await
            .unwrap();
        be.insert_summary(&make_summary(ConsolidationLevel::Cluster))
            .await
            .unwrap();

        let all = be.list_all_summaries().await.unwrap();
        assert_eq!(all.len(), 2);

        let deleted = be
            .delete_summaries_by_level(&ConsolidationLevel::Local)
            .await
            .unwrap();
        assert_eq!(deleted, 1);

        let remaining = be.list_all_summaries().await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].level, ConsolidationLevel::Cluster);
    }

    #[tokio::test]
    async fn for_each_summary_parity() {
        let be = backend();
        be.insert_summary(&make_summary(ConsolidationLevel::Local))
            .await
            .unwrap();
        be.insert_summary(&make_summary(ConsolidationLevel::Cluster))
            .await
            .unwrap();

        let expected = be.list_all_summaries().await.unwrap();
        let mut streamed: Vec<i64> = Vec::new();
        be.for_each_summary(&mut |s| {
            streamed.push(s.id);
            Ok(())
        })
        .await
        .unwrap();

        let expected_ids: Vec<i64> = expected.iter().map(|s| s.id).collect();
        assert_eq!(streamed, expected_ids);
    }

    #[tokio::test]
    async fn for_each_summary_early_exit() {
        let be = backend();
        for _ in 0..5 {
            be.insert_summary(&make_summary(ConsolidationLevel::Local))
                .await
                .unwrap();
        }
        let mut count = 0usize;
        let err = be
            .for_each_summary(&mut |_| {
                count += 1;
                if count == 2 {
                    return Err(MemoryError::Internal("stop at 2".into()));
                }
                Ok(())
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::Internal(_)));
        assert_eq!(count, 2);
    }

    // -------------------------------------------------------------------------
    // lineage
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn lineage_insert_then_get() {
        let be = seeded_with_facts();
        // Get fact ids from the seeded backend
        let facts = {
            use crate::storage::graph::FactGraph as _;
            be.list_all_facts().await.unwrap()
        };
        let (wf_id, sf_id) = (facts[0].id, facts[1].id);

        let record = NewLineageRecord {
            wisdom_fact_id: wf_id,
            source_fact_ids: vec![sf_id],
        };
        let lineage_id = be
            .insert_lineage(&record, &test_provenance())
            .await
            .unwrap();
        assert!(lineage_id > 0);

        let (lr, prov) = be.get_lineage_by_wisdom_fact(wf_id).await.unwrap();
        assert_eq!(lr.wisdom_fact_id, wf_id);
        assert_eq!(lr.source_fact_ids, vec![sf_id]);
        assert!((prov.confidence - 0.9).abs() < f64::EPSILON);

        let ids = be.get_lineage_source_fact_ids(wf_id).await.unwrap();
        assert_eq!(ids, vec![sf_id]);
    }

    #[tokio::test]
    async fn lineage_missing_yields_lineage_error() {
        let be = backend();
        let err = be.get_lineage_by_wisdom_fact(9999).await.unwrap_err();
        assert!(
            matches!(err, MemoryError::Lineage(_)),
            "expected Lineage error, got {err:?}"
        );
    }

    #[tokio::test]
    async fn lineage_has_and_delete() {
        let be = seeded_with_facts();
        let facts = {
            use crate::storage::graph::FactGraph as _;
            be.list_all_facts().await.unwrap()
        };
        let (wf_id, sf_id) = (facts[0].id, facts[1].id);

        assert!(!be.has_lineage(wf_id).await.unwrap());
        be.insert_lineage(
            &NewLineageRecord {
                wisdom_fact_id: wf_id,
                source_fact_ids: vec![sf_id],
            },
            &test_provenance(),
        )
        .await
        .unwrap();
        assert!(be.has_lineage(wf_id).await.unwrap());

        let deleted = be.delete_lineage(wf_id).await.unwrap();
        assert!(deleted);
        assert!(!be.has_lineage(wf_id).await.unwrap());

        let not_deleted = be.delete_lineage(wf_id).await.unwrap();
        assert!(!not_deleted);
    }

    #[tokio::test]
    async fn lineage_insert_raw_and_for_each() {
        let be = seeded_with_facts();
        let facts = {
            use crate::storage::graph::FactGraph as _;
            be.list_all_facts().await.unwrap()
        };
        let (wf_id, sf_id) = (facts[0].id, facts[1].id);

        let prov = test_provenance();
        let entry = LineageSnapshotEntry {
            lineage_id: 42,
            wisdom_fact_id: wf_id,
            source_fact_ids: vec![sf_id],
            provenance: prov,
        };
        be.insert_lineage_raw(&entry).await.unwrap();

        let mut collected: Vec<i64> = Vec::new();
        be.for_each_lineage(&mut |e| {
            collected.push(e.wisdom_fact_id);
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(collected, vec![wf_id]);
    }
}
