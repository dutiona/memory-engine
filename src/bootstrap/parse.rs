//! JSONL session log parser for Claude Code session files.
//!
//! Provides types and functions for deserializing raw JSONL lines into
//! [`SessionEntry`] values and extracting structured content blocks.

use std::io::{BufRead, Read};

use chrono::{DateTime, Utc};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// The `type` discriminant at the JSONL line level.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntryType {
    Assistant,
    User,
    Progress,
    QueueOperation,
    FileHistorySnapshot,
    LastPrompt,
    #[serde(other)]
    Unknown,
}

/// A single line from a Claude Code JSONL session file.
///
/// Several fields (`cwd`, `git_branch`, `data`) mirror the on-disk JSONL schema
/// for round-trip fidelity but are not consumed by the current pipeline; they
/// are retained as deserialization targets and documentation of the wire
/// format. The `#[allow(dead_code)]` is scoped to those fields and surfaced
/// only because the module is `pub(crate)`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEntry {
    #[serde(rename = "type")]
    pub entry_type: EntryType,
    pub session_id: Option<String>,
    pub timestamp: Option<String>,
    pub uuid: Option<String>,
    pub parent_uuid: Option<String>,
    #[allow(dead_code)] // schema field, parsed but not yet consumed
    pub cwd: Option<String>,
    #[allow(dead_code)] // schema field, parsed but not yet consumed
    pub git_branch: Option<String>,
    #[serde(default)]
    pub message: Option<MessagePayload>,
    #[serde(default)]
    pub tool_use_result: Option<ToolUseResult>,
    #[serde(default)]
    #[allow(dead_code)] // schema field, parsed but not yet consumed
    pub data: Option<serde_json::Value>,
}

/// Message payload attached to user/assistant entries.
#[derive(Debug, Clone, Deserialize)]
pub struct MessagePayload {
    #[allow(dead_code)] // schema field; role is inferred from `EntryType` instead
    pub role: Option<String>,
    pub content: Option<serde_json::Value>,
}

/// Result of a tool invocation, carried on user-type entries.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolUseResult {
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub interrupted: Option<bool>,
    pub is_error: Option<bool>,
}

/// Parsed content block from `message.content` array.
#[derive(Debug, Clone)]
pub enum ContentBlock {
    Text(String),
    ToolUse {
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        // Captured from the wire but not yet inspected by the filter, which
        // only needs to know a tool-result block is present.
        #[allow(dead_code)]
        content: String,
        #[allow(dead_code)]
        is_error: bool,
    },
    Thinking(String),
    Other,
}

// ---------------------------------------------------------------------------
// Functions
// ---------------------------------------------------------------------------

/// Maximum bytes buffered for a single JSONL line.
///
/// `BufRead::lines()` grows one `String` per line with no ceiling, so a hostile
/// (or corrupt) file containing a single newline-free run would force an
/// allocation proportional to its size — an unbounded-memory `DoS` on otherwise
/// best-effort parsing. Any logical line longer than this cap is drained and
/// skipped as malformed rather than buffered (see [`parse_session_file`]).
///
/// 8 MiB is generously above any legitimate Claude Code session line (whole
/// tool outputs and message payloads sit well under it) while bounding the
/// worst-case working set to a constant.
pub const MAX_JSONL_LINE_BYTES: usize = 8 * 1024 * 1024;

/// Default ceiling on total bytes read from one session stream (#293).
///
/// The per-line cap ([`MAX_JSONL_LINE_BYTES`]) bounds a single allocation, but a
/// hostile file of *many* in-bounds lines still streams unboundedly. This caps
/// the whole stream: the reader is wrapped in `Read::take`, so reading stops
/// after this many bytes regardless of line structure. Any logical line
/// straddling the boundary is truncated and, lacking its terminating newline,
/// is treated as a (final) malformed/oversized line rather than parsed.
///
/// 256 MiB is generously above any real Claude Code session file while keeping
/// the worst-case I/O bounded. Surfaced as [`crate::bootstrap::BootstrapConfig::max_session_bytes`].
pub const DEFAULT_MAX_SESSION_BYTES: u64 = 256 * 1024 * 1024;

/// Default ceiling on the number of [`SessionEntry`] values retained from one
/// stream (#293).
///
/// Each parsed entry is pushed into an in-memory `Vec`, and every downstream
/// pass (turn reconstruction, classification, extraction) is at least linear in
/// the entry count — so an unbounded count is both a memory and a CPU `DoS`.
/// Once this many entries are collected the parse loop stops with a `warn`,
/// analogous to the oversized-line skip. The session is processed best-effort on
/// the retained prefix.
///
/// 1,000,000 entries is far beyond any genuine session while bounding the
/// working set to a constant. Surfaced as
/// [`crate::bootstrap::BootstrapConfig::max_entries`].
pub const DEFAULT_MAX_ENTRIES: usize = 1_000_000;

/// Outcome of reading one logical line under a byte cap.
enum BoundedLine {
    /// A complete line within the cap, trailing `\n` stripped.
    Line(Vec<u8>),
    /// The line exceeded the cap; its bytes were drained to the next newline
    /// (or EOF) and discarded. Counts as malformed.
    Oversized,
    /// End of input.
    Eof,
}

/// Read one logical line, buffering at most `cap` bytes.
///
/// On an oversized line the excess is drained via `fill_buf`/`consume` so memory
/// stays flat and the *next* read resynchronizes at the following newline,
/// rather than emitting a cascade of fragment "lines".
fn read_bounded_line(
    reader: &mut impl std::io::BufRead,
    cap: usize,
) -> std::io::Result<BoundedLine> {
    let mut buf = Vec::new();
    // Read at most cap+1 bytes or until a newline, whichever comes first.
    // `saturating_add` guards the (unreachable for our 8 MiB cap) usize::MAX case.
    let n = reader
        .by_ref()
        .take((cap as u64).saturating_add(1))
        .read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(BoundedLine::Eof);
    }
    // Only `\n` is stripped here; a trailing `\r` (CRLF) is left in `buf` and
    // removed downstream by `line.trim()` before JSON parsing.
    if buf.last() == Some(&b'\n') {
        buf.pop();
        return Ok(BoundedLine::Line(buf));
    }
    // No newline seen. Either a final unterminated line within the cap, or a
    // line longer than the cap (we stopped one byte past it).
    if buf.len() <= cap {
        return Ok(BoundedLine::Line(buf));
    }
    // Oversized: drain the remainder of this line, discarding in place.
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            break; // EOF mid-line
        }
        if let Some(pos) = chunk.iter().position(|&b| b == b'\n') {
            reader.consume(pos + 1); // consume through the newline
            break;
        }
        let len = chunk.len();
        reader.consume(len);
    }
    Ok(BoundedLine::Oversized)
}

/// Parse a JSONL file into a sequence of [`SessionEntry`] values, under three
/// independent resource bounds against hostile or corrupt input (#293):
///
/// 1. **Per-line** ([`MAX_JSONL_LINE_BYTES`]): a single newline-free run cannot
///    force an allocation proportional to its size.
/// 2. **Per-stream** (`max_session_bytes`): the reader is wrapped in
///    [`std::io::Read::take`], so total bytes read are capped even for a file of
///    many in-bounds lines. A line straddling the boundary loses its newline and
///    is counted as a (final) oversized line. `0` = no per-stream limit.
/// 3. **Per-entry-count** (`max_entries`): parsing stops once this many entries
///    are retained, so the in-memory `Vec` — and every downstream linear pass —
///    stays bounded. `0` = no entry-count limit.
///
/// Both `0`-sentinels follow the `max_turns` convention (`0` = unbounded); the
/// real defaults live in [`DEFAULT_MAX_SESSION_BYTES`] / [`DEFAULT_MAX_ENTRIES`].
///
/// Malformed lines are skipped with a `tracing::warn`; this function never
/// fails so callers always get a best-effort result.
///
/// Returns `(entries, malformed_count)` where `malformed_count` is the number
/// of non-empty lines that failed to parse (including oversized ones).
pub fn parse_session_file(
    reader: impl std::io::BufRead,
    max_session_bytes: u64,
    max_entries: usize,
) -> (Vec<SessionEntry>, usize) {
    // `0` is the "no per-stream limit" sentinel (matching the `max_turns`
    // convention); map it to the maximum so `Read::take` does not read *nothing*.
    let stream_cap = if max_session_bytes == 0 {
        u64::MAX
    } else {
        max_session_bytes
    };
    // `Read::take` enforces the per-stream byte ceiling; re-buffer because the
    // `Take` adapter is `Read`, not `BufRead`, and the line reader needs the
    // latter (`read_until`/`fill_buf`/`consume`).
    let capped = std::io::BufReader::new(reader.take(stream_cap));
    parse_session_file_capped(capped, MAX_JSONL_LINE_BYTES, max_entries)
}

/// [`parse_session_file`] with explicit per-line and per-entry caps (test seam).
fn parse_session_file_capped(
    mut reader: impl std::io::BufRead,
    cap: usize,
    max_entries: usize,
) -> (Vec<SessionEntry>, usize) {
    let mut entries = Vec::new();
    let mut malformed = 0;
    let mut line_no = 0usize;
    loop {
        if max_entries > 0 && entries.len() >= max_entries {
            tracing::warn!(
                max_entries,
                "truncating session: reached entry-count cap (remaining lines discarded)"
            );
            break;
        }
        match read_bounded_line(&mut reader, cap) {
            Ok(BoundedLine::Eof) => break,
            Ok(BoundedLine::Oversized) => {
                line_no += 1;
                tracing::warn!(
                    line = line_no,
                    cap,
                    "skipping oversized JSONL line (exceeds per-line byte cap)"
                );
                malformed += 1;
            }
            Ok(BoundedLine::Line(bytes)) => {
                line_no += 1;
                let line = match std::str::from_utf8(&bytes) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(line = line_no, error = %e, "skipping non-UTF8 JSONL line");
                        malformed += 1;
                        continue;
                    }
                };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<SessionEntry>(trimmed) {
                    Ok(entry) => entries.push(entry),
                    Err(e) => {
                        tracing::warn!(line = line_no, error = %e, "skipping malformed JSONL line");
                        malformed += 1;
                    }
                }
            }
            Err(e) => {
                // A read error is typically persistent (bad descriptor); stop to
                // avoid a busy loop and return the best-effort result so far.
                tracing::warn!(line = line_no + 1, error = %e, "failed to read line; stopping");
                malformed += 1;
                break;
            }
        }
    }
    (entries, malformed)
}

/// Parse the content blocks from a [`MessagePayload`]'s `content` field.
///
/// Returns an empty vec if `content` is `None` or not a JSON array.
pub fn parse_content_blocks(content: &serde_json::Value) -> Vec<ContentBlock> {
    let Some(arr) = content.as_array() else {
        return Vec::new();
    };
    arr.iter().map(parse_single_block).collect()
}

fn parse_single_block(obj: &serde_json::Value) -> ContentBlock {
    let block_type = obj
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    match block_type {
        "text" => {
            let text = obj
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned();
            ContentBlock::Text(text)
        }
        "tool_use" => {
            let name = obj
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned();
            let input = obj.get("input").cloned().unwrap_or(serde_json::Value::Null);
            ContentBlock::ToolUse { name, input }
        }
        "tool_result" => {
            let content = match obj.get("content") {
                Some(v) if v.is_string() => v.as_str().unwrap_or("").to_owned(),
                Some(v) => v.to_string(),
                None => String::new(),
            };
            let is_error = obj
                .get("is_error")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            ContentBlock::ToolResult { content, is_error }
        }
        "thinking" => {
            let text = obj
                .get("thinking")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned();
            ContentBlock::Thinking(text)
        }
        _ => ContentBlock::Other,
    }
}

/// Parse a timestamp string to `DateTime<Utc>`, returning `None` on failure.
///
/// Tries RFC 3339 first, then falls back to `%Y-%m-%dT%H:%M:%S` (no timezone).
#[must_use]
pub fn parse_timestamp(s: &str) -> Option<DateTime<Utc>> {
    // Try RFC 3339 / ISO 8601 with timezone
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    // Fallback: naive ISO 8601 without timezone
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
        .ok()
        .map(|naive| naive.and_utc())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use chrono::{Datelike, Timelike};

    use super::*;
    use std::io::BufReader;

    /// Parse with no per-stream/per-entry caps (per-line cap still applies).
    /// Keeps the line-level tests focused on their own concern.
    fn parse_all(reader: impl std::io::BufRead) -> (Vec<SessionEntry>, usize) {
        parse_session_file(reader, 0, 0)
    }

    /// Parse with an explicit per-line cap and no entry-count cap.
    fn parse_capped(reader: impl std::io::BufRead, cap: usize) -> (Vec<SessionEntry>, usize) {
        parse_session_file_capped(reader, cap, 0)
    }

    #[test]
    fn parse_empty_file() {
        let reader = BufReader::new(b"" as &[u8]);
        let (entries, _malformed) = parse_all(reader);
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_skips_oversized_line_and_resyncs() {
        // A line longer than the cap is drained and skipped; the *next* line
        // must still parse (proving the drain resynchronized at the newline
        // rather than emitting a cascade of fragment lines).
        let big = "x".repeat(200); // no embedded newline, far over the cap
        let input = format!("{big}\n{{\"type\":\"user\"}}\n");
        let (entries, malformed) = parse_capped(input.as_bytes(), 64);
        assert_eq!(
            entries.len(),
            1,
            "valid line after oversized must still parse"
        );
        assert_eq!(entries[0].entry_type, EntryType::User);
        assert_eq!(malformed, 1, "the oversized line counts once as malformed");
    }

    #[test]
    fn parse_line_at_cap_boundary_parses() {
        // A valid line whose length is exactly the cap must parse normally —
        // the cap is inclusive of legitimate lines, exclusive of overflow.
        let line = r#"{"type":"user"}"#;
        assert_eq!(line.len(), 15);
        let (entries, malformed) = parse_capped(line.as_bytes(), 15);
        assert_eq!(entries.len(), 1);
        assert_eq!(malformed, 0);
    }

    #[test]
    fn parse_oversized_unterminated_at_eof() {
        // An oversized final line with no trailing newline drains to EOF and is
        // counted once; the parser terminates rather than looping.
        let big = "y".repeat(100);
        let (entries, malformed) = parse_capped(big.as_bytes(), 16);
        assert!(entries.is_empty());
        assert_eq!(malformed, 1);
    }

    #[test]
    fn parse_truncates_at_entry_count_cap() {
        // #293 residual: a flood of small, individually-valid lines must not grow
        // the entry Vec without bound. With max_entries=3, only the first 3 of
        // 10 valid lines are retained; the rest are discarded (not parsed), so
        // downstream linear passes stay bounded.
        let mut input = String::new();
        for _ in 0..10 {
            input.push_str("{\"type\":\"user\"}\n");
        }
        let (entries, malformed) =
            parse_session_file_capped(input.as_bytes(), MAX_JSONL_LINE_BYTES, 3);
        assert_eq!(
            entries.len(),
            3,
            "entry count must be capped at max_entries"
        );
        assert_eq!(
            malformed, 0,
            "truncated-away lines are discarded, not counted as malformed"
        );
    }

    #[test]
    fn parse_entry_count_cap_zero_is_unbounded() {
        // max_entries == 0 disables the entry-count cap: all valid lines parse.
        let mut input = String::new();
        for _ in 0..10 {
            input.push_str("{\"type\":\"user\"}\n");
        }
        let (entries, _malformed) =
            parse_session_file_capped(input.as_bytes(), MAX_JSONL_LINE_BYTES, 0);
        assert_eq!(entries.len(), 10);
    }

    #[test]
    fn parse_truncates_at_session_byte_cap() {
        // #293 residual: the per-stream byte ceiling (via Read::take) stops the
        // reader mid-file. Three 15-byte lines + newlines = 48 bytes total; a
        // 31-byte cap admits the first two whole lines (32 bytes would include
        // the 2nd newline, but at 31 we stop one byte short, leaving the 2nd
        // line unterminated → counted as a final oversized/malformed line).
        let line = "{\"type\":\"user\"}"; // 15 bytes
        assert_eq!(line.len(), 15);
        let input = format!("{line}\n{line}\n{line}\n");
        // Admit exactly the first full line + newline (16 bytes), then 15 bytes
        // of the second line with no terminating newline.
        let (entries, _malformed) = parse_session_file(input.as_bytes(), 31, 0);
        assert_eq!(
            entries.len(),
            2,
            "per-stream cap admits the first whole line plus the unterminated second"
        );
    }

    #[test]
    fn parse_session_byte_cap_zero_is_unbounded() {
        // max_session_bytes == 0 is the "no per-stream limit" sentinel (matching
        // the max_turns convention) — it must NOT be passed verbatim to
        // Read::take, which would read nothing. All valid lines must parse.
        let mut input = String::new();
        for _ in 0..5 {
            input.push_str("{\"type\":\"user\"}\n");
        }
        let (entries, _malformed) = parse_session_file(input.as_bytes(), 0, 0);
        assert_eq!(
            entries.len(),
            5,
            "the 0 sentinel must mean unbounded, not take(0)/read-nothing"
        );
    }

    #[test]
    fn parse_malformed_line_skipped() {
        let input = r#"not valid json
{"type":"user","sessionId":"a1b2c3"}
"#;
        let reader = BufReader::new(input.as_bytes());
        let (entries, malformed) = parse_all(reader);
        assert_eq!(entries.len(), 1);
        assert_eq!(malformed, 1, "one malformed line should be counted");
        assert_eq!(entries[0].entry_type, EntryType::User);
    }

    #[test]
    fn parse_user_message() {
        let line = r#"{"type":"user","sessionId":"sess-001","timestamp":"2026-03-19T10:00:00Z","uuid":"u1","parentUuid":null,"cwd":"/tmp","message":{"role":"user","content":[{"type":"text","text":"hello world"}]}}"#;
        let reader = BufReader::new(line.as_bytes());
        let (entries, _malformed) = parse_all(reader);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry_type, EntryType::User);
        let msg = entries[0].message.as_ref().unwrap();
        assert_eq!(msg.role.as_deref(), Some("user"));
    }

    #[test]
    fn parse_assistant_tool_use() {
        let line = r#"{"type":"assistant","sessionId":"sess-001","timestamp":"2026-03-19T10:01:00Z","uuid":"u2","parentUuid":"u1","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}]}}"#;
        let reader = BufReader::new(line.as_bytes());
        let (entries, _malformed) = parse_all(reader);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry_type, EntryType::Assistant);
        let content = entries[0]
            .message
            .as_ref()
            .unwrap()
            .content
            .as_ref()
            .unwrap();
        let blocks = parse_content_blocks(content);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], ContentBlock::ToolUse { name, .. } if name == "Bash"));
    }

    #[test]
    fn parse_tool_result_entry() {
        let line = r#"{"type":"user","sessionId":"sess-001","uuid":"u3","parentUuid":"u2","toolUseResult":{"stdout":"ok","stderr":"","isError":false}}"#;
        let reader = BufReader::new(line.as_bytes());
        let (entries, _malformed) = parse_all(reader);
        assert_eq!(entries.len(), 1);
        let result = entries[0].tool_use_result.as_ref().unwrap();
        assert_eq!(result.stdout.as_deref(), Some("ok"));
        assert_eq!(result.is_error, Some(false));
    }

    #[test]
    fn parse_progress_entry() {
        let line = r#"{"type":"progress","sessionId":"sess-001","data":{"status":"running"}}"#;
        let reader = BufReader::new(line.as_bytes());
        let (entries, _malformed) = parse_all(reader);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry_type, EntryType::Progress);
    }

    #[test]
    fn parse_content_blocks_text() {
        let content = serde_json::json!([{"type": "text", "text": "hello"}]);
        let blocks = parse_content_blocks(&content);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], ContentBlock::Text(t) if t == "hello"));
    }

    #[test]
    fn parse_content_blocks_tool_use() {
        let content =
            serde_json::json!([{"type": "tool_use", "name": "Read", "input": {"path": "/tmp"}}]);
        let blocks = parse_content_blocks(&content);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::ToolUse { name, input } => {
                assert_eq!(name, "Read");
                assert_eq!(input["path"], "/tmp");
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn parse_content_blocks_thinking() {
        let content =
            serde_json::json!([{"type": "thinking", "thinking": "let me reason about this"}]);
        let blocks = parse_content_blocks(&content);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], ContentBlock::Thinking(t) if t == "let me reason about this"));
    }

    #[test]
    fn parse_timestamp_valid() {
        let dt = parse_timestamp("2026-03-19T10:30:00Z").unwrap();
        assert_eq!(dt.year(), 2026);
        assert_eq!(dt.month(), 3);
        assert_eq!(dt.day(), 19);
    }

    #[test]
    fn parse_timestamp_invalid() {
        assert!(parse_timestamp("not-a-date").is_none());
    }

    #[test]
    fn parse_timestamp_naive_no_timezone_fallback() {
        // No timezone suffix: RFC 3339 parse fails, the naive fallback
        // (parse.rs:185-188) takes over and interprets it as UTC.
        let dt = parse_timestamp("2026-03-19T10:30:45").unwrap();
        assert_eq!(dt.year(), 2026);
        assert_eq!(dt.month(), 3);
        assert_eq!(dt.day(), 19);
        assert_eq!(dt.hour(), 10);
        assert_eq!(dt.minute(), 30);
        assert_eq!(dt.second(), 45);
        // The naive timestamp must be promoted to the UTC zone.
        assert_eq!(dt, parse_timestamp("2026-03-19T10:30:45Z").unwrap());
    }

    #[test]
    fn parse_content_blocks_tool_result_string_content() {
        // String content + is_error=true exercises the `v.is_string()` arm
        // and the explicit is_error read (parse.rs:152-163).
        let content = serde_json::json!([{
            "type": "tool_result",
            "content": "command output",
            "is_error": true
        }]);
        let blocks = parse_content_blocks(&content);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::ToolResult { content, is_error } => {
                assert_eq!(content, "command output");
                assert!(*is_error);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn parse_content_blocks_tool_result_non_string_content_defaults_no_error() {
        // Non-string content takes the `Some(v) => v.to_string()` arm, and
        // a missing `is_error` defaults to false.
        let content = serde_json::json!([{
            "type": "tool_result",
            "content": [{"type": "text", "text": "nested"}]
        }]);
        let blocks = parse_content_blocks(&content);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::ToolResult { content, is_error } => {
                // Serialized JSON of the non-string content.
                assert!(content.contains("nested"));
                assert!(!*is_error, "missing is_error must default to false");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn parse_content_blocks_tool_result_missing_content_empty_string() {
        // Absent `content` takes the `None => String::new()` arm.
        let content = serde_json::json!([{"type": "tool_result"}]);
        let blocks = parse_content_blocks(&content);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::ToolResult { content, is_error } => {
                assert_eq!(content, "");
                assert!(!*is_error);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }
}
