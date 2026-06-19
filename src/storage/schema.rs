//! Backend lifecycle: migrations, schema version, capability probe.
//!
//! P1 skeleton — expanded in P5 with `migrate`, `validate_schema_version`, and
//! the embedding-fingerprint identity surface. *Constructing* a backend (open
//! flags, pool URL, init DDL, `VACUUM INTO` backup) is backend-specific and stays
//! off this shared port.

use async_trait::async_trait;

use crate::error::Result;
use crate::storage::capabilities::BackendCapabilities;

/// Backend lifecycle the engine drives post-open. Mixes one **synchronous**
/// method (`capabilities` — fixed at open, not a per-call round-trip) with the
/// async lifecycle ones; legal under `#[async_trait]`.
#[async_trait]
pub trait SchemaManager: Send + Sync {
    /// The schema version currently recorded in the store.
    ///
    /// # Errors
    /// [`MemoryError::Storage`](crate::error::MemoryError::Storage) on a backend failure.
    async fn schema_version(&self) -> Result<u32>;

    /// Probe backend capabilities. **Synchronous** — capabilities are fixed at open.
    fn capabilities(&self) -> BackendCapabilities;
}
