//! Session log bootstrap: parse Claude Code JSONL session logs into historical memory.
//!
//! Pipeline (#816 E.4b — orchestration is engine-side, DB writes are one atomic
//! port call): JSONL → `parse_session` → *idempotency check (engine-side)* →
//! `prepare_session` (reconstruct → classify → keyword pre-filter → extract →
//! **embed**) → `StorageBackend::ingest_bootstrap_batch_atomic`. Parse + prepare run on
//! a blocking thread (the consumer `EmbeddingProvider`/`SessionExtractor` may block);
//! no connection or driver type is threaded through this module anymore.

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

use crate::error::Result;
use crate::traits::{EmbeddingProvider, PersistenceClassifier};
use crate::types::{ClassifierInput, EventType, NewEvent, NewFact};

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

/// Parsed-but-not-yet-embedded session data — the cheap output of the parse phase.
///
/// Produced on a blocking thread (file I/O + JSON parsing), then the engine makes
/// the idempotency decision from [`session_id`](Self::session_id) *before* the
/// expensive [`prepare_session`] embed phase, so an already-imported session skips
/// embedding entirely.
pub(crate) struct ParsedSession {
    /// Every parsed entry (bounded by `max_entries`); `entries.len()` is the
    /// `entries_parsed` report count.
    pub entries: Vec<parse::SessionEntry>,
    /// Count of malformed JSONL lines skipped during parsing (#293).
    pub malformed: usize,
    /// The session id (first entry carrying one); empty ⟹ nothing to ingest.
    pub session_id: String,
}

/// Parse a JSONL session stream into [`ParsedSession`] — pure I/O + parsing, no DB,
/// no embedding, no consumer callback. Cheap enough to run *before* the idempotency
/// check (bounded against hostile/corrupt input, #293).
pub(crate) fn parse_session(reader: impl BufRead, config: &BootstrapConfig) -> ParsedSession {
    let (entries, malformed) =
        parse::parse_session_file(reader, config.max_session_bytes, config.max_entries);
    let session_id = entries
        .iter()
        .find_map(|e| e.session_id.clone())
        .unwrap_or_default();
    ParsedSession {
        entries,
        malformed,
        session_id,
    }
}

/// Engine-side handles + config for the [`prepare_session`] compute phase.
///
/// The borrows are created *inside* the `spawn_blocking` closure (from owned `Arc`s
/// the engine holds), so this bundle never needs to be `'static`. It replaces the
/// old `BootstrapContext` minus the connection/upcaster (no DB access here anymore).
pub(crate) struct PrepareCtx<'a> {
    /// Consumer-supplied embedder (no LLM/network owned by the engine).
    pub embedder: &'a dyn EmbeddingProvider,
    /// Fact extractor (default keyword, or a consumer LLM extractor).
    pub extractor: &'a dyn SessionExtractor,
    /// Per-run configuration (scope, caps, redaction).
    pub config: &'a BootstrapConfig,
    /// Optional auto-pin classifier consulted per created fact.
    pub classifier: Option<&'a dyn PersistenceClassifier>,
    /// Resolved scope id facts are ingested into.
    pub scope_id: i64,
}

/// The pre-embedded output of one session, ready for a single atomic ingest.
pub(crate) struct PreparedSession {
    /// The idempotency-marker event. Inserted below the seam by
    /// [`StorageBackend::ingest_bootstrap_batch_atomic`](crate::storage::StorageBackend);
    /// its assigned id anchors the batch (becomes each fact's `source_event_id`).
    pub marker: NewEvent,
    /// One `(fact, redaction_count)` per extracted fact, in extraction order.
    /// Redactions are folded into the report only on the *created* branch (so the
    /// audit counter stays idempotent), hence carried out per-fact rather than
    /// pre-summed — the caller adds them once it learns which facts were created.
    pub facts: Vec<(NewFact, usize)>,
    /// Report tallies known at prepare time: `turns_reconstructed` / `candidates_found`
    /// / `outcome_counts` / `category_counts` / turn-level `secrets_redacted` /
    /// `events_ingested`. `entries_*`, `facts_*`, prewarm, and `sessions_processed`
    /// are finalized by the caller (they need the parse counts / the ingest result).
    pub report: BootstrapReport,
}

/// Reconstruct → redact → classify → keyword-prefilter → extract → **embed** one
/// parsed session into a pre-embedded batch.
///
/// Pure compute plus the consumer callbacks (`extract`/`embed`/`should_pin`) — **no
/// DB access**. Run inside `spawn_blocking` so a blocking `EmbeddingProvider` /
/// `SessionExtractor` never parks the async executor. The marker event is *built*
/// here (so its `session_id`/`scope` travel with the batch) but *inserted* below the
/// storage seam, atomically with the facts.
///
/// # Errors
///
/// Returns errors from extraction or embedding computation.
pub(crate) fn prepare_session(
    ctx: &PrepareCtx<'_>,
    parsed: &ParsedSession,
) -> Result<PreparedSession> {
    let mut report = BootstrapReport::default();

    // Marker event: idempotency anchor + batch provenance. Built here; inserted at
    // the seam (its id backfills every fact's source_event_id). Counted now because
    // the returned (successful) report always reflects one ingested marker.
    let marker = NewEvent {
        timestamp: Utc::now(),
        event_type: EventType::SystemEvent,
        payload: serde_json::json!({
            "action": "bootstrap_session",
            "session_id": parsed.session_id,
        }),
        source: "bootstrap".into(),
        session_id: Some(parsed.session_id.clone()),
        scope_id: ctx.scope_id,
        origin_node_id: "bootstrap".into(),
        sequence_id: 0,
        created_at: None,
    };
    report.events_ingested = 1;

    // --- Reconstruct turns ---
    let mut turns = filter::reconstruct_turns(&parsed.entries);
    report.turns_reconstructed = turns.len();

    // --- Redaction gate (#45/#51), applied UPFRONT ---
    // Scrub secrets/PII from every turn BEFORE extraction so a pluggable
    // (possibly LLM-powered) SessionExtractor never receives unredacted content —
    // the gate covers extraction, embedding, AND storage. Counted here (per
    // session); the caller skips already-bootstrapped sessions before this point,
    // so the count stays idempotent.
    if ctx.config.redact {
        report.secrets_redacted += redact_turns(&mut turns, &ctx.config.denylist);
    }

    // --- Classify outcome on FULL turns (before truncation) ---
    // Outcome evidence (commits, test results) lives at the end of sessions, so we
    // must not truncate before classification.
    let (outcome, _signals) = outcome::classify_outcome(&turns);

    // Apply max_turns AFTER outcome classification, keeping the TAIL (most recent
    // turns hold resolution evidence).
    let turns = if ctx.config.max_turns > 0 && turns.len() > ctx.config.max_turns {
        let mut turns = turns;
        let drop_head = turns.len() - ctx.config.max_turns;
        turns.drain(..drop_head);
        turns
    } else {
        turns
    };
    match outcome {
        SessionOutcome::Success => report.outcome_counts.success += 1,
        SessionOutcome::Failure => report.outcome_counts.failure += 1,
        SessionOutcome::Indeterminate => report.outcome_counts.indeterminate += 1,
    }

    // --- Keyword pre-filter ---
    let candidates = filter::keyword_prefilter(&turns, &parsed.session_id);
    report.candidates_found = candidates.len();
    for candidate in &candidates {
        match candidate.category {
            EpisodeCategory::Bug => report.category_counts.bug += 1,
            EpisodeCategory::Decision => report.category_counts.decision += 1,
            EpisodeCategory::Convention => report.category_counts.convention += 1,
            EpisodeCategory::Learning => report.category_counts.learning += 1,
        }
    }

    // --- Extract + prepare (embed) facts ---
    let mut facts = Vec::new();
    for candidate in &candidates {
        let extracted = ctx.extractor.extract(candidate, &outcome)?;
        for fact in &extracted {
            if let Some(prepared) = prepare_extracted_fact(ctx, fact, candidate.timestamp)? {
                facts.push(prepared);
            }
        }
    }

    Ok(PreparedSession {
        marker,
        facts,
        report,
    })
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

/// Redact, size-check, embed, and build one `NewFact` from an extracted fact.
///
/// The pure-compute half of the former `store_extracted_fact` — the DB insert
/// (dedup-with-reinforce), the marker linkage, the identity stamp, and the report
/// accounting all moved to
/// [`StorageBackend::ingest_bootstrap_batch_atomic`](crate::storage::StorageBackend)
/// (below the seam) + the caller (facade). Returns `None` when the fact is skipped
/// (oversized after redaction — mirrors the best-effort malformed-line policy), else
/// `(fact, redaction_count)` with the redactions carried out for idempotent audit
/// accounting on the *created* branch. The session-path analogue of
/// `memory_dir::prepare_one_memory`.
fn prepare_extracted_fact(
    ctx: &PrepareCtx<'_>,
    fact: &ExtractedFact,
    effective_created: DateTime<Utc>,
) -> Result<Option<(NewFact, usize)>> {
    // Defense-in-depth redaction (#45/#51): the turns were already scrubbed upfront
    // (see `redact_turns`), so `fact.content` is normally clean — but this guards
    // against an extractor introducing text not present verbatim in the turns.
    // Findings are held in `redactions` and only added to the report on the *created*
    // branch by the caller, so the audit counter stays idempotent (a reinforced
    // re-run re-scrubs but does not re-count). Disabled only when `config.redact` is
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

    // Redact the extractor-supplied metadata too: a pluggable SessionExtractor may
    // place turn-derived text in metadata, and the stored row must carry no
    // unredacted secret anywhere (not just in content). Keys are left intact.
    let mut metadata = fact.metadata.clone();
    if ctx.config.redact {
        redactions += redact::redact_json_strings(&mut metadata, &ctx.config.denylist);
    }

    // Session files are third-party input: a fact whose (redacted) content or
    // metadata exceeds the ingest bound is skipped best-effort — mirroring the
    // malformed-line policy — rather than aborting the whole import. Checked on the
    // redacted, about-to-be-stored values, before the costly embed (issue #572 /
    // L10). A skipped fact is neither created nor reinforced.
    if let Err(e) = crate::limits::check_str_size(&content, "fact content")
        .and_then(|()| crate::limits::check_json_size(&metadata, "fact metadata"))
    {
        tracing::warn!(error = %e, "skipping oversized bootstrap fact");
        return Ok(None);
    }

    let embedding = ctx.embedder.embed(&content)?;

    // Classifiers read only content/fact_type/importance/metadata — build the owned
    // `ClassifierInput` view, not a 20-field synthetic `Fact` cloning the embedding
    // (#118/#343/#388).
    let is_pinned = ctx.classifier.is_some_and(|c| {
        let input = ClassifierInput {
            content: content.clone(),
            fact_type: fact.fact_type,
            base_importance: fact.importance,
            metadata: metadata.clone(),
        };
        c.should_pin(&input)
    });

    // Bi-temporal note (#521): t_created is backdated to the historical turn
    // timestamp, but t_valid is deliberately left None. Valid-time is the
    // externally-asserted "true in the world" interval, which a retro-observed
    // session fact does not carry — transaction-time (t_created) is the temporal
    // signal here. Consequences: these facts are visible to active-at queries
    // (None = unbounded-valid) but are NOT scheduled by list_due (which requires
    // t_valid IS NOT NULL); memarch #42 sweeps on t_created for the same reason.
    //
    // `source_event_id` is left None here: the marker event is inserted below the
    // seam and its id backfilled onto every fact in the atomic batch.
    let new_fact = NewFact {
        content,
        content_hash: String::new(),
        embedding,
        fact_type: fact.fact_type,
        t_created: effective_created,
        t_expired: None,
        t_valid: None,
        t_invalid: None,
        source_event_id: None,
        scope_id: ctx.scope_id,
        base_importance: fact.importance,
        access_count: 0,
        last_accessed: effective_created,
        metadata,
        is_pinned,
    };

    Ok(Some((new_fact, redactions)))
}

/// Recursively collect `*.jsonl` session files under `dir`, skipping any
/// `subagents/` subdirectory (those hold lower-value subagent tool-call logs).
///
/// Real Claude/Codex/Gemini transcripts live one level down
/// (`<project-slug>/<uuid>.jsonl`), so a flat top-level scan would silently find
/// nothing when pointed at `~/.claude/projects`. This mirrors the `--memory-dir`
/// path's recursion (`memory_dir::collect_md_files`).
pub(crate) fn collect_jsonl_files(
    dir: &Path,
    out: &mut Vec<std::path::PathBuf>,
) -> std::io::Result<()> {
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

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// Valid JSONL whose entries carry no `sessionId`: [`parse_session`] returns the
    /// parsed entries but an empty session id, so the engine short-circuits to an
    /// empty report (no idempotency check, no embed, no ingest below the seam).
    #[test]
    fn parse_session_valid_jsonl_no_session_id_yields_empty_session_id() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"hello"}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}
"#;
        let reader = Cursor::new(jsonl);
        let config = BootstrapConfig::default();

        let parsed = parse_session(reader, &config);

        assert!(!parsed.entries.is_empty(), "expected entries to be parsed");
        assert!(
            parsed.session_id.is_empty(),
            "no session id should be extracted"
        );
        assert_eq!(parsed.malformed, 0);
    }
}
