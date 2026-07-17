//! `#[cfg(test)]`-only harness shared by this crate's own test suites
//! (`apply`, `default_impl`, `llm_impl`).
//!
//! `me-cognitive` cannot depend on the facade (that back-edge is exactly what this
//! carve removes — see the crate-root doc), so its tests cannot build a
//! `MemoryEngine` the way the pre-carve facade tests did. `TestEngine` is a minimal,
//! `#[cfg(test)]`-only stand-in: it owns a real backend (`SqliteFactory`, `me-test-support`)
//! plus the loose parameters (`graph`, `reopen_required`, `upcaster_registry`) the
//! orchestration free functions take explicitly, and it implements
//! [`me_traits::DreamCtx`] by delegating to that backend — mirroring the facade's own
//! `EngineDreamCtx` adapter (see `memory-engine/src/engine/cognitive.rs`) at
//! test-double fidelity.
//!
//! Only the methods the moved test suites actually exercise
//! (`get_fact`/`list_active_facts`/`list_undreamt_in_period`/`outcome_counts*`/
//! `promote`) are implemented against the storage port directly — genuinely cheap,
//! one-line delegations. `query`/`consolidate`/`forget` are unreachable by any test
//! in this crate (verified: neither `DefaultDreamCycle::run` nor `LlmDreamCycle::run`
//! nor `apply_cycle_report` calls them) and would otherwise require a same-layer
//! dependency on `me-query`/`me-consolidate`/`me-forget` this crate does not carry, so
//! they panic loudly if a future test does reach them instead of silently
//! misbehaving.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use parking_lot::RwLock;

use me_index::MemoryGraph;
use me_storage::{MemoryCtx, StorageBackend, UpcasterRegistry};
use me_traits::{ConsolidationConfig, ConsolidationStats, EmbeddingProvider, SummaryGenerator};
use me_types::error::Result;
use me_types::types::forgetting::{ForgetPolicy, PruneStats};
use me_types::types::search::{SearchQuery, SearchResult};
use me_types::types::{Fact, NewFact, OutcomeCounts, PromoteRequest, PromotionResult};

use me_test_support::factory::{ConformanceBackend, SqliteFactory};

/// A minimal `DreamCtx` implementor over a real (in-memory `SQLite`) backend.
pub struct TestEngine {
    pub storage: Arc<dyn StorageBackend>,
    embed_dim: usize,
    reopen_required: AtomicUsize,
    pub graph: RwLock<MemoryGraph>,
    pub upcaster_registry: UpcasterRegistry,
}

impl TestEngine {
    /// A fresh, empty, writable in-memory backend at `embed_dim`, migrated to HEAD.
    pub async fn new(embed_dim: usize) -> Self {
        Self {
            storage: SqliteFactory.make().await,
            embed_dim,
            reopen_required: AtomicUsize::new(0),
            graph: RwLock::new(MemoryGraph::new()),
            upcaster_registry: UpcasterRegistry::new(),
        }
    }

    /// Build the `MemoryCtx` the orchestration free functions take, mirroring
    /// `MemoryEngine::mem_ctx`.
    pub fn ctx(&self) -> MemoryCtx<'_> {
        MemoryCtx {
            storage: &self.storage,
            embed_dim: self.embed_dim,
            read_only: false,
            reopen_required: &self.reopen_required,
        }
    }

    /// Record an embedding-space identity (#613) directly, mirroring what the
    /// facade's `add_fact` stamps as a side effect of its first live-embedder write.
    /// Needed before applying any delta that carries a **pre-computed** vector
    /// (`AddFact`/`Synthesize`/`Promote`) — those are rejected against a store with
    /// no recorded identity. Tests that specifically exercise the "unstamped store"
    /// rejection path construct a bare [`Self::new`] instead of calling this.
    pub async fn stamp_identity(&self) {
        self.storage
            .store_embedding_fingerprint(&me_types::types::EmbeddingFingerprint::new(
                "mock",
                "test",
                self.embed_dim,
            ))
            .await
            .expect("store_embedding_fingerprint against a freshly-migrated in-memory backend");
    }

    /// Insert a fact directly via the storage port (bypassing the facade's
    /// `add_fact` ceremony — scope resolution, identity stamping — which this
    /// crate's tests either don't need or arrange explicitly via
    /// [`Self::stamp_identity`]). Returns the assigned id.
    pub async fn add(&self, content: &str) -> i64 {
        self.add_typed(content, me_types::types::FactType::Semantic, 0.5)
            .await
    }

    /// Append an [`me_types::types::EventType::OutcomeSignal`] event for `fact_id`,
    /// mirroring the facade's `MemoryEngine::record_outcome` (minus its
    /// fact-existence pre-check — the moved tests only ever record against a fact
    /// they just inserted).
    pub async fn record_outcome(&self, fact_id: i64, outcome: me_types::types::Outcome) {
        let event = me_types::types::NewEvent {
            timestamp: chrono::Utc::now(),
            event_type: me_types::types::EventType::OutcomeSignal,
            payload: serde_json::json!({
                "fact_id": fact_id,
                "outcome": outcome,
            }),
            source: "outcome_tracking".into(),
            session_id: None,
            scope_id: 1,
            origin_node_id: "local".into(),
            sequence_id: 0,
            created_at: None,
        };
        self.storage
            .insert_event(&event)
            .await
            .expect("insert_event against a freshly-migrated in-memory backend");
    }

    /// [`Self::add`], with an explicit `fact_type` and `base_importance`.
    pub async fn add_typed(
        &self,
        content: &str,
        fact_type: me_types::types::FactType,
        base_importance: f64,
    ) -> i64 {
        self.add_at(content, fact_type, base_importance, 1).await
    }

    /// [`Self::add`], resolving `scope` (a scope *path*, e.g. `"proj"`) to a scope id
    /// first via the storage port directly (`FactGraph::ensure_scope_path` — the
    /// facade's `ScopeTree` is purely a read-cache over the same port call, so this
    /// test double needs no cache of its own). `None` inserts at the root scope (1).
    pub async fn add_scoped(&self, content: &str, scope: Option<&str>) -> i64 {
        let scope_id = match scope {
            Some(path) => self
                .storage
                .ensure_scope_path(path)
                .await
                .expect("ensure_scope_path against a freshly-migrated in-memory backend"),
            None => 1,
        };
        self.add_at(content, me_types::types::FactType::Semantic, 0.5, scope_id)
            .await
    }

    /// The shared insert primitive behind [`Self::add`] / [`Self::add_typed`] /
    /// [`Self::add_scoped`].
    async fn add_at(
        &self,
        content: &str,
        fact_type: me_types::types::FactType,
        base_importance: f64,
        scope_id: i64,
    ) -> i64 {
        let now = chrono::Utc::now();
        self.storage
            .insert_fact(&NewFact {
                content: content.to_owned(),
                content_hash: String::new(),
                embedding: vec![0.1; self.embed_dim],
                fact_type,
                t_created: now,
                t_expired: None,
                t_valid: None,
                t_invalid: None,
                source_event_id: None,
                base_importance,
                access_count: 0,
                last_accessed: now,
                metadata: serde_json::json!({}),
                scope_id,
                is_pinned: false,
            })
            .await
            .expect("insert_fact against a freshly-migrated in-memory backend")
    }
}

#[async_trait::async_trait]
impl me_traits::DreamCtx for TestEngine {
    async fn query(&self, _query: &SearchQuery) -> Result<Vec<SearchResult>> {
        unimplemented!("not exercised by this crate's tests — no me-query dependency")
    }

    async fn list_active_facts(&self, limit: Option<usize>) -> Result<Vec<Fact>> {
        self.storage.list_active_facts(limit).await
    }

    async fn get_fact(&self, id: i64) -> Result<Fact> {
        self.storage.get_fact(id).await
    }

    async fn consolidate(
        &self,
        _generator: Arc<dyn SummaryGenerator>,
        _embedder: Arc<dyn EmbeddingProvider>,
        _config: &ConsolidationConfig,
    ) -> Result<ConsolidationStats> {
        unimplemented!("not exercised by this crate's tests — no me-consolidate dependency")
    }

    async fn forget(&self, _policy: &ForgetPolicy) -> Result<PruneStats> {
        unimplemented!("not exercised by this crate's tests — no me-forget dependency")
    }

    async fn promote(&self, req: &PromoteRequest) -> Result<PromotionResult> {
        // Mirrors `MemoryEngine::promote_with_lineage` minus the scope-tree cache
        // update (this test double has no `ScopeTree`; no moved test promotes into a
        // non-root scope through `DreamCtx`, so `req.scope` is always `None` here).
        let now = chrono::Utc::now();
        let mut metadata = match req.metadata.clone() {
            serde_json::Value::Object(map) => serde_json::Value::Object(map),
            _ => serde_json::json!({}),
        };
        if let serde_json::Value::Object(ref mut map) = metadata {
            map.insert(
                "promotion_provenance".to_owned(),
                serde_json::to_value(&req.provenance)
                    .expect("PromotionProvenance always serializes"),
            );
        }
        let new_fact = NewFact {
            content: req.content.clone(),
            content_hash: String::new(),
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
            scope_id: 1,
            is_pinned: true,
        };
        let (result, _scope_ids_to_cache) = self
            .storage
            .promote_atomic(
                &new_fact,
                req.scope.as_deref(),
                &req.source_fact_ids,
                &req.provenance,
            )
            .await?;
        Ok(result)
    }

    async fn list_undreamt_in_period(
        &self,
        window: me_types::types::cycle_report::TimeWindow,
    ) -> Result<Vec<Fact>> {
        self.storage
            .list_undreamt_facts_in_period(window.start, window.end, &[], None)
            .await
    }

    async fn outcome_counts(&self, fact_id: i64) -> Result<OutcomeCounts> {
        self.storage.get_fact(fact_id).await.map(|_| ())?;
        self.storage.count_outcome_signals(fact_id).await
    }

    async fn outcome_counts_batch(&self, fact_ids: &[i64]) -> Result<HashMap<i64, OutcomeCounts>> {
        self.storage.count_outcome_signals_batch(fact_ids).await
    }
}
