use std::sync::Arc;

use crate::error::Result;
use crate::traits::{EmbeddingProvider, PersistenceClassifier};

use super::MemoryEngine;

impl MemoryEngine {
    // --- Internal helpers ---

    /// Resolve (or create) the scope for a bootstrap config, returning its ID.
    ///
    /// When `scope` is `Some(path)`, ensures the path exists in the DB (port write)
    /// and mirrors the chain into the in-memory scope tree cache. When `None`,
    /// returns 1 (root scope). The scope is resolved up front — autocommit-separate
    /// from the import savepoints below the seam, matching the prior behavior.
    ///
    /// # Errors
    ///
    /// Returns errors from `ensure_scope_path` or the scope cache walk.
    async fn resolve_bootstrap_scope(&self, scope: Option<&str>) -> Result<i64> {
        match scope {
            Some(path) => {
                let id = self.storage.ensure_scope_path(path).await?;
                self.cache_scope_chain(id).await?;
                Ok(id)
            }
            None => Ok(1), // root scope
        }
    }

    // --- Public API: Bootstrap ---

    /// Bootstrap a single JSONL session log into historical memory.
    ///
    /// Parses the session log, extracts noteworthy episodes via keyword
    /// pre-filter, classifies session outcome, and ingests extracted facts.
    /// The entire session import runs in one `SQLite` savepoint below the seam
    /// (all-or-nothing per session); the embedder/extractor are `Arc<dyn _>` so the
    /// (possibly blocking) consumer calls run on the backend's blocking thread.
    ///
    /// For LLM-powered extraction, provide a custom [`SessionExtractor`](crate::bootstrap::SessionExtractor).
    /// The default [`KeywordExtractor`](crate::bootstrap::KeywordExtractor) requires no LLM.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only.
    /// Returns errors from embedding, DB insertion, or scope resolution.
    pub async fn bootstrap_session(
        &self,
        reader: impl std::io::BufRead + Send + 'static,
        embedder: Arc<dyn EmbeddingProvider>,
        extractor: Arc<dyn crate::bootstrap::SessionExtractor>,
        config: &crate::bootstrap::BootstrapConfig,
        classifier: Option<Arc<dyn PersistenceClassifier>>,
    ) -> Result<crate::bootstrap::BootstrapReport> {
        let scope_id = self
            .resolve_bootstrap_scope(config.scope.as_deref())
            .await?;
        self.storage
            .bootstrap_session_atomic(
                Box::new(reader),
                embedder,
                extractor,
                config.clone(),
                classifier,
                scope_id,
            )
            .await
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
    pub async fn bootstrap_directory(
        &self,
        dir: &std::path::Path,
        embedder: Arc<dyn EmbeddingProvider>,
        extractor: Arc<dyn crate::bootstrap::SessionExtractor>,
        config: &crate::bootstrap::BootstrapConfig,
        classifier: Option<Arc<dyn PersistenceClassifier>>,
    ) -> Result<crate::bootstrap::BootstrapReport> {
        let scope_id = self
            .resolve_bootstrap_scope(config.scope.as_deref())
            .await?;
        self.storage
            .bootstrap_directory_atomic(
                dir.to_path_buf(),
                embedder,
                extractor,
                config.clone(),
                classifier,
                scope_id,
            )
            .await
    }

    /// Import native `.md` memory files (recursive) from a directory.
    ///
    /// Each file becomes one fact with a **backdated** `t_created` (frontmatter
    /// date, else a filename-encoded date, else file mtime), type-routed from
    /// its frontmatter `type:`,
    /// redaction-gated, and dedup-with-reinforced. Unlike [`Self::bootstrap_directory`]
    /// this path has no session/turn structure and no LLM extractor — the file
    /// body is the fact. Sources are read-only.
    ///
    /// Dedup matches on body content (per #520), so a re-import after editing
    /// ONLY a file's frontmatter (e.g. correcting `type:` or `description`) while
    /// the body is unchanged reinforces the existing row and **keeps its original
    /// `fact_type`/metadata** — frontmatter-only corrections are not re-synced.
    /// This is an import/backfill tool, not an edit-sync tool; change the body or
    /// expire the fact to re-derive metadata.
    ///
    /// The embedding identity is stamped meta-first (#643) below the seam, because
    /// this path is autocommit-per-file (no wrapping savepoint).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only.
    /// Returns `MemoryError::Io` for directory traversal failures, or an
    /// embedding/DB error (which aborts; a re-run resumes idempotently).
    pub async fn bootstrap_memory_directory(
        &self,
        dir: &std::path::Path,
        embedder: Arc<dyn EmbeddingProvider>,
        config: &crate::bootstrap::BootstrapConfig,
        classifier: Option<Arc<dyn PersistenceClassifier>>,
    ) -> Result<crate::bootstrap::BootstrapReport> {
        let scope_id = self
            .resolve_bootstrap_scope(config.scope.as_deref())
            .await?;
        self.storage
            .bootstrap_memory_directory_atomic(
                dir.to_path_buf(),
                embedder,
                config.clone(),
                classifier,
                scope_id,
            )
            .await
    }
}
