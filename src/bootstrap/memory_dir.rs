//! Native `.md` memory-file import (#551, Stream B Deliverable E).
//!
//! The Claude Code harness keeps durable memory as flat `.md` files: a
//! `MEMORY.md` index plus one file per fact, each with shallow YAML frontmatter
//! (`name`, `description`, `metadata.type`) and a Markdown body. This module
//! parses those files into ME facts with **backdated** `t_created` (a frontmatter
//! date when present, else a filename-encoded date, else the file mtime),
//! redaction-gated and dedup-with-reinforced exactly like the JSONL session
//! path. Sources are opened read-only and never modified.
//!
//! The frontmatter scanner here is deliberately minimal — it understands only
//! the handful of fields the native-memory schema uses, not arbitrary YAML — so
//! the engine takes on no YAML dependency. Anything it cannot parse degrades to
//! "no frontmatter, the whole file is the body."
//!
//! Crash semantics: facts are written one at a time in autocommit (no
//! per-directory savepoint), trading a single all-or-nothing transaction for
//! per-file resilience — one bad file does not abort the batch. A run
//! interrupted partway leaves the facts written so far; a re-run is idempotent
//! (dedup-with-reinforcement collapses them), so partial imports self-heal
//! rather than duplicate. The one accepted side effect is that the
//! already-committed prefix gets its `access_count` reinforced once on the
//! recovery re-run — a benign frequency-signal drift; row count and `t_created`
//! (via `min`) are unaffected.

use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use rusqlite::Connection;

use crate::error::Result;
use crate::store::facts::FactStore;
use crate::traits::{EmbeddingProvider, PersistenceClassifier};
use crate::types::{FactType, NewFact};

use super::metrics::BootstrapReport;
use super::redact;
use super::BootstrapConfig;

/// Baseline importance for curated native-memory facts. Higher than the
/// keyword-extracted session candidates: these files are hand-authored durable
/// memory, not auto-mined episodes.
const MEMORY_IMPORTANCE: f64 = 0.6;

/// Fields lifted from a native memory `.md` file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedMemory {
    /// `name:` slug from frontmatter, if present.
    pub name: Option<String>,
    /// `description:` one-liner from frontmatter, if present.
    pub description: Option<String>,
    /// Memory `type` (`metadata.type`, or a top-level `type:`), lowercased.
    pub mem_type: Option<String>,
    /// A date parsed from a frontmatter `valid_from`/`date`/`created` field.
    pub frontmatter_date: Option<DateTime<Utc>>,
    /// Everything after the frontmatter block, trimmed. The whole input when
    /// there is no frontmatter.
    pub body: String,
}

/// Parse a native memory `.md` file into its frontmatter fields + body.
///
/// Recognizes a leading `---`-delimited block. Only the fields the
/// native-memory schema uses are extracted; unknown keys are ignored. With no
/// frontmatter (or an unterminated block) the whole input becomes `body` and
/// every field is `None`.
#[must_use]
pub fn parse_memory_file(raw: &str) -> ParsedMemory {
    // Some editors (notably on Windows) prepend a UTF-8 BOM, which would sit
    // before the leading `---` and defeat frontmatter detection. Strip it first.
    let raw = raw.strip_prefix('\u{FEFF}').unwrap_or(raw);
    let (front, body) = split_frontmatter(raw);

    let mut parsed = ParsedMemory {
        body: body.trim().to_owned(),
        ..ParsedMemory::default()
    };

    let Some(front) = front else {
        return parsed;
    };

    for line in front.lines() {
        let line = line.trim_end_matches('\r');
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = unquote(value.trim());
        match key {
            "name" if parsed.name.is_none() => parsed.name = nonempty(value),
            "description" if parsed.description.is_none() => parsed.description = nonempty(value),
            // `type:` appears top-level (wiki) or nested under `metadata:`
            // (native). We accept either — first non-empty wins.
            "type" if parsed.mem_type.is_none() => {
                parsed.mem_type = nonempty(value).map(|s| s.to_ascii_lowercase());
            }
            "valid_from" | "date" | "created" if parsed.frontmatter_date.is_none() => {
                parsed.frontmatter_date = parse_date(value);
                if parsed.frontmatter_date.is_none() && !value.is_empty() && value != "null" {
                    // A present-but-unparseable date is a likely authoring
                    // mistake; warn so the silent fall-through to filename/mtime
                    // is observable rather than mysterious.
                    tracing::warn!(
                        key,
                        value,
                        "unparseable frontmatter date; falling back to filename/mtime"
                    );
                }
            }
            _ => {}
        }
    }

    parsed
}

/// Split a leading `---` frontmatter block from the body.
///
/// Returns `(Some(frontmatter), body)` when the file opens with a `---` line and
/// has a matching closing `---`; otherwise `(None, whole_input)`.
fn split_frontmatter(raw: &str) -> (Option<&str>, &str) {
    let after_open = if let Some(r) = raw.strip_prefix("---\n") {
        r
    } else if let Some(r) = raw.strip_prefix("---\r\n") {
        r
    } else {
        return (None, raw);
    };

    let mut search_from = 0usize;
    loop {
        let line_end = after_open[search_from..]
            .find('\n')
            .map_or(after_open.len(), |i| search_from + i);
        let line = after_open[search_from..line_end].trim_end_matches('\r');
        if line == "---" {
            let front = &after_open[..search_from];
            let body_start = if line_end < after_open.len() {
                line_end + 1
            } else {
                after_open.len()
            };
            return (Some(front), &after_open[body_start..]);
        }
        // Guard against a leading `---` that opened a horizontal rule / prose
        // document rather than frontmatter: if we hit a line that is not
        // frontmatter-shaped (blank, comment, indented/nested, list item, or a
        // `key: value`) before any closing fence, abort and treat the whole
        // input as body. Otherwise a stray `---` divider later in the prose
        // would be mistaken for the closing fence and the body above it
        // silently swallowed as frontmatter.
        let yaml_shaped = line.is_empty()
            || line.starts_with(' ')
            || line.starts_with('\t')
            || line.starts_with('#')
            || line.starts_with('-')
            || line.contains(':');
        if !yaml_shaped {
            return (None, raw);
        }
        if line_end >= after_open.len() {
            // No closing delimiter — not a frontmatter block.
            return (None, raw);
        }
        search_from = line_end + 1;
    }
}

/// Strip a single pair of surrounding ASCII quotes, if present.
fn unquote(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() >= 2
        && ((b[0] == b'"' && b[b.len() - 1] == b'"') || (b[0] == b'\'' && b[b.len() - 1] == b'\''))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn nonempty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_owned())
    }
}

/// Parse a frontmatter date: RFC-3339, then naive `YYYY-MM-DDThh:mm:ss` (as UTC),
/// then bare `YYYY-MM-DD` (→ midnight UTC). `null`/empty yields `None`.
///
/// The naive-datetime fallback mirrors the JSONL path's `parse::parse_timestamp`
/// so both importers accept the same timestamp grammar.
fn parse_date(s: &str) -> Option<DateTime<Utc>> {
    if s.is_empty() || s == "null" {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return Some(DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc));
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|ndt| DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc))
}

/// Map a native memory `type` to an ME [`FactType`] and a route-to-KB hint.
///
/// Routing follows the ratified ownership map (Option A): preferences →
/// Memory, durable directives → Wisdom, reference material → Knowledge. There is
/// no KB sink reachable from here yet, so `reference` facts are stored in ME
/// tagged `route: knowledge-base` for a later migration pass to relocate.
fn classify_memory_type(mem_type: Option<&str>) -> (FactType, bool) {
    match mem_type {
        Some("feedback") => (FactType::Procedural, false),
        Some("project") => (FactType::Episodic, false),
        Some("reference") => (FactType::Semantic, true),
        // `user`, unknown, or absent: a durable semantic fact, ME-resident.
        _ => (FactType::Semantic, false),
    }
}

/// Extract a `YYYY[-_]MM[-_]DD` date embedded in the file stem (e.g.
/// `project_s0_review_2026_06_14.md`) as UTC midnight.
///
/// Native memory files encode their authored date in the filename but carry no
/// frontmatter date, so this is preferred over mtime — mtime tracks in-place
/// rewrites (≈ import time), not authorship. Scans `-`/`_`-separated tokens for
/// the first valid `4-digit / 2-digit / 2-digit` run.
fn filename_date(path: &Path) -> Option<DateTime<Utc>> {
    let stem = path.file_stem()?.to_str()?;
    let tokens: Vec<&str> = stem.split(['-', '_']).collect();
    for w in tokens.windows(3) {
        let [y, m, d] = [w[0], w[1], w[2]];
        let shaped = y.len() == 4
            && m.len() == 2
            && d.len() == 2
            && [y, m, d]
                .iter()
                .all(|t| t.bytes().all(|b| b.is_ascii_digit()));
        if !shaped {
            continue;
        }
        if let (Ok(yy), Ok(mm), Ok(dd)) = (y.parse::<i32>(), m.parse::<u32>(), d.parse::<u32>()) {
            // from_ymd_opt rejects impossible dates (e.g. month 13), so a numeric
            // run that is not a real calendar date is skipped, not mis-dated.
            if let Some(date) = NaiveDate::from_ymd_opt(yy, mm, dd) {
                return date
                    .and_hms_opt(0, 0, 0)
                    .map(|ndt| DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc));
            }
        }
    }
    None
}

/// File modification time as a UTC instant, if the metadata is readable.
fn file_mtime(path: &Path) -> Option<DateTime<Utc>> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()
        .map(DateTime::<Utc>::from)
}

/// Recursively collect `*.md` files under `dir` into `out`.
fn collect_md_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        // `file_type()` reads the dirent's own type (no extra `stat`) and does
        // NOT follow symlinks, so a circular symlink cannot drive infinite
        // recursion — a symlink is neither dir nor file here and is skipped.
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            collect_md_files(&path, out)?;
        } else if file_type.is_file() && path.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(path);
        }
    }
    Ok(())
}

/// Build the provenance metadata object for a memory-dir fact.
fn build_metadata(path: &Path, parsed: &ParsedMemory, route_kb: bool) -> serde_json::Value {
    let mut metadata = serde_json::json!({
        "source": "memory-dir",
        "file": path.file_name().and_then(|f| f.to_str()).unwrap_or_default(),
    });
    if let Some(name) = &parsed.name {
        metadata["name"] = serde_json::Value::String(name.clone());
    }
    if let Some(desc) = &parsed.description {
        metadata["description"] = serde_json::Value::String(desc.clone());
    }
    if let Some(mt) = &parsed.mem_type {
        metadata["memory_type"] = serde_json::Value::String(mt.clone());
    }
    if route_kb {
        metadata["route"] = serde_json::Value::String("knowledge-base".to_owned());
    }
    metadata
}

/// Build and store one fact from a parsed `.md` memory file.
///
/// Returns the importance to fold into the prewarm average — `0.0` when the
/// fact was *reinforced* rather than created (reinforcement updates no prewarm
/// counters). Updates `report`'s `facts_created`/`facts_reinforced`/
/// `secrets_redacted` and the prewarm tallies in place.
#[allow(clippy::too_many_arguments)]
fn import_one_memory(
    fact_store: &FactStore,
    embedder: &dyn EmbeddingProvider,
    config: &BootstrapConfig,
    classifier: Option<&dyn PersistenceClassifier>,
    scope_id: i64,
    path: &Path,
    parsed: &ParsedMemory,
    report: &mut BootstrapReport,
) -> Result<f64> {
    // Backdate priority: frontmatter date > filename-encoded date > file mtime
    // > now(). Native memory files carry no frontmatter date but encode the
    // authored date in the filename; mtime tracks in-place rewrites (≈ import
    // time), so it is the last historical signal before falling back to now()
    // (reached only when none of the above is available — effectively never).
    let timestamp = parsed
        .frontmatter_date
        .or_else(|| filename_date(path))
        .or_else(|| file_mtime(path))
        .unwrap_or_else(Utc::now);

    // Redaction gate (#45/#51): scrub BEFORE embed/store. ME copy only — the
    // source .md is never modified. The finding count is held in `redactions`
    // and only added to the report on the *created* branch below, so the audit
    // counter is idempotent (a reinforced re-run re-scrubs but does not re-count).
    let mut redactions = 0usize;
    let content = if config.redact {
        let (clean, findings) = redact::redact_text_with_denylist(&parsed.body, &config.denylist);
        redactions += findings.len();
        clean
    } else {
        parsed.body.clone()
    };

    let (fact_type, route_kb) = classify_memory_type(parsed.mem_type.as_deref());
    let embedding = embedder.embed(&content)?;

    // Redact the metadata too: the frontmatter `name`/`description` are author
    // free-text and can carry a secret (e.g. a memory quoting a leaked token).
    // Scrub the whole metadata object so the stored row holds no unredacted
    // secret anywhere, not just in the body content.
    let mut metadata = build_metadata(path, parsed, route_kb);
    if config.redact {
        redactions += redact::redact_json_strings(&mut metadata, &config.denylist);
    }

    let is_pinned = classifier.is_some_and(|c| {
        let temp = crate::types::Fact {
            id: 0,
            content: content.clone(),
            content_hash: String::new(),
            embedding: embedding.clone(),
            fact_type,
            t_created: timestamp,
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            importance: MEMORY_IMPORTANCE,
            access_count: 0,
            last_accessed: timestamp,
            metadata: metadata.clone(),
            scope_id,
            is_pinned: false,
            importance_score: MEMORY_IMPORTANCE,
            surfaced_at: None,
        };
        c.should_pin(&temp)
    });

    // Bi-temporal note (#521): t_created backdated to the file's date/mtime;
    // t_valid deliberately None (a retro-observed memory carries no asserted
    // valid-time interval — transaction-time is the temporal signal).
    let new_fact = NewFact {
        content,
        content_hash: String::new(),
        embedding,
        fact_type,
        t_created: timestamp,
        t_expired: None,
        t_valid: None,
        t_invalid: None,
        source_event_id: None,
        importance: MEMORY_IMPORTANCE,
        access_count: 0,
        last_accessed: timestamp,
        metadata,
        scope_id,
        is_pinned,
    };

    let (_, reinforced) = fact_store.insert_or_reinforce(&new_fact)?;
    if reinforced {
        report.facts_reinforced += 1;
        return Ok(0.0);
    }
    report.facts_created += 1;
    report.secrets_redacted += redactions;
    match fact_type {
        FactType::Episodic => report.prewarm_metrics.episodic_count += 1,
        FactType::Semantic => report.prewarm_metrics.semantic_count += 1,
        FactType::Procedural => report.prewarm_metrics.procedural_count += 1,
    }
    Ok(MEMORY_IMPORTANCE)
}

/// Import every `*.md` memory file under `dir` (recursive) into the store.
///
/// Each file becomes one fact: body as content, `t_created` backdated from the
/// frontmatter date, else a filename-encoded date, else the file mtime,
/// type-routed per [`classify_memory_type`],
/// redaction-gated, and dedup-with-reinforced. Files with an empty body (e.g. a
/// frontmatter-only stub) are skipped. Unreadable files are logged and skipped,
/// not fatal.
///
/// # Errors
///
/// Returns `MemoryError::Io` if `dir` cannot be traversed, or an embedding/DB
/// error from processing a file (which aborts the run; a re-run resumes
/// idempotently).
pub fn bootstrap_memory_directory_inner(
    conn: &Connection,
    embed_dim: usize,
    dir: &Path,
    embedder: &dyn EmbeddingProvider,
    config: &BootstrapConfig,
    classifier: Option<&dyn PersistenceClassifier>,
    scope_id: i64,
) -> Result<BootstrapReport> {
    let mut report = BootstrapReport::default();
    let fact_store = FactStore::new(conn, embed_dim);

    let mut files = Vec::new();
    collect_md_files(dir, &mut files)?;
    files.sort();

    let mut importance_sum = 0.0;

    for path in &files {
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "skipping unreadable memory file");
                report.memory_files_skipped += 1;
                continue;
            }
        };

        let parsed = parse_memory_file(&raw);
        if parsed.body.is_empty() {
            report.memory_files_skipped += 1;
            continue;
        }
        report.memory_files_parsed += 1;

        importance_sum += import_one_memory(
            &fact_store,
            embedder,
            config,
            classifier,
            scope_id,
            path,
            &parsed,
            &mut report,
        )?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_native_memory_frontmatter() {
        let raw = "---\n\
            name: stream-b-handoff\n\
            description: \"resume pointer for Stream B\"\n\
            metadata:\n  \
              node_type: memory\n  \
              type: project\n\
            ---\n\
            The body of the memory.\nSecond line.\n";
        let p = parse_memory_file(raw);
        assert_eq!(p.name.as_deref(), Some("stream-b-handoff"));
        assert_eq!(
            p.description.as_deref(),
            Some("resume pointer for Stream B")
        );
        assert_eq!(p.mem_type.as_deref(), Some("project"));
        assert_eq!(p.body, "The body of the memory.\nSecond line.");
    }

    #[test]
    fn parses_top_level_type_and_date() {
        let raw = "---\n\
            type: Reference\n\
            valid_from: 2026-03-01\n\
            ---\nbody\n";
        let p = parse_memory_file(raw);
        assert_eq!(p.mem_type.as_deref(), Some("reference")); // lowercased
        let d = p.frontmatter_date.expect("date parsed");
        assert_eq!(d.to_rfc3339(), "2026-03-01T00:00:00+00:00");
    }

    #[test]
    fn rfc3339_frontmatter_date() {
        let raw = "---\ncreated: 2026-06-16T09:30:00+00:00\n---\nx\n";
        let p = parse_memory_file(raw);
        assert_eq!(
            p.frontmatter_date.unwrap().to_rfc3339(),
            "2026-06-16T09:30:00+00:00"
        );
    }

    #[test]
    fn naive_datetime_parses_as_utc() {
        // Grammar parity with the JSONL path's parse_timestamp.
        let d = parse_date("2024-05-01T10:30:00").expect("naive datetime parses");
        assert_eq!(d.to_rfc3339(), "2024-05-01T10:30:00+00:00");
    }

    #[test]
    fn filename_date_extracts_embedded_date() {
        use std::path::Path;
        assert_eq!(
            filename_date(Path::new("/m/project_s0_review_2026_06_14.md"))
                .unwrap()
                .to_rfc3339(),
            "2026-06-14T00:00:00+00:00"
        );
        // Hyphen separators also work.
        assert_eq!(
            filename_date(Path::new("note-2024-05-01-final.md"))
                .unwrap()
                .to_rfc3339(),
            "2024-05-01T00:00:00+00:00"
        );
        // No embedded date → None; an impossible date is rejected, not mis-dated.
        assert!(filename_date(Path::new("MEMORY.md")).is_none());
        assert!(filename_date(Path::new("x_2024_13_40.md")).is_none());
    }

    #[test]
    fn bom_prefixed_frontmatter_parses() {
        // A UTF-8 BOM before the leading `---` must not defeat detection.
        let raw = "\u{FEFF}---\nname: x\nmetadata:\n  type: user\n---\nbody text\n";
        let p = parse_memory_file(raw);
        assert_eq!(p.name.as_deref(), Some("x"));
        assert_eq!(p.mem_type.as_deref(), Some("user"));
        assert_eq!(p.body, "body text");
    }

    #[test]
    fn no_frontmatter_whole_file_is_body() {
        let raw = "# MEMORY index\n- [a](a.md) — hook\n";
        let p = parse_memory_file(raw);
        assert!(p.name.is_none());
        assert!(p.mem_type.is_none());
        assert!(p.frontmatter_date.is_none());
        assert_eq!(p.body, "# MEMORY index\n- [a](a.md) — hook");
    }

    #[test]
    fn unterminated_frontmatter_is_not_parsed() {
        // Opens with `---` but never closes — treat the whole thing as body.
        let raw = "---\nname: x\nbody with no closing fence\n";
        let p = parse_memory_file(raw);
        assert!(p.name.is_none());
        assert!(p.body.starts_with("---"));
    }

    #[test]
    fn leading_rule_with_later_divider_keeps_body() {
        // A prose doc that merely OPENS with a `---` rule and has another `---`
        // divider further down must NOT be parsed as frontmatter — otherwise the
        // prose above the second `---` is silently swallowed. The non-YAML first
        // body line aborts frontmatter detection (regression: metadata leak/data
        // loss found in review).
        let raw = "---\nThis is prose, not frontmatter.\n\n---\n\nMore prose.\n";
        let p = parse_memory_file(raw);
        assert!(p.name.is_none());
        assert!(p.mem_type.is_none());
        assert!(
            p.body.contains("This is prose, not frontmatter."),
            "body above the divider must be preserved, got: {:?}",
            p.body
        );
        assert!(p.body.contains("More prose."));
    }

    #[test]
    fn null_date_yields_none() {
        let raw = "---\nvalid_to: null\ndate: null\n---\nbody\n";
        let p = parse_memory_file(raw);
        assert!(p.frontmatter_date.is_none());
    }

    #[test]
    fn classify_routes_reference_to_kb() {
        assert_eq!(
            classify_memory_type(Some("user")),
            (FactType::Semantic, false)
        );
        assert_eq!(
            classify_memory_type(Some("feedback")),
            (FactType::Procedural, false)
        );
        assert_eq!(
            classify_memory_type(Some("project")),
            (FactType::Episodic, false)
        );
        assert_eq!(
            classify_memory_type(Some("reference")),
            (FactType::Semantic, true)
        );
        assert_eq!(classify_memory_type(None), (FactType::Semantic, false));
    }
}
