//! Session log bootstrap: parse Claude Code JSONL session logs into historical memory.
//!
//! Pipeline: JSONL → parse → [idempotency check] → ingest marker event (savepoint anchor)
//! → reconstruct turns → classify outcome → keyword pre-filter → extract facts → add facts.

pub(crate) mod extract;
pub(crate) mod filter;
pub(crate) mod memory_dir;
pub(crate) mod metrics;
pub(crate) mod outcome;
pub(crate) mod parse;
pub(crate) mod redact;

use std::io::BufRead;
use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::error::Result;
use crate::store::events::{EventFilter, EventStore};
use crate::store::facts::FactStore;
use crate::store::upcaster::UpcasterRegistry;
use crate::traits::{EmbeddingProvider, PersistenceClassifier};
use crate::types::{EventType, FactType, NewEvent, NewFact};

pub use extract::{ExtractedFact, KeywordExtractor, SessionExtractor};
pub use filter::{CandidateEpisode, ConversationTurn, EpisodeCategory, ToolCallRecord};
pub use memory_dir::{ParsedMemory, parse_memory_file};
pub use metrics::{BootstrapConfig, BootstrapReport, PrewarmMetrics};
pub use outcome::{OutcomeSignals, SessionOutcome};
pub use redact::{
    DENYLIST_ENV_VAR, Finding, RedactionReport, load_secret_denylist, redact_entries,
    redact_entries_with_denylist, redact_json_strings, redact_text, redact_text_with_denylist,
    shannon_entropy,
};

/// Shared dependencies threaded through the JSONL bootstrap pipeline (#123).
///
/// Bundles the engine-level handles and per-run configuration that every stage
/// of `bootstrap_session_inner` → `bootstrap_within_savepoint` →
/// `store_extracted_fact` (and the directory driver) needs, so each function
/// takes the context plus only its own stage-specific arguments instead of a
/// 9-to-11-parameter list. All members are borrows scoped to one bootstrap call.
pub(crate) struct BootstrapContext<'a> {
    /// Open write connection (the caller holds the write lock).
    pub conn: &'a Connection,
    /// Embedding dimensionality the [`FactStore`] is opened with.
    pub embed_dim: usize,
    /// Event-schema upcaster registry for the marker/event writes.
    pub upcaster_registry: &'a UpcasterRegistry,
    /// Consumer-supplied embedder (no LLM/network owned by the engine).
    pub embedder: &'a dyn EmbeddingProvider,
    /// Fact extractor (default keyword, or a consumer LLM extractor).
    pub extractor: &'a dyn SessionExtractor,
    /// Per-run configuration (scope, caps, redaction, idempotency).
    pub config: &'a BootstrapConfig,
    /// Optional auto-pin classifier consulted per created fact.
    pub classifier: Option<&'a dyn PersistenceClassifier>,
    /// Resolved scope id facts are ingested into.
    pub scope_id: i64,
}

/// Bootstrap one session from a JSONL reader into the memory engine.
///
/// Pipeline: parse → [idempotency check] → ingest marker event (savepoint anchor)
/// → reconstruct turns → classify outcome → keyword pre-filter → extract facts → add facts.
/// The marker event is ingested *first* inside the savepoint so it can anchor the
/// idempotency check; turn reconstruction, classification, and extraction follow.
///
/// # Errors
///
/// Returns errors from the engine (DB, embedding) or extraction failures.
/// Malformed JSONL lines are skipped with `tracing::warn`.
pub(crate) fn bootstrap_session_inner(
    ctx: &BootstrapContext<'_>,
    reader: impl BufRead,
) -> Result<BootstrapReport> {
    let mut report = BootstrapReport::default();

    // --- Parse (bounded against hostile/corrupt input, #293) ---
    let (entries, malformed) =
        parse::parse_session_file(reader, ctx.config.max_session_bytes, ctx.config.max_entries);
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
    if ctx.config.skip_existing {
        let event_store = EventStore::new(ctx.conn, ctx.upcaster_registry);
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
    ctx.conn.execute_batch("SAVEPOINT bootstrap")?;

    let result = bootstrap_within_savepoint(ctx, &entries, &session_id, &mut report);

    match result {
        Ok(()) => {
            ctx.conn.execute_batch("RELEASE bootstrap")?;
            report.sessions_processed = 1;
            Ok(report)
        }
        Err(e) => {
            // ROLLBACK TO restores the savepoint but keeps it open —
            // we must RELEASE to close it and avoid leaving the writer
            // in an open transaction.
            if let Err(rb_err) = ctx.conn.execute_batch("ROLLBACK TO bootstrap") {
                tracing::warn!(error = %rb_err, "savepoint ROLLBACK TO bootstrap failed");
            }
            if let Err(rel_err) = ctx.conn.execute_batch("RELEASE bootstrap") {
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

/// Redact secrets/PII from every turn in place (user + assistant text and each
/// tool call's input/stdout/stderr), returning the total finding count.
///
/// Run BEFORE extraction so the extractor — which may be a caller-supplied
/// LLM-powered [`SessionExtractor`] reaching an external service — never sees an
/// unredacted secret. A `[REDACTED:*]` placeholder is never re-matched, so this
/// is idempotent.
fn redact_turns(turns: &mut [ConversationTurn], denylist: &[String]) -> usize {
    let redact_field = |s: &mut String, n: &mut usize| {
        let (clean, findings) = redact::redact_text_with_denylist(s, denylist);
        if !findings.is_empty() {
            *s = clean;
            *n += findings.len();
        }
    };
    let mut total = 0;
    for turn in turns {
        redact_field(&mut turn.user_text, &mut total);
        redact_field(&mut turn.assistant_text, &mut total);
        for tc in &mut turn.tool_calls {
            total += redact::redact_json_strings(&mut tc.input, denylist);
            if let Some(s) = tc.stdout.as_mut() {
                redact_field(s, &mut total);
            }
            if let Some(s) = tc.stderr.as_mut() {
                redact_field(s, &mut total);
            }
        }
    }
    total
}

/// Redact, embed, and store one extracted fact (dedup-with-reinforcement).
///
/// Returns the importance to fold into the prewarm average — `0.0` when the
/// fact was *reinforced* rather than created. Updates `report`'s
/// `facts_created`/`facts_reinforced`/`secrets_redacted` and the prewarm tallies
/// in place. The session-path analogue of `memory_dir::import_one_memory`.
fn store_extracted_fact(
    ctx: &BootstrapContext<'_>,
    fact_store: &FactStore,
    fact: &ExtractedFact,
    effective_created: DateTime<Utc>,
    marker_event_id: i64,
    report: &mut BootstrapReport,
) -> Result<f64> {
    // Defense-in-depth redaction (#45/#51): the turns were already scrubbed
    // upfront (see `redact_turns`), so `fact.content` derived from them is
    // normally clean and this pass finds nothing — but it guards against an
    // extractor that introduces text not present verbatim in the turns. Findings
    // here are held in `redactions` and only added to the report on the *created*
    // branch, so the audit counter stays idempotent (a reinforced re-run
    // re-scrubs but does not re-count). Disabled only when `config.redact` is
    // false (library/test callers); the CLI has no bypass.
    let mut redactions = 0usize;
    let content = if ctx.config.redact {
        let (clean, findings) =
            redact::redact_text_with_denylist(&fact.content, &ctx.config.denylist);
        redactions += findings.len();
        clean
    } else {
        fact.content.clone()
    };

    // Redact the extractor-supplied metadata too: a pluggable SessionExtractor
    // may place turn-derived text in metadata, and the stored row must carry no
    // unredacted secret anywhere (not just in content). Keys are left intact.
    let mut metadata = fact.metadata.clone();
    if ctx.config.redact {
        redactions += redact::redact_json_strings(&mut metadata, &ctx.config.denylist);
    }

    // Session files are third-party input: a fact whose (redacted) content or
    // metadata exceeds the ingest bound is skipped best-effort — mirroring the
    // malformed-line policy — rather than aborting the whole import. Checked on
    // the redacted, about-to-be-stored values, before the costly embed
    // (issue #572 / L10). A skipped fact is neither created nor reinforced.
    if let Err(e) = crate::limits::check_str_size(&content, "fact content")
        .and_then(|()| crate::limits::check_json_size(&metadata, "fact metadata"))
    {
        tracing::warn!(error = %e, "skipping oversized bootstrap fact");
        return Ok(0.0);
    }

    let embedding = ctx.embedder.embed(&content)?;

    let is_pinned = ctx.classifier.is_some_and(|c| {
        let temp = crate::types::Fact {
            id: 0,
            content: content.clone(),
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
            metadata: metadata.clone(),
            scope_id: ctx.scope_id,
            is_pinned: false,
            importance_score: fact.importance,
            surfaced_at: None,
        };
        c.should_pin(&temp)
    });

    // Bi-temporal note (#521): t_created is backdated to the historical turn
    // timestamp, but t_valid is deliberately left None. Valid-time is the
    // externally-asserted "true in the world" interval, which a retro-observed
    // session fact does not carry — transaction-time (t_created) is the temporal
    // signal here. Consequences: these facts are visible to active-at queries
    // (None = unbounded-valid) but are NOT scheduled by list_due (which requires
    // t_valid IS NOT NULL); memarch #42 sweeps on t_created for the same reason.
    let new_fact = NewFact {
        content,
        content_hash: String::new(),
        embedding,
        fact_type: fact.fact_type,
        t_created: effective_created,
        t_expired: None,
        t_valid: None,
        t_invalid: None,
        source_event_id: Some(marker_event_id),
        scope_id: ctx.scope_id,
        importance: fact.importance,
        access_count: 0,
        last_accessed: effective_created,
        metadata,
        is_pinned,
    };

    // Dedup-with-reinforcement (#520): a fact whose content already exists
    // (active, same scope) is reinforced — recency/frequency bumped — rather than
    // duplicated. A 9-month backfill re-encounters recurring conventions and
    // decisions across sessions; this collapses them to one reinforced row.
    let (_, reinforced) = fact_store.insert_or_reinforce(&new_fact)?;
    if reinforced {
        // A reinforcement adds no new row, so it does not count toward prewarm
        // metrics or the created-fact importance average.
        report.facts_reinforced += 1;
        return Ok(0.0);
    }
    report.facts_created += 1;
    report.secrets_redacted += redactions;
    match fact.fact_type {
        FactType::Episodic => report.prewarm_metrics.episodic_count += 1,
        FactType::Semantic => report.prewarm_metrics.semantic_count += 1,
        FactType::Procedural => report.prewarm_metrics.procedural_count += 1,
    }
    Ok(fact.importance)
}

/// Inner pipeline logic running within a savepoint.
fn bootstrap_within_savepoint(
    ctx: &BootstrapContext<'_>,
    entries: &[parse::SessionEntry],
    session_id: &str,
    report: &mut BootstrapReport,
) -> Result<()> {
    // --- Ingest marker event (idempotency anchor) ---
    let marker_event_id =
        ingest_bootstrap_marker(ctx.conn, ctx.upcaster_registry, session_id, ctx.scope_id)?;
    report.events_ingested = 1;

    // --- Reconstruct turns ---
    let mut turns = filter::reconstruct_turns(entries);
    report.turns_reconstructed = turns.len();

    // --- Redaction gate (#45/#51), applied UPFRONT ---
    // Scrub secrets/PII from every turn BEFORE extraction so a pluggable
    // (possibly LLM-powered) SessionExtractor never receives unredacted content —
    // the gate covers extraction, embedding, AND storage, not just the stored
    // row. Secrets are never outcome markers or keywords, so this does not
    // perturb classification or the keyword pre-filter. Counted here (per
    // session); a redundant re-run is skipped on the bootstrap marker before
    // reaching this point, so the count stays idempotent.
    if ctx.config.redact {
        report.secrets_redacted += redact_turns(&mut turns, &ctx.config.denylist);
    }

    // --- Classify outcome on FULL turns (before truncation) ---
    // Outcome evidence (commits, test results) lives at the end of sessions,
    // so we must not truncate before classification.
    let (outcome, _signals) = outcome::classify_outcome(&turns);

    // Apply max_turns limit AFTER outcome classification, keeping the TAIL
    // (most recent turns) since they contain resolution evidence.
    let turns = if ctx.config.max_turns > 0 && turns.len() > ctx.config.max_turns {
        turns[turns.len() - ctx.config.max_turns..].to_vec()
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
    let fact_store = FactStore::new(ctx.conn, ctx.embed_dim);

    for candidate in &candidates {
        let facts = ctx.extractor.extract(candidate, &outcome)?;
        for fact in &facts {
            importance_sum += store_extracted_fact(
                ctx,
                &fact_store,
                fact,
                candidate.timestamp,
                marker_event_id,
                report,
            )?;
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

    // Stamp the embedding identity on first write (#613, ADR 0015 §2) — but only once
    // a fact vector has actually been written (#643). Done here, at the tail of the
    // savepoint, so the identity commits atomically with the facts it describes (the
    // outer RELEASE/ROLLBACK covers it). `insert_or_reinforce` only writes a new
    // vector when it *creates* a row, so `facts_created` is the precise gate: a
    // session that parses to zero episodes, or only reinforces existing facts, writes
    // no new vector and leaves the store unstamped — letting a later real first write
    // with a different embedder establish the true identity (the #614-enforcement
    // landmine this averts). The `embedder.fingerprint()` and `embed_dim` are the same
    // values the engine's `record_embedding_identity` seam would have used.
    if report.facts_created > 0 {
        crate::store::embedding_meta::record_if_absent(
            ctx.conn,
            &ctx.embedder.fingerprint(),
            ctx.embed_dim,
        )?;
    }

    Ok(())
}

/// Recursively collect `*.jsonl` session files under `dir`, skipping any
/// `subagents/` subdirectory (those hold lower-value subagent tool-call logs).
///
/// Real Claude/Codex/Gemini transcripts live one level down
/// (`<project-slug>/<uuid>.jsonl`), so a flat top-level scan would silently find
/// nothing when pointed at `~/.claude/projects`. This mirrors the `--memory-dir`
/// path's recursion (`memory_dir::collect_md_files`).
fn collect_jsonl_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        // `file_type()` reads the dirent's own type (no extra `stat`) and does
        // NOT follow symlinks, so a circular symlink cannot drive infinite
        // recursion — a symlink is neither dir nor file here and is skipped.
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            if path.file_name().and_then(|n| n.to_str()) == Some("subagents") {
                continue;
            }
            collect_jsonl_files(&path, out)?;
        } else if file_type.is_file() && path.extension().and_then(|e| e.to_str()) == Some("jsonl")
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Bootstrap all JSONL session logs under a directory (recursive).
///
/// Discovers `*.jsonl` files at any depth, skipping `subagents/` subdirectories.
/// Processes each session independently, aggregating reports.
///
/// # Errors
///
/// Returns `MemoryError::Io` for directory traversal failures.
/// Individual session failures are logged and skipped (not fatal).
pub(crate) fn bootstrap_directory_inner(
    ctx: &BootstrapContext<'_>,
    dir: &Path,
) -> Result<BootstrapReport> {
    let mut aggregate = BootstrapReport::default();

    let mut files = Vec::new();
    collect_jsonl_files(dir, &mut files)?;
    files.sort();

    for path in &files {
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "skipping unreadable session file");
                continue;
            }
        };
        let reader = std::io::BufReader::new(file);

        match bootstrap_session_inner(ctx, reader) {
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

        fn fingerprint(&self) -> crate::types::EmbeddingFingerprint {
            crate::types::EmbeddingFingerprint::new("mock", "test", 4)
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

        let ctx = BootstrapContext {
            conn: &conn,
            embed_dim: 4,
            upcaster_registry: &registry,
            embedder: &NeverCalledEmbedder,
            extractor: &extractor,
            config: &config,
            classifier: None,
            scope_id: 1, // root scope id
        };
        let report =
            bootstrap_session_inner(&ctx, reader).expect("bootstrap_session_inner should succeed");

        // Early-exit path: entries parsed but nothing processed.
        assert!(report.entries_parsed > 0, "expected entries to be parsed");
        assert_eq!(
            report.sessions_processed, 0,
            "no session should be processed"
        );
        assert_eq!(report.facts_created, 0);
    }
}
