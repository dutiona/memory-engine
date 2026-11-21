//! `POST /lifecycle/events` (ADR-0020 §3, §5, §6, §8, §11), driven with the checked-in Claude Code hook fixtures.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// The fixtures every acceptance test draws from: one per ADR-0020 §11 hook, plus the variants the tests need.
const REQUIRED_FIXTURES: [&str; 14] = [
    "session-start-startup",
    "session-start-resume",
    "session-start-compact",
    "user-prompt-submit",
    "post-tool-use-bash",
    "post-tool-use-read",
    "post-tool-use-mcp-me-recall",
    "post-tool-use-failure-read",
    "subagent-start",
    "subagent-stop",
    "stop",
    "stop-failure",
    "post-compact",
    "session-end",
];

const ENVELOPE: [&str; 5] = [
    "ME-Event-Id",
    "ME-Event-Time",
    "ME-Event-Seq",
    "ME-Harness",
    "ME-Client-Version",
];

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-code-hooks")
}

fn string_leaves<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(s) => out.push(s),
        Value::Array(items) => items.iter().for_each(|v| string_leaves(v, out)),
        Value::Object(map) => map.values().for_each(|v| string_leaves(v, out)),
        _ => {}
    }
}

/// The repository is public: a fixture holds no secret shape a scanner could flag, no local path, and a header file
/// holds only the envelope and the project facts.
#[test]
fn fixtures_are_public_safe() {
    let dir = fixtures_dir();
    for name in REQUIRED_FIXTURES {
        assert!(dir.join(name).join("body").is_file(), "no fixture {name}");
    }
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if !path.is_dir() {
            continue;
        }
        let body = std::fs::read(path.join("body")).unwrap();
        let text = String::from_utf8(body).unwrap();
        for local in ["/home/", "/Users/", "/tmp/", "/var/folders/"] {
            assert!(!text.contains(local), "{} holds {local}", path.display());
        }
        let json: Value = serde_json::from_str(&text).unwrap();
        assert!(json.is_object(), "{}", path.display());
        let mut leaves = Vec::new();
        string_leaves(&json, &mut leaves);
        for leaf in leaves {
            assert_eq!(
                memory_engine::redact::scrub(leaf).unwrap(),
                leaf,
                "{}: a redaction rule matches a fixture string",
                path.display()
            );
        }
        let headers = std::fs::read_to_string(path.join("headers")).unwrap();
        for local in ["/home/", "/Users/", "/tmp/", "/var/folders/"] {
            assert!(
                !headers.contains(local),
                "{} headers hold {local}",
                path.display()
            );
        }
        let names: Vec<&str> = headers
            .lines()
            .map(|l| l.split_once(": ").unwrap().0)
            .collect();
        for want in ENVELOPE {
            assert!(names.contains(&want), "{}: no {want}", path.display());
        }
        assert!(
            names.iter().all(|n| n.starts_with("ME-")),
            "{}",
            path.display()
        );
        checked += 1;
    }
    assert!(checked >= REQUIRED_FIXTURES.len());
}
