use crate::error::{MemoryError, Result};
use crate::store::events::EventStore;

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
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn statistics(&self) -> Result<crate::inspect::EngineStatistics> {
        self.with_read(|conn| {
            crate::inspect::statistics::compute_statistics(conn, self.pool.path())
        })
    }

    /// Replay a segment of the event log for debugging.
    ///
    /// Supports filtering by time range, ID range, session, and event type.
    /// Default ordering is by insertion order (ID ascending) for deterministic replay.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn replay_events(
        &self,
        filter: &crate::inspect::ReplayFilter,
    ) -> Result<Vec<crate::types::Event>> {
        let event_filter = crate::inspect::replay::to_event_filter(filter);
        self.with_read(|conn| {
            let store = EventStore::new(conn, &self.upcaster_registry);
            if filter.upcast {
                store.list_upcasted(&event_filter)
            } else {
                store.list(&event_filter)
            }
        })
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
    /// Returns `MemoryError::Database` on SQL failure.
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
    /// an informational API — see [`inspect::explain::explain_fact_with_graph_context`].
    pub fn explain_fact(&self, id: i64) -> Result<crate::inspect::FactExplanation> {
        // Snapshot graph context under the graph lock, then release it.
        let graph_context = {
            let graph = self.graph.read();
            crate::inspect::explain::build_graph_context(&graph, id)
        };
        // scope_tree lock is still held across with_read because scope_path
        // resolution requires fact.scope_id, which comes from the DB.
        let scope_tree = self.scope_tree.read();
        self.with_read(|conn| {
            crate::inspect::explain::explain_fact_with_graph_context(
                conn,
                &scope_tree,
                self.embed_dim,
                id,
                &self.upcaster_registry,
                graph_context,
            )
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
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn fact_history(&self, id: i64) -> Result<crate::inspect::FactHistory> {
        self.with_read(|conn| crate::inspect::explain::fact_history(conn, self.embed_dim, id))
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
    /// Returns [`MemoryError::Io`] on filesystem failure.
    /// Returns [`MemoryError::Conflict`] if the target path resolves to the live database.
    /// Returns [`MemoryError::Database`] on SQL failure.
    /// Returns [`MemoryError::NotImplemented`] if a compression format is used
    /// without the corresponding feature enabled.
    #[allow(unreachable_patterns)] // wildcard needed for #[non_exhaustive] forward compat
    pub fn dump_state(&self, format: &crate::inspect::DumpFormat) -> Result<()> {
        match format {
            crate::inspect::DumpFormat::Json(path) => {
                self.with_read(|conn| crate::inspect::dump::dump_json(conn, self.embed_dim, path))
            }
            #[cfg(feature = "compress-gzip")]
            crate::inspect::DumpFormat::JsonGzip(path) => self
                .with_read(|conn| crate::inspect::dump::dump_json_gzip(conn, self.embed_dim, path)),
            #[cfg(not(feature = "compress-gzip"))]
            crate::inspect::DumpFormat::JsonGzip(_) => Err(MemoryError::NotImplemented(
                "gzip compression requires the `compress-gzip` feature".into(),
            )),
            #[cfg(feature = "compress-zstd")]
            crate::inspect::DumpFormat::JsonZstd(path) => self
                .with_read(|conn| crate::inspect::dump::dump_json_zstd(conn, self.embed_dim, path)),
            #[cfg(not(feature = "compress-zstd"))]
            crate::inspect::DumpFormat::JsonZstd(_) => Err(MemoryError::NotImplemented(
                "zstd compression requires the `compress-zstd` feature".into(),
            )),
            crate::inspect::DumpFormat::Sqlite(path) => {
                let conn = self.write_conn()?;
                crate::inspect::dump::dump_sqlite(&conn, path)
            }
            _ => Err(MemoryError::NotImplemented(
                "unsupported dump format".into(),
            )),
        }
    }
}
