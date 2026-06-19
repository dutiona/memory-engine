//! The core knowledge-graph trait: facts, edges, and the scope hierarchy.
//!
//! P1 skeleton (one method) — expanded to the full surveyed `FactStore` +
//! `EdgeStore` + `ScopeStore` surface in P5. Scopes fold in here because
//! `scope_id` is an FK on facts/edges (they partition the graph, not an
//! independent concern).

use async_trait::async_trait;

use crate::error::Result;
use crate::types::Fact;

/// Core knowledge graph: facts, edges, the scope hierarchy.
///
/// All methods are async (boxed via `async_trait`): the `SQLite` backend wraps sync
/// `rusqlite` in `spawn_blocking`; a Postgres backend is natively async. No SQL
/// or driver type crosses this boundary.
#[async_trait]
pub trait FactGraph: Send + Sync {
    /// Fetch one fact by id.
    ///
    /// # Errors
    /// Returns [`MemoryError::NotFound`](crate::error::MemoryError::NotFound) if no
    /// fact has `id`, or [`MemoryError::Storage`](crate::error::MemoryError::Storage)
    /// on a backend failure.
    async fn get_fact(&self, id: i64) -> Result<Fact>;
}
