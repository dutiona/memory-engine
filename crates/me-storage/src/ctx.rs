//! [`MemoryCtx`] — the universal capability handle every primitive receives (plan §B).
//!
//! Wave 2 #816: homed in `me-storage` (L1) because its load-bearing field is
//! `&Arc<dyn StorageBackend>` — it is *about* storage access. Defined in S1; the L3
//! primitive crates (me-forget/me-resolve/me-query/me-ingest/me-consolidate/me-archive)
//! consume it in S3/S4. Per-primitive extras (`graph`, `scope_tree`, `reranker`,
//! `cold`, `db_path`) are passed as explicit parameters so each free-fn signature
//! *declares* exactly which extra capabilities it uses.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use me_types::error::{MemoryError, Result};

use crate::StorageBackend;

/// The universal capability handle every primitive receives.
///
/// Carries ONLY what every primitive needs: the persistence port, the embedding
/// dimension, and the read-fence / `read_only` gate.
///
/// `Copy` (two references + a `usize` + a `bool`): a primitive can hand it to a helper
/// without ceremony. The lifetime `'a` ties it to the facade-owned state for the duration
/// of one call — it is intentionally NOT `'static`/`Serialize` (mirrors how the in-tree
/// `CycleContext` borrows the engine, design §5).
#[derive(Clone, Copy)]
pub struct MemoryCtx<'a> {
    /// The single persistence handle. All DB-touching work awaits this port.
    pub storage: &'a Arc<dyn StorageBackend>,
    /// Embedding dimension this handle was opened at.
    pub embed_dim: usize,
    /// Whether the engine was opened read-only (write primitives check this).
    pub read_only: bool,
    /// The reconstruction dimension fence (#742): `0` = open; non-zero `D′` = fenced
    /// (the consumer must reopen at `D′`).
    pub reopen_required: &'a AtomicUsize,
}

impl MemoryCtx<'_> {
    /// The `ensure_open` read-fence gate, relocated verbatim from `engine::mod`
    /// (#742). Every embedding-touching primitive calls this at entry.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::EmbeddingReopenRequired`] once a different-dim
    /// reconstruction has fenced this handle, until the consumer reopens at `D′`.
    pub fn ensure_open(&self) -> Result<()> {
        match self.reopen_required.load(Ordering::Acquire) {
            0 => Ok(()),
            new_dim => Err(MemoryError::EmbeddingReopenRequired { new_dim }),
        }
    }

    /// The write gate. Write primitives call this before mutating.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::ReadOnly`] if the engine was opened read-only.
    pub const fn ensure_writable(&self) -> Result<()> {
        if self.read_only {
            return Err(MemoryError::ReadOnly);
        }
        Ok(())
    }
}
