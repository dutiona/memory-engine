//! JSONL session log parser for Claude Code session files.
//!
//! Provides types and functions for deserializing raw JSONL lines into
//! [`SessionEntry`] values and extracting structured content blocks.

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
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEntry {
    #[serde(rename = "type")]
    pub entry_type: EntryType,
    pub session_id: Option<String>,
    pub timestamp: Option<String>,
    pub uuid: Option<String>,
    pub parent_uuid: Option<String>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    #[serde(default)]
    pub message: Option<MessagePayload>,
    #[serde(default)]
    pub tool_use_result: Option<ToolUseResult>,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
}

/// Message payload attached to user/assistant entries.
#[derive(Debug, Clone, Deserialize)]
pub struct MessagePayload {
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
        content: String,
        is_error: bool,
    },
    Thinking(String),
    Other,
}

// ---------------------------------------------------------------------------
// Functions
// ---------------------------------------------------------------------------

/// Parse a JSONL file into a sequence of [`SessionEntry`] values.
///
/// Malformed lines are skipped with a `tracing::warn`; this function never
/// fails so callers always get a best-effort result.
///
/// Returns `(entries, malformed_count)` where `malformed_count` is the number
/// of non-empty lines that failed to parse.
pub fn parse_session_file(reader: impl std::io::BufRead) -> (Vec<SessionEntry>, usize) {
    let mut entries = Vec::new();
    let mut malformed = 0;
    for (idx, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(line = idx + 1, error = %e, "failed to read line");
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
                tracing::warn!(line = idx + 1, error = %e, "skipping malformed JSONL line");
                malformed += 1;
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

    #[test]
    fn parse_empty_file() {
        let reader = BufReader::new(b"" as &[u8]);
        let (entries, _malformed) = parse_session_file(reader);
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_malformed_line_skipped() {
        let input = r#"not valid json
{"type":"user","sessionId":"a1b2c3"}
"#;
        let reader = BufReader::new(input.as_bytes());
        let (entries, malformed) = parse_session_file(reader);
        assert_eq!(entries.len(), 1);
        assert_eq!(malformed, 1, "one malformed line should be counted");
        assert_eq!(entries[0].entry_type, EntryType::User);
    }

    #[test]
    fn parse_user_message() {
        let line = r#"{"type":"user","sessionId":"sess-001","timestamp":"2026-03-19T10:00:00Z","uuid":"u1","parentUuid":null,"cwd":"/tmp","message":{"role":"user","content":[{"type":"text","text":"hello world"}]}}"#;
        let reader = BufReader::new(line.as_bytes());
        let (entries, _malformed) = parse_session_file(reader);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry_type, EntryType::User);
        let msg = entries[0].message.as_ref().unwrap();
        assert_eq!(msg.role.as_deref(), Some("user"));
    }

    #[test]
    fn parse_assistant_tool_use() {
        let line = r#"{"type":"assistant","sessionId":"sess-001","timestamp":"2026-03-19T10:01:00Z","uuid":"u2","parentUuid":"u1","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}]}}"#;
        let reader = BufReader::new(line.as_bytes());
        let (entries, _malformed) = parse_session_file(reader);
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
        let (entries, _malformed) = parse_session_file(reader);
        assert_eq!(entries.len(), 1);
        let result = entries[0].tool_use_result.as_ref().unwrap();
        assert_eq!(result.stdout.as_deref(), Some("ok"));
        assert_eq!(result.is_error, Some(false));
    }

    #[test]
    fn parse_progress_entry() {
        let line = r#"{"type":"progress","sessionId":"sess-001","data":{"status":"running"}}"#;
        let reader = BufReader::new(line.as_bytes());
        let (entries, _malformed) = parse_session_file(reader);
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
