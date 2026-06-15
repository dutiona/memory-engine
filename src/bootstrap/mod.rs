//! Session log bootstrap: parse Claude Code JSONL session logs into historical memory.
//!
//! Pipeline: JSONL → parse → reconstruct turns → classify outcome → keyword pre-filter
//! → extract facts → ingest marker event + add facts.

pub(crate) mod extract;
pub(crate) mod filter;
pub(crate) mod metrics;
pub(crate) mod outcome;
pub(crate) mod parse;
pub(crate) mod redact;

use std::io::BufRead;
use std::path::Path;

use chrono::Utc;
use rusqlite::Connection;

use crate::error::Result;
use crate::store::events::{EventFilter, EventStore};
use crate::store::facts::FactStore;
use crate::store::upcaster::UpcasterRegistry;
use crate::traits::{EmbeddingProvider, PersistenceClassifier};
use crate::types::{EventType, FactType, NewEvent, NewFact};

pub use extract::{ExtractedFact, KeywordExtractor, SessionExtractor};
pub use filter::{CandidateEpisode, ConversationTurn, EpisodeCategory, ToolCallRecord};
pub use metrics::{BootstrapConfig, BootstrapReport, PrewarmMetrics};
pub use outcome::{OutcomeSignals, SessionOutcome};
pub use redact::{
    DENYLIST_ENV_VAR, Finding, RedactionReport, load_secret_denylist, redact_entries,
    redact_entries_with_denylist, redact_text, redact_text_with_denylist, shannon_entropy,
};

/// Bootstrap one session from a JSONL reader into the memory engine.
///
/// Pipeline: parse → reconstruct turns → classify outcome → keyword pre-filter
/// → extract facts → ingest marker event + add facts (within a savepoint).
///
/// # Errors
///
/// Returns errors from the engine (DB, embedding) or extraction failures.
/// Malformed JSONL lines are skipped with `tracing::warn`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn bootstrap_session_inner(
    conn: &Connection,
    embed_dim: usize,
    upcaster_registry: &UpcasterRegistry,
    reader: impl BufRead,
    embedder: &dyn EmbeddingProvider,
    extractor: &dyn SessionExtractor,
    config: &BootstrapConfig,
    classifier: Option<&dyn PersistenceClassifier>,
    scope_id: i64,
) -> Result<BootstrapReport> {
    let mut report = BootstrapReport::default();

    // --- Parse ---
    let (entries, malformed) = parse::parse_session_file(reader);
    report.entries_parsed = entries.len();
    report.entries_malformed = malformed;

    // Extract session_id from first entry that has one
    let session_id = entries
        .iter()
        .find_map(|e| e.session_id.clone())
        .unwrap_or_default();

    if session_id.is_empty() {
        // No session_id found — nothing to bootstrap (but entries parsed fine)
        return Ok(report);
    }

    // --- Idempotency check ---
    if config.skip_existing {
        let event_store = EventStore::new(conn, upcaster_registry);
        let filter = EventFilter {
            session_id: Some(session_id.clone()),
            source: Some("bootstrap".into()),
            ..EventFilter::default()
        };
        let count = event_store.count(&filter)?;
        if count > 0 {
            report.sessions_skipped = 1;
            return Ok(report);
        }
    }

    // --- Savepoint for crash safety ---
    conn.execute_batch("SAVEPOINT bootstrap")?;

    let result = bootstrap_within_savepoint(
        conn,
        embed_dim,
        upcaster_registry,
        &entries,
        &session_id,
        embedder,
        extractor,
        config,
        classifier,
        scope_id,
        &mut report,
    );

    match result {
        Ok(()) => {
            conn.execute_batch("RELEASE bootstrap")?;
            report.sessions_processed = 1;
            Ok(report)
        }
        Err(e) => {
            // ROLLBACK TO restores the savepoint but keeps it open —
            // we must RELEASE to close it and avoid leaving the writer
            // in an open transaction.
            if let Err(rb_err) = conn.execute_batch("ROLLBACK TO bootstrap") {
                tracing::warn!(error = %rb_err, "savepoint ROLLBACK TO bootstrap failed");
            }
            if let Err(rel_err) = conn.execute_batch("RELEASE bootstrap") {
                tracing::warn!(error = %rel_err, "savepoint RELEASE bootstrap (after rollback) failed");
            }
            Err(e)
        }
    }
}

/// Ingest the bootstrap marker event (idempotency anchor) and return its id.
fn ingest_bootstrap_marker(
    conn: &Connection,
    upcaster_registry: &UpcasterRegistry,
    session_id: &str,
    scope_id: i64,
) -> Result<i64> {
    let marker_event = NewEvent {
        timestamp: Utc::now(),
        event_type: EventType::SystemEvent,
        payload: serde_json::json!({
            "action": "bootstrap_session",
            "session_id": session_id,
        }),
        source: "bootstrap".into(),
        session_id: Some(session_id.into()),
        scope_id,
        origin_node_id: "bootstrap".into(),
        sequence_id: 0,
        created_at: None,
    };
    EventStore::new(conn, upcaster_registry).insert(&marker_event)
}

/// Inner pipeline logic running within a savepoint.
#[allow(clippy::too_many_arguments)]
fn bootstrap_within_savepoint(
    conn: &Connection,
    embed_dim: usize,
    upcaster_registry: &UpcasterRegistry,
    entries: &[parse::SessionEntry],
    session_id: &str,
    embedder: &dyn EmbeddingProvider,
    extractor: &dyn SessionExtractor,
    config: &BootstrapConfig,
    classifier: Option<&dyn PersistenceClassifier>,
    scope_id: i64,
    report: &mut BootstrapReport,
) -> Result<()> {
    // --- Ingest marker event (idempotency anchor) ---
    let marker_event_id = ingest_bootstrap_marker(conn, upcaster_registry, session_id, scope_id)?;
    report.events_ingested = 1;

    // --- Reconstruct turns ---
    let turns = filter::reconstruct_turns(entries);
    report.turns_reconstructed = turns.len();

    // --- Classify outcome on FULL turns (before truncation) ---
    // Outcome evidence (commits, test results) lives at the end of sessions,
    // so we must not truncate before classification.
    let (outcome, _signals) = outcome::classify_outcome(&turns);

    // Apply max_turns limit AFTER outcome classification, keeping the TAIL
    // (most recent turns) since they contain resolution evidence.
    let turns = if config.max_turns > 0 && turns.len() > config.max_turns {
        turns[turns.len() - config.max_turns..].to_vec()
    } else {
        turns
    };
    match outcome {
        SessionOutcome::Success => report.outcome_counts.success += 1,
        SessionOutcome::Failure => report.outcome_counts.failure += 1,
        SessionOutcome::Indeterminate => report.outcome_counts.indeterminate += 1,
    }

    // Outcome is stored in fact metadata rather than updating the marker event.

    // --- Keyword pre-filter ---
    let candidates = filter::keyword_prefilter(&turns, session_id);
    report.candidates_found = candidates.len();

    for candidate in &candidates {
        match candidate.category {
            EpisodeCategory::Bug => report.category_counts.bug += 1,
            EpisodeCategory::Decision => report.category_counts.decision += 1,
            EpisodeCategory::Convention => report.category_counts.convention += 1,
            EpisodeCategory::Learning => report.category_counts.learning += 1,
        }
    }

    // --- Extract + add facts ---
    let mut importance_sum = 0.0;
    // Note: We use FactStore::insert() directly (bypassing engine.add_fact())
    // because we're inside a savepoint that already holds the write lock.
    // When the `ann` feature is enabled, bootstrapped facts won't be in the
    // HNSW index until the engine is reopened (index is built from DB at open).
    // This is acceptable for a batch import operation.
    let fact_store = FactStore::new(conn, embed_dim);

    for candidate in &candidates {
        let facts = extractor.extract(candidate, &outcome)?;

        for fact in &facts {
            let embedding = embedder.embed(&fact.content)?;
            let effective_created = candidate.timestamp;

            // Determine pinning
            let is_pinned = classifier.is_some_and(|c| {
                let temp = crate::types::Fact {
                    id: 0,
                    content: fact.content.clone(),
                    content_hash: String::new(),
                    embedding: embedding.clone(),
                    fact_type: fact.fact_type,
                    t_created: effective_created,
                    t_expired: None,
                    t_valid: None,
                    t_invalid: None,
                    source_event_id: Some(marker_event_id),
                    importance: fact.importance,
                    access_count: 0,
                    last_accessed: effective_created,
                    metadata: fact.metadata.clone(),
                    scope_id,
                    is_pinned: false,
                    importance_score: fact.importance,
                    surfaced_at: None,
                };
                c.should_pin(&temp)
            });

            let new_fact = NewFact {
                content: fact.content.clone(),
                content_hash: String::new(),
                embedding,
                fact_type: fact.fact_type,
                t_created: effective_created,
                t_expired: None,
                t_valid: None,
                t_invalid: None,
                source_event_id: Some(marker_event_id),
                scope_id,
                importance: fact.importance,
                access_count: 0,
                last_accessed: effective_created,
                metadata: fact.metadata.clone(),
                is_pinned,
            };

            fact_store.insert(&new_fact)?;
            report.facts_created += 1;

            // Update prewarm metrics
            match fact.fact_type {
                FactType::Episodic => report.prewarm_metrics.episodic_count += 1,
                FactType::Semantic => report.prewarm_metrics.semantic_count += 1,
                FactType::Procedural => report.prewarm_metrics.procedural_count += 1,
            }
            importance_sum += fact.importance;
        }
    }

    let total = report.prewarm_metrics.total_count();
    if total > 0 {
        // Episode tally is tiny (<< 2^52); the usize -> f64 cast cannot lose
        // precision, so the lint is a non-issue and the direct cast is clearest.
        #[allow(clippy::cast_precision_loss)]
        {
            report.prewarm_metrics.avg_importance = importance_sum / total as f64;
        }
    }

    Ok(())
}

/// Bootstrap all JSONL session logs in a directory.
///
/// Discovers top-level `*.jsonl` files (not in `subagents/` subdirectories).
/// Processes each session independently, aggregating reports.
///
/// # Errors
///
/// Returns `MemoryError::Io` for directory traversal failures.
/// Individual session failures are logged and skipped (not fatal).
#[allow(clippy::too_many_arguments)]
pub(crate) fn bootstrap_directory_inner(
    conn: &Connection,
    embed_dim: usize,
    upcaster_registry: &UpcasterRegistry,
    dir: &Path,
    embedder: &dyn EmbeddingProvider,
    extractor: &dyn SessionExtractor,
    config: &BootstrapConfig,
    classifier: Option<&dyn PersistenceClassifier>,
    scope_id: i64,
) -> Result<BootstrapReport> {
    let mut aggregate = BootstrapReport::default();

    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        // Only top-level .jsonl files (skip directories like subagents/)
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }

        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "skipping unreadable session file");
                continue;
            }
        };
        let reader = std::io::BufReader::new(file);

        match bootstrap_session_inner(
            conn,
            embed_dim,
            upcaster_registry,
            reader,
            embedder,
            extractor,
            config,
            classifier,
            scope_id,
        ) {
            Ok(report) => aggregate.merge(&report),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "skipping session file");
            }
        }
    }

    Ok(aggregate)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::store::schema::{init_schema, open_memory};
    use crate::store::upcaster::UpcasterRegistry;

    /// A no-op embedder used when we know embedding will never be called.
    struct NeverCalledEmbedder;
    impl crate::traits::EmbeddingProvider for NeverCalledEmbedder {
        fn embed(&self, _text: &str) -> crate::error::Result<Vec<f32>> {
            panic!("embed() must not be called in this test");
        }
    }

    /// JSONL with two valid entries but neither carries a `sessionId`.
    #[test]
    fn bootstrap_valid_jsonl_no_session_id_returns_empty_report() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        let registry = UpcasterRegistry::new();

        let jsonl = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"hello"}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}
"#;
        let reader = Cursor::new(jsonl);
        let config = BootstrapConfig::default();
        let extractor = extract::KeywordExtractor;

        let report = bootstrap_session_inner(
            &conn,
            4,
            &registry,
            reader,
            &NeverCalledEmbedder,
            &extractor,
            &config,
            None,
            1, // root scope id
        )
        .expect("bootstrap_session_inner should succeed");

        // Early-exit path: entries parsed but nothing processed.
        assert!(report.entries_parsed > 0, "expected entries to be parsed");
        assert_eq!(
            report.sessions_processed, 0,
            "no session should be processed"
        );
        assert_eq!(report.facts_created, 0);
    }
}
