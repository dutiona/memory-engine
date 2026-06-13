//! Conversation turn reconstruction and `NeuroStack` keyword pre-filter.
//!
//! Transforms raw [`SessionEntry`] sequences into structured [`ConversationTurn`]s,
//! then applies rule-based keyword matching to surface [`CandidateEpisode`]s worth
//! promoting to long-term memory.

use std::collections::HashSet;

use chrono::{DateTime, Utc};

use super::parse::{ContentBlock, EntryType, SessionEntry};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A conversation turn: user message paired with assistant response and tool results.
#[derive(Debug, Clone)]
pub struct ConversationTurn {
    pub timestamp: DateTime<Utc>,
    pub user_text: String,
    pub assistant_text: String,
    pub tool_calls: Vec<ToolCallRecord>,
    pub uuid: String,
}

/// Record of a single tool invocation and its outcome.
#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    pub tool_name: String,
    pub input: serde_json::Value,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub is_error: bool,
    pub interrupted: bool,
}

/// Category of a noteworthy episode detected by keyword pre-filter.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EpisodeCategory {
    Bug,
    Decision,
    Convention,
    Learning,
}

/// A candidate episode extracted by the rule-based pre-filter.
#[derive(Debug, Clone)]
pub struct CandidateEpisode {
    pub category: EpisodeCategory,
    pub turns: Vec<ConversationTurn>,
    pub matched_keywords: Vec<String>,
    pub session_id: String,
    pub timestamp: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Keyword tables
// ---------------------------------------------------------------------------

/// Keywords that trigger on `user_text` + `assistant_text` + tool stderr (but NOT
/// tool stdout, to avoid false positives like "0 errors").
const BUG_KEYWORDS: &[&str] = &["root cause", "fix", "traceback", "exception"];

/// "error" is handled specially — see `check_error_keyword`.
const BUG_ERROR_KEYWORD: &str = "error";

const DECISION_KEYWORDS: &[&str] = &["switched from", "chose", "decided", "went with"];

const CONVENTION_KEYWORDS: &[&str] = &["always use", "never use", "rule", "convention"];

const LEARNING_KEYWORDS: &[&str] = &["discovered", "til", "turns out", "reason is"];

// ---------------------------------------------------------------------------
// Turn reconstruction
// ---------------------------------------------------------------------------

/// Reconstruct [`ConversationTurn`]s from a raw `SessionEntry` sequence.
///
/// Progress, queue-operation, file-history-snapshot, last-prompt, and unknown
/// entries are filtered out. User + assistant entries are paired first by
/// UUID/parent-UUID linkage, then by sequential fallback.
#[must_use]
pub fn reconstruct_turns(entries: &[SessionEntry]) -> Vec<ConversationTurn> {
    // Keep only user/assistant entries.
    let relevant: Vec<&SessionEntry> = entries
        .iter()
        .filter(|e| matches!(e.entry_type, EntryType::User | EntryType::Assistant))
        .collect();

    if relevant.is_empty() {
        return Vec::new();
    }

    let mut used: HashSet<&str> = HashSet::new();
    let mut turns: Vec<ConversationTurn> = Vec::new();

    // --- UUID-linked pairing ---
    // Separate user entries that are tool results (carry tool_use_result)
    // from user entries that are genuine prompts.
    let is_tool_result = |e: &SessionEntry| -> bool { e.tool_use_result.is_some() };

    for entry in &relevant {
        if !matches!(entry.entry_type, EntryType::User) || is_tool_result(entry) {
            continue;
        }
        let Some(ref user_uuid) = entry.uuid else {
            continue;
        };
        if used.contains(user_uuid.as_str()) {
            continue;
        }

        // Find assistant reply whose parent_uuid matches this user's uuid.
        let assistant = relevant.iter().find(|a| {
            matches!(a.entry_type, EntryType::Assistant)
                && a.parent_uuid
                    .as_deref()
                    .is_some_and(|p| !p.is_empty() && p == user_uuid)
                && a.uuid.as_deref().is_some_and(|u| !used.contains(u))
        });

        if let Some(assistant) = assistant {
            // Collect tool-result user entries that follow this assistant.
            let tool_results = collect_tool_result_entries(&relevant, assistant);
            turns.push(build_turn(entry, assistant, &tool_results));
            used.insert(user_uuid.as_str());
            if let Some(ref a_uuid) = assistant.uuid {
                used.insert(a_uuid.as_str());
            }
            for tr in &tool_results {
                if let Some(ref u) = tr.uuid {
                    used.insert(u.as_str());
                }
            }
        }
    }

    // --- Sequential fallback for unpaired entries ---
    // Only pair a user entry with the next assistant that appears *before*
    // the next genuine user entry in the sequence (issue #82).
    let mut i = 0;
    while i < relevant.len() {
        let entry = relevant[i];
        let entry_uuid = entry.uuid.as_deref().unwrap_or("");
        if matches!(entry.entry_type, EntryType::User)
            && !is_tool_result(entry)
            && !entry_uuid.is_empty()
            && !used.contains(entry_uuid)
        {
            // Search for an assistant, stopping at the next genuine user entry.
            let assistant = relevant[i + 1..]
                .iter()
                .take_while(|e| !matches!(e.entry_type, EntryType::User) || is_tool_result(e))
                .find(|a| {
                    matches!(a.entry_type, EntryType::Assistant)
                        && a.uuid.as_deref().is_some_and(|u| !used.contains(u))
                });

            if let Some(assistant) = assistant {
                let tool_results = collect_tool_result_entries(&relevant, assistant);
                turns.push(build_turn(entry, assistant, &tool_results));
                used.insert(entry_uuid);
                if let Some(ref a_uuid) = assistant.uuid {
                    used.insert(a_uuid.as_str());
                }
                for tr in &tool_results {
                    if let Some(ref u) = tr.uuid {
                        used.insert(u.as_str());
                    }
                }
            }
        }
        i += 1;
    }

    turns.sort_by_key(|t| t.timestamp);
    turns
}

/// Build a single [`ConversationTurn`] from a user entry and its assistant reply.
///
/// `tool_result_entries` are user entries that carry `tool_use_result` for the
/// assistant's `tool_use` blocks (collected by the caller from subsequent entries).
fn build_turn(
    user: &SessionEntry,
    assistant: &SessionEntry,
    tool_result_entries: &[&SessionEntry],
) -> ConversationTurn {
    let user_text = extract_text(user);
    let assistant_text = extract_text(assistant);
    let tool_calls = extract_tool_calls(assistant, tool_result_entries);

    let timestamp = user
        .timestamp
        .as_deref()
        .and_then(super::parse::parse_timestamp)
        .or_else(|| {
            assistant
                .timestamp
                .as_deref()
                .and_then(super::parse::parse_timestamp)
        })
        .unwrap_or_else(Utc::now);

    ConversationTurn {
        timestamp,
        user_text,
        assistant_text,
        tool_calls,
        uuid: user.uuid.clone().unwrap_or_default(),
    }
}

/// Collect user entries that carry `tool_use_result` and whose `parent_uuid`
/// points to the given assistant entry. These are the tool-result rows that
/// follow an assistant's `tool_use` blocks in Claude Code JSONL.
fn collect_tool_result_entries<'a>(
    all: &[&'a SessionEntry],
    assistant: &SessionEntry,
) -> Vec<&'a SessionEntry> {
    let Some(ref assistant_uuid) = assistant.uuid else {
        return Vec::new();
    };
    all.iter()
        .filter(|e| {
            e.tool_use_result.is_some()
                && e.parent_uuid
                    .as_deref()
                    .is_some_and(|p| p == assistant_uuid)
        })
        .copied()
        .collect()
}

/// Extract plain text from an entry's message content blocks.
fn extract_text(entry: &SessionEntry) -> String {
    let Some(ref msg) = entry.message else {
        return String::new();
    };
    let Some(ref content) = msg.content else {
        return String::new();
    };

    let blocks = super::parse::parse_content_blocks(content);
    let mut parts: Vec<String> = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text(t) | ContentBlock::Thinking(t) => parts.push(t),
            ContentBlock::ToolUse { .. }
            | ContentBlock::ToolResult { .. }
            | ContentBlock::Other => {}
        }
    }
    parts.join("\n")
}

/// Extract tool call records from the assistant's tool-use blocks, enriching
/// them with tool-result data from corresponding user entries.
///
/// In Claude Code JSONL, `tool_use_result` lives on user rows that follow
/// the assistant's `tool_use` blocks. Each user row carries the result for
/// one tool invocation. We collect results from `tool_result_entries` (user
/// entries that carry `tool_use_result`) and pair them positionally with
/// the assistant's tool-use blocks.
fn extract_tool_calls(
    assistant: &SessionEntry,
    tool_result_entries: &[&SessionEntry],
) -> Vec<ToolCallRecord> {
    let Some(ref msg) = assistant.message else {
        return Vec::new();
    };
    let Some(ref content) = msg.content else {
        return Vec::new();
    };

    let blocks = super::parse::parse_content_blocks(content);
    let mut calls: Vec<ToolCallRecord> = Vec::new();

    for block in blocks {
        if let ContentBlock::ToolUse { name, input } = block {
            calls.push(ToolCallRecord {
                tool_name: name,
                input,
                stdout: None,
                stderr: None,
                is_error: false,
                interrupted: false,
            });
        }
    }

    // Pair tool results positionally: the i-th tool_result_entry corresponds
    // to the i-th tool_use block in the assistant's message.
    for (i, result_entry) in tool_result_entries.iter().enumerate() {
        if i >= calls.len() {
            break;
        }
        if let Some(ref result) = result_entry.tool_use_result {
            calls[i].stdout.clone_from(&result.stdout);
            calls[i].stderr.clone_from(&result.stderr);
            calls[i].is_error = result.is_error.unwrap_or(false);
            calls[i].interrupted = result.interrupted.unwrap_or(false);
        }
    }

    calls
}

// ---------------------------------------------------------------------------
// Keyword pre-filter
// ---------------------------------------------------------------------------

/// Apply `NeuroStack` keyword patterns to identify [`CandidateEpisode`]s.
///
/// Each turn is checked against keyword tables for every category. A turn can
/// match multiple categories, producing one `CandidateEpisode` per category per
/// matching turn.
#[must_use]
pub fn keyword_prefilter(turns: &[ConversationTurn], session_id: &str) -> Vec<CandidateEpisode> {
    let mut episodes: Vec<CandidateEpisode> = Vec::new();

    for turn in turns {
        let searchable = build_searchable_text(turn);
        let assistant_lower = turn.assistant_text.to_lowercase();
        let stderr_text = collect_stderr(turn);

        // --- Bug ---
        let mut bug_keywords: Vec<String> = Vec::new();
        for &kw in BUG_KEYWORDS {
            if searchable.contains(kw) {
                bug_keywords.push(kw.to_owned());
            }
        }
        if check_error_keyword(&assistant_lower, &stderr_text) {
            bug_keywords.push(BUG_ERROR_KEYWORD.to_owned());
        }
        if !bug_keywords.is_empty() {
            episodes.push(CandidateEpisode {
                category: EpisodeCategory::Bug,
                turns: vec![turn.clone()],
                matched_keywords: bug_keywords,
                session_id: session_id.to_owned(),
                timestamp: turn.timestamp,
            });
        }

        // --- Decision ---
        let decision_kws = match_keywords(&searchable, DECISION_KEYWORDS);
        if !decision_kws.is_empty() {
            episodes.push(CandidateEpisode {
                category: EpisodeCategory::Decision,
                turns: vec![turn.clone()],
                matched_keywords: decision_kws,
                session_id: session_id.to_owned(),
                timestamp: turn.timestamp,
            });
        }

        // --- Convention ---
        let convention_kws = match_keywords(&searchable, CONVENTION_KEYWORDS);
        if !convention_kws.is_empty() {
            episodes.push(CandidateEpisode {
                category: EpisodeCategory::Convention,
                turns: vec![turn.clone()],
                matched_keywords: convention_kws,
                session_id: session_id.to_owned(),
                timestamp: turn.timestamp,
            });
        }

        // --- Learning ---
        let learning_kws = match_keywords(&searchable, LEARNING_KEYWORDS);
        if !learning_kws.is_empty() {
            episodes.push(CandidateEpisode {
                category: EpisodeCategory::Learning,
                turns: vec![turn.clone()],
                matched_keywords: learning_kws,
                session_id: session_id.to_owned(),
                timestamp: turn.timestamp,
            });
        }
    }

    episodes
}

/// Build case-insensitive searchable text from a turn: user text, assistant
/// text, and tool stderr. Tool stdout is deliberately excluded to avoid false
/// positives on success messages.
fn build_searchable_text(turn: &ConversationTurn) -> String {
    let mut buf = String::new();
    buf.push_str(&turn.user_text.to_lowercase());
    buf.push('\n');
    buf.push_str(&turn.assistant_text.to_lowercase());
    buf.push('\n');
    for tc in &turn.tool_calls {
        if let Some(ref stderr) = tc.stderr {
            buf.push_str(&stderr.to_lowercase());
            buf.push('\n');
        }
    }
    buf
}

/// Collect all stderr text from a turn's tool calls (lowercased).
fn collect_stderr(turn: &ConversationTurn) -> String {
    turn.tool_calls
        .iter()
        .filter_map(|tc| tc.stderr.as_deref())
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase()
}

/// Check whether "error" should trigger a Bug match. We only fire when "error"
/// appears in tool stderr or in assistant text that also contains a bug-related
/// word ("fix", "bug", "issue"), filtering out benign occurrences.
fn check_error_keyword(assistant_lower: &str, stderr_lower: &str) -> bool {
    if stderr_lower.contains(BUG_ERROR_KEYWORD) {
        return true;
    }
    if assistant_lower.contains(BUG_ERROR_KEYWORD) {
        let bug_context = ["fix", "bug", "issue"];
        return bug_context.iter().any(|ctx| assistant_lower.contains(ctx));
    }
    false
}

/// Return the list of matched keywords from `table` found in `haystack`.
fn match_keywords(haystack: &str, table: &[&str]) -> Vec<String> {
    table
        .iter()
        .filter(|kw| haystack.contains(**kw))
        .map(|kw| (*kw).to_owned())
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;
    use crate::bootstrap::parse::{EntryType, MessagePayload, SessionEntry, ToolUseResult};

    // -- helpers --

    fn ts(secs: i64) -> Option<DateTime<Utc>> {
        Utc.timestamp_opt(secs, 0).single()
    }

    fn opt(s: &str) -> Option<String> {
        if s.is_empty() { None } else { Some(s.into()) }
    }

    fn user_entry(uuid: &str, parent: &str, text: &str, secs: i64) -> SessionEntry {
        SessionEntry {
            entry_type: EntryType::User,
            session_id: Some("sess-1".into()),
            timestamp: ts(secs).map(|t| t.to_rfc3339()),
            uuid: opt(uuid),
            parent_uuid: opt(parent),
            cwd: None,
            git_branch: None,
            message: Some(MessagePayload {
                role: Some("user".into()),
                content: Some(json!([{"type": "text", "text": text}])),
            }),
            tool_use_result: None,
            data: None,
        }
    }

    fn assistant_entry(uuid: &str, parent: &str, text: &str, secs: i64) -> SessionEntry {
        SessionEntry {
            entry_type: EntryType::Assistant,
            session_id: Some("sess-1".into()),
            timestamp: ts(secs).map(|t| t.to_rfc3339()),
            uuid: opt(uuid),
            parent_uuid: opt(parent),
            cwd: None,
            git_branch: None,
            message: Some(MessagePayload {
                role: Some("assistant".into()),
                content: Some(json!([{"type": "text", "text": text}])),
            }),
            tool_use_result: None,
            data: None,
        }
    }

    fn assistant_with_tool(
        uuid: &str,
        parent: &str,
        text: &str,
        tool_name: &str,
        secs: i64,
    ) -> SessionEntry {
        SessionEntry {
            entry_type: EntryType::Assistant,
            session_id: Some("sess-1".into()),
            timestamp: ts(secs).map(|t| t.to_rfc3339()),
            uuid: opt(uuid),
            parent_uuid: opt(parent),
            cwd: None,
            git_branch: None,
            message: Some(MessagePayload {
                role: Some("assistant".into()),
                content: Some(json!([
                    {"type": "text", "text": text},
                    {"type": "tool_use", "name": tool_name, "input": {"cmd": "ls"}}
                ])),
            }),
            tool_use_result: None,
            data: None,
        }
    }

    fn noise_entry(entry_type: EntryType, secs: i64) -> SessionEntry {
        SessionEntry {
            entry_type,
            session_id: Some("sess-1".into()),
            timestamp: ts(secs).map(|t| t.to_rfc3339()),
            uuid: Some(format!("noise-{secs}")),
            parent_uuid: None,
            cwd: None,
            git_branch: None,
            message: None,
            tool_use_result: None,
            data: None,
        }
    }

    fn user_with_tool_result(
        uuid: &str,
        parent: &str,
        text: &str,
        stdout: Option<&str>,
        stderr: Option<&str>,
        is_error: bool,
        secs: i64,
    ) -> SessionEntry {
        SessionEntry {
            entry_type: EntryType::User,
            session_id: Some("sess-1".into()),
            timestamp: ts(secs).map(|t| t.to_rfc3339()),
            uuid: opt(uuid),
            parent_uuid: opt(parent),
            cwd: None,
            git_branch: None,
            message: Some(MessagePayload {
                role: Some("user".into()),
                content: Some(json!([{"type": "text", "text": text}])),
            }),
            tool_use_result: Some(ToolUseResult {
                stdout: stdout.map(String::from),
                stderr: stderr.map(String::from),
                interrupted: Some(false),
                is_error: Some(is_error),
            }),
            data: None,
        }
    }

    fn user_with_interrupted_tool_result(uuid: &str, parent: &str, secs: i64) -> SessionEntry {
        SessionEntry {
            entry_type: EntryType::User,
            session_id: Some("sess-1".into()),
            timestamp: ts(secs).map(|t| t.to_rfc3339()),
            uuid: opt(uuid),
            parent_uuid: opt(parent),
            cwd: None,
            git_branch: None,
            message: Some(MessagePayload {
                role: Some("user".into()),
                content: Some(json!([{"type": "text", "text": ""}])),
            }),
            tool_use_result: Some(ToolUseResult {
                stdout: None,
                stderr: Some("interrupted".into()),
                interrupted: Some(true),
                is_error: Some(false),
            }),
            data: None,
        }
    }

    // -- reconstruct_turns tests --

    #[test]
    fn reconstruct_empty() {
        let turns = reconstruct_turns(&[]);
        assert!(turns.is_empty());
    }

    #[test]
    fn reconstruct_filters_noise() {
        let entries = vec![
            noise_entry(EntryType::Progress, 1),
            noise_entry(EntryType::QueueOperation, 2),
            noise_entry(EntryType::FileHistorySnapshot, 3),
            noise_entry(EntryType::Unknown, 4),
        ];
        let turns = reconstruct_turns(&entries);
        assert!(turns.is_empty());
    }

    #[test]
    fn reconstruct_pairs_user_assistant() {
        let entries = vec![
            user_entry("u1", "", "Hello", 100),
            assistant_entry("a1", "u1", "Hi there", 101),
        ];
        let turns = reconstruct_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].user_text, "Hello");
        assert_eq!(turns[0].assistant_text, "Hi there");
        assert_eq!(turns[0].uuid, "u1");
    }

    #[test]
    fn reconstruct_extracts_tool_calls() {
        // Real JSONL flow: User(prompt) → Assistant(tool_use) → User(tool_result)
        let entries = vec![
            user_entry("u1", "", "run ls", 100),
            assistant_with_tool("a1", "u1", "Sure, running ls", "Bash", 101),
            // Tool result entry: parent_uuid points to assistant, carries tool_use_result
            user_with_tool_result("u2", "a1", "", Some("file.txt"), None, false, 102),
        ];
        let turns = reconstruct_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        assert_eq!(turns[0].tool_calls[0].tool_name, "Bash");
        assert_eq!(turns[0].tool_calls[0].stdout.as_deref(), Some("file.txt"));
        assert!(!turns[0].tool_calls[0].is_error);
    }

    #[test]
    fn reconstruct_propagates_interrupted_flag() {
        let entries = vec![
            user_entry("u1", "", "run ls", 100),
            assistant_with_tool("a1", "u1", "Sure", "Bash", 101),
            user_with_interrupted_tool_result("u2", "a1", 102),
        ];
        let turns = reconstruct_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        assert!(turns[0].tool_calls[0].interrupted);
        assert!(!turns[0].tool_calls[0].is_error);
    }

    #[test]
    fn reconstruct_interrupted_false_by_default() {
        let entries = vec![
            user_entry("u1", "", "run ls", 100),
            assistant_with_tool("a1", "u1", "Sure", "Bash", 101),
            user_with_tool_result("u2", "a1", "", Some("ok"), None, false, 102),
        ];
        let turns = reconstruct_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert!(!turns[0].tool_calls[0].interrupted);
    }

    // -- keyword_prefilter tests --

    fn make_turn(user: &str, assistant: &str) -> ConversationTurn {
        ConversationTurn {
            timestamp: Utc.timestamp_opt(1000, 0).unwrap(),
            user_text: user.into(),
            assistant_text: assistant.into(),
            tool_calls: vec![],
            uuid: "t1".into(),
        }
    }

    fn make_turn_with_stderr(user: &str, assistant: &str, stderr: &str) -> ConversationTurn {
        ConversationTurn {
            timestamp: Utc.timestamp_opt(1000, 0).unwrap(),
            user_text: user.into(),
            assistant_text: assistant.into(),
            tool_calls: vec![ToolCallRecord {
                tool_name: "Bash".into(),
                input: json!({}),
                stdout: None,
                stderr: Some(stderr.into()),
                is_error: true,
                interrupted: false,
            }],
            uuid: "t1".into(),
        }
    }

    #[test]
    fn keyword_match_bug() {
        let turn = make_turn("what happened?", "The root cause was a null pointer.");
        let episodes = keyword_prefilter(&[turn], "s1");
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].category, EpisodeCategory::Bug);
        assert!(
            episodes[0]
                .matched_keywords
                .contains(&"root cause".to_owned())
        );
    }

    #[test]
    fn keyword_match_bug_error_in_stderr() {
        let turn = make_turn_with_stderr("help", "Let me check", "error: compilation failed");
        let episodes = keyword_prefilter(&[turn], "s1");
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].category, EpisodeCategory::Bug);
        assert!(episodes[0].matched_keywords.contains(&"error".to_owned()));
    }

    #[test]
    fn keyword_match_decision() {
        let turn = make_turn("which approach?", "I decided to use serde for parsing.");
        let episodes = keyword_prefilter(&[turn], "s1");
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].category, EpisodeCategory::Decision);
        assert!(episodes[0].matched_keywords.contains(&"decided".to_owned()));
    }

    #[test]
    fn keyword_match_convention() {
        let turn = make_turn("We should always use thiserror for libs", "Agreed.");
        let episodes = keyword_prefilter(&[turn], "s1");
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].category, EpisodeCategory::Convention);
        assert!(
            episodes[0]
                .matched_keywords
                .contains(&"always use".to_owned())
        );
    }

    #[test]
    fn keyword_match_learning() {
        let turn = make_turn("why?", "Turns out the borrow checker was right.");
        let episodes = keyword_prefilter(&[turn], "s1");
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].category, EpisodeCategory::Learning);
        assert!(
            episodes[0]
                .matched_keywords
                .contains(&"turns out".to_owned())
        );
    }

    #[test]
    fn keyword_no_match() {
        let turn = make_turn("Hello world", "Hello! How can I help?");
        let episodes = keyword_prefilter(&[turn], "s1");
        assert!(episodes.is_empty());
    }

    // -- Issue #82: sequential fallback pairing must not skip user entries --

    #[test]
    fn sequential_fallback_does_not_skip_intermediate_user() {
        // Scenario: User1, User2, Assistant2 — none UUID-linked.
        // The fallback should NOT pair User1 with Assistant2 (skipping User2).
        // User1 should remain unpaired; User2 should pair with Assistant2.
        let entries = vec![
            user_entry("u1", "", "First question", 100),
            user_entry("u2", "", "Second question", 101),
            assistant_entry("a2", "", "Answer to second", 102),
        ];
        let turns = reconstruct_turns(&entries);
        // Only User2→Assistant2 should pair; User1 has no adjacent assistant.
        assert_eq!(turns.len(), 1, "expected 1 turn, got {}", turns.len());
        assert_eq!(turns[0].user_text, "Second question");
        assert_eq!(turns[0].assistant_text, "Answer to second");
    }

    #[test]
    fn sequential_fallback_pairs_adjacent_correctly() {
        // User1, Assistant1, User2, Assistant2 — none UUID-linked.
        // Fallback should pair User1→Assistant1 and User2→Assistant2.
        let entries = vec![
            user_entry("u1", "", "First", 100),
            assistant_entry("a1", "", "Reply one", 101),
            user_entry("u2", "", "Second", 102),
            assistant_entry("a2", "", "Reply two", 103),
        ];
        let turns = reconstruct_turns(&entries);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].user_text, "First");
        assert_eq!(turns[0].assistant_text, "Reply one");
        assert_eq!(turns[1].user_text, "Second");
        assert_eq!(turns[1].assistant_text, "Reply two");
    }
}
