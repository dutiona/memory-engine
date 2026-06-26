//! Generic activity filtering configuration and decision logic.
//!
//! The core engine provides pattern-matching infrastructure. Concrete
//! ignore/promote policies are consumer-supplied (e.g., the MCP adapter
//! provides Claude Code-specific heuristics).

use crate::types::FactType;

/// Configuration for server-side activity filtering.
///
/// Consumers supply tool-name patterns for ignore and promote rules.
/// The engine applies these generically — no built-in tool-name knowledge.
///
/// # Pattern normalization invariant
///
/// `ignore_patterns` and `promote_patterns` are stored **already lowercased**.
/// Matching is case-insensitive substring, and `apply_filter` is called once
/// per tool-activity event (a hot path); normalizing at construction means the
/// per-call comparison never has to allocate a lowercased copy of each pattern.
/// The fields are private to keep that invariant unbreakable — build via
/// [`ActivityFilterConfig::new`] / [`ActivityFilterConfig::default`], read via
/// [`ActivityFilterConfig::ignore_patterns`] /
/// [`ActivityFilterConfig::promote_patterns`].
#[derive(Debug, Clone)]
pub struct ActivityFilterConfig {
    /// Dedup window in seconds. Activities with the same
    /// `(session_id, tool_name, args_hash, outcome_class)` within this
    /// window are collapsed. Default: 300 (5 minutes).
    pub dedup_window_secs: i64,

    /// Tool name patterns to drop before storage (stored lowercased).
    /// Matching is case-insensitive substring.
    ignore_patterns: Vec<String>,

    /// Tool name patterns that auto-promote to facts when newly inserted
    /// (not when deduplicated; stored lowercased).
    promote_patterns: Vec<String>,
}

impl Default for ActivityFilterConfig {
    fn default() -> Self {
        Self {
            dedup_window_secs: 300,
            ignore_patterns: Vec::new(),
            promote_patterns: Vec::new(),
        }
    }
}

impl ActivityFilterConfig {
    /// Construct a config, normalizing all patterns to lowercase up front.
    ///
    /// This is the only way to populate the pattern lists, so the
    /// "patterns are lowercase" invariant always holds — `apply_filter` can
    /// then substring-match without allocating per call.
    #[must_use]
    pub fn new(
        dedup_window_secs: i64,
        ignore_patterns: impl IntoIterator<Item = impl Into<String>>,
        promote_patterns: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            dedup_window_secs,
            ignore_patterns: ignore_patterns
                .into_iter()
                .map(|p| Into::<String>::into(p).to_lowercase())
                .collect(),
            promote_patterns: promote_patterns
                .into_iter()
                .map(|p| Into::<String>::into(p).to_lowercase())
                .collect(),
        }
    }

    /// The (already-lowercased) ignore patterns.
    #[must_use]
    pub fn ignore_patterns(&self) -> &[String] {
        &self.ignore_patterns
    }

    /// The (already-lowercased) promote patterns.
    #[must_use]
    pub fn promote_patterns(&self) -> &[String] {
        &self.promote_patterns
    }
}

/// Decision from the filter pipeline (before store-level dedup).
#[derive(Debug, Clone, PartialEq)]
pub enum ActivityFilterDecision {
    /// Drop the activity — do not persist.
    Ignore,
    /// Persist as a normal activity record.
    Record,
    /// Persist and promote to a fact (only if not deduplicated).
    Promote(PromoteAction),
}

/// Parameters for promoting an activity to a fact.
#[derive(Debug, Clone, PartialEq)]
pub struct PromoteAction {
    pub fact_content: String,
    pub fact_type: FactType,
    pub importance: f64,
}

/// Apply ignore and promote rules to an incoming activity.
///
/// Dedup is NOT handled here — it happens at the store layer via
/// `ActivityStore::insert_or_dedup`.
///
/// Evaluation order: ignore first (short-circuit), then promote, else record.
#[must_use]
pub fn apply_filter(
    tool_name: &str,
    _args: &serde_json::Value,
    result: Option<&str>,
    config: &ActivityFilterConfig,
) -> ActivityFilterDecision {
    let tool_lower = tool_name.to_lowercase();

    // Ignore check (case-insensitive substring match). Patterns are stored
    // pre-lowercased (see `ActivityFilterConfig`), so no per-call allocation.
    for pattern in &config.ignore_patterns {
        if tool_lower.contains(pattern.as_str()) {
            return ActivityFilterDecision::Ignore;
        }
    }

    // Promote check (case-insensitive substring match; patterns pre-lowercased).
    for pattern in &config.promote_patterns {
        if tool_lower.contains(pattern.as_str()) {
            let content = format_promote_content(tool_name, result);
            return ActivityFilterDecision::Promote(PromoteAction {
                fact_content: content,
                fact_type: FactType::Episodic,
                importance: 0.7,
            });
        }
    }

    ActivityFilterDecision::Record
}

fn format_promote_content(tool_name: &str, result: Option<&str>) -> String {
    match result {
        Some(r) if !r.is_empty() => {
            let truncated = if r.len() > 200 {
                // Find the last char boundary at or before 200 bytes.
                // `str::floor_char_boundary` is unstable until Rust 1.91; the
                // crate MSRV is 1.85, so walk back manually via the
                // long-stable `is_char_boundary`.
                let mut end = 200;
                while !r.is_char_boundary(end) {
                    end -= 1;
                }
                &r[..end]
            } else {
                r
            };
            format!("[{tool_name}] {truncated}")
        }
        _ => format!("[{tool_name}] (no result summary)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_ignore(patterns: &[&str]) -> ActivityFilterConfig {
        ActivityFilterConfig::new(300, patterns.iter().copied(), [] as [&str; 0])
    }

    fn config_with_promote(patterns: &[&str]) -> ActivityFilterConfig {
        ActivityFilterConfig::new(300, [] as [&str; 0], patterns.iter().copied())
    }

    #[test]
    fn default_config_records_everything() {
        let config = ActivityFilterConfig::default();
        let decision = apply_filter("Read", &serde_json::json!({}), None, &config);
        assert_eq!(decision, ActivityFilterDecision::Record);
    }

    #[test]
    fn ignore_pattern_drops_matching_tool() {
        let config = config_with_ignore(&["format", "lint"]);
        assert_eq!(
            apply_filter("FormatCode", &serde_json::json!({}), None, &config),
            ActivityFilterDecision::Ignore
        );
        assert_eq!(
            apply_filter("eslint_fix", &serde_json::json!({}), None, &config),
            ActivityFilterDecision::Ignore
        );
        // Non-matching passes through.
        assert_eq!(
            apply_filter("Read", &serde_json::json!({}), None, &config),
            ActivityFilterDecision::Record
        );
    }

    #[test]
    fn ignore_is_case_insensitive() {
        let config = config_with_ignore(&["Format"]);
        assert_eq!(
            apply_filter("formatcode", &serde_json::json!({}), None, &config),
            ActivityFilterDecision::Ignore
        );
    }

    #[test]
    fn promote_pattern_triggers_promotion() {
        let config = config_with_promote(&["commit"]);
        let decision = apply_filter(
            "git_commit",
            &serde_json::json!({}),
            Some("feat: add feature"),
            &config,
        );
        match decision {
            ActivityFilterDecision::Promote(action) => {
                assert!(action.fact_content.contains("git_commit"));
                assert!(action.fact_content.contains("feat: add feature"));
                assert_eq!(action.fact_type, FactType::Episodic);
                assert!((action.importance - 0.7).abs() < f64::EPSILON);
            }
            other => panic!("expected Promote, got {other:?}"),
        }
    }

    #[test]
    fn ignore_takes_precedence_over_promote() {
        let config = ActivityFilterConfig::new(300, ["lint".to_string()], ["lint".to_string()]);
        assert_eq!(
            apply_filter("lint_check", &serde_json::json!({}), None, &config),
            ActivityFilterDecision::Ignore
        );
    }

    #[test]
    fn new_normalizes_patterns_to_lowercase() {
        // The constructor must lowercase patterns up front so the hot-path
        // `apply_filter` never has to re-allocate a lowercased copy per call.
        let config = ActivityFilterConfig::new(
            300,
            ["Format".to_string(), "LINT".to_string()],
            ["Git_Commit".to_string()],
        );
        assert_eq!(config.ignore_patterns(), ["format", "lint"]);
        assert_eq!(config.promote_patterns(), ["git_commit"]);
        // Mixed-case patterns still match mixed-case tool names.
        assert_eq!(
            apply_filter("FormatCode", &serde_json::json!({}), None, &config),
            ActivityFilterDecision::Ignore
        );
    }

    #[test]
    fn promote_truncates_long_results() {
        let config = config_with_promote(&["test"]);
        let long_result = "x".repeat(500);
        let decision = apply_filter(
            "test_runner",
            &serde_json::json!({}),
            Some(&long_result),
            &config,
        );
        if let ActivityFilterDecision::Promote(action) = decision {
            assert!(action.fact_content.len() < 250);
        } else {
            panic!("expected Promote");
        }
    }
}
