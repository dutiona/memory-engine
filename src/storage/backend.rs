//! The [`StorageBackend`] umbrella supertrait + the object-safety / callability
//! gate (the load-bearing guarantee for epic #628: the engine holds one
//! `Arc<dyn StorageBackend>`).

use crate::storage::{FactGraph, SchemaManager};

/// The single persistence handle the engine holds (`Arc<dyn StorageBackend>`).
///
/// A pure aggregation supertrait: any type implementing all the bounded-context
/// traits *is* a `StorageBackend` via the blanket impl below — backends never
/// write `impl StorageBackend`, they implement the parts. The bounded traits stay
/// what tests mock in isolation (a forgetting test mocks only [`FactGraph`]); this
/// umbrella is what the engine depends on.
///
/// `ColdStorage` is intentionally **not** a supertrait bound — it is feature-gated
/// and held separately (`Option<Arc<dyn ColdStorage>>`), so this umbrella's type
/// stays stable across feature sets.
pub trait StorageBackend: FactGraph + SchemaManager {}

/// Blanket impl: implementing the bounded traits is sufficient to be a `StorageBackend`.
impl<T> StorageBackend for T where T: FactGraph + SchemaManager {}

#[cfg(test)]
mod tests {
    use super::*;

    // Object-safety: a `&dyn` reference forces vtable formation; fails to compile
    // if any super-trait method is not object-safe. The negative control (a
    // generic method on a bounded trait) was verified to break this with E0038.
    fn _assert_obj_safe(_: &dyn StorageBackend) {}
    fn _assert_fact_graph_obj_safe(_: &dyn FactGraph) {}
    fn _assert_schema_obj_safe(_: &dyn SchemaManager) {}
    fn _assert_arc(_: std::sync::Arc<dyn StorageBackend>) {}

    // Callability (Codex BLOCKER): vtable-forms ≠ callable under async_trait's
    // hidden `Self: Sync` future bound. Actually `.await` a method through the
    // trait object. Gated on `async` (tokio) so default builds need no runtime.
    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_method_callable_through_dyn() {
        use crate::storage::BackendCapabilities;
        use async_trait::async_trait;

        struct Dummy;
        #[async_trait]
        impl FactGraph for Dummy {
            async fn get_fact(&self, _id: i64) -> crate::error::Result<crate::types::Fact> {
                Err(crate::error::MemoryError::NotFound("dummy".into()))
            }
        }
        #[async_trait]
        impl SchemaManager for Dummy {
            async fn schema_version(&self) -> crate::error::Result<u32> {
                Ok(0)
            }
            fn capabilities(&self) -> BackendCapabilities {
                BackendCapabilities {
                    lexical_ranker: crate::storage::LexicalRanker::Bm25,
                    server_side_vector: false,
                    true_idf: true,
                }
            }
        }

        let b: std::sync::Arc<dyn StorageBackend> = std::sync::Arc::new(Dummy);
        assert_eq!(b.schema_version().await.unwrap(), 0);
        let _ = b.capabilities();
        assert!(b.get_fact(1).await.is_err());
    }
}
