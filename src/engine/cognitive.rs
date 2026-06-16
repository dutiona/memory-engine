use chrono::{DateTime, Utc};

use crate::engine::MemoryEngine;
use crate::engine::cycle::{CycleContext, CycleMetadata, CycleReport, TimeWindow};
use crate::error::{MemoryError, MigrationError, Result};
use crate::search::hybrid::{SearchQuery, SearchResult};
use crate::store::facts::FactStore;
use crate::store::lineage::LineageStore;
use crate::store::schema::get_config;
use crate::traits::{
    ConsolidationConfig, ConsolidationStats, EmbeddingProvider, ForgetPolicy, PruneStats,
    SummaryGenerator,
};
use crate::types::{Fact, NewFact, NewLineageRecord, PromoteRequest, PromotionResult};

#[cfg(feature = "ann")]
use crate::search::strategy::VectorSearchStrategy;

// Re-import trait types used in public API signatures
pub use crate::traits::{DreamCycle, InsightStream};
pub use crate::types::Insight;

/// Top-level `metadata` key marking a fact captured via the pre-compaction insight
/// flush. Written by the MCP `memory_flush_insights` tool and read by
/// [`MemoryEngine::list_recent_insights`](crate::MemoryEngine::list_recent_insights).
///
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
    /// Returns `MemoryError::NotFound` if no fact with this ID exists.
    pub fn get_fact(&self, id: i64) -> Result<Fact> {
        self.engine
            .with_read(|conn| FactStore::new(conn, self.engine.embed_dim).get(id))
    }

    /// Run engine-internal consolidation (dedup → cluster → global summaries).
    ///
    /// Delegates to `MemoryEngine::consolidate()`, which manages its own locks.
    /// `generator` produces the summary text; `embedder` projects it into the
    /// fact vector space (issue #116).
    ///
    /// # Errors
    ///
    /// Returns an error if consolidation fails.
    pub fn consolidate(
        &self,
        generator: &dyn SummaryGenerator,
        embedder: &dyn EmbeddingProvider,
        config: &ConsolidationConfig,
    ) -> Result<ConsolidationStats> {
        self.engine.consolidate(generator, embedder, config)
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

    /// List active facts in `window` that have not yet been dream-cycled.
    ///
    /// This is a cycle's input-selection query: the metadata `dream_cycle` marker
    /// excludes facts a previous cycle already processed (idempotency). Root scope,
    /// all fact types.
    ///
    /// # Errors
    ///
    /// Returns an error if the store read fails.
    pub fn list_undreamt_in_period(&self, window: TimeWindow) -> Result<Vec<Fact>> {
        self.engine.with_read(|conn| {
            FactStore::new(conn, self.engine.embed_dim).list_undreamt_in_period(
                window.start,
                window.end,
                &[],
                None,
            )
        })
    }

    /// Aggregated outcome counts for a fact (for importance rescoring).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if the fact does not exist, or a store error.
    pub fn outcome_counts(&self, fact_id: i64) -> Result<crate::types::OutcomeCounts> {
        self.engine.get_outcome_counts(fact_id)
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
    pub fn outcome_counts_batch(
        &self,
        fact_ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, crate::types::OutcomeCounts>> {
        self.engine.get_outcome_counts_batch(fact_ids)
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
    pub fn run_dream_cycle(&self, cycle: &dyn DreamCycle) -> Result<CycleReport> {
        // Verify write access without holding the lock (apply happens separately).
        {
            let _guard = self.write_conn()?;
        }
        let cycle_ctx = self.build_cycle_context()?;
        cycle.run(&cycle_ctx)
    }

    /// Build the retrieve-before-reflect context for a cycle: prior wisdom (active
    /// pinned facts), the recent cycle-metadata history, and the default time
    /// window `[last_dream_cycle_at, now)`.
    fn build_cycle_context(&self) -> Result<CycleContext<'_>> {
        let now = Utc::now();
        let (prior_wisdom, start, prior_reports) = self.with_read(|conn| {
            let wisdom = FactStore::new(conn, self.embed_dim).list_pinned(&[])?;
            let start = match get_config(conn, "last_dream_cycle_at")? {
                Some(s) => DateTime::parse_from_rfc3339(&s)
                    .map_err(|e| {
                        MemoryError::Migration(MigrationError::Incompatible(format!(
                            "invalid last_dream_cycle_at: {e}"
                        )))
                    })?
                    .with_timezone(&Utc),
                None => DateTime::from_timestamp(0, 0).expect("unix epoch is a valid timestamp"),
            };
            let history = match get_config(conn, "dream_cycle_history")? {
                Some(s) => serde_json::from_str::<Vec<CycleMetadata>>(&s)?,
                None => Vec::new(),
            };
            Ok((wisdom, start, history))
        })?;
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
    pub(crate) fn promote_with_lineage(&self, req: &PromoteRequest) -> Result<PromotionResult> {
        // Validate embedding dimension up-front (before taking the lock).
        if req.embedding.len() != self.embed_dim {
            return Err(MemoryError::EmbeddingDimension {
                expected: self.embed_dim,
                actual: req.embedding.len(),
            });
        }

        // Embed HNSW copy before acquiring write lock (for post-lock notification)
        #[cfg(feature = "ann")]
        let emb_copy = req.embedding.clone();

        // Acquire write lock and insert both in a savepoint
        let mut conn = self.write_conn()?;
        let sp = conn.savepoint().map_err(MemoryError::Database)?;
        let result = self.promote_in_conn(&sp, req)?;
        sp.commit().map_err(MemoryError::Database)?;

        drop(conn); // release write lock before HNSW notification

        // Notify HNSW if enabled
        #[cfg(feature = "ann")]
        if let Some(ref hnsw) = self.hnsw_strategy {
            hnsw.notify_insert(result.fact_id, &emb_copy);
        }

        Ok(result)
    }

    /// Promotion steps on an already-open connection/savepoint.
    ///
    /// Resolves scope, injects the provenance envelope into metadata, inserts the
    /// pinned wisdom fact, and writes the lineage record — all against the caller's
    /// `conn`. Acquires **no** lock, does **not** commit, and does **not** notify
    /// HNSW; the caller owns the transaction boundary and post-commit index
    /// notification. This is the shared pipeline behind both
    /// [`Self::promote_with_lineage`] and `apply_cycle_report`'s `Promote` delta —
    /// the latter must reuse it (rather than call the lock-acquiring wrapper) to
    /// avoid self-deadlocking on the non-reentrant connection mutex.
    pub(crate) fn promote_in_conn(
        &self,
        conn: &rusqlite::Connection,
        req: &PromoteRequest,
    ) -> Result<PromotionResult> {
        // Validate embedding dimension
        if req.embedding.len() != self.embed_dim {
            return Err(MemoryError::EmbeddingDimension {
                expected: self.embed_dim,
                actual: req.embedding.len(),
            });
        }

        // Ensure metadata is an object (normalize non-objects to avoid silent provenance loss)
        let mut metadata = match req.metadata.clone() {
            serde_json::Value::Object(map) => serde_json::Value::Object(map),
            _ => serde_json::json!({}),
        };

        // Inject provenance envelope into metadata. lineage_id has #[serde(skip_serializing)]
        // on PromotionProvenance, so only descriptive fields are stored — the lineage table
        // is authoritative for the lineage_id → source_fact_ids mapping.
        if let serde_json::Value::Object(ref mut map) = metadata {
            map.insert(
                "promotion_provenance".to_owned(),
                serde_json::to_value(&req.provenance)
                    .map_err(|e| MemoryError::Internal(format!("serialize provenance: {e}")))?,
            );
        }

        let now = Utc::now();

        // Resolve scope on the caller's connection (same pattern as add_fact)
        let scope_id = match &req.scope {
            Some(path) => self.ensure_scope_with_conn(conn, path)?,
            None => 1, // root scope
        };

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
            importance: req.importance,
            access_count: 0,
            last_accessed: now,
            metadata,
            scope_id,
            is_pinned: true, // promoted wisdom is pinned (unforgettable)
        };

        let fact_id = FactStore::new(conn, self.embed_dim).insert(&new_fact)?;

        let lineage_record = NewLineageRecord {
            wisdom_fact_id: fact_id,
            source_fact_ids: req.source_fact_ids.clone(),
        };
        let lineage_id = LineageStore::new(conn).insert(&lineage_record, &req.provenance)?;

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

    impl DreamCycle for NoopCycle {
        fn run(&self, ctx: &CycleContext) -> Result<CycleReport> {
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

    #[test]
    fn run_dream_cycle_creates_context_and_delegates() {
        let engine = MemoryEngine::builder(4).build().unwrap();
        let cycle = NoopCycle;

        let report = engine.run_dream_cycle(&cycle).unwrap();
        assert!(report.deltas.is_empty());
        assert_eq!(report.metadata.method_version, "noop");
    }

    #[test]
    fn promote_with_lineage_rejects_wrong_dimension() {
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
        let engine = MemoryEngine::builder(4).build().unwrap();
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
        let engine = MemoryEngine::builder(4).build().unwrap();
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
