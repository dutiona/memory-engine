use crate::engine::MemoryEngine;
use crate::error::Result;
use crate::search::hybrid::{SearchQuery, SearchResult};
use crate::store::facts::FactStore;
use crate::traits::{
    ConsolidationConfig, ConsolidationStats, ForgetPolicy, PruneStats, SummaryGenerator,
};
use crate::types::{CycleReport, Fact, PromoteRequest, PromotionResult};

// Re-import trait types used in public API signatures
pub use crate::traits::{DreamCycle, InsightStream};
pub use crate::types::Insight;

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
    pub fn query(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        self.engine.query(query)
    }

    /// List all active (non-expired) facts, optionally limited.
    ///
    /// # Errors
    ///
    /// Returns an error if the store read fails.
    pub fn list_active_facts(&self, limit: Option<usize>) -> Result<Vec<Fact>> {
        self.engine
            .with_read(|conn| FactStore::new(conn, self.engine.embed_dim).list_active(limit))
    }

    /// Retrieve a single fact by ID.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if no active fact with this ID exists.
    pub fn get_fact(&self, id: i64) -> Result<Fact> {
        self.engine
            .with_read(|conn| FactStore::new(conn, self.engine.embed_dim).get(id))
    }

    /// Run engine-internal consolidation (dedup → cluster → global summaries).
    ///
    /// Delegates to `MemoryEngine::consolidate()`, which manages its own locks.
    ///
    /// # Errors
    ///
    /// Returns an error if consolidation fails.
    pub fn consolidate(
        &self,
        generator: &dyn SummaryGenerator,
        config: &ConsolidationConfig,
    ) -> Result<ConsolidationStats> {
        self.engine.consolidate(generator, config)
    }

    /// Run Ebbinghaus decay + pruning on stale facts.
    ///
    /// Delegates to `MemoryEngine::forget()`, which manages its own locks.
    ///
    /// # Errors
    ///
    /// Returns an error if the forget operation fails.
    pub fn forget(&self, policy: &ForgetPolicy) -> Result<PruneStats> {
        self.engine.forget(policy)
    }

    /// Atomically promote a fact to wisdom with lineage tracking.
    ///
    /// Inserts the promoted fact and its lineage record in a single `SQLite`
    /// savepoint. The provenance envelope is serialized into the fact's
    /// metadata under the `"promotion_provenance"` key.
    ///
    /// # Errors
    ///
    /// Returns an error if the promotion fails (dimension mismatch, DB error, etc.).
    pub fn promote(&self, req: &PromoteRequest) -> Result<PromotionResult> {
        self.engine.promote_with_lineage(req)
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

    /// Run a `DreamCycle` using a capability-restricted [`DreamContext`].
    ///
    /// Creates the context, delegates to `cycle.run()`, returns the report.
    /// Verifies write access is available before creating the context,
    /// since `DreamCycle` may need to promote facts.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine is read-only.
    /// Returns an error if the cycle's `run()` fails.
    pub fn run_dream_cycle(&self, cycle: &dyn DreamCycle) -> Result<CycleReport> {
        // Verify write access without holding the lock
        {
            let _guard = self.write_conn()?;
        }
        let ctx = DreamContext::new(self);
        cycle.run(&ctx)
    }

    /// Atomic promotion: insert promoted fact + lineage record in one savepoint.
    ///
    /// Reuses `FactStore::insert()` (the same store-level helper used by
    /// `add_fact` and `add_facts_batch`) to avoid a divergent insert pipeline.
    /// Wraps fact insert + lineage insert in a single savepoint for atomicity.
    pub(crate) fn promote_with_lineage(&self, req: &PromoteRequest) -> Result<PromotionResult> {
        use crate::error::MemoryError;
        use crate::store::lineage::LineageStore;
        use crate::types::{NewFact, NewLineageRecord};
        use chrono::Utc;

        // Validate embedding dimension
        if req.embedding.len() != self.embed_dim {
            return Err(MemoryError::EmbeddingDimension {
                expected: self.embed_dim,
                actual: req.embedding.len(),
            });
        }

        // Ensure metadata is an object for provenance injection
        let mut metadata = match req.metadata.clone() {
            serde_json::Value::Object(map) => serde_json::Value::Object(map),
            _ => serde_json::json!({}),
        };

        // Inject provenance envelope into metadata (lineage_id is skip_serializing,
        // so only the descriptive fields are stored — the lineage table is authoritative
        // for the lineage_id → source_fact_ids mapping).
        if let serde_json::Value::Object(ref mut map) = metadata {
            map.insert(
                "promotion_provenance".to_owned(),
                serde_json::to_value(&req.provenance)
                    .map_err(|e| MemoryError::Internal(format!("serialize provenance: {e}")))?,
            );
        }

        let now = Utc::now();

        // Embed HNSW copy before acquiring write lock (for post-lock notification)
        #[cfg(feature = "ann")]
        let emb_copy = req.embedding.clone();

        // Acquire write lock and insert both in a savepoint
        let mut conn = self.write_conn()?;

        // Resolve scope within the write lock (same pattern as add_fact)
        let scope_id = match &req.scope {
            Some(path) => self.ensure_scope_with_conn(&conn, path)?,
            None => 1, // root scope
        };

        let new_fact = NewFact {
            content: req.content.clone(),
            content_hash: String::new(), // FactStore::insert computes this via blake3
            embedding: req.embedding.clone(),
            fact_type: req.fact_type.clone(),
            t_created: now,
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            importance: req.importance,
            access_count: 0,
            last_accessed: now,
            metadata,
            scope_id,
            is_pinned: true, // promoted wisdom is pinned (unforgettable)
        };

        let sp = conn.savepoint().map_err(MemoryError::Database)?;

        let fact_id = FactStore::new(&sp, self.embed_dim).insert(&new_fact)?;

        let lineage_record = NewLineageRecord {
            wisdom_fact_id: fact_id,
            source_fact_ids: req.source_fact_ids.clone(),
        };
        let lineage_id = LineageStore::new(&sp).insert(&lineage_record, &req.provenance)?;

        sp.commit().map_err(MemoryError::Database)?;

        drop(conn); // release write lock before HNSW notification

        // Notify HNSW if enabled
        #[cfg(feature = "ann")]
        if let Some(ref hnsw) = self.hnsw_strategy {
            hnsw.notify_insert(fact_id, &emb_copy);
        }

        Ok(PromotionResult {
            fact_id,
            lineage_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::MemoryEngine;
    use crate::error::MemoryError;
    use crate::types::{CycleReport, FactType, Insight, PromoteRequest, PromotionProvenance};

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

    impl DreamCycle for NoopCycle {
        fn run(&self, _ctx: &DreamContext) -> Result<CycleReport> {
            Ok(CycleReport {
                facts_evaluated: 0,
                facts_promoted: 0,
                facts_rescored: 0,
                facts_expired: 0,
                promotions: vec![],
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
            lineage_id: 0, // reconstructed from DB row PK on read
        }
    }

    /// Helper: add source facts so that lineage validation passes.
    fn add_source_facts(engine: &MemoryEngine, ids: &[i64]) -> Vec<i64> {
        use crate::traits::EmbeddingProvider;
        use crate::types::AddFactRequest;

        struct FixedEmbed;
        impl EmbeddingProvider for FixedEmbed {
            fn embed(&self, _text: &str) -> Result<Vec<f32>> {
                Ok(vec![0.1, 0.2, 0.3, 0.4])
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
            actual_ids.push(engine.add_fact(&req, &FixedEmbed, None).unwrap());
        }
        actual_ids
    }

    // --- Tests ---

    #[test]
    fn record_insight_delegates_to_stream() {
        let engine = MemoryEngine::open_memory(4).unwrap();
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

    #[test]
    fn run_dream_cycle_creates_context_and_delegates() {
        let engine = MemoryEngine::open_memory(4).unwrap();
        let cycle = NoopCycle;

        let report = engine.run_dream_cycle(&cycle).unwrap();
        assert_eq!(report.facts_evaluated, 0);
        assert_eq!(report.facts_promoted, 0);
    }

    #[test]
    fn promote_with_lineage_rejects_wrong_dimension() {
        let engine = MemoryEngine::open_memory(4).unwrap();
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

        let err = engine.promote_with_lineage(&req).unwrap_err();
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

    #[test]
    fn promote_with_lineage_atomic_insert() {
        let engine = MemoryEngine::open_memory(4).unwrap();
        let source_ids = add_source_facts(&engine, &[1, 2, 3]);

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

        let result = engine.promote_with_lineage(&req).unwrap();
        assert!(result.fact_id > 0, "fact_id should be assigned");
        assert!(result.lineage_id > 0, "lineage_id should be assigned");

        // Verify fact was inserted
        engine
            .with_read(|conn| {
                let store = FactStore::new(conn, 4);
                let fact = store.get(result.fact_id)?;
                assert_eq!(fact.content, "User prefers terse responses");
                assert!(fact.is_pinned, "promoted fact should be pinned");

                // Verify provenance in metadata
                let prov = fact.metadata.get("promotion_provenance");
                assert!(
                    prov.is_some(),
                    "metadata should contain promotion_provenance"
                );
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn promote_with_lineage_preserves_existing_metadata() {
        let engine = MemoryEngine::open_memory(4).unwrap();
        let source_ids = add_source_facts(&engine, &[1, 2]);

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

        let result = engine.promote_with_lineage(&req).unwrap();

        // Verify existing metadata preserved alongside provenance
        engine
            .with_read(|conn| {
                let store = FactStore::new(conn, 4);
                let fact = store.get(result.fact_id)?;
                assert_eq!(fact.metadata["existing"], "data");
                assert!(fact.metadata.get("promotion_provenance").is_some());
                Ok(())
            })
            .unwrap();

        // Verify lineage record via wisdom_fact_id lookup
        engine
            .with_read(|conn| {
                let lineage_store = crate::store::lineage::LineageStore::new(conn);
                let ids = lineage_store.get_source_fact_ids(result.fact_id)?;
                assert_eq!(ids, source_ids);
                Ok(())
            })
            .unwrap();
    }
}
