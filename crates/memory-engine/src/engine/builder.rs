//! Typestate builder for [`MemoryEngine`] (issue #541).
//!
//! Replaces the five telescoping `MemoryEngine::open*` constructors with a
//! single fluent builder whose *type state* encodes the storage backing:
//!
//! - [`MemoryEngine::builder(embed_dim)`](MemoryEngine::builder) starts in the
//!   [`InMemory`] state — the overwhelmingly common case (`builder(384).build()?`).
//! - [`.path(p)`](MemoryEngineBuilder::path) transitions to the [`File`] state,
//!   which alone exposes the file-only knobs `read_only`, `backup_dir`, and
//!   `read_pool_size`.
//!
//! Because the file-only setters exist *only* on `MemoryEngineBuilder<File>`,
//! `MemoryEngine::builder(d).read_only(true)` is a **compile error** — the
//! nonsensical "in-memory engine with a read-only/backup file knob" state is
//! structurally unrepresentable, matching `ConnectionPool::open_memory`, which
//! hardcodes `read_only = false` and has no backup path.
//!
//! ## Capability growth is O(1)
//!
//! Adding a new optional capability is *one setter + one field*, not a new
//! constructor per combination. A capability that applies to both backings goes
//! on `impl<B: Backing>`; a file-only one goes on `impl MemoryEngineBuilder<File>`.
//! This is what kills the telescoping-constructor explosion #541 targets.
//!
//! ## Non-`Clone` by design
//!
//! The builder owns an `Option<Box<dyn Reranker>>`. [`Reranker`] is not `Clone`,
//! so the builder is consumed by [`build`](MemoryEngineBuilder::build), moving the
//! reranker straight into the engine. No `Clone` bound leaks onto consumers.
//!
//! ```
//! use memory_engine::MemoryEngine;
//! // In-memory engine — the common path.
//! let engine = MemoryEngine::builder(384).build()?;
//! assert_eq!(engine.embed_dim(), 384);
//! # Ok::<(), memory_engine::MemoryError>(())
//! ```
//!
//! ```
//! use memory_engine::MemoryEngine;
//! let dir = tempfile::tempdir().unwrap();
//! // File-backed engine — `.path()` unlocks the file-only knobs.
//! let engine = MemoryEngine::builder(384)
//!     .path(dir.path().join("agent.db"))
//!     .read_pool_size(2)
//!     .build()?;
//! # Ok::<(), memory_engine::MemoryError>(())
//! ```
//!
//! A file-only knob on the in-memory state does not compile:
//!
//! ```compile_fail
//! use memory_engine::MemoryEngine;
//! // `read_only` does not exist on MemoryEngineBuilder<InMemory>.
//! let _ = MemoryEngine::builder(384).read_only(true).build();
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use crate::error::Result;
use crate::pool::ConnectionPool;
use crate::search::strategy::SearchConfig;
use crate::store::upcaster::UpcasterRegistry;
use crate::traits::Reranker;

use super::{EngineConfig, MemoryEngine};

mod sealed {
    /// Sealed marker trait for the builder's storage backing. Implemented only by
    /// [`super::InMemory`] and [`super::File`]; downstream crates cannot add states.
    pub trait Backing {}
}

/// Builder type state: the engine will be in-memory. File-only knobs are not
/// callable in this state. This is the default backing of [`MemoryEngine::builder`].
#[derive(Debug)]
pub struct InMemory(());
impl sealed::Backing for InMemory {}

/// Builder type state: the engine will be file-backed. Carries the file-only
/// configuration as data, so those knobs are structurally absent from [`InMemory`].
#[derive(Debug)]
pub struct File {
    path: PathBuf,
    read_only: bool,
    backup_dir: Option<PathBuf>,
    read_pool_size: usize,
}
impl sealed::Backing for File {}

/// Fluent, type-state builder for [`MemoryEngine`]. See the [module docs](self).
#[must_use = "a builder does nothing until `.build()` is called"]
pub struct MemoryEngineBuilder<B: sealed::Backing = InMemory> {
    embed_dim: usize,
    // CAPS — meaningful for both backings:
    search_config: Option<SearchConfig>,
    reranker: Option<Box<dyn Reranker>>,
    upcaster_registry: UpcasterRegistry,
    // BACKING — in-memory marker or file payload:
    backing: B,
}

impl MemoryEngine {
    /// Start building an engine. Begins in the in-memory state; call
    /// [`.path()`](MemoryEngineBuilder::path) to make it file-backed.
    ///
    /// `embed_dim` is the embedding dimension, required for both backings and
    /// authoritative — for a file-backed engine it is validated against (or
    /// written to) the stored schema at [`build`](MemoryEngineBuilder::build).
    pub fn builder(embed_dim: usize) -> MemoryEngineBuilder<InMemory> {
        MemoryEngineBuilder {
            embed_dim,
            search_config: None,
            reranker: None,
            upcaster_registry: UpcasterRegistry::new(),
            backing: InMemory(()),
        }
    }
}

/// Capability setters available in every backing state.
impl<B: sealed::Backing> MemoryEngineBuilder<B> {
    /// Set the search configuration (ANN strategy dispatch threshold).
    pub const fn search_config(mut self, config: SearchConfig) -> Self {
        self.search_config = Some(config);
        self
    }

    /// Attach a reranker. Applies to both in-memory and file-backed engines.
    // Cannot be `const`: assigning `Option<Box<dyn Reranker>>` runs a destructor
    // (clippy::missing_const_for_fn is a false positive for `Box`-bearing fields).
    #[allow(clippy::missing_const_for_fn)]
    pub fn reranker(mut self, reranker: Box<dyn Reranker>) -> Self {
        self.reranker = Some(reranker);
        self
    }

    /// Provide a custom upcaster registry for event-payload versioning.
    pub fn upcaster_registry(mut self, registry: UpcasterRegistry) -> Self {
        self.upcaster_registry = registry;
        self
    }
}

/// Transition and terminal for the in-memory state.
impl MemoryEngineBuilder<InMemory> {
    /// Promote this builder to a file-backed engine at `path`. Unlocks the
    /// file-only setters ([`read_only`](MemoryEngineBuilder::read_only),
    /// [`backup_dir`](MemoryEngineBuilder::backup_dir),
    /// [`read_pool_size`](MemoryEngineBuilder::read_pool_size)).
    pub fn path(self, path: impl Into<PathBuf>) -> MemoryEngineBuilder<File> {
        MemoryEngineBuilder {
            embed_dim: self.embed_dim,
            search_config: self.search_config,
            reranker: self.reranker,
            upcaster_registry: self.upcaster_registry,
            backing: File {
                path: path.into(),
                read_only: false,
                backup_dir: None,
                read_pool_size: 4,
            },
        }
    }

    /// Open the in-memory engine.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` if the connection or schema setup fails.
    pub fn build(self) -> Result<MemoryEngine> {
        let pool = ConnectionPool::open_memory(self.embed_dim)?;
        MemoryEngine::init_from_pool(
            pool,
            self.embed_dim,
            self.search_config,
            self.upcaster_registry,
            self.reranker.map(Arc::from),
        )
    }
}

/// File-only setters and terminal for the file-backed state.
impl MemoryEngineBuilder<File> {
    /// Open in read-only mode: skip init/migration, reject writes.
    pub const fn read_only(mut self, read_only: bool) -> Self {
        self.backing.read_only = read_only;
        self
    }

    /// Directory for a WAL-safe pre-migration backup (`VACUUM INTO`).
    pub fn backup_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.backing.backup_dir = Some(dir.into());
        self
    }

    /// Number of read connections for a file-backed pool (default 4).
    ///
    /// Must be in `[1, 256]`. A value of `0` or one above the cap (256) is
    /// **rejected at open time** with [`MemoryError::Pool`] — `0` is not a covert
    /// in-memory request (#340/#356) and each read connection is a real OS file
    /// descriptor, so an oversized value is refused rather than allowed to
    /// exhaust the FD table (#415). Ignored for in-memory engines, which serve
    /// reads through the single shared connection.
    ///
    /// [`MemoryError::Pool`]: crate::MemoryError::Pool
    pub const fn read_pool_size(mut self, size: usize) -> Self {
        self.backing.read_pool_size = size;
        self
    }

    /// Assemble the [`EngineConfig`] this builder describes, *without* opening.
    ///
    /// Note: the reranker is **not** part of `EngineConfig` (it is not `Clone`),
    /// so `into_config` drops it. Use [`build`](Self::build) to open with the
    /// reranker attached.
    #[must_use]
    pub fn into_config(self) -> EngineConfig {
        self.to_config()
    }

    /// Internal: build the config shared by `into_config` and `build`.
    fn to_config(&self) -> EngineConfig {
        EngineConfig {
            path: self.backing.path.clone(),
            embed_dim: self.embed_dim,
            read_pool_size: self.backing.read_pool_size,
            search_config: self.search_config.clone(),
            backup_dir: self.backing.backup_dir.clone(),
            upcaster_registry: self.upcaster_registry.clone(),
            read_only: self.backing.read_only,
            backend: super::BackendKind::Sqlite,
        }
    }

    /// Open the file-backed engine.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Migration` if the stored `embed_dim` doesn't match.
    pub fn build(self) -> Result<MemoryEngine> {
        let config = self.to_config();
        MemoryEngine::open_from_config(&config, self.reranker.map(Arc::from))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIM: usize = 384;

    #[test]
    fn in_memory_minimal() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        assert_eq!(engine.embed_dim(), DIM);
        assert!(!engine.is_file_backed());
        assert_eq!(engine.reranker_name(), None);
    }

    #[test]
    fn file_minimal() {
        let dir = tempfile::tempdir().unwrap();
        let engine = MemoryEngine::builder(DIM)
            .path(dir.path().join("b.db"))
            .build()
            .unwrap();
        assert!(engine.is_file_backed());
    }

    // R3 / Gemini review: a search_config must be accepted even when the `ann`
    // feature is OFF (default build) — the builder must not force-link HNSW.
    #[test]
    fn in_memory_with_search_config_compiles_without_ann() {
        let engine = MemoryEngine::builder(DIM)
            .search_config(SearchConfig { ann_threshold: 10 })
            .build()
            .unwrap();
        assert_eq!(engine.embed_dim(), DIM);
    }

    #[test]
    fn into_config_round_trips_file_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.db");
        let config = MemoryEngine::builder(DIM)
            .path(path.clone())
            .read_only(true)
            .read_pool_size(2)
            .backup_dir(dir.path().join("backups"))
            .search_config(SearchConfig { ann_threshold: 99 })
            .into_config();
        assert_eq!(config.path, path);
        assert_eq!(config.embed_dim, DIM);
        assert!(config.read_only);
        assert_eq!(config.read_pool_size, 2);
        assert_eq!(config.backup_dir, Some(dir.path().join("backups")));
        assert_eq!(
            config.search_config,
            Some(SearchConfig { ann_threshold: 99 })
        );
    }

    #[test]
    fn file_with_backup_dir_builds() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        std::fs::create_dir_all(&backups).unwrap();
        // Initialize the DB once, then reopen with a backup dir so migration has
        // an existing file to VACUUM INTO.
        let path = dir.path().join("d.db");
        let _ = MemoryEngine::builder(DIM)
            .path(path.clone())
            .build()
            .unwrap();
        let engine = MemoryEngine::builder(DIM)
            .path(path)
            .backup_dir(backups)
            .build()
            .unwrap();
        assert!(engine.is_file_backed());
    }
}
