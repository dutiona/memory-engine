//! `POST /backfill/uploads` (#1026), driven with the checked-in synthetic Claude Code transcripts
//! and native memory files.

use std::path::{Path, PathBuf};

use serde_json::Value;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Every file under `dir`, its subdirectories included, sorted.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(files(&path));
        } else {
            out.push(path);
        }
    }
    out.sort();
    out
}

fn string_leaves<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(s) => out.push(s),
        Value::Array(items) => items.iter().for_each(|v| string_leaves(v, out)),
        Value::Object(map) => map.values().for_each(|v| string_leaves(v, out)),
        _ => {}
    }
}

fn holds_no_local_path(path: &Path, text: &str) {
    for local in ["/home/", "/Users/", "/tmp/", "/var/folders/"] {
        assert!(!text.contains(local), "{} holds {local}", path.display());
    }
}

/// The repository is public: no fixture holds a local path or a string a redaction rule would
/// change. Every transcript line but the one malformed on purpose is a JSON object.
#[test]
fn fixtures_are_public_safe() {
    let transcripts = files(&fixtures_dir().join("claude-code-transcripts"));
    let mut malformed = 0;
    let mut lines = 0;
    for path in transcripts
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
    {
        let text = std::fs::read_to_string(path).unwrap();
        holds_no_local_path(path, &text);
        for line in text.lines() {
            lines += 1;
            let Ok(json) = serde_json::from_str::<Value>(line) else {
                malformed += 1;
                continue;
            };
            assert!(json.is_object(), "{}: {line}", path.display());
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
        }
    }
    assert_eq!(malformed, 1, "exactly one line is malformed on purpose");
    assert!(lines > 40, "{lines} transcript lines");

    let memories = files(&fixtures_dir().join("claude-code-memory"));
    assert!(memories.len() >= 6, "{memories:?}");
    for path in &memories {
        let text = String::from_utf8(std::fs::read(path).unwrap()).unwrap();
        holds_no_local_path(path, &text);
        assert_eq!(
            memory_engine::redact::scrub(&text).unwrap(),
            text,
            "{}: a redaction rule matches the file",
            path.display()
        );
    }
    let readme = fixtures_dir().join("claude-code-transcripts/README.md");
    holds_no_local_path(&readme, &std::fs::read_to_string(&readme).unwrap());
}
