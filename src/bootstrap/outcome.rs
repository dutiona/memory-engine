//! Heuristic session outcome classification.
//!
//! Analyses [`ConversationTurn`](super::filter::ConversationTurn) sequences to
//! produce a best-effort [`SessionOutcome`] label together with the
//! [`OutcomeSignals`] that motivated the classification.
//!
//! All keyword matching is case-insensitive and English-only.

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Heuristic outcome of a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionOutcome {
    Success,
    Failure,
    Indeterminate,
}

/// Heuristic signals used to classify the session.
#[derive(Debug, Clone, Default)]
pub struct OutcomeSignals {
    pub has_commit: bool,
    pub tests_passed: TestOutcome,
    pub has_error_loops: bool,
    /// True when the last tool call in the session was interrupted, as reported
    /// by `ToolUseResult::interrupted` propagated through `filter.rs`.
    pub was_interrupted: bool,
    pub final_user_sentiment: Option<Sentiment>,
}

/// Whether a session's tests were observed passing.
///
/// An enum (not a `bool`) keeps [`OutcomeSignals`] within clippy's bool budget
/// and names the negative case honestly: the heuristic only detects a *passing*
/// signal, so its absence is `NotObserved`, not a definite failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TestOutcome {
    /// A passing-test signal (`test result: ok` / `passed`) was detected.
    Passed,
    /// No passing-test signal observed — the session may have failed its tests
    /// or never run any. Mirrors the original boolean's `false`, which likewise
    /// did not distinguish the two.
    #[default]
    NotObserved,
}

/// Coarse user-sentiment bucket derived from keyword matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sentiment {
    Positive,
    Negative,
    Neutral,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Classify a session's outcome from its conversation turns.
///
/// # Heuristics (best-effort, English-only)
///
/// | Signal | Detection |
/// |---|---|
/// | `has_commit` | `"git commit"` in tool input **or** commit-like patterns (`[main`, `[master`, 7+ hex chars) in stdout |
/// | `tests_passed` | `"test result: ok"` / `"passed"` with test-runner patterns in stdout |
/// | `has_error_loops` | Same stderr prefix (first 100 chars) in 3+ consecutive tool calls |
/// | `was_interrupted` | Last `ToolCallRecord` has `interrupted == true` (from `ToolUseResult::interrupted`) |
/// | `final_user_sentiment` | Keyword match on the last user message |
///
/// # Classification
///
/// - **Success**: `(has_commit && !has_error_loops) || (tests_passed && !was_interrupted)`
/// - **Failure**: `has_error_loops || was_interrupted || (negative sentiment && !has_commit)`
/// - **Indeterminate**: everything else
#[must_use]
pub fn classify_outcome(
    turns: &[super::filter::ConversationTurn],
) -> (SessionOutcome, OutcomeSignals) {
    let has_commit = detect_commit(turns);
    let tests_passed = detect_tests_passed(turns);
    let has_error_loops = detect_error_loops(turns);
    let was_interrupted = detect_interrupted(turns);
    let final_user_sentiment = detect_sentiment(turns);

    let signals = OutcomeSignals {
        has_commit,
        tests_passed: if tests_passed {
            TestOutcome::Passed
        } else {
            TestOutcome::NotObserved
        },
        has_error_loops,
        was_interrupted,
        final_user_sentiment: Some(final_user_sentiment),
    };

    let outcome = if (has_commit && !has_error_loops) || (tests_passed && !was_interrupted) {
        SessionOutcome::Success
    } else if has_error_loops
        || was_interrupted
        || (final_user_sentiment == Sentiment::Negative && !has_commit)
    {
        SessionOutcome::Failure
    } else {
        SessionOutcome::Indeterminate
    };

    (outcome, signals)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Extract the `"command"` string from a `serde_json::Value` tool input.
fn input_command(input: &serde_json::Value) -> Option<&str> {
    input.get("command").and_then(serde_json::Value::as_str)
}

/// True when any tool call looks like it triggered or reported a git commit.
fn detect_commit(turns: &[super::filter::ConversationTurn]) -> bool {
    for turn in turns {
        for tc in &turn.tool_calls {
            let is_bash = tc.tool_name.eq_ignore_ascii_case("bash");

            if is_bash
                && let Some(cmd) = input_command(&tc.input)
                && cmd.to_lowercase().contains("git commit")
            {
                return true;
            }

            if let Some(stdout) = &tc.stdout {
                let lower = stdout.to_lowercase();
                if lower.contains("[main") || lower.contains("[master") || has_short_sha(stdout) {
                    return true;
                }
            }
        }
    }
    false
}

/// Check whether `s` contains a 7+ hex-character run (commit SHA heuristic).
fn has_short_sha(s: &str) -> bool {
    let mut hex_run = 0u32;
    for ch in s.chars() {
        if ch.is_ascii_hexdigit() {
            hex_run += 1;
            if hex_run >= 7 {
                return true;
            }
        } else {
            hex_run = 0;
        }
    }
    false
}

/// True when any tool call stdout looks like a passing test suite.
fn detect_tests_passed(turns: &[super::filter::ConversationTurn]) -> bool {
    for turn in turns {
        for tc in &turn.tool_calls {
            if let Some(stdout) = &tc.stdout {
                let lower = stdout.to_lowercase();
                if lower.contains("test result: ok") {
                    return true;
                }
                if lower.contains("passed") && (lower.contains("test") || lower.contains("tests")) {
                    return true;
                }
                // A `pytest` + `passed` line is already caught by the generic
                // `passed && test` branch above (`pytest` contains `test`), so this
                // branch only adds the pytest-specific ` ok` summary form.
                if lower.contains("pytest") && lower.contains(" ok") {
                    return true;
                }
            }
        }
    }
    false
}

/// True when the same non-empty stderr prefix appears in 3+ consecutive tool
/// calls (across turns).
fn detect_error_loops(turns: &[super::filter::ConversationTurn]) -> bool {
    let stderr_prefixes: Vec<Option<String>> = turns
        .iter()
        .flat_map(|t| &t.tool_calls)
        .map(|tc| {
            tc.stderr.as_ref().and_then(|s| {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    let prefix: String = trimmed.chars().take(100).collect();
                    Some(prefix)
                }
            })
        })
        .collect();

    let mut consecutive = 1u32;
    for window in stderr_prefixes.windows(2) {
        match (&window[0], &window[1]) {
            (Some(a), Some(b)) if a == b => {
                consecutive += 1;
                if consecutive >= 3 {
                    return true;
                }
            }
            _ => {
                consecutive = 1;
            }
        }
    }
    false
}

/// True when the very last tool call was interrupted.
fn detect_interrupted(turns: &[super::filter::ConversationTurn]) -> bool {
    turns
        .iter()
        .rev()
        .flat_map(|t| t.tool_calls.iter().rev())
        .next()
        .is_some_and(|tc| tc.interrupted)
}

/// Keyword-based sentiment of the final user message.
fn detect_sentiment(turns: &[super::filter::ConversationTurn]) -> Sentiment {
    const POSITIVE: &[&str] = &[
        "thanks", "great", "perfect", "works", "awesome", "good", "nice",
    ];
    const NEGATIVE: &[&str] = &[
        "wrong",
        "broken",
        "revert",
        "undo",
        "doesn't work",
        "failed",
    ];

    let Some(last_user_text) = turns.last().map(|t| t.user_text.to_lowercase()) else {
        return Sentiment::Neutral;
    };

    if POSITIVE.iter().any(|kw| last_user_text.contains(kw)) {
        return Sentiment::Positive;
    }
    if NEGATIVE.iter().any(|kw| last_user_text.contains(kw)) {
        return Sentiment::Negative;
    }
    Sentiment::Neutral
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap::filter::{ConversationTurn, ToolCallRecord};
    use chrono::Utc;

    fn make_turn(
        user_text: &str,
        assistant_text: &str,
        tool_calls: Vec<ToolCallRecord>,
    ) -> ConversationTurn {
        ConversationTurn {
            timestamp: Utc::now(),
            user_text: user_text.into(),
            assistant_text: assistant_text.into(),
            tool_calls,
            uuid: "test-uuid".into(),
        }
    }

    fn make_tool_call(
        stdout: Option<&str>,
        stderr: Option<&str>,
        is_error: bool,
    ) -> ToolCallRecord {
        ToolCallRecord {
            tool_name: "Bash".into(),
            input: serde_json::json!({"command": "test"}),
            stdout: stdout.map(Into::into),
            stderr: stderr.map(Into::into),
            is_error,
            interrupted: false,
        }
    }

    #[test]
    fn classify_success_commit() {
        let turns = vec![make_turn(
            "commit this",
            "done",
            vec![make_tool_call(
                Some("[main abc1234] feat: add feature\n 1 file changed"),
                None,
                false,
            )],
        )];
        let (outcome, signals) = classify_outcome(&turns);
        assert_eq!(outcome, SessionOutcome::Success);
        assert!(signals.has_commit);
    }

    #[test]
    fn classify_success_tests_pass() {
        let turns = vec![make_turn(
            "run tests",
            "running",
            vec![make_tool_call(
                Some("test result: ok. 42 passed; 0 failed"),
                None,
                false,
            )],
        )];
        let (outcome, signals) = classify_outcome(&turns);
        assert_eq!(outcome, SessionOutcome::Success);
        assert_eq!(signals.tests_passed, TestOutcome::Passed);
    }

    #[test]
    fn classify_failure_error_loop_with_interruption() {
        let err = "error[E0308]: mismatched types";
        let mut last_call = make_tool_call(None, Some(err), true);
        last_call.interrupted = true;
        let turns = vec![
            make_turn(
                "fix it",
                "trying",
                vec![make_tool_call(None, Some(err), true)],
            ),
            make_turn(
                "fix it",
                "trying",
                vec![make_tool_call(None, Some(err), true)],
            ),
            make_turn("fix it", "trying", vec![last_call]),
        ];
        let (outcome, signals) = classify_outcome(&turns);
        assert_eq!(outcome, SessionOutcome::Failure);
        assert!(signals.has_error_loops);
        assert!(signals.was_interrupted);
    }

    #[test]
    fn error_loop_without_interruption_is_failure() {
        let err = "error[E0308]: mismatched types";
        let turns = vec![
            make_turn(
                "fix it",
                "trying",
                vec![make_tool_call(None, Some(err), true)],
            ),
            make_turn(
                "fix it",
                "trying",
                vec![make_tool_call(None, Some(err), true)],
            ),
            make_turn(
                "fix it",
                "trying",
                vec![make_tool_call(None, Some(err), true)],
            ),
        ];
        let (outcome, signals) = classify_outcome(&turns);
        // Error loops alone are sufficient for Failure classification
        assert_eq!(outcome, SessionOutcome::Failure);
        assert!(signals.has_error_loops);
        assert!(!signals.was_interrupted);
    }

    #[test]
    fn classify_failure_negative_sentiment() {
        let turns = vec![make_turn(
            "this is broken, revert it",
            "ok",
            vec![make_tool_call(None, None, false)],
        )];
        let (outcome, signals) = classify_outcome(&turns);
        assert_eq!(outcome, SessionOutcome::Failure);
        assert_eq!(signals.final_user_sentiment, Some(Sentiment::Negative));
        assert!(!signals.has_commit);
    }

    #[test]
    fn classify_indeterminate() {
        let turns = vec![make_turn(
            "do something",
            "ok",
            vec![make_tool_call(Some("some output"), None, false)],
        )];
        let (outcome, signals) = classify_outcome(&turns);
        assert_eq!(outcome, SessionOutcome::Indeterminate);
        assert!(!signals.has_commit);
        assert_eq!(signals.tests_passed, TestOutcome::NotObserved);
        assert!(!signals.has_error_loops);
        assert!(!signals.was_interrupted);
    }

    #[test]
    fn sentiment_positive() {
        let turns = vec![make_turn("thanks, that works!", "", vec![])];
        let (_, signals) = classify_outcome(&turns);
        assert_eq!(signals.final_user_sentiment, Some(Sentiment::Positive));
    }

    #[test]
    fn sentiment_negative() {
        let turns = vec![make_turn("this is broken", "", vec![])];
        let (_, signals) = classify_outcome(&turns);
        assert_eq!(signals.final_user_sentiment, Some(Sentiment::Negative));
    }

    #[test]
    fn sentiment_neutral() {
        let turns = vec![make_turn("ok", "", vec![])];
        let (_, signals) = classify_outcome(&turns);
        assert_eq!(signals.final_user_sentiment, Some(Sentiment::Neutral));
    }

    #[test]
    fn detect_commit_via_git_commit_command() {
        let tc = ToolCallRecord {
            tool_name: "Bash".into(),
            input: serde_json::json!({"command": "git commit -m 'feat: stuff'"}),
            stdout: None,
            stderr: None,
            is_error: false,
            interrupted: false,
        };
        let turns = vec![make_turn("commit", "ok", vec![tc])];
        let (_, signals) = classify_outcome(&turns);
        assert!(signals.has_commit);
    }

    #[test]
    fn no_error_loop_with_different_errors() {
        let turns = vec![
            make_turn(
                "fix",
                "try",
                vec![make_tool_call(None, Some("error A"), true)],
            ),
            make_turn(
                "fix",
                "try",
                vec![make_tool_call(None, Some("error B"), true)],
            ),
            make_turn(
                "fix",
                "try",
                vec![make_tool_call(None, Some("error C"), true)],
            ),
        ];
        let (_, signals) = classify_outcome(&turns);
        assert!(!signals.has_error_loops);
    }

    #[test]
    fn interrupted_flag_true_triggers_was_interrupted() {
        let mut tc = make_tool_call(Some("output"), None, false);
        tc.interrupted = true;
        let turns = vec![make_turn("do thing", "ok", vec![tc])];
        let (_, signals) = classify_outcome(&turns);
        assert!(signals.was_interrupted);
    }

    #[test]
    fn is_error_alone_does_not_trigger_was_interrupted() {
        let tc_err = make_tool_call(None, Some("fail"), true);
        let turns = vec![make_turn("do thing", "ok", vec![tc_err])];
        let (_, signals) = classify_outcome(&turns);
        assert!(!signals.was_interrupted);
    }

    #[test]
    fn has_short_sha_works() {
        assert!(has_short_sha("abc1234"));
        assert!(has_short_sha("prefix abc1234 suffix"));
        assert!(!has_short_sha("abc12")); // only 5 hex chars
        assert!(!has_short_sha("xyz"));
    }

    #[test]
    fn detect_tests_passed_pytest_branch() {
        // Isolates the pytest-specific branch (`pytest` + ` ok`): the stdout
        // deliberately omits "passed" so the earlier `passed && test` branch
        // cannot fire first (note "pytest" itself contains "test").
        let tc = ToolCallRecord {
            tool_name: "Bash".into(),
            input: serde_json::json!({"command": "pytest"}),
            stdout: Some("pytest collected 5 items\n5 ok in 0.12s".into()),
            stderr: None,
            is_error: false,
            interrupted: false,
        };
        let turns = vec![make_turn("run tests", "running pytest", vec![tc])];
        let (outcome, signals) = classify_outcome(&turns);
        assert_eq!(signals.tests_passed, TestOutcome::Passed);
        assert_eq!(outcome, SessionOutcome::Success);
    }

    #[test]
    fn detect_commit_via_master_branch_in_stdout() {
        // Isolates the `[master` stdout pattern: the input command is NOT
        // `git commit` (so the command branch is skipped) and the SHA is <7 hex
        // (so `has_short_sha` cannot fire) — only `[master` can satisfy it here.
        let tc = ToolCallRecord {
            tool_name: "Bash".into(),
            input: serde_json::json!({"command": "make release"}),
            stdout: Some("[master ab12] release: cut version\n 1 file changed".into()),
            stderr: None,
            is_error: false,
            interrupted: false,
        };
        let turns = vec![make_turn("commit", "ok", vec![tc])];
        let (_, signals) = classify_outcome(&turns);
        assert!(signals.has_commit);
    }
}
