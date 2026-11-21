//! The Claude Code adapter (ADR-0020 §11): one hook input in, one neutral record out.
//!
//! Field names are the ones Claude Code's hooks carry, checked in as fixtures. The adapter also
//! owns what ADR-0020 §11 leaves to it: the activity filter (the ignore list and the 300 s
//! duplicate window), the paths a tool call touches, and the ME call line of a result.

use memory_engine::types::lifecycle::{
    Actor, BodyFault, Content, NeutralRecord, ParseClass, ParseFailure, StopOutcome, ToolOutcome,
};
use serde_json::{Map, Value};

/// Tools whose calls are formatting or lint fixes: stored as events, kept out of the activity
/// stream. A case-insensitive substring of the tool name.
const IGNORED: [&str; 4] = ["prettier", "eslint_fix", "ruff_format", "clang_format"];

/// The window in which a repeated call is one activity.
const DEDUP_WINDOW_SECS: i64 = 300;

/// No file path is longer than this, so a longer string is not one.
const PATH_MAX: usize = 4096;

/// Parse `body` as a Claude Code hook input and map it to a neutral record.
///
/// # Errors
///
/// [`BodyFault::Parse`] when the body is not UTF-8, not JSON or not a JSON object, with the
/// class and the position and never a byte of the body or the parser's message.
/// [`BodyFault::Unmappable`] when it is an object the adapter cannot map: an unknown
/// `hook_event_name`, no `session_id`, no `tool_name` on a tool hook, or no non-empty string
/// `compact_summary` on `PostCompact`.
///
/// # Examples
///
/// ```
/// use memory_engine::types::lifecycle::{Content, ParseClass};
/// use memory_engine_mcp::adapters::claude_code::read;
///
/// let record = read(br#"{"hook_event_name":"UserPromptSubmit","session_id":"s","prompt":"hi"}"#)?;
/// assert!(matches!(record.content, Content::Prompt { text: Some(t) } if t == "hi"));
///
/// let fault = read(b"[1]").unwrap_err();
/// assert!(matches!(fault, memory_engine::types::lifecycle::BodyFault::Parse(f) if f.class == ParseClass::NotObject));
/// # Ok::<(), memory_engine::types::lifecycle::BodyFault>(())
/// ```
pub fn read(body: &[u8]) -> Result<NeutralRecord, BodyFault> {
    let object = parse(body).map_err(BodyFault::Parse)?;
    map(&object)
}

fn parse(body: &[u8]) -> Result<Map<String, Value>, ParseFailure> {
    if let Err(e) = std::str::from_utf8(body) {
        let mut lines = body[..e.valid_up_to()].split(|b| *b == b'\n');
        let last = lines.next_back().map_or(0, <[u8]>::len);
        return Err(ParseFailure {
            class: ParseClass::NotUtf8,
            line: lines.count() as u64 + 1,
            column: last as u64 + 1,
        });
    }
    serde_json::from_slice(body).map_err(|e| ParseFailure {
        class: if e.is_data() {
            ParseClass::NotObject
        } else {
            ParseClass::NotJson
        },
        line: e.line() as u64,
        column: e.column() as u64,
    })
}

fn text<'a>(object: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    object.get(name).and_then(Value::as_str)
}

fn owned(object: &Map<String, Value>, name: &str) -> Option<String> {
    text(object, name).map(str::to_owned)
}

fn map(object: &Map<String, Value>) -> Result<NeutralRecord, BodyFault> {
    let hook = text(object, "hook_event_name").unwrap_or_default();
    let session_id = match text(object, "session_id") {
        Some(id) if !id.is_empty() => id.to_owned(),
        _ => return Err(BodyFault::Unmappable("no session_id")),
    };
    let content = match hook {
        "SessionStart" => Content::SessionStart {
            source: owned(object, "source"),
        },
        "UserPromptSubmit" => Content::Prompt {
            text: owned(object, "prompt"),
        },
        "SubagentStart" => Content::Prompt { text: None },
        "PostToolUse" => tool_use(object, ToolOutcome::Success)?,
        "PostToolUseFailure" => {
            let interrupted = object.get("is_interrupt").and_then(Value::as_bool) == Some(true);
            tool_use(
                object,
                if interrupted {
                    ToolOutcome::Interrupted
                } else {
                    ToolOutcome::Failure
                },
            )?
        }
        "Stop" | "SubagentStop" => Content::Stop {
            outcome: StopOutcome::Completed,
            message: owned(object, "last_assistant_message"),
        },
        // Its `error` is a class such as `server_error`; the message is the error text.
        "StopFailure" => Content::Stop {
            outcome: StopOutcome::ApiError,
            message: owned(object, "last_assistant_message"),
        },
        "PostCompact" => match text(object, "compact_summary") {
            Some(summary) if !summary.is_empty() => Content::Compact {
                trigger: owned(object, "trigger"),
                summary: summary.to_owned(),
            },
            _ => return Err(BodyFault::Unmappable("no compact_summary")),
        },
        "SessionEnd" => Content::SessionEnd {
            reason: owned(object, "reason"),
        },
        _ => return Err(BodyFault::Unmappable("unknown hook_event_name")),
    };
    let subagent = matches!(hook, "SubagentStart" | "SubagentStop")
        || object.get("agent_id").is_some_and(|v| !v.is_null());
    Ok(NeutralRecord {
        session_id,
        actor: if subagent {
            Actor::Subagent {
                id: owned(object, "agent_id"),
                kind: owned(object, "agent_type"),
            }
        } else {
            Actor::Main
        },
        cwd: owned(object, "cwd"),
        harness_version: None,
        content,
    })
}

fn tool_use(object: &Map<String, Value>, outcome: ToolOutcome) -> Result<Content, BodyFault> {
    let tool = match text(object, "tool_name") {
        Some(tool) if !tool.is_empty() => tool.to_owned(),
        _ => return Err(BodyFault::Unmappable("no tool_name")),
    };
    let input = object.get("tool_input").cloned().unwrap_or(Value::Null);
    let result = if outcome == ToolOutcome::Success {
        object.get("tool_response").and_then(result_text)
    } else {
        owned(object, "error")
    };
    let lowered = tool.to_lowercase();
    let ignored = IGNORED.iter().any(|name| lowered.contains(name));
    Ok(Content::ToolUse {
        paths: touched_paths(&tool, &input),
        call: result.as_deref().and_then(call_line),
        dedup_window_secs: (!ignored).then_some(DEDUP_WINDOW_SECS),
        tool,
        input,
        result,
        outcome,
    })
}

/// A tool response as text: a string as it is, the text blocks of an MCP result joined by line
/// breaks, any other value as compact JSON.
fn result_text(response: &Value) -> Option<String> {
    match response {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Array(blocks) => {
            let texts: Vec<&str> = blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect();
            (!texts.is_empty()).then(|| texts.join("\n"))
        }
        other => Some(other.to_string()),
    }
}

/// The call ID of ADR-0020 §8: the first line of the text is `call ` and 16 lowercase hex
/// digits, exactly. Read before any redaction or cut, which would lose it.
fn call_line(result: &str) -> Option<String> {
    let line = result.split('\n').next()?;
    let id = line.strip_prefix("call ")?;
    (id.len() == 16 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        .then(|| id.to_owned())
}

/// The path tokens a call touches, as it spelled them; the core decides which are sensitive.
/// Every string of the input, whole (a path may hold a space), and every word of a shell
/// command. A bare name relative to the call's working directory holds no `/` and no leading
/// dot, so no shape narrows the candidates. The rule errs wide: over-stubbing loses one
/// excerpt, a missed path leaks one.
fn touched_paths(tool: &str, input: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    collect(tool == "Bash", input, &mut paths);
    paths
}

fn collect(shell: bool, value: &Value, paths: &mut Vec<String>) {
    match value {
        Value::String(s) => {
            if s.len() <= PATH_MAX {
                paths.push(s.clone());
            }
        }
        Value::Array(items) => items.iter().for_each(|v| collect(shell, v, paths)),
        Value::Object(members) => {
            for (name, v) in members {
                match v {
                    Value::String(command) if shell && name == "command" => {
                        paths.extend(words(command));
                    }
                    _ => collect(shell, v, paths),
                }
            }
        }
        _ => {}
    }
}

/// The words of a shell command, split the shell's way: whitespace and `;|&<>()` outside quotes
/// end a word, `'…'` and `"…"` group, a backslash escapes. An unquoted `=` also ends a word, so
/// `X=.env` and `--file=/etc/x` yield their value.
fn words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut chars = command.chars();
    let mut flush = |word: &mut String| {
        if !word.is_empty() && word.len() <= PATH_MAX {
            words.push(std::mem::take(word));
        }
        word.clear();
    };
    while let Some(c) = chars.next() {
        match (quote, c) {
            (None, '\\') => word.extend(chars.next()),
            (Some('"'), '\\') => match chars.next() {
                Some(next @ ('"' | '\\' | '$' | '`')) => word.push(next),
                Some(next) => {
                    word.push('\\');
                    word.push(next);
                }
                None => word.push('\\'),
            },
            (Some(q), _) if c == q => quote = None,
            (None, '\'' | '"') => quote = Some(c),
            (None, _) if c.is_whitespace() || ";|&<>()=".contains(c) => flush(&mut word),
            _ => word.push(c),
        }
    }
    flush(&mut word);
    words
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-code-hooks");
        std::fs::read(dir.join(name).join("body")).unwrap()
    }

    fn mapped(name: &str) -> NeutralRecord {
        read(&fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
    }

    fn body(value: &Value) -> Vec<u8> {
        value.to_string().into_bytes()
    }

    fn tool_content(
        record: NeutralRecord,
    ) -> (
        String,
        ToolOutcome,
        Vec<String>,
        Option<String>,
        Option<i64>,
    ) {
        match record.content {
            Content::ToolUse {
                tool,
                outcome,
                paths,
                call,
                dedup_window_secs,
                ..
            } => (tool, outcome, paths, call, dedup_window_secs),
            other => panic!("not a tool call: {other:?}"),
        }
    }

    #[test]
    fn every_fixture_maps_to_its_kind() {
        use memory_engine::types::lifecycle::Kind::{
            Compact, Prompt, SessionEnd, SessionStart, Stop, ToolUse,
        };
        // fixture, kind, a subagent's, and the outcome the README gives.
        let table = [
            ("session-start-startup", SessionStart, false, ""),
            ("session-start-resume", SessionStart, false, ""),
            ("session-start-compact", SessionStart, false, ""),
            ("user-prompt-submit", Prompt, false, ""),
            ("post-tool-use-bash", ToolUse, false, "Success"),
            ("post-tool-use-read", ToolUse, false, "Success"),
            ("post-tool-use-agent", ToolUse, false, "Success"),
            ("post-tool-use-toolsearch", ToolUse, false, "Success"),
            ("post-tool-use-mcp-me-recall", ToolUse, false, "Success"),
            ("post-tool-use-bash-subagent", ToolUse, true, "Success"),
            ("post-tool-use-failure-read", ToolUse, false, "Failure"),
            ("subagent-start", Prompt, true, ""),
            ("subagent-stop", Stop, true, "Completed"),
            ("subagent-stop-internal", Stop, true, "Completed"),
            ("stop", Stop, false, "Completed"),
            ("stop-failure", Stop, false, "ApiError"),
            ("post-compact", Compact, false, ""),
            ("session-end", SessionEnd, false, ""),
        ];
        for (name, kind, subagent, outcome) in table {
            let record = mapped(name);
            assert_eq!(record.content.kind(), kind, "{name}");
            assert_eq!(
                matches!(record.actor, Actor::Subagent { .. }),
                subagent,
                "{name}"
            );
            assert!(!record.session_id.is_empty(), "{name}");
            assert_eq!(
                record.harness_version, None,
                "{name}: no hook input carries the harness's version"
            );
            let got = match &record.content {
                Content::ToolUse { outcome, .. } => format!("{outcome:?}"),
                Content::Stop { outcome, .. } => format!("{outcome:?}"),
                _ => String::new(),
            };
            assert_eq!(got, outcome, "{name}");
        }
    }

    #[test]
    fn the_fixtures_keep_their_texts() {
        let start = mapped("session-start-resume");
        assert!(
            matches!(start.content, Content::SessionStart { source: Some(s) } if s == "resume")
        );
        assert!(start.cwd.is_some());
        let Content::Prompt { text: Some(prompt) } = mapped("user-prompt-submit").content else {
            panic!("a prompt")
        };
        assert!(prompt.starts_with("Do these steps"));
        assert!(matches!(
            mapped("subagent-start").content,
            Content::Prompt { text: None }
        ));
        let Actor::Subagent { id, kind } = mapped("subagent-stop-internal").actor else {
            panic!("the compaction agent is a subagent")
        };
        assert_eq!((id.is_some(), kind.as_deref()), (true, Some("")));
        let Content::Stop {
            message: Some(done),
            ..
        } = mapped("stop").content
        else {
            panic!("a final message")
        };
        assert!(done.ends_with("done"));
        let Content::Stop {
            message: Some(error),
            ..
        } = mapped("stop-failure").content
        else {
            panic!("the error text")
        };
        assert!(error.starts_with("API Error"), "{error}");
        let Content::Compact { trigger, summary } = mapped("post-compact").content else {
            panic!("a compaction")
        };
        assert_eq!(trigger.as_deref(), Some("manual"));
        assert!(!summary.is_empty(), "the summary is kept");
        assert!(
            matches!(mapped("session-end").content, Content::SessionEnd { reason: Some(r) } if r == "other")
        );
        let Content::ToolUse {
            result: Some(error),
            ..
        } = mapped("post-tool-use-failure-read").content
        else {
            panic!("an error text")
        };
        assert!(error.starts_with("File does not exist"));
    }

    #[test]
    fn unparseable_bodies_carry_class_and_position() {
        let fault = |body: &[u8]| match read(body).unwrap_err() {
            BodyFault::Parse(f) => (f.class, f.line, f.column),
            other @ BodyFault::Unmappable(_) => panic!("{other:?}"),
        };
        assert_eq!(fault(b"{\"a\":1}\n\xff"), (ParseClass::NotUtf8, 2, 1));
        assert_eq!(fault(b"ab\xc3"), (ParseClass::NotUtf8, 1, 3));
        let (class, line, _) = fault(b"{\"a\":\n");
        assert_eq!((class, line), (ParseClass::NotJson, 2));
        assert_eq!(fault(b"{} x").0, ParseClass::NotJson);
        assert_eq!(fault(b"").0, ParseClass::NotJson);
        assert_eq!(fault(b"[1]").0, ParseClass::NotObject);
        assert_eq!(fault(b"[1]").1, 1, "serde's line, whatever its column");
        assert_eq!(fault(b"\"text\"").0, ParseClass::NotObject);
        assert_eq!(fault(b"null").0, ParseClass::NotObject);
    }

    #[test]
    fn a_fault_never_quotes_the_body() {
        let secret = "sk-planted-secret-value";
        let fault = read(format!("{{\"a\": \"{secret}\" oops").as_bytes()).unwrap_err();
        assert!(!format!("{fault:?} {fault}").contains(secret));
    }

    #[test]
    fn an_unknown_hook_or_no_session_is_unmappable() {
        let unmappable = |value: Value| match read(&body(&value)).unwrap_err() {
            BodyFault::Unmappable(reason) => reason,
            other @ BodyFault::Parse(_) => panic!("{other:?}"),
        };
        assert_eq!(
            unmappable(json!({ "hook_event_name": "PreCompact", "session_id": "s" })),
            "unknown hook_event_name"
        );
        assert_eq!(
            unmappable(json!({ "session_id": "s" })),
            "unknown hook_event_name"
        );
        for session in [
            json!({}),
            json!({ "session_id": "" }),
            json!({ "session_id": 7 }),
        ] {
            let mut object = json!({ "hook_event_name": "Stop" });
            object
                .as_object_mut()
                .unwrap()
                .extend(session.as_object().unwrap().clone());
            assert_eq!(unmappable(object), "no session_id");
        }
        assert_eq!(
            unmappable(json!({ "hook_event_name": "PostToolUse", "session_id": "s" })),
            "no tool_name"
        );
        assert_eq!(
            unmappable(
                json!({ "hook_event_name": "PostToolUseFailure", "session_id": "s", "tool_name": "" })
            ),
            "no tool_name"
        );
    }

    #[test]
    fn a_compact_without_a_summary_is_unmappable() {
        for summary in [None, Some(json!(7)), Some(json!("")), Some(Value::Null)] {
            let mut object = json!({ "hook_event_name": "PostCompact", "session_id": "s" });
            if let Some(summary) = summary {
                object["compact_summary"] = summary;
            }
            assert!(
                matches!(
                    read(&body(&object)),
                    Err(BodyFault::Unmappable("no compact_summary"))
                ),
                "{object}"
            );
        }
    }

    #[test]
    fn an_interrupted_failure_is_interrupted() {
        let failure = |interrupt: Option<bool>| {
            let mut object = json!({
                "hook_event_name": "PostToolUseFailure", "session_id": "s",
                "tool_name": "Bash", "tool_input": {}, "error": "killed"
            });
            if let Some(interrupt) = interrupt {
                object["is_interrupt"] = json!(interrupt);
            }
            read(&body(&object)).unwrap()
        };
        for (flag, want) in [
            (Some(true), ToolOutcome::Interrupted),
            (Some(false), ToolOutcome::Failure),
            (None, ToolOutcome::Failure),
        ] {
            let record = failure(flag);
            let Content::ToolUse {
                outcome, result, ..
            } = record.content
            else {
                panic!("a tool call")
            };
            assert_eq!((outcome, result.as_deref()), (want, Some("killed")));
        }
    }

    #[test]
    fn an_agent_id_makes_any_hook_a_subagents() {
        let record = read(&body(&json!({
            "hook_event_name": "UserPromptSubmit", "session_id": "s", "prompt": "x",
            "agent_id": "a1", "agent_type": "Explore"
        })))
        .unwrap();
        assert_eq!(
            record.actor,
            Actor::Subagent {
                id: Some("a1".into()),
                kind: Some("Explore".into())
            }
        );
    }

    #[test]
    fn ignored_tools_have_no_window() {
        let tool = |name: &str| {
            let object = json!({
                "hook_event_name": "PostToolUse", "session_id": "s", "tool_name": name, "tool_input": {}
            });
            tool_content(read(&body(&object)).unwrap()).4
        };
        for ignored in [
            "prettier",
            "Prettier",
            "mcp__fmt__ESLINT_FIX",
            "ruff_format",
            "run_clang_format",
        ] {
            assert_eq!(tool(ignored), None, "{ignored}");
        }
        for kept in ["Bash", "Read", "mcp__me__recall", "format"] {
            assert_eq!(tool(kept), Some(300), "{kept}");
        }
    }

    fn paths_of(tool: &str, input: &Value) -> Vec<String> {
        let object = json!({
            "hook_event_name": "PostToolUse", "session_id": "s", "tool_name": tool, "tool_input": input
        });
        tool_content(read(&body(&object)).unwrap()).2
    }

    #[test]
    fn paths_follow_the_rule() {
        assert!(
            paths_of("Read", &json!({ "file_path": "/work/my project/.env" }))
                .contains(&"/work/my project/.env".to_owned())
        );
        for (command, path) in [
            (r#"cat "/work/my project/.env""#, "/work/my project/.env"),
            (r"cat my\ project/.env", "my project/.env"),
            ("X=.env cmd", ".env"),
            ("cat '/a b/c'; rm ./x", "./x"),
            ("echo hi>~/.ssh/id_rsa", "~/.ssh/id_rsa"),
            ("run --file=/etc/shadow|tee", "/etc/shadow"),
            ("a&&(cat ../.env)", "../.env"),
            (r#"cat "a \"b\" c/.env""#, r#"a "b" c/.env"#),
        ] {
            let paths = paths_of("Bash", &json!({ "command": command }));
            assert!(paths.contains(&path.to_owned()), "{command}: {paths:?}");
        }
        // Nested leaves count, whitespace included; a number is not a path.
        let paths = paths_of(
            "Write",
            &json!({ "edits": [{ "path": "~/notes", "text": "plain" }], "n": 3, "dot": ".hidden" }),
        );
        assert_eq!(paths, [".hidden", "~/notes", "plain"]);
        // The whole command is not a token, only its words, beside every other string; one no
        // path can be, being too long, is skipped.
        let paths = paths_of(
            "Bash",
            &json!({ "command": "cat /x/y", "description": "d" }),
        );
        assert_eq!(paths, ["cat", "/x/y", "d"]);
        let long = format!("/{}", "a".repeat(PATH_MAX));
        assert!(
            paths_of("Write", &json!({ "content": long })).is_empty(),
            "no path is that long"
        );
    }

    /// A bare name relative to the call's working directory holds no `/` and no leading dot, and
    /// names a secret all the same: every string of the input is a candidate.
    #[test]
    fn a_bare_relative_sensitive_name_is_a_candidate() {
        for (tool, input, name) in [
            (
                "Read",
                json!({ "file_path": "credentials.json" }),
                "credentials.json",
            ),
            ("Read", json!({ "file_path": "id_rsa" }), "id_rsa"),
            (
                "Edit",
                json!({ "file_path": "secrets.json", "new_string": "x" }),
                "secrets.json",
            ),
            (
                "Bash",
                json!({ "command": "cat id_rsa", "description": "token.json" }),
                "id_rsa",
            ),
            (
                "Bash",
                json!({ "command": "ls", "description": "credentials.json" }),
                "credentials.json",
            ),
        ] {
            let paths = paths_of(tool, &input);
            assert!(
                paths.contains(&name.to_owned()),
                "{tool} {input}: {paths:?}"
            );
        }
    }
    #[test]
    fn the_call_line_is_read_before_any_cut() {
        let mut object: Value =
            serde_json::from_slice(&fixture("post-tool-use-mcp-me-recall")).unwrap();
        let id = "518b327349710550";
        let long = format!("call {id}\n{}", "x".repeat(600));
        object["tool_response"] = json!([{ "type": "text", "text": long }]);
        let (_, _, _, call, _) = tool_content(read(&body(&object)).unwrap());
        assert_eq!(call.as_deref(), Some(id));

        let (_, _, _, call, _) = tool_content(mapped("post-tool-use-mcp-me-recall"));
        assert_eq!(call.as_deref(), Some(id));
        for text in [
            "result: call 518b327349710550".to_owned(),
            "call 518B327349710550".to_owned(),
            "call 518b32734971055".to_owned(),
            "call 518b3273497105501".to_owned(),
            "\ncall 518b327349710550".to_owned(),
            "call 518b327349710550 ".to_owned(),
        ] {
            object["tool_response"] = json!([{ "type": "text", "text": text }]);
            let (_, _, _, call, _) = tool_content(read(&body(&object)).unwrap());
            assert_eq!(call, None, "{text:?}");
        }
        // A string response, and an error, carry the line too.
        object["tool_response"] = json!("call 0123456789abcdef\nrest");
        assert_eq!(
            tool_content(read(&body(&object)).unwrap()).3.as_deref(),
            Some("0123456789abcdef")
        );
        let failure = json!({
            "hook_event_name": "PostToolUseFailure", "session_id": "s", "tool_name": "mcp__me__recall",
            "error": "call 0123456789abcdef\nerror: boom"
        });
        assert_eq!(
            tool_content(read(&body(&failure)).unwrap()).3.as_deref(),
            Some("0123456789abcdef")
        );
    }
}
