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
#[derive(Debug, Clone)]
pub struct ActivityFilterConfig {
    /// Dedup window in seconds. Activities with the same
    /// `(session_id, tool_name, args_hash, outcome_class)` within this
    /// window are collapsed. Default: 300 (5 minutes).
    pub dedup_window_secs: i64,

    /// Tool name patterns to drop before storage.
    /// Matching is case-insensitive substring.
    pub ignore_patterns: Vec<String>,

    /// Tool name patterns that auto-promote to facts when newly inserted
    /// (not when deduplicated).
    pub promote_patterns: Vec<String>,
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
pub fn apply_filter(
    tool_name: &str,
    _args: &serde_json::Value,
    result: Option<&str>,
    config: &ActivityFilterConfig,
) -> ActivityFilterDecision {
    let tool_lower = tool_name.to_lowercase();

    // Ignore check (case-insensitive substring match).
    for pattern in &config.ignore_patterns {
        if tool_lower.contains(&pattern.to_lowercase()) {
            return ActivityFilterDecision::Ignore;
        }
    }

    // Promote check (case-insensitive substring match).
    for pattern in &config.promote_patterns {
        if tool_lower.contains(&pattern.to_lowercase()) {
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
            let truncated = if r.len() > 200 { &r[..200] } else { r };
            format!("[{tool_name}] {truncated}")
        }
        _ => format!("[{tool_name}] (no result summary)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_ignore(patterns: &[&str]) -> ActivityFilterConfig {
        ActivityFilterConfig {
            ignore_patterns: patterns.iter().map(|s| (*s).to_string()).collect(),
            ..Default::default()
        }
    }

    fn config_with_promote(patterns: &[&str]) -> ActivityFilterConfig {
        ActivityFilterConfig {
            promote_patterns: patterns.iter().map(|s| (*s).to_string()).collect(),
            ..Default::default()
        }
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
        let config = ActivityFilterConfig {
            ignore_patterns: vec!["lint".into()],
            promote_patterns: vec!["lint".into()],
            ..Default::default()
        };
        assert_eq!(
            apply_filter("lint_check", &serde_json::json!({}), None, &config),
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
