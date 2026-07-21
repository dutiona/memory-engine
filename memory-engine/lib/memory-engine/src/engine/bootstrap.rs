use std::sync::Arc;

use crate::bootstrap::{
    BootstrapConfig, BootstrapReport, ParsedSession, PrepareCtx, PreparedSession, SessionExtractor,
};
use crate::error::Result;
use crate::storage::BootstrapIngestOutcome;
use crate::traits::{EmbeddingProvider, PersistenceClassifier};
use crate::types::{EventFilter, FactType};

use super::{MemoryEngine, spawn_join_err};

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

    /// The idempotency filter for a bootstrap session marker (`session_id` +
    /// `source = "bootstrap"`) — the same predicate the marker event is written with.
    fn bootstrap_marker_filter(session_id: &str) -> EventFilter {
        EventFilter {
            session_id: Some(session_id.to_string()),
            source: Some("bootstrap".into()),
            ..EventFilter::default()
        }
    }

    /// Drive one parsed session to completion: idempotency check → prepare (embed,
    /// off the async executor) → one atomic marked-batch ingest → report assembly.
    ///
    /// Shared by [`bootstrap_session`](Self::bootstrap_session) and the per-file loop
    /// of [`bootstrap_directory`](Self::bootstrap_directory). The consumer callbacks
    /// (`extract`/`embed`/`should_pin`) run inside [`spawn_blocking`](tokio::task::spawn_blocking)
    /// (a blocking `EmbeddingProvider`/`SessionExtractor` must not park the runtime);
    /// the only DB touch is the single `ingest_bootstrap_batch_atomic` call, so the write
    /// lock is held only for the durable write, never across embedding.
    ///
    /// # Errors
    ///
    /// Returns errors from embedding, extraction, or the atomic ingest.
    async fn bootstrap_one_session(
        &self,
        parsed: ParsedSession,
        embedder: Arc<dyn EmbeddingProvider>,
        extractor: Arc<dyn SessionExtractor>,
        config: &BootstrapConfig,
        classifier: Option<Arc<dyn PersistenceClassifier>>,
        scope_id: i64,
    ) -> Result<BootstrapReport> {
        let mut report = BootstrapReport {
            entries_parsed: parsed.entries.len(),
            entries_malformed: parsed.malformed,
            ..BootstrapReport::default()
        };

        // No session_id — nothing to bootstrap (but entries parsed fine).
        if parsed.session_id.is_empty() {
            return Ok(report);
        }

        // The bootstrap-marker idempotency filter (only when `skip_existing`). Built here,
        // before `parsed` is moved into the prepare closure, so it can be reused for BOTH
        // the cheap read-pool early-out below AND the authoritative under-lock guard
        // passed to `ingest_bootstrap_batch_atomic` (#816 B1).
        let skip_filter = config
            .skip_existing
            .then(|| Self::bootstrap_marker_filter(&parsed.session_id));

        // --- Cheap early-out, BEFORE the expensive embed: skip a session already
        //     bootstrapped. Optimization only — it runs on the read pool and races the
        //     write, so it is NOT authoritative; the guard inside the atomic ingest below
        //     is what makes concurrent same-session bootstraps non-duplicating. ---
        if let Some(filter) = &skip_filter
            && self.storage.count_events(filter).await? > 0
        {
            report.sessions_skipped = 1;
            return Ok(report);
        }

        // --- Prepare (reconstruct → classify → prefilter → extract → embed) off the
        //     async executor — the consumer callbacks may block. ---
        let fingerprint = embedder.fingerprint();
        let config_owned = config.clone();
        let prepared: PreparedSession = tokio::task::spawn_blocking(move || {
            let ctx = PrepareCtx {
                embedder: &*embedder,
                extractor: &*extractor,
                config: &config_owned,
                classifier: classifier.as_deref(),
                scope_id,
            };
            crate::bootstrap::prepare_session(&ctx, &parsed)
        })
        .await
        .map_err(spawn_join_err)??;

        let PreparedSession {
            marker,
            facts,
            report: prepared_report,
        } = prepared;
        // Split the accounting metadata off before the facts are moved into the port
        // (which consumes them to stamp `source_event_id`).
        let mut metas = Vec::with_capacity(facts.len());
        let mut new_facts = Vec::with_capacity(facts.len());
        for (fact, redactions) in facts {
            metas.push((fact.fact_type, fact.base_importance, redactions));
            new_facts.push(fact);
        }

        // --- Atomic ingest: under-lock idempotency guard + marker + batch,
        //     dedup-with-reinforce, identity stamp. ---
        let flags = match self
            .storage
            .ingest_bootstrap_batch_atomic(
                Some(&marker),
                skip_filter.as_ref(),
                new_facts,
                &fingerprint,
                self.embed_dim,
            )
            .await?
        {
            // Lost the race with a concurrent same-session bootstrap that committed its
            // marker between our early-out and this write. Report a clean skip (matching
            // the pre-embed early-out) and discard the prepared work — do NOT merge the
            // prepare-time tallies, so a raced-skip looks exactly like an early skip.
            BootstrapIngestOutcome::Skipped => {
                report.sessions_skipped = 1;
                return Ok(report);
            }
            BootstrapIngestOutcome::Ingested(flags) => flags,
        };

        // Fold the prepare-time tallies (turns / candidates / outcome / category /
        // turn-level redactions / events) into the entries-seeded report — only on the
        // ingested path (a raced-skip returned above with just entries + sessions_skipped).
        report.merge(&prepared_report);

        // --- Report accounting from the per-fact created/reinforced flags. ---
        // The port returns exactly one flag per input fact, in order (its documented
        // contract), so `metas` (one per fact) and `flags` align 1:1. Guard that now-
        // cross-crate invariant: a mismatch would let `zip` silently truncate into an
        // under-counted report, so surface it as a typed error rather than drop rows (#725).
        if metas.len() != flags.len() {
            return Err(crate::error::MemoryError::Internal(format!(
                "bootstrap ingest returned {} flags for {} facts (port contract violation)",
                flags.len(),
                metas.len()
            )));
        }
        let mut importance_sum = 0.0;
        for ((fact_type, base_importance, redactions), (_, reinforced)) in
            metas.into_iter().zip(flags)
        {
            if reinforced {
                // A reinforcement adds no new row: no prewarm / importance / redaction
                // count (the audit counter stays idempotent across re-runs).
                report.facts_reinforced += 1;
            } else {
                report.facts_created += 1;
                report.secrets_redacted += redactions;
                match fact_type {
                    FactType::Episodic => report.prewarm_metrics.episodic_count += 1,
                    FactType::Semantic => report.prewarm_metrics.semantic_count += 1,
                    FactType::Procedural => report.prewarm_metrics.procedural_count += 1,
                }
                importance_sum += base_importance;
            }
        }
        let total = report.prewarm_metrics.total_count();
        if total > 0 {
            // Episode tally is tiny (<< 2^52): the usize -> f64 cast cannot lose
            // precision, so the direct cast is clearest.
            #[allow(clippy::cast_precision_loss)]
            {
                report.prewarm_metrics.avg_importance = importance_sum / total as f64;
            }
        }
        report.sessions_processed = 1;
        Ok(report)
    }

    // --- Public API: Bootstrap ---

    /// Bootstrap a single JSONL session log into historical memory.
    ///
    /// Parses the session log, extracts noteworthy episodes via keyword pre-filter,
    /// classifies session outcome, embeds the extracted facts, and ingests them in a
    /// single `SQLite` savepoint below the seam (all-or-nothing per session). Parsing
    /// and the (possibly blocking) embedder/extractor calls run on a blocking thread;
    /// only the atomic ingest touches the DB.
    ///
    /// For LLM-powered extraction, provide a custom [`SessionExtractor`].
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
        self.ensure_open()?;
        let scope_id = self
            .resolve_bootstrap_scope(config.scope.as_deref())
            .await?;
        // Parse on a blocking thread (file I/O), then drive the shared pipeline.
        let cfg = config.clone();
        let parsed =
            tokio::task::spawn_blocking(move || crate::bootstrap::parse_session(reader, &cfg))
                .await
                .map_err(spawn_join_err)?;
        self.bootstrap_one_session(parsed, embedder, extractor, config, classifier, scope_id)
            .await
    }

    /// Bootstrap all JSONL session logs in a directory.
    ///
    /// Discovers `*.jsonl` files at any depth, skipping `subagents/` subdirectories.
    /// Each session is processed independently within its own savepoint; an
    /// unreadable file or a per-session failure is logged and skipped (not fatal),
    /// and reports are aggregated.
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
        self.ensure_open()?;
        let scope_id = self
            .resolve_bootstrap_scope(config.scope.as_deref())
            .await?;

        // Discover the session files on a blocking thread (recursive dir I/O).
        let dir = dir.to_path_buf();
        let files = tokio::task::spawn_blocking(move || {
            let mut files = Vec::new();
            crate::bootstrap::collect_jsonl_files(&dir, &mut files)?;
            files.sort();
            Ok::<_, std::io::Error>(files)
        })
        .await
        .map_err(spawn_join_err)??;

        let mut aggregate = crate::bootstrap::BootstrapReport::default();
        for path in files {
            let cfg = config.clone();
            let path_for_log = path.clone();
            // Open + parse this file on a blocking thread; a read failure is skipped.
            let opened = tokio::task::spawn_blocking(move || {
                std::fs::File::open(&path).map(|file| {
                    crate::bootstrap::parse_session(std::io::BufReader::new(file), &cfg)
                })
            })
            .await
            .map_err(spawn_join_err)?;
            let parsed = match opened {
                Ok(parsed) => parsed,
                Err(e) => {
                    tracing::warn!(path = %path_for_log.display(), error = %e, "skipping unreadable session file");
                    continue;
                }
            };
            match self
                .bootstrap_one_session(
                    parsed,
                    embedder.clone(),
                    extractor.clone(),
                    config,
                    classifier.clone(),
                    scope_id,
                )
                .await
            {
                Ok(report) => aggregate.merge(&report),
                Err(e) => {
                    tracing::warn!(path = %path_for_log.display(), error = %e, "skipping session file");
                }
            }
        }
        Ok(aggregate)
    }

    /// Import native `.md` memory files (recursive) from a directory.
    ///
    /// Each file becomes one fact with a **backdated** `t_created` (frontmatter
    /// date, else a filename-encoded date, else file mtime), type-routed from its
    /// frontmatter `type:`, redaction-gated, and dedup-with-reinforced. Unlike
    /// [`Self::bootstrap_directory`] this path has no session/turn structure and no
    /// LLM extractor — the file body is the fact. Sources are read-only.
    ///
    /// Facts are written one at a time in **autocommit** (no wrapping savepoint):
    /// one bad file does not abort the batch, and a re-run is idempotent
    /// (dedup-with-reinforcement). Per-file parsing + embedding run on a blocking
    /// thread; the write is a single `insert_or_reinforce_fact` port call.
    ///
    /// The embedding identity is stamped meta-first (#643) before the first file,
    /// because this path is autocommit-per-file (no wrapping savepoint to defer under).
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
        self.ensure_open()?;
        let scope_id = self
            .resolve_bootstrap_scope(config.scope.as_deref())
            .await?;

        // Meta-first identity stamp (#643): record before the first file, because this
        // path is autocommit-per-file (no wrapping savepoint to defer the stamp under).
        let fingerprint = embedder.fingerprint();
        self.storage
            .record_embedding_fingerprint_if_absent(&fingerprint, self.embed_dim)
            .await?;

        // Discover the `.md` files on a blocking thread (recursive dir I/O).
        let dir = dir.to_path_buf();
        let files = tokio::task::spawn_blocking(move || {
            let mut files = Vec::new();
            crate::bootstrap::memory_dir::collect_md_files(&dir, &mut files)?;
            files.sort();
            Ok::<_, std::io::Error>(files)
        })
        .await
        .map_err(spawn_join_err)??;

        let mut report = crate::bootstrap::BootstrapReport::default();
        let mut importance_sum = 0.0;
        for path in files {
            let embedder = Arc::clone(&embedder);
            let cfg = config.clone();
            let classifier = classifier.clone();
            // Read + parse + embed one file on a blocking thread.
            let outcome = tokio::task::spawn_blocking(move || {
                crate::bootstrap::memory_dir::prepare_memory_file(
                    &path,
                    &*embedder,
                    &cfg,
                    classifier.as_deref(),
                    scope_id,
                )
            })
            .await
            .map_err(spawn_join_err)??;

            match outcome {
                None => report.memory_files_skipped += 1,
                Some((fact, redactions)) => {
                    report.memory_files_parsed += 1;
                    let fact_type = fact.fact_type;
                    let base_importance = fact.base_importance;
                    // Autocommit-per-file semantics preserved: each file is its own
                    // one-fact, marker-less bootstrap batch (own savepoint). Routing
                    // through the bootstrap primitive (rather than `insert_or_reinforce_fact`)
                    // keeps the `.md` path from populating the live HNSW index, matching
                    // the session path and the pre-#816 behavior.
                    // No idempotency filter: the `.md` path dedups on content, so it
                    // never skips → always `Ingested` with exactly one flag (one fact in).
                    // Enforce that storage-port contract with a typed error, mirroring the
                    // session path's length guard above (#725): an out-of-contract backend
                    // must fail loudly, not silently miscount facts_created/reinforced/
                    // secrets_redacted in release (a `debug_assert` would compile out there).
                    let reinforced = match self
                        .storage
                        .ingest_bootstrap_batch_atomic(
                            None,
                            None,
                            vec![fact],
                            &fingerprint,
                            self.embed_dim,
                        )
                        .await?
                    {
                        BootstrapIngestOutcome::Ingested(flags) => match flags.as_slice() {
                            [(_, reinforced)] => *reinforced,
                            other => {
                                return Err(crate::error::MemoryError::Internal(format!(
                                    "memory-dir bootstrap ingest returned {} flags for 1 fact (port contract violation)",
                                    other.len()
                                )));
                            }
                        },
                        BootstrapIngestOutcome::Skipped => {
                            return Err(crate::error::MemoryError::Internal(
                                "memory-dir bootstrap passed no skip filter but storage returned Skipped (port contract violation)"
                                    .to_string(),
                            ));
                        }
                    };
                    if reinforced {
                        report.facts_reinforced += 1;
                    } else {
                        report.facts_created += 1;
                        report.secrets_redacted += redactions;
                        match fact_type {
                            FactType::Episodic => report.prewarm_metrics.episodic_count += 1,
                            FactType::Semantic => report.prewarm_metrics.semantic_count += 1,
                            FactType::Procedural => report.prewarm_metrics.procedural_count += 1,
                        }
                        importance_sum += base_importance;
                    }
                }
            }
        }
        let total = report.prewarm_metrics.total_count();
        if total > 0 {
            // Tiny tally (<< 2^52): the usize -> f64 cast cannot lose precision.
            #[allow(clippy::cast_precision_loss)]
            {
                report.prewarm_metrics.avg_importance = importance_sum / total as f64;
            }
        }
        Ok(report)
    }
}
