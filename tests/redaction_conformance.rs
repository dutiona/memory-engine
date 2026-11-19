//! The redaction engine against the cases it shares with knowledge-base (#1024).
//!
//! `tests/fixtures/redaction-cases.jsonl` is carried byte for byte by dutiona/memory-engine and
//! dutiona/knowledge-base. Change it only through a linked pair of PRs, memory-engine first, both
//! quoting the same `sha256sum`; the pin below fails on an edit of this copy alone.
#![allow(clippy::unwrap_used)] // test code: panic-on-unwrap is the intended failure signal (#725)

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use memory_engine::redact::{redact, scrub};
use proptest::prelude::*;
use serde_json::Value;
use sha2::{Digest, Sha256};

const FIXTURE_SHA256: &str = "adc8e496cef3e495fbc57c1c9f9133d8e17553d02cffb710b454103bb69bf7e9";

fn fixture() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/redaction-cases.jsonl");
    std::fs::read_to_string(path).expect("the shared fixture is readable")
}

struct Case {
    name: String,
    input: String,
    output: String,
    rules: Vec<String>,
}

fn cases(text: &str) -> (Value, Vec<Case>) {
    let mut lines = text.split('\n');
    let header: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let cases = lines
        .filter(|l| !l.is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap();
            let s = |k: &str| v[k].as_str().unwrap().to_owned();
            Case {
                name: s("name"),
                input: s("input"),
                output: s("output"),
                rules: v["rules"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| r.as_str().unwrap().to_owned())
                    .collect(),
            }
        })
        .collect();
    (header, cases)
}

/// One string as both repositories' encoders write it (knowledge-base `tests/test_redact.py`
/// `_string`): `hide` escapes `@` and the first character of each ASCII-alphanumeric run, so no
/// scanner reads a token out of the file.
fn encode_string(value: &str, hide: bool) -> String {
    let mut out = String::from("\"");
    let mut prev: Option<char> = None;
    for c in value.chars() {
        let starts_run =
            c.is_ascii_alphanumeric() && !prev.is_some_and(|p| p.is_ascii_alphanumeric());
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || (hide && (c == '@' || starts_run)) => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
        prev = Some(c);
    }
    out.push('"');
    out
}

fn encode_file(header: &Value, cases: &[Case]) -> String {
    let mut out = format!(
        "{{\"about\":{}}}\n",
        encode_string(header["about"].as_str().unwrap(), false)
    );
    for c in cases {
        let rules: Vec<String> = c.rules.iter().map(|r| encode_string(r, false)).collect();
        let _ = writeln!(
            out,
            "{{\"name\":{},\"input\":{},\"output\":{},\"rules\":[{}]}}",
            encode_string(&c.name, false),
            encode_string(&c.input, true),
            encode_string(&c.output, true),
            rules.join(",")
        );
    }
    out
}

#[test]
fn fixture_is_the_shared_copy() {
    let digest = Sha256::digest(fixture().as_bytes());
    let hex = digest.iter().fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    });
    assert_eq!(
        hex, FIXTURE_SHA256,
        "the fixture changed: a rule change is a ME-then-KB PR pair"
    );
}

#[test]
fn fixture_one_pass_reproduces_every_shared_case() {
    let (_, cases) = cases(&fixture());
    let mismatches: Vec<String> = cases
        .iter()
        .filter_map(|c| {
            let (out, rules) = redact(&c.input);
            let rules: Vec<String> = rules.into_iter().map(str::to_owned).collect();
            (out != c.output || rules != c.rules)
                .then(|| format!("{}: got ({out:?}, {rules:?})", c.name))
        })
        .collect();
    assert!(
        mismatches.is_empty(),
        "{} cases differ:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

#[test]
fn fixture_is_canonical_and_holds_no_secret() {
    let text = fixture();
    let (header, cases) = cases(&text);
    let mut names: Vec<&str> = cases.iter().map(|c| c.name.as_str()).collect();
    assert!(names.iter().all(|n| {
        n.split(['-', '.']).all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
    }));
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), cases.len(), "case names are unique");
    assert_eq!(
        encode_file(&header, &cases),
        text,
        "the fixture is in its canonical encoding"
    );
    for line in text.split('\n') {
        assert_eq!(
            scrub(line).unwrap(),
            line,
            "a fixture line holds a secret: {line}"
        );
    }
}

/// Re-encode the fixture from its parsed cases, after a case was appended by hand.
#[test]
#[ignore = "rewrites tests/fixtures/redaction-cases.jsonl; run on purpose only"]
fn rewrite_fixture_canonically() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/redaction-cases.jsonl");
    let (header, cases) = cases(&fixture());
    std::fs::write(path, encode_file(&header, &cases)).unwrap();
}

#[test]
fn an_address_is_not_a_secret() {
    let aws = format!("AKIA{}", "IOSFODNN7EXAMPLE");
    for text in [
        "contact craig@example.com please".to_owned(),
        r#"{"email":"a@b.com"}"#.to_owned(),
        format!("aws {aws} and a@b.com"),
    ] {
        let (out, rules) = redact(&text);
        assert!(
            out.contains("@b.com") || out.contains("@example.com"),
            "{out}"
        );
        assert!(!rules.contains(&"email"));
    }
}

#[test]
fn an_authority_ending_at_a_multibyte_space_does_not_panic() {
    for space in ['\u{a0}', '\u{3000}'] {
        let (out, rules) = redact(&format!(
            "see https://u:pw@example.com{space}more https://v:pw2@example.org"
        ));
        assert_eq!(
            rules,
            ["connection-string-password", "connection-string-password"]
        );
        assert!(
            !out.replace("[REDACTED:connection-string-password]", "")
                .contains("pw"),
            "{out}"
        );
    }
}

#[test]
fn scrub_reaches_the_fixpoint_one_pass_misses() {
    let text = "api_key=a3f1b2c4d5e6f7a8b9c0d1e2f3a4b5c6qWeRtYuIoPaSdFgHjKl9876";
    let once = redact(text).0;
    let settled = scrub(text).unwrap();
    assert_ne!(once, settled);
    assert_eq!(
        settled,
        "api_key=[REDACTED:hex-secret][REDACTED:high-entropy]"
    );
    assert_eq!(scrub(&settled).unwrap(), settled);
}

#[test]
fn a_code_identifier_is_not_a_machine_token() {
    let token = format!("me_{}", "Zq7Xw3Rt9Lp2Vb6Nm4Kc8Hd1Fs5Gj0Ay");
    let (out, rules) = redact(&format!("use me_types::Fact; key {token} end"));
    assert_eq!(out, "use me_types::Fact; key [REDACTED:machine-token] end");
    assert_eq!(rules, ["machine-token"]);
}

/// 1 MiB, the largest text a write accepts (`MAX_PAYLOAD_BYTES`). A quadratic scan needs about
/// 10^12 steps at this size, hours; the ceiling holds four orders of magnitude of margin, so a
/// loaded host does not make it flaky.
#[test]
fn redaction_stays_bounded_on_hostile_input() {
    const SIZE: usize = 1 << 20;
    let hex = "a3f1b2c4d5e6f7a8b9c0d1e2f3a4b5c6";
    let zeros = "0".repeat(32);
    let shapes = [
        ("jwt-prefix", "=eyJ".to_owned()),
        ("url-authority", "://u:p@".to_owned()),
        ("hex-context", format!("token={hex} ")),
        ("pem-line", "-----BEGIN ".to_owned()),
        ("hex-words", format!("_{hex}")),
        ("interleaved", format!("me_{zeros} Ab3dE5fG7hI9jK1lM2nO ")),
        ("placeholders", "[REDACTED:x]".to_owned()),
        ("link-userinfo", "https://a:b@c ".to_owned()),
    ];
    for (name, unit) in shapes {
        let text = unit.repeat(SIZE / unit.len());
        let started = Instant::now();
        let _ = scrub(&text);
        let spent = started.elapsed();
        assert!(spent < Duration::from_secs(10), "{name}: {spent:?}");
    }
    // One long word with one underscore: a copied context-word tail made this quadratic in KB.
    let text = format!("_{}", format!("g{zeros}").repeat(SIZE / 33));
    let started = Instant::now();
    let _ = scrub(&text);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "one-long-word: {:?}",
        started.elapsed()
    );
}

const FRAGMENTS: &[&str] = &[
    "://",
    ":",
    "@",
    "/",
    " ",
    "\n",
    "\u{a0}",
    "\u{3000}",
    "é",
    "e\u{301}",
    "\"",
    "'",
    "=",
    "_",
    ".",
    "sk-",
    "ghp_",
    "AKIA",
    "eyJ",
    "SG.",
    "xoxb-",
    "-----BEGIN ",
    "me_",
    "api_key=",
    "token: ",
    "0123456789abcdef",
    "Zq7Xw3Rt9Lp2Vb6N",
    "[REDACTED:x]",
    "[REDACTED:high-entropy]",
    "https://",
];

proptest! {
    #[test]
    fn scrub_never_panics_and_is_idempotent(parts in prop::collection::vec(prop::sample::select(FRAGMENTS), 0..48)) {
        let text: String = parts.concat();
        let settled = scrub(&text).expect("every fragment mix settles");
        prop_assert_eq!(&redact(&settled).0, &settled);
        prop_assert_eq!(scrub(&settled).unwrap(), settled);
    }
}
