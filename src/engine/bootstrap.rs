use rusqlite::Connection;

use crate::error::Result;
use crate::traits::{EmbeddingProvider, PersistenceClassifier};

use super::MemoryEngine;

impl MemoryEngine {
    // --- Internal helpers ---

    /// Resolve (or create) the scope for a bootstrap config, returning its ID.
    ///
    /// When `config.scope` is `Some(path)`, ensures the path exists in the DB
    /// and inserts the node into the in-memory scope tree cache.
    /// When `None`, returns 1 (root scope).
    ///
    /// # Errors
    ///
    /// Returns errors from `ScopeStore::ensure_path` or `ScopeStore::get`.
    fn ensure_bootstrap_scope(&self, conn: &Connection, scope: Option<&str>) -> Result<i64> {
        scope.map_or_else(|| Ok(1), |path| self.ensure_scope_with_conn(conn, path))
    }

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
        let scope_id = self.ensure_bootstrap_scope(&conn, config.scope.as_deref())?;
        // The embedding identity is stamped (#613) inside the session savepoint, gated
        // on a fact actually being written (#643) — see `bootstrap_within_savepoint`.
        // A no-op session (zero extracted facts) therefore leaves the store unstamped.
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
        let scope_id = self.ensure_bootstrap_scope(&conn, config.scope.as_deref())?;
        // Each session is stamped (#613) inside its own savepoint, gated on a fact
        // being written (#643) — see `bootstrap_within_savepoint`. This preserves
        // per-session independence (each commits its identity atomically with its
        // facts) and leaves an empty/no-op directory unstamped.
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
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only.
    /// Returns `MemoryError::Io` for directory traversal failures, or an
    /// embedding/DB error (which aborts; a re-run resumes idempotently).
    #[allow(clippy::significant_drop_tightening)]
    pub fn bootstrap_memory_directory(
        &self,
        dir: &std::path::Path,
        embedder: &dyn EmbeddingProvider,
        config: &crate::bootstrap::BootstrapConfig,
        classifier: Option<&dyn PersistenceClassifier>,
    ) -> Result<crate::bootstrap::BootstrapReport> {
        let conn = self.write_conn()?;
        let scope_id = self.ensure_bootstrap_scope(&conn, config.scope.as_deref())?;
        // Stamp the embedding identity on first write (#613) — and, unlike the
        // savepoint paths above, this one MUST stay meta-first (#643). It is
        // autocommit-per-file (no wrapping savepoint), so each file commits its
        // vector independently; deferring the stamp until after a vector is written
        // would reopen the orphan-vector crash window (a vector committed before its
        // identity). Recording before the first file keeps a crash benign: identity
        // declared, possibly no facts — the same harmless no-op-stamp #643 removes
        // from the deferrable paths, retained here because it is the crash-safe choice.
        self.record_embedding_identity(&conn, embedder)?;
        crate::bootstrap::memory_dir::bootstrap_memory_directory_inner(
            &conn,
            self.embed_dim,
            dir,
            embedder,
            config,
            classifier,
            scope_id,
        )
    }
}
