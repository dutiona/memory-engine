use crate::error::Result;

use super::MemoryEngine;

impl MemoryEngine {
    // --- Public API: Inspection ---

    /// Compute aggregate statistics about the engine state.
    ///
    /// Returns counts of facts (active, expired, pinned, due), edges,
    /// summaries, scopes, events, and storage metrics.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on SQL failure.
    pub async fn statistics(&self) -> Result<crate::inspect::EngineStatistics> {
        self.ensure_open()?;
        self.storage.statistics().await
    }

    /// List recent insight facts within a project scope subtree, newest-first.
    ///
    /// Returns active facts whose `metadata` carries the insight marker
    /// ([`INSIGHT_MARKER_KEY`](crate::INSIGHT_MARKER_KEY)) — written by the MCP
    /// `memory_flush_insights` tool — anywhere in the subtree rooted at `scope_path`,
    /// ordered `t_created` DESC and capped at `limit`. An unknown `scope_path` yields
    /// an empty vec: a not-yet-created project legitimately has no scope node, so this
    /// is "no insights" rather than an error.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on query failure.
    pub async fn list_recent_insights(
        &self,
        scope_path: &str,
        limit: usize,
    ) -> Result<Vec<crate::types::Fact>> {
        // Resolve the scope subtree under a short-lived read lock. `subtree(unknown_id)`
        // would return a singleton, so branch on `resolve_path` returning `None` and
        // early-return empty — never call `subtree` with a fallback id.
        //
        // The `scope_tree` read guard is dropped at the end of this block (before any
        // `.await`) so no parking_lot guard is held across the port call (keeps the
        // future `Send`).
        let scope_ids = {
            let tree = self.scope_tree.read();
            match tree.resolve_path(scope_path) {
                Some(id) => tree.subtree(id),
                None => return Ok(Vec::new()),
            }
        };
        self.storage
            .list_active_facts_by_metadata_key_recent(&scope_ids, crate::INSIGHT_MARKER_KEY, limit)
            .await
    }

    /// Replay a segment of the event log for debugging.
    ///
    /// Supports filtering by time range, ID range, session, and event type.
    /// Default ordering is by insertion order (ID ascending) for deterministic replay.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on SQL failure.
    pub async fn replay_events(
        &self,
        filter: &crate::inspect::ReplayFilter,
    ) -> Result<Vec<crate::types::Event>> {
        let event_filter = crate::inspect::replay::to_event_filter(filter);
        // The backend owns the `UpcasterRegistry`, so the `list_upcasted_events` /
        // `list_events` port split absorbs the former `EventStore::new(conn, &registry)`
        // construction and the `filter.upcast` branch is preserved verbatim.
        if filter.upcast {
            self.storage.list_upcasted_events(&event_filter).await
        } else {
            self.storage.list_events(&event_filter).await
        }
    }

    /// Explain why a fact is in its current state.
    ///
    /// Returns provenance, temporal state, graph context, and scope path.
    /// For expired facts, the graph context reflects the current (active-only) graph state.
    ///
    /// **Note:** `ExpiredReason` is best-effort. Most expired facts return `Unknown`
    /// until event-based audit trail is added in a future version.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if the fact doesn't exist.
    /// Returns `MemoryError::Storage` on SQL failure.
    ///
    /// # Lock strategy
    ///
    /// The graph `RwLock` is acquired and released *before* the database read
    /// to reduce lock contention — connected-component traversal is the expensive
    /// part and does not need to be held across the DB call. The `scope_tree`
    /// lock is kept across `with_read` because scope-path resolution depends on
    /// `fact.scope_id` fetched from the database.
    ///
    /// This means the graph context may reflect a slightly older snapshot than the
    /// database state under concurrent writes. This trade-off is intentional for
    /// an informational API: the graph snapshot is taken via
    /// `inspect::explain::build_graph_context` before the DB read.
    pub async fn explain_fact(&self, id: i64) -> Result<crate::inspect::FactExplanation> {
        use crate::inspect::explain;
        use crate::inspect::types::FactProvenance;

        self.ensure_open()?;

        // Snapshot graph context under the graph lock, then release it (the guard is
        // dropped at the end of this block, before any `.await`).
        let graph_context = {
            let graph = self.graph.read();
            explain::build_graph_context(&graph, id)
        };

        // Fetch the fact + (when present) its upcasted source event via the port.
        let fact = self.storage.get_fact(id).await?;
        let state = explain::determine_state(&fact, chrono::Utc::now());
        let source_event = match fact.source_event_id {
            Some(event_id) => Some(self.storage.get_upcasted_event(event_id).await?),
            None => None,
        };
        let provenance = FactProvenance {
            source_event_id: fact.source_event_id,
            source_event,
            base_importance: fact.base_importance,
            importance_score: fact.importance_score,
            is_pinned: fact.is_pinned,
            access_count: fact.access_count,
        };

        // Resolve the scope path off a `ScopeTree` snapshot — the read guard is taken
        // only after the awaits above, so no parking_lot guard crosses an `.await`
        // (keeps the future `Send`). The old code held this guard across `with_read`.
        let scope_path = {
            let tree = self.scope_tree.read();
            tree.path_for_id(fact.scope_id)
                .unwrap_or_else(|| format!("scope:{}", fact.scope_id))
        };

        Ok(crate::inspect::FactExplanation {
            fact_id: id,
            state,
            provenance,
            graph_context,
            scope_path,
        })
    }

    /// Reconstruct the temporal history of a fact from its bi-temporal timestamps.
    ///
    /// Returns a sorted timeline of lifecycle events computed from the fact's
    /// `t_created`, `t_valid`, `t_invalid`, and `t_expired` timestamps.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if the fact doesn't exist.
    /// Returns `MemoryError::Storage` on SQL failure.
    pub async fn fact_history(&self, id: i64) -> Result<crate::inspect::FactHistory> {
        self.ensure_open()?;
        // The bi-temporal timeline is pure over a single fact's timestamps: fetch via
        // the port, then derive it with no `&Connection` needed.
        let fact = self.storage.get_fact(id).await?;
        Ok(crate::inspect::explain::fact_history_from_fact(id, &fact))
    }

    /// Export full engine state to a file.
    ///
    /// - `DumpFormat::Json(path)`: Serializes all facts, edges, summaries, scopes,
    ///   events, and config to JSON. Works for both file-backed and in-memory engines.
    ///   Uses raw events (not upcasted) for snapshot fidelity.
    /// - `DumpFormat::JsonGzip(path)`: Same as `Json`, but gzip-compressed.
    ///   Requires the `compress-gzip` feature.
    /// - `DumpFormat::JsonZstd(path)`: Same as `Json`, but zstd-compressed.
    ///   Requires the `compress-zstd` feature.
    /// - `DumpFormat::Sqlite(path)`: Creates an atomic backup via `VACUUM INTO`.
    ///   Works for both file-backed and in-memory engines (`SQLite` 3.27+).
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Io`](crate::MemoryError::Io) on filesystem failure.
    /// Returns [`MemoryError::Conflict`](crate::MemoryError::Conflict) if the target path resolves to the live database.
    /// Returns [`MemoryError::Storage`](crate::MemoryError::Storage) on SQL failure.
    /// Returns [`MemoryError::Serialization`](crate::MemoryError::Serialization) for the JSON formats if snapshot
    /// serialization fails.
    /// Returns [`MemoryError::NotImplemented`](crate::MemoryError::NotImplemented) if a compression format is used
    /// without the corresponding feature enabled.
    /// Returns [`MemoryError::ReadOnly`](crate::MemoryError::ReadOnly) for the `Sqlite` format if the engine
    /// was opened read-only (the `VACUUM INTO` backup acquires the write lock).
    pub async fn dump_state(&self, format: &crate::inspect::DumpFormat) -> Result<()> {
        self.ensure_open()?;
        // The whole format dispatch (including the feature-gated `NotImplemented`
        // arms and the read-vs-write connection choice) lives below the seam in
        // [`SchemaManager::dump_state`](crate::storage::SchemaManager::dump_state).
        self.storage
            .dump_state(self.embed_dim, format.clone())
            .await
    }
}

// The MemoryEngine-level statistics/dump tests relocated here from
// inspect/statistics.rs and inspect/dump.rs (Wave 2 #816 / S2, sub-PR 2b): they build a
// full MemoryEngine, which the me-backend-sqlite backend crate (where compute_statistics/
// dump_json now live) cannot reach. The raw-Connection tests that drive those functions
// directly moved WITH them into me-backend-sqlite's own inspect::{statistics,dump} test
// modules.
#[cfg(test)]
mod tests {
    use crate::engine::MemoryEngine;
    use crate::inspect::types::{DumpFormat, EngineSnapshot};
    use me_traits::EmbeddingProvider;
    use me_types::types::{AddFactOptions, AddFactRequest, FactType};

    const DIM: usize = 4;

    // --- statistics.rs's engine-level tests ---------------------------------

    #[tokio::test]
    async fn empty_engine_statistics() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let stats = engine.statistics().await.unwrap();
        assert_eq!(stats.facts.total, 0);
        assert_eq!(stats.facts.active, 0);
        assert_eq!(stats.facts.expired, 0);
        assert_eq!(stats.facts.pinned, 0);
        assert_eq!(stats.facts.due, 0);
        assert_eq!(stats.edges.total, 0);
        assert_eq!(stats.summaries.total, 0);
        // Root scope always exists
        assert!(stats.scopes.total >= 1);
        assert_eq!(stats.events.total, 0);
        assert!(stats.storage.page_count > 0);
        assert!(stats.storage.page_size > 0);
        assert!(stats.storage.main_db_bytes > 0);
        assert!(stats.storage.file_path.is_none());
    }

    #[tokio::test]
    async fn statistics_with_facts() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "fact one".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                    as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "fact two".into(),
                    fact_type: FactType::Episodic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                    as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();
        // Add a pinned fact
        let pin_opts = AddFactOptions {
            pinned: Some(true),
            ..Default::default()
        };
        engine
            .add_fact(
                &AddFactRequest {
                    content: "pinned fact".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: Some(pin_opts),
                },
                std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                    as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();

        let stats = engine.statistics().await.unwrap();
        assert_eq!(stats.facts.total, 3);
        assert_eq!(stats.facts.active, 3);
        assert_eq!(stats.facts.expired, 0);
        assert_eq!(stats.facts.pinned, 1);
    }

    #[tokio::test]
    async fn snapshot_empty_engine_statistics() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let stats = engine.statistics().await.unwrap();
        insta::assert_yaml_snapshot!(stats, {
            ".storage.page_count" => "[page_count]",
            ".storage.page_size" => "[page_size]",
            ".storage.main_db_bytes" => "[db_bytes]",
        });
    }

    #[tokio::test]
    async fn snapshot_populated_statistics() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "fact one".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                    as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "fact two".into(),
                    fact_type: FactType::Episodic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                    as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();
        let pin_opts = AddFactOptions {
            pinned: Some(true),
            ..Default::default()
        };
        engine
            .add_fact(
                &AddFactRequest {
                    content: "pinned fact".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: Some(pin_opts),
                },
                std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                    as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();

        let stats = engine.statistics().await.unwrap();
        insta::assert_yaml_snapshot!(stats, {
            ".storage.page_count" => "[page_count]",
            ".storage.page_size" => "[page_size]",
            ".storage.main_db_bytes" => "[db_bytes]",
        });
    }

    // --- dump.rs's engine-level tests ---------------------------------------

    #[tokio::test]
    async fn json_dump_roundtrip() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "test fact".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                    as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let json_path = dir.path().join("dump.json");
        engine
            .dump_state(&DumpFormat::Json(json_path.clone()))
            .await
            .unwrap();

        // Deserialize and verify
        let content = std::fs::read_to_string(&json_path).unwrap();
        let snapshot: EngineSnapshot = serde_json::from_str(&content).unwrap();
        assert_eq!(snapshot.facts.len(), 1);
        assert_eq!(snapshot.facts[0].content, "test fact");
        assert_eq!(snapshot.embed_dim, DIM);
        assert!(!snapshot.scopes.is_empty()); // root scope
    }

    #[tokio::test]
    async fn sqlite_dump_from_in_memory() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "in-memory fact".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                    as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("dump.db");
        engine
            .dump_state(&DumpFormat::Sqlite(db_path.clone()))
            .await
            .unwrap();

        // Verify the dump is a valid SQLite database with our data.
        let dump_conn = rusqlite::Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let count: i64 = dump_conn
            .query_row("SELECT count(*) FROM facts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[cfg(feature = "compress-gzip")]
    #[tokio::test]
    async fn gzip_dump_has_correct_magic_bytes() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "gzip test".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                    as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dump.json.gz");
        engine
            .dump_state(&DumpFormat::JsonGzip(path.clone()))
            .await
            .unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > 2, "file too small");
        assert_eq!(bytes[0], 0x1f, "gzip magic byte 0");
        assert_eq!(bytes[1], 0x8b, "gzip magic byte 1");
    }

    #[cfg(feature = "compress-zstd")]
    #[tokio::test]
    async fn zstd_dump_has_correct_magic_bytes() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "zstd test".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                    as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dump.json.zst");
        engine
            .dump_state(&DumpFormat::JsonZstd(path.clone()))
            .await
            .unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > 4, "file too small");
        assert_eq!(&bytes[..4], &[0x28, 0xb5, 0x2f, 0xfd], "zstd magic bytes");
    }

    #[tokio::test]
    async fn snapshot_populated_engine_dump() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "snapshot fact".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                    as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let json_path = dir.path().join("dump.json");
        engine
            .dump_state(&DumpFormat::Json(json_path.clone()))
            .await
            .unwrap();

        let content = std::fs::read_to_string(&json_path).unwrap();
        let snapshot: EngineSnapshot = serde_json::from_str(&content).unwrap();

        insta::assert_yaml_snapshot!(snapshot, {
            ".facts[].t_created" => "[timestamp]",
            ".facts[].last_accessed" => "[timestamp]",
            ".facts[].embedding" => "[embedding]",
            ".facts[].content_hash" => "[hash]",
            ".events" => "[]",
            ".config" => "{}",
        });
    }
}
