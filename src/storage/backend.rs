//! The [`StorageBackend`] umbrella supertrait + the object-safety / callability
//! gate (the load-bearing guarantee for epic #628: the engine holds one
//! `Arc<dyn StorageBackend>`).
//!
//! ## Coverage checklist (the R-COMPLETE proof — every surveyed concrete method
//! maps to exactly one trait method, or is intentionally dropped/kept-private)
//!
//! - **FactStore** (facts.rs): every `pub`/`pub(crate)` method → a `FactGraph`
//!   `*_fact(s)` method (the `pub(crate)` `merge_metadata`/`mark_dream_cycled`/
//!   `list_undreamt_in_period` become public trait methods). `for_each` →
//!   `for_each_fact` (object-safe streaming). `list_active_scoring` →
//!   `list_active_facts_scoring`.
//! - **EdgeStore** (edges.rs): every method → a `FactGraph` `*_edge(s)` method;
//!   `for_each` → `for_each_edge`.
//! - **ScopeStore** (scopes.rs): every method → a `FactGraph` `*_scope` method;
//!   `for_each` → `for_each_scope`.
//! - **EventStore** (events.rs): every method → `EventLog`; `for_each` →
//!   `for_each_event`. The `UpcasterRegistry` ctor param stays backend-private.
//! - **fts/vector/ann** (search/*): `fts_search`/`vector_search` →
//!   `SearchIndex::{lexical,vector}_search` (ranked `(id, score)`);
//!   `fts_count_expired` → `lexical_count_expired`. HNSW index-maintenance hooks
//!   (`build_from_db`/`notify_insert`/`notify_expire`/snapshot) stay impl-private —
//!   SQLite-ann internals, not a port contract.
//! - **SummaryStore + LineageStore** (summaries.rs/lineage.rs) → `ConsolidationStore`;
//!   the two `for_each`s → `for_each_summary` / `for_each_lineage`.
//! - **ActivityStore + CheckpointStore** (activities.rs/checkpoints.rs) →
//!   `SessionStore` (the `#[cfg(test)]` reads become unconditional trait methods).
//! - **schema/mod** (schema/*): `migrate`/`schema_version`/`validate_schema_version`
//!   → `SchemaManager`; `capabilities` (new, sync). Connection-open / `init_schema`
//!   / generic config K/V / `backup_before_migration` stay backend-private ctor.
//! - **embedding_meta** (embedding_meta.rs): `load`/`store`/`record_if_absent`/
//!   `require_present` → `SchemaManager::*_embedding_fingerprint*` (live engine
//!   behavior, so on the port).
//! - **ArchiveManifestStore** (archive_manifest.rs) → `ColdStorage` (cfg archive);
//!   pak file I/O stays free functions.

use crate::storage::{
    ConsolidationStore, EventLog, FactGraph, SchemaManager, SearchIndex, SessionStore,
};

/// The single persistence handle the engine holds (`Arc<dyn StorageBackend>`).
///
/// A pure aggregation supertrait: any type implementing all six bounded-context
/// traits *is* a `StorageBackend` via the blanket impl below — backends never
/// write `impl StorageBackend`, they implement the parts. The bounded traits stay
/// what tests mock in isolation (a forgetting test mocks only [`FactGraph`]); this
/// umbrella is what the engine depends on.
///
/// `ColdStorage` (feature `archive`) is intentionally **not** a supertrait bound —
/// it is feature-gated and held separately (`Option<Arc<dyn ColdStorage>>`), so
/// this umbrella's type stays stable across feature sets.
pub trait StorageBackend:
    FactGraph + EventLog + SearchIndex + ConsolidationStore + SessionStore + SchemaManager
{
}

/// Blanket impl: implementing the six bounded traits is sufficient to be a
/// `StorageBackend`. A backend cannot accidentally be "a `StorageBackend` that
/// forgot to be a `FactGraph`".
impl<T> StorageBackend for T where
    T: FactGraph + EventLog + SearchIndex + ConsolidationStore + SessionStore + SchemaManager
{
}

#[cfg(test)]
mod tests {
    use super::*;

    // Object-safety: a `&dyn` reference forces vtable formation; fails to compile
    // if any (super-)trait method is not object-safe. The negative control (a
    // generic method on a bounded trait) was verified to break this with E0038.
    fn _assert_obj_safe(_: &dyn StorageBackend) {}
    fn _assert_fact_graph_obj_safe(_: &dyn FactGraph) {}
    fn _assert_event_log_obj_safe(_: &dyn EventLog) {}
    fn _assert_search_index_obj_safe(_: &dyn SearchIndex) {}
    fn _assert_consolidation_obj_safe(_: &dyn ConsolidationStore) {}
    fn _assert_session_obj_safe(_: &dyn SessionStore) {}
    fn _assert_schema_obj_safe(_: &dyn SchemaManager) {}
    fn _assert_arc(_: std::sync::Arc<dyn StorageBackend>) {}

    #[cfg(feature = "archive")]
    fn _assert_cold_storage_obj_safe(_: &dyn crate::storage::ColdStorage) {}

    // Callability (Codex BLOCKER): vtable-forms ≠ callable under async_trait's
    // hidden `Self: Sync` future bound. Actually `.await` an async method through a
    // trait object. `SearchIndex` (3 methods) is the cheap, sufficient witness of
    // the async-through-`dyn` mechanism; constructing a full `Arc<dyn StorageBackend>`
    // value (a ~90-method impl) is #630's `SqliteBackend`, exercised by the #632
    // conformance suite — not pulled forward here. Gated on `async` (tokio) so
    // default builds need no runtime.
    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_method_callable_through_dyn() {
        use crate::storage::FactFilter;
        use async_trait::async_trait;

        struct Dummy;
        #[async_trait]
        impl SearchIndex for Dummy {
            async fn lexical_search(
                &self,
                _q: &str,
                _f: &FactFilter,
                _k: usize,
            ) -> crate::error::Result<Vec<(i64, f64)>> {
                Ok(vec![(7, 1.5)])
            }
            async fn vector_search(
                &self,
                _e: &[f32],
                _f: &FactFilter,
                _k: usize,
            ) -> crate::error::Result<Vec<(i64, f64)>> {
                Ok(vec![])
            }
            async fn lexical_count_expired(
                &self,
                _q: &str,
                _f: &FactFilter,
            ) -> crate::error::Result<usize> {
                Ok(0)
            }
        }

        let idx: &dyn SearchIndex = &Dummy;
        let hits = idx
            .lexical_search("q", &FactFilter::default(), 5)
            .await
            .unwrap();
        assert_eq!(hits, vec![(7, 1.5)]);
        assert!(
            idx.vector_search(&[0.0], &FactFilter::default(), 5)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            idx.lexical_count_expired("q", &FactFilter::default())
                .await
                .unwrap(),
            0
        );
    }
}
