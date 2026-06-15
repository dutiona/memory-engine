use crate::error::Result;
use crate::store::scopes::ScopeStore;
use crate::traits::{EmbeddingProvider, PersistenceClassifier};

use super::MemoryEngine;

impl MemoryEngine {
    // --- Public API: Bootstrap ---

    /// Bootstrap a single JSONL session log into historical memory.
    ///
    /// Parses the session log, extracts noteworthy episodes via keyword
    /// pre-filter, classifies session outcome, and ingests extracted facts.
    /// The entire session import is wrapped in a `SQLite` savepoint for
    /// crash safety (all-or-nothing per session).
    ///
    /// For LLM-powered extraction, provide a custom [`SessionExtractor`].
    /// The default [`KeywordExtractor`] requires no LLM.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only.
    /// Returns errors from embedding, DB insertion, or scope resolution.
    // `conn` (write lock) legitimately spans scope resolution and the inner
    // bootstrap call (the function's return expression), so it cannot be
    // tightened without splitting the atomic import across two lock
    // acquisitions. clippy's nursery suggestion would not compile here.
    #[allow(clippy::significant_drop_tightening)]
    pub fn bootstrap_session(
        &self,
        reader: impl std::io::BufRead,
        embedder: &dyn EmbeddingProvider,
        extractor: &dyn crate::bootstrap::SessionExtractor,
        config: &crate::bootstrap::BootstrapConfig,
        classifier: Option<&dyn PersistenceClassifier>,
    ) -> Result<crate::bootstrap::BootstrapReport> {
        let conn = self.write_conn()?;
        let scope_id = match &config.scope {
            Some(path) => {
                let scope_store = ScopeStore::new(&conn);
                let id = scope_store.ensure_path(path)?;
                let node = scope_store.get(id)?;
                self.scope_tree.write().insert(node);
                id
            }
            None => 1,
        };
        crate::bootstrap::bootstrap_session_inner(
            &conn,
            self.embed_dim,
            &self.upcaster_registry,
            reader,
            embedder,
            extractor,
            config,
            classifier,
            scope_id,
        )
    }

    /// Bootstrap all JSONL session logs in a directory.
    ///
    /// Discovers top-level `*.jsonl` files (not subagent subdirectories).
    /// Each session is processed independently within its own savepoint.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only.
    /// Returns `MemoryError::Io` for directory traversal failures.
    // `conn` (write lock) legitimately spans scope resolution and the inner
    // bootstrap call (the function's return expression), so it cannot be
    // tightened without splitting the atomic import across two lock
    // acquisitions. clippy's nursery suggestion would not compile here.
    #[allow(clippy::significant_drop_tightening)]
    pub fn bootstrap_directory(
        &self,
        dir: &std::path::Path,
        embedder: &dyn EmbeddingProvider,
        extractor: &dyn crate::bootstrap::SessionExtractor,
        config: &crate::bootstrap::BootstrapConfig,
        classifier: Option<&dyn PersistenceClassifier>,
    ) -> Result<crate::bootstrap::BootstrapReport> {
        let conn = self.write_conn()?;
        let scope_id = match &config.scope {
            Some(path) => {
                let scope_store = ScopeStore::new(&conn);
                let id = scope_store.ensure_path(path)?;
                let node = scope_store.get(id)?;
                self.scope_tree.write().insert(node);
                id
            }
            None => 1,
        };
        crate::bootstrap::bootstrap_directory_inner(
            &conn,
            self.embed_dim,
            &self.upcaster_registry,
            dir,
            embedder,
            extractor,
            config,
            classifier,
            scope_id,
        )
    }
}
