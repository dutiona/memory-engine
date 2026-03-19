//! Session log bootstrap: parse Claude Code JSONL session logs into historical memory.
//!
//! Pipeline: JSONL → parse → reconstruct turns → classify outcome → keyword pre-filter
//! → extract facts → ingest marker event + add facts.

pub mod extract;
pub mod filter;
pub mod metrics;
pub mod outcome;
pub mod parse;

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

/// Bootstrap one session from a JSONL reader into the memory engine.
///
/// Pipeline: parse → reconstruct turns → classify outcome → keyword pre-filter
/// → extract facts → ingest marker event + add facts (within a savepoint).
///
/// # Errors
///
/// Returns errors from the engine (DB, embedding, scope resolution) or
/// extraction failures. Malformed JSONL lines are skipped with `tracing::warn`.
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
    let entries = parse::parse_session_file(reader);
    report.entries_parsed = entries.len();

    // Extract session_id from first entry that has one
    let session_id = entries
        .iter()
        .find_map(|e| e.session_id.clone())
        .unwrap_or_default();

    if session_id.is_empty() {
        report.entries_malformed = entries.len();
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
            let _ = conn.execute_batch("ROLLBACK TO bootstrap");
            Err(e)
        }
    }
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
    let marker_event_id = EventStore::new(conn, upcaster_registry).insert(&marker_event)?;
    report.events_ingested = 1;

    // --- Reconstruct turns ---
    let mut turns = filter::reconstruct_turns(entries);
    report.turns_reconstructed = turns.len();

    if config.max_turns > 0 && turns.len() > config.max_turns {
        turns.truncate(config.max_turns);
    }

    // --- Classify outcome ---
    let (outcome, _signals) = outcome::classify_outcome(&turns);
    match outcome {
        SessionOutcome::Success => report.outcome_counts.success += 1,
        SessionOutcome::Failure => report.outcome_counts.failure += 1,
        SessionOutcome::Indeterminate => report.outcome_counts.indeterminate += 1,
    }

    // Update marker event payload with outcome
    // (We don't update the already-inserted event; outcome is in fact metadata)

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
                    fact_type: fact.fact_type.clone(),
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
                };
                c.should_pin(&temp)
            });

            let new_fact = NewFact {
                content: fact.content.clone(),
                content_hash: String::new(),
                embedding,
                fact_type: fact.fact_type.clone(),
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
        report.prewarm_metrics.avg_importance = importance_sum / total as f64;
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

        let file = std::fs::File::open(&path)?;
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
