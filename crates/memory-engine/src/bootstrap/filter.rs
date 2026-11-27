//! Conversation turn reconstruction and `NeuroStack` keyword pre-filter.
//!
//! Transforms raw [`SessionEntry`] sequences into structured [`ConversationTurn`]s,
//! then applies rule-based keyword matching to surface [`CandidateEpisode`]s worth
//! promoting to long-term memory.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};

use super::parse::{ContentBlock, EntryType, INTERRUPTED, SessionEntry, records_an_interrupt};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A conversation turn: a prompt, the assistant entries that answer it, and their tool calls, each
/// paired with its result by `tool_use_id` (`reconstruct_turns`).
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EpisodeCategory {
    Bug,
    Decision,
    Convention,
    Learning,
}

impl EpisodeCategory {
    /// Stable lowercase string stored in fact metadata (the `category` key).
    ///
    /// This is the persisted on-wire form and must not change — it aligns with
    /// the adjacent `session_outcome` convention (`"failure"`/`"success"`/…)
    /// and decouples the stored schema from the Rust variant names, so a variant
    /// rename can no longer silently alter stored data (#334). Round-trips via
    /// `EpisodeCategory::from_str`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bug => "bug",
            Self::Decision => "decision",
            Self::Convention => "convention",
            Self::Learning => "learning",
        }
    }
}

impl std::str::FromStr for EpisodeCategory {
    type Err = ();

    /// Parse the stored [`EpisodeCategory::as_str`] form back into a variant.
    ///
    /// Returns `Err(())` for any unrecognized string.
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "bug" => Ok(Self::Bug),
            "decision" => Ok(Self::Decision),
            "convention" => Ok(Self::Convention),
            "learning" => Ok(Self::Learning),
            _ => Err(()),
        }
    }
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

/// The raw `tool_result` objects of `entry`'s `message.content`, as sent: the parsed
/// [`ContentBlock::ToolResult`] keeps only a lossy string of each, and
/// [`records_an_interrupt`] reads the raw value.
fn tool_results(entry: &SessionEntry) -> impl Iterator<Item = &serde_json::Value> {
    entry
        .message
        .as_ref()
        .and_then(|m| m.content.as_ref())
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|b| b.get("type").and_then(serde_json::Value::as_str) == Some("tool_result"))
}

/// Whether `entry` is a tool-result row: a user row holding a `tool_result` block (the rule of
/// the backfill reader and of the API), or carrying a `toolUseResult`. A result row is never a
/// prompt, so a block-less one still opens no turn.
fn is_result_row(entry: &SessionEntry) -> bool {
    matches!(entry.entry_type, EntryType::User)
        && (entry.tool_use_result.is_some() || tool_results(entry).next().is_some())
}

/// Whether `entry` takes part in a turn: a user or assistant entry, except a user entry that is
/// neither a result row nor a prompt (an `isMeta` or `isCompactSummary` row, or one whose text
/// starts with [`INTERRUPTED`]). A result row always does.
fn takes_part(entry: &SessionEntry) -> bool {
    let flagged = |flag: &Option<serde_json::Value>| *flag == Some(serde_json::Value::Bool(true));
    match entry.entry_type {
        EntryType::Assistant => true,
        EntryType::User => {
            is_result_row(entry)
                || !(flagged(&entry.is_meta)
                    || flagged(&entry.is_compact_summary)
                    || extract_text(entry).starts_with(INTERRUPTED))
        }
        _ => false,
    }
}

/// A genuine prompt: a user row that takes part and is no result row. A turn's walk stops at one.
fn is_prompt(entry: &SessionEntry) -> bool {
    matches!(entry.entry_type, EntryType::User) && takes_part(entry) && !is_result_row(entry)
}

/// Reconstruct [`ConversationTurn`]s from a raw `SessionEntry` sequence.
///
/// A turn is a prompt, the assistant entry that answers it (the first valid, unused one whose
/// `parentUuid` is the prompt's uuid, else the next assistant entry before the next prompt,
/// #82), and every entry linked beneath that assistant entry through `parentUuid` up to the
/// next prompt: Claude Code writes one content block per row, so one assistant message spans
/// several rows, with its tool results and any `system`/`attachment` rows in the chain.
///
/// Each `tool_result` block answers the call whose `tool_use` `id` its `tool_use_id` names, in
/// file order: a later call with the same id replaces the earlier one, and a result naming no
/// pending call answers nothing. This is the backfill reader's rule
/// (`memory-engine-mcp` `adapters::claude_code::transcript`, #1159); the two readers differ in
/// payload, not pairing: for a result without `toolUseResult` the backfill's result text falls
/// back to the block's text, while the bootstrap's `stdout` stays `None`. The rule is written
/// twice rather than shared, because the readers walk different types (`SessionEntry` here, a
/// JSON map there) and the backfill emits records as it walks; the backfill's
/// `both_readers_pair_results_by_tool_use_id` test holds both to one expected table instead.
///
/// Progress, queue-operation, file-history-snapshot, last-prompt, and unknown entries make no
/// prompt, nor do `isMeta` and `isCompactSummary` rows or rows whose text starts with
/// [`INTERRUPTED`]; a result row is never a prompt.
#[must_use]
pub fn reconstruct_turns(entries: &[SessionEntry]) -> Vec<ConversationTurn> {
    // The index in `entries` of every entry that takes part in a turn.
    let relevant: Vec<usize> = (0..entries.len())
        .filter(|&i| takes_part(&entries[i]))
        .collect();

    if relevant.is_empty() {
        return Vec::new();
    }

    let mut used: HashSet<&str> = HashSet::new();
    let mut turns: Vec<ConversationTurn> = Vec::new();

    // --- Pre-index for O(n) pairing (#406) ---
    // `entries` is attacker-controlled; every index below is built in one pass.
    //
    // `assistant_by_parent`: parent_uuid -> ALL assistants carrying it, in slice order. The
    // lookup picks the first VALID (uuid present) and not-yet-`used` candidate: keeping a
    // single entry would drop the turn when the first assistant for a parent is invalid
    // (uuid-less) or already used, even though a later valid one exists.
    //
    // `children`: parent_uuid -> every entry naming it, over ALL entries, since rows that
    // take no part in a turn still sit in its chain.
    let mut assistant_by_parent: HashMap<&str, Vec<usize>> = HashMap::new();
    for &i in &relevant {
        if matches!(entries[i].entry_type, EntryType::Assistant)
            && let Some(parent) = entries[i].parent_uuid.as_deref()
            && !parent.is_empty()
        {
            assistant_by_parent.entry(parent).or_default().push(i);
        }
    }
    let mut children: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, entry) in entries.iter().enumerate() {
        if let Some(parent) = entry.parent_uuid.as_deref()
            && !parent.is_empty()
        {
            children.entry(parent).or_default().push(i);
        }
    }
    // The turn each entry belongs to, by its index in `entries`.
    let mut owner: Vec<Option<usize>> = vec![None; entries.len()];
    let unused = |used: &HashSet<&str>, i: usize| {
        entries[i]
            .uuid
            .as_deref()
            .is_some_and(|u| !used.contains(u))
    };

    // --- UUID-linked pairing ---
    for &p in &relevant {
        let prompt = &entries[p];
        if !is_prompt(prompt) {
            continue;
        }
        let Some(user_uuid) = prompt.uuid.as_deref() else {
            continue;
        };
        if used.contains(user_uuid) {
            continue;
        }
        let assistant = assistant_by_parent
            .get(user_uuid)
            .and_then(|cands| cands.iter().copied().find(|&a| unused(&used, a)));
        if let Some(a) = assistant {
            used.insert(user_uuid);
            claim(entries, &children, &mut used, &mut owner, a, turns.len());
            turns.push(build_turn(prompt, &entries[a]));
        }
    }

    // --- Sequential fallback for unpaired entries ---
    // Only pair a user entry with the next assistant that appears *before* the next genuine
    // user entry in the sequence (issue #82).
    for (k, &p) in relevant.iter().enumerate() {
        let prompt = &entries[p];
        let user_uuid = prompt.uuid.as_deref().unwrap_or("");
        if is_prompt(prompt) && !user_uuid.is_empty() && !used.contains(user_uuid) {
            let assistant = relevant[k + 1..]
                .iter()
                .copied()
                .take_while(|&i| !is_prompt(&entries[i]))
                .find(|&i| {
                    matches!(entries[i].entry_type, EntryType::Assistant) && unused(&used, i)
                });
            if let Some(a) = assistant {
                used.insert(user_uuid);
                claim(entries, &children, &mut used, &mut owner, a, turns.len());
                turns.push(build_turn(prompt, &entries[a]));
            }
        }
    }

    // Filling runs before the sort: `owner` holds indices into `turns` as built.
    fill_turns(entries, &owner, &mut turns);
    turns.sort_by_key(|t| t.timestamp);
    turns
}

/// Give turn `turn` the assistant entry at `start` and every entry linked beneath it, up to the
/// next prompt. Each uuid joins at most one turn (a fork that repeats rows counts them once) and
/// its children are read once, on an explicit stack: O(n) on hostile input, no recursion (#406).
fn claim<'a>(
    entries: &'a [SessionEntry],
    children: &HashMap<&str, Vec<usize>>,
    used: &mut HashSet<&'a str>,
    owner: &mut [Option<usize>],
    start: usize,
    turn: usize,
) {
    owner[start] = Some(turn);
    let Some(uuid) = entries[start].uuid.as_deref() else {
        return;
    };
    used.insert(uuid);
    let mut stack = vec![uuid];
    while let Some(parent) = stack.pop() {
        for &i in children.get(parent).map_or(&[][..], Vec::as_slice) {
            let entry = &entries[i];
            if is_prompt(entry) {
                continue;
            }
            let Some(uuid) = entry.uuid.as_deref() else {
                continue;
            };
            if used.insert(uuid) {
                owner[i] = Some(turn);
                stack.push(uuid);
            }
        }
    }
}

/// Fill each turn's assistant text and calls, by one pass over `entries` in file order: an owned
/// assistant entry adds its visible text (never its thinking, and nothing when Claude Code wrote
/// it, see [`written_by_claude_code`]) and its `tool_use` blocks become its turn's calls; each
/// `tool_result` block fills the pending call its `tool_use_id` names (see [`reconstruct_turns`]).
fn fill_turns(entries: &[SessionEntry], owner: &[Option<usize>], turns: &mut [ConversationTurn]) {
    let mut calls: Vec<Vec<ToolCallRecord>> = turns.iter().map(|_| Vec::new()).collect();
    let mut texts: Vec<Vec<String>> = turns.iter().map(|_| Vec::new()).collect();
    // Every call with an id, owned or not, so that a result naming an unowned call answers
    // nothing rather than an older call of the same id.
    let mut pending: HashMap<String, Option<(usize, usize)>> = HashMap::new();
    for (i, entry) in entries.iter().enumerate() {
        match entry.entry_type {
            EntryType::Assistant => {
                let Some(content) = entry.message.as_ref().and_then(|m| m.content.as_ref()) else {
                    continue;
                };
                if let Some(turn) = owner[i]
                    && !written_by_claude_code(entry)
                {
                    let text = extract_text(entry);
                    if !text.is_empty() {
                        texts[turn].push(text);
                    }
                }
                for block in super::parse::parse_content_blocks(content) {
                    let ContentBlock::ToolUse { id, name, input } = block else {
                        continue;
                    };
                    let slot = owner[i].map(|turn| {
                        calls[turn].push(ToolCallRecord {
                            tool_name: name,
                            input,
                            stdout: None,
                            stderr: None,
                            is_error: false,
                            interrupted: false,
                        });
                        (turn, calls[turn].len() - 1)
                    });
                    if let Some(id) = id {
                        pending.insert(id, slot);
                    }
                }
            }
            EntryType::User => {
                for block in tool_results(entry) {
                    let Some(Some((turn, k))) = block
                        .get("tool_use_id")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|id| pending.remove(id))
                    else {
                        continue;
                    };
                    let call = &mut calls[turn][k];
                    let result = entry.tool_use_result.as_ref();
                    call.stdout = result.and_then(|r| r.stdout.clone());
                    call.stderr = result.and_then(|r| r.stderr.clone());
                    call.is_error = block.get("is_error") == Some(&serde_json::Value::Bool(true))
                        || result.and_then(|r| r.is_error).unwrap_or(false);
                    call.interrupted = result.is_some_and(|r| r.interrupted)
                        || block.get("content").is_some_and(records_an_interrupt);
                }
            }
            _ => {}
        }
    }
    for ((turn, calls), texts) in turns.iter_mut().zip(calls).zip(texts) {
        turn.tool_calls = calls;
        turn.assistant_text = texts.join("\n");
    }
}

/// Whether Claude Code wrote this assistant entry itself, not the model: a `<synthetic>` message
/// or an `isApiErrorMessage` row. Its text is no assistant text, as in the backfill reader.
fn written_by_claude_code(entry: &SessionEntry) -> bool {
    entry.is_api_error_message == Some(serde_json::Value::Bool(true))
        || entry
            .message
            .as_ref()
            .and_then(|m| m.model.as_ref())
            .and_then(serde_json::Value::as_str)
            == Some("<synthetic>")
}

/// Build a [`ConversationTurn`] from a prompt and the assistant entry that answers it; its
/// assistant text and calls are filled in by [`fill_turns`].
fn build_turn(user: &SessionEntry, assistant: &SessionEntry) -> ConversationTurn {
    let user_text = extract_text(user);

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
        assistant_text: String::new(),
        tool_calls: Vec::new(),
        uuid: user.uuid.clone().unwrap_or_default(),
    }
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
            ContentBlock::Text(t) => parts.push(t),
            ContentBlock::ToolUse { .. }
            | ContentBlock::ToolResult { .. }
            | ContentBlock::Other => {}
        }
    }
    parts.join("\n")
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
    use crate::bootstrap::parse::{
        EntryType, MessagePayload, SessionEntry, ToolUseResult, parse_session_file,
    };

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
                model: None,
                content: Some(json!([{"type": "text", "text": text}])),
            }),
            tool_use_result: None,
            is_meta: None,
            is_compact_summary: None,
            is_api_error_message: None,
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
                model: None,
                content: Some(json!([{"type": "text", "text": text}])),
            }),
            tool_use_result: None,
            is_meta: None,
            is_compact_summary: None,
            is_api_error_message: None,
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
                model: None,
                content: Some(json!([
                    {"type": "text", "text": text},
                    {"type": "tool_use", "id": format!("toolu_{uuid}"), "name": tool_name, "input": {"cmd": "ls"}}
                ])),
            }),
            tool_use_result: None,
            is_meta: None,
            is_compact_summary: None,
            is_api_error_message: None,
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
            is_meta: None,
            is_compact_summary: None,
            is_api_error_message: None,
            data: None,
        }
    }

    fn user_with_tool_result(
        uuid: &str,
        parent: &str,
        tool_use_id: &str,
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
                model: None,
                content: Some(json!([
                    {
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": stdout.or(stderr).unwrap_or(""),
                        "is_error": is_error
                    }
                ])),
            }),
            tool_use_result: Some(ToolUseResult {
                stdout: stdout.map(String::from),
                stderr: stderr.map(String::from),
                interrupted: false,
                is_error: Some(is_error),
            }),
            is_meta: None,
            is_compact_summary: None,
            is_api_error_message: None,
            data: None,
        }
    }

    fn user_with_interrupted_tool_result(
        uuid: &str,
        parent: &str,
        tool_use_id: &str,
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
                model: None,
                content: Some(json!([
                    {
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": "interrupted",
                        "is_error": false
                    }
                ])),
            }),
            tool_use_result: Some(ToolUseResult {
                stdout: None,
                stderr: Some("interrupted".into()),
                interrupted: true,
                is_error: Some(false),
            }),
            is_meta: None,
            is_compact_summary: None,
            is_api_error_message: None,
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
            user_with_tool_result("u2", "a1", "toolu_a1", Some("file.txt"), None, false, 102),
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
            user_with_interrupted_tool_result("u2", "a1", "toolu_a1", 102),
        ];
        let turns = reconstruct_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        assert!(turns[0].tool_calls[0].interrupted);
        assert!(!turns[0].tool_calls[0].is_error);
    }

    #[test]
    fn meta_summary_and_interrupt_lines_are_not_prompts() {
        let jsonl = [
            r#"{"type":"user","sessionId":"sess-1","timestamp":"2026-03-19T10:00:00Z","uuid":"u1","parentUuid":null,"message":{"role":"user","content":"run ls"}}"#,
            r#"{"type":"assistant","sessionId":"sess-1","timestamp":"2026-03-19T10:00:01Z","uuid":"a1","parentUuid":"u1","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"cmd":"ls"}}]}}"#,
            r#"{"type":"user","sessionId":"sess-1","timestamp":"2026-03-19T10:00:02Z","uuid":"u2","parentUuid":"a1","isMeta":true,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"tool output"}]},"toolUseResult":"ok, ls ran"}"#,
            r#"{"type":"user","sessionId":"sess-1","timestamp":"2026-03-19T10:01:00Z","uuid":"u3","parentUuid":null,"isMeta":true,"message":{"role":"user","content":"isMeta text"}}"#,
            r#"{"type":"assistant","sessionId":"sess-1","timestamp":"2026-03-19T10:01:01Z","uuid":"a2","parentUuid":"u3","message":{"role":"assistant","content":"reply to meta"}}"#,
            r#"{"type":"user","sessionId":"sess-1","timestamp":"2026-03-19T10:02:00Z","uuid":"u4","parentUuid":null,"isCompactSummary":true,"message":{"role":"user","content":"summary text"}}"#,
            r#"{"type":"assistant","sessionId":"sess-1","timestamp":"2026-03-19T10:02:01Z","uuid":"a3","parentUuid":"u4","message":{"role":"assistant","content":"reply to summary"}}"#,
            r#"{"type":"user","sessionId":"sess-1","timestamp":"2026-03-19T10:03:00Z","uuid":"u5","parentUuid":null,"message":{"role":"user","content":"[Request interrupted by user for tool use]"}}"#,
            r#"{"type":"assistant","sessionId":"sess-1","timestamp":"2026-03-19T10:03:01Z","uuid":"a4","parentUuid":"u5","message":{"role":"assistant","content":"reply to interrupt"}}"#,
        ]
        .join("\n");
        let (entries, malformed) = parse_session_file(jsonl.as_bytes(), 0, 0);
        assert_eq!(malformed, 0);
        let turns = reconstruct_turns(&entries);

        for turn in &turns {
            assert!(!turn.user_text.contains("isMeta text"));
            assert!(!turn.user_text.contains("summary text"));
            assert!(!turn.user_text.contains("[Request interrupted"));
        }
        assert!(turns.iter().any(|t| t.user_text == "run ls"));
        let turn = turns.iter().find(|t| t.user_text == "run ls").unwrap();
        // The tool-result row is flagged `isMeta` too: a row with a result is never dropped.
        assert_eq!(turn.tool_calls[0].stdout.as_deref(), Some("ok, ls ran"));
    }

    /// A turn's `assistant_text` is the non-empty visible text blocks of every
    /// assistant row the turn owns, joined by "\n": never a thinking block,
    /// never a row Claude Code writes itself (a `<synthetic>` model or an
    /// `isApiErrorMessage` row).
    #[test]
    fn a_turns_assistant_text_is_its_visible_text() {
        let jsonl = [
            json!({
                "type": "user",
                "sessionId": "sess-1",
                "timestamp": "2026-03-19T10:00:00Z",
                "uuid": "u1",
                "parentUuid": null,
                "message": {
                    "role": "user",
                    "content": [{"type": "text", "text": "Fix the parser."}],
                },
            }),
            json!({
                "type": "assistant",
                "sessionId": "sess-1",
                "timestamp": "2026-03-19T10:00:01Z",
                "uuid": "a0",
                "parentUuid": "u1",
                "message": {
                    "role": "assistant",
                    "content": [{"type": "thinking", "thinking": "PRIVATE-REASONING"}],
                },
            }),
            json!({
                "type": "assistant",
                "sessionId": "sess-1",
                "timestamp": "2026-03-19T10:00:02Z",
                "uuid": "a1",
                "parentUuid": "a0",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "first"}]},
            }),
            json!({
                "type": "assistant",
                "sessionId": "sess-1",
                "timestamp": "2026-03-19T10:00:03Z",
                "uuid": "a2",
                "parentUuid": "a1",
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "tool_use", "id": "t1", "name": "Bash", "input": {}},
                    ],
                },
            }),
            json!({
                "type": "user",
                "sessionId": "sess-1",
                "timestamp": "2026-03-19T10:00:04Z",
                "uuid": "r1",
                "parentUuid": "a2",
                "message": {
                    "role": "user",
                    "content": [
                        {"type": "tool_result", "tool_use_id": "t1", "content": "ok"},
                    ],
                },
                "toolUseResult": "ok",
            }),
            json!({
                "type": "assistant",
                "sessionId": "sess-1",
                "timestamp": "2026-03-19T10:00:05Z",
                "uuid": "a3",
                "parentUuid": "r1",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "done"}]},
            }),
            json!({
                "type": "assistant",
                "sessionId": "sess-1",
                "timestamp": "2026-03-19T10:00:06Z",
                "uuid": "a4",
                "parentUuid": "a3",
                "message": {
                    "role": "assistant",
                    "model": "<synthetic>",
                    "content": [{"type": "text", "text": "No response requested."}],
                },
            }),
            json!({
                "type": "assistant",
                "sessionId": "sess-1",
                "timestamp": "2026-03-19T10:00:07Z",
                "uuid": "a5",
                "parentUuid": "a4",
                "isApiErrorMessage": true,
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "API Error: overloaded"}],
                },
            }),
        ]
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
        let (entries, malformed) = parse_session_file(jsonl.as_bytes(), 0, 0);
        assert_eq!(malformed, 0);
        let turns = reconstruct_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].assistant_text, "first\ndone");
    }

    /// The reference transcript's interrupt (README line 21), `isMeta` caveat (line 30) and
    /// compaction summary (line 32) reach no turn as a prompt, while its real prompts do.
    #[test]
    fn the_reference_transcript_makes_no_prompt_of_its_non_prompts() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../memory-engine-mcp/tests/fixtures/claude-code-transcripts/-work-fixture-p/",
            "52c6c7b7-c890-43ec-81a5-a1cf99d3cc84.jsonl"
        );
        let bytes = std::fs::read(path).unwrap();
        let (entries, _) = parse_session_file(bytes.as_slice(), 0, 0);
        let turns = reconstruct_turns(&entries);
        assert!(
            turns.iter().any(|t| !t.user_text.is_empty()),
            "its prompts make turns"
        );
        for turn in &turns {
            for not_a_prompt in [
                INTERRUPTED,
                "Caveat: The messages below",
                "This session is being continued",
            ] {
                assert!(
                    !turn.user_text.contains(not_a_prompt),
                    "{not_a_prompt}: {}",
                    turn.user_text
                );
            }
        }
    }

    /// Every call of the reference transcript pairs with its result by id,
    /// including across the assistant rows linked under each prompt.
    #[test]
    fn the_reference_transcript_pairs_every_call_by_id() {
        fn turn_at<'t>(
            line: usize,
            lines: &[String],
            turns: &'t [ConversationTurn],
        ) -> &'t ConversationTurn {
            let text = &lines[line - 1];
            let row: serde_json::Value = serde_json::from_str(text).unwrap();
            let uuid = row["uuid"].as_str().unwrap();
            turns
                .iter()
                .find(|t| t.uuid == uuid)
                .unwrap_or_else(|| panic!("no turn for line {line} uuid {uuid}"))
        }

        fn calls_at(
            line: usize,
            lines: &[String],
            turns: &[ConversationTurn],
        ) -> Vec<(String, bool)> {
            turn_at(line, lines, turns)
                .tool_calls
                .iter()
                .map(|c| (c.tool_name.clone(), c.interrupted))
                .collect()
        }

        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../memory-engine-mcp/tests/fixtures/claude-code-transcripts/-work-fixture-p/",
            "52c6c7b7-c890-43ec-81a5-a1cf99d3cc84.jsonl"
        );
        let bytes = std::fs::read(path).unwrap();
        let (entries, _) = parse_session_file(bytes.as_slice(), 0, 0);
        let turns = reconstruct_turns(&entries);
        assert_eq!(turns.len(), 8);

        let lines: Vec<String> = std::str::from_utf8(&bytes)
            .unwrap()
            .lines()
            .map(String::from)
            .collect();

        assert_eq!(
            calls_at(2, &lines, &turns),
            vec![
                ("Bash".into(), false),
                ("Read".into(), false),
                ("Read".into(), false),
                ("mcp__me__recall".into(), false)
            ]
        );
        let first = turn_at(2, &lines, &turns);
        assert_eq!(
            first.tool_calls[1].stdout.as_deref(),
            Some("Error: File does not exist.")
        );

        assert_eq!(calls_at(18, &lines, &turns), vec![("Bash".into(), true)]);
        assert_eq!(calls_at(22, &lines, &turns), vec![("Grep".into(), false)]);
        assert_eq!(calls_at(25, &lines, &turns), Vec::<(String, bool)>::new());
        assert_eq!(calls_at(27, &lines, &turns), Vec::<(String, bool)>::new());
        assert_eq!(calls_at(34, &lines, &turns), Vec::<(String, bool)>::new());
        assert_eq!(calls_at(43, &lines, &turns), vec![("Task".into(), false)]);
        assert_eq!(calls_at(47, &lines, &turns), vec![("Bash".into(), false)]);
        let last = turn_at(47, &lines, &turns);
        assert_eq!(last.tool_calls[0].stdout, None);

        // The turn's assistant_text is its visible text: line 14's text block
        // shows up, while no thinking block of any row ever does.
        assert!(
            turn_at(2, &lines, &turns)
                .assistant_text
                .contains("The help text lists it.")
        );
        let thinking: Vec<String> = lines
            .iter()
            .filter_map(|text| serde_json::from_str::<serde_json::Value>(text).ok())
            .flat_map(|row| {
                row["message"]["content"]
                    .as_array()
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter(|b| b["type"] == "thinking")
                            .filter_map(|b| b["thinking"].as_str().filter(|t| !t.is_empty()))
                            .map(String::from)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            })
            .collect();
        assert!(!thinking.is_empty(), "the fixture carries thinking blocks");
        for turn in &turns {
            for thought in &thinking {
                assert!(
                    !turn.assistant_text.contains(thought),
                    "thinking leaked into assistant_text: {thought}"
                );
            }
        }
    }

    #[test]
    fn reconstruct_interrupted_false_by_default() {
        let entries = vec![
            user_entry("u1", "", "run ls", 100),
            assistant_with_tool("a1", "u1", "Sure", "Bash", 101),
            user_with_tool_result("u2", "a1", "toolu_a1", Some("ok"), None, false, 102),
        ];
        let turns = reconstruct_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert!(!turns[0].tool_calls[0].interrupted);
    }

    /// Results pair by `tool_use_id`, not position (#1159).
    #[test]
    fn reconstruct_pairs_results_by_tool_use_id() {
        let assistant = SessionEntry {
            entry_type: EntryType::Assistant,
            session_id: Some("sess-1".into()),
            timestamp: ts(101).map(|t| t.to_rfc3339()),
            uuid: opt("a1"),
            parent_uuid: opt("u1"),
            cwd: None,
            git_branch: None,
            message: Some(MessagePayload {
                role: Some("assistant".into()),
                model: None,
                content: Some(json!([
                    {"type": "tool_use", "id": "t-first", "name": "Bash", "input": {"cmd": "first"}},
                    {"type": "tool_use", "id": "t-second", "name": "Read", "input": {"path": "second"}}
                ])),
            }),
            tool_use_result: None,
            is_meta: None,
            is_compact_summary: None,
            is_api_error_message: None,
            data: None,
        };
        let entries = vec![
            user_entry("u1", "", "do two things", 100),
            assistant,
            user_with_tool_result("u2", "a1", "t-second", Some("out-second"), None, false, 102),
            user_with_tool_result("u3", "a1", "t-first", Some("out-first"), None, false, 103),
        ];
        let turns = reconstruct_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 2);
        assert_eq!(turns[0].tool_calls[0].tool_name, "Bash");
        assert_eq!(turns[0].tool_calls[0].stdout.as_deref(), Some("out-first"));
        assert_eq!(turns[0].tool_calls[1].tool_name, "Read");
        assert_eq!(turns[0].tool_calls[1].stdout.as_deref(), Some("out-second"));
    }

    /// Regression (#406 index): when the first assistant carrying a `parent_uuid`
    /// is invalid (uuid-less), the per-parent index must still find a LATER valid
    /// assistant for the same parent, matching the old linear `.find()`. A single
    /// first-only index dropped the turn here.
    #[test]
    fn parent_index_skips_invalid_assistant_to_later_valid_one() {
        let entries = vec![
            user_entry("u1", "", "please fix it", 0),
            // Invalid assistant for u1 first (no uuid) — must NOT shadow the valid one.
            assistant_entry("", "u1", "corrupt partial row", 1),
            assistant_entry("a1", "u1", "the real reply", 2),
        ];
        let turns = reconstruct_turns(&entries);
        assert_eq!(turns.len(), 1, "the valid later assistant must still pair");
        assert!(turns[0].assistant_text.contains("the real reply"));
    }

    /// #406: a quadratic or recursive walk of hostile link graphs fails it.
    #[test]
    fn reconstruct_turns_stays_linear_on_hostile_links() {
        use std::time::{Duration, Instant};

        // 100_000 assistant rows all sharing one uuid and one parent.
        let mut dup_entries: Vec<SessionEntry> = vec![user_entry("u0", "", "prompt", 0)];
        for i in 0..100_000_i64 {
            dup_entries.push(assistant_with_tool("dup", "u0", "", "Bash", i));
        }
        let start = Instant::now();
        let _ = reconstruct_turns(&dup_entries);
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "duplicate-uuid fan-out must stay linear"
        );

        // A 100_000-deep chain: prompt, then assistant/tool-result rows linked one by one.
        let mut chain: Vec<SessionEntry> = vec![user_entry("p0", "", "prompt", 0)];
        let mut prev = "p0".to_string();
        for i in 0..50_000_i64 {
            let a = format!("a{i}");
            let r = format!("r{i}");
            chain.push(assistant_with_tool(&a, &prev, "", "Bash", i));
            chain.push(user_with_tool_result(
                &r,
                &a,
                &format!("toolu_a{i}"),
                Some("ok"),
                None,
                false,
                i,
            ));
            prev = r;
        }
        let start = Instant::now();
        let _ = reconstruct_turns(&chain);
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "a 100_000-deep parent chain must stay linear"
        );

        // 50_000 prompts sharing one uuid, each with its own reply.
        let mut prompts: Vec<SessionEntry> = Vec::new();
        for i in 0..50_000_i64 {
            prompts.push(user_entry("p", "", "prompt", i));
            prompts.push(assistant_entry(&format!("a{i}"), "p", "reply", i));
        }
        let start = Instant::now();
        let _ = reconstruct_turns(&prompts);
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "prompts sharing a uuid must stay linear"
        );
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
    fn keyword_match_bug_error_in_assistant_with_context() {
        // #423: the second branch of `check_error_keyword` fires when "error"
        // appears in assistant text AND a bug-context word (fix/bug/issue) is
        // also present — with no tool stderr. Previously only the stderr branch
        // was covered.
        let turn = make_turn("", "There is an error in the fix, see below.");
        let episodes = keyword_prefilter(&[turn], "s1");
        let bug = episodes
            .iter()
            .find(|e| e.category == EpisodeCategory::Bug)
            .expect("assistant 'error' + 'fix' context must produce a Bug episode");
        assert!(
            bug.matched_keywords.contains(&"error".to_owned()),
            "the special-cased 'error' keyword must be recorded"
        );
    }

    #[test]
    fn keyword_no_bug_error_without_context() {
        // #423 negative: "error" in assistant text with NO bug-context word and
        // no other bug keyword (root cause/traceback/exception/fix) and no
        // stderr must NOT classify as Bug — `check_error_keyword` returns false.
        let turn = make_turn("", "An error occurred during compilation overnight.");
        let episodes = keyword_prefilter(&[turn], "s1");
        assert!(
            !episodes.iter().any(|e| e.category == EpisodeCategory::Bug),
            "bare 'error' without bug context must not be a Bug episode"
        );
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
