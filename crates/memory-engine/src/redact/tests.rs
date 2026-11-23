//! The engine's ME-only parts; the shared rules are pinned by `tests/redaction_conformance.rs`.
//! Every token-shaped value is assembled at run time.

use serde_json::json;

use super::*;

fn ghp() -> String {
    format!("ghp_{}", "0123456789abcdefghij".repeat(2))
}

fn aws() -> String {
    format!("AKIA{}", "IOSFODNN7EXAMPLE")
}

fn machine_token() -> String {
    format!("me_{}", "Zq7Xw3Rt9Lp2Vb6Nm4Kc8Hd1Fs5Gj0Ay")
}

fn dl(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

// ── The pass cap ─────────────────────────────────────────────────────────────

#[test]
fn a_text_that_does_not_settle_within_the_cap_is_refused() {
    let text = "api_key=a3f1b2c4d5e6f7a8b9c0d1e2f3a4b5c6qWeRtYuIoPaSdFgHjKl9876";
    let err = scrub_passes(text, &[], 1).unwrap_err();
    assert!(
        matches!(
            err,
            MemoryError::Conflict(ConflictError::Redaction(UNSETTLED))
        ),
        "{err}"
    );
    assert_eq!(scrub_passes(text, &[], 3).unwrap().1, 2);
}

#[test]
fn scrub_counts_only_new_secrets() {
    let (once, found) = scrub_with(&format!("a {} b {}", aws(), ghp()), &[]).unwrap();
    assert_eq!(found, 2);
    assert_eq!(scrub_with(&once, &[]).unwrap(), (once, 0));
}

#[test]
fn a_relabelled_placeholder_is_not_a_new_secret() {
    // Pass 1 redacts the token as `github-pat`; pass 2 relabels that placeholder as the URL's
    // password. One secret was found, not two.
    let text = format!("clone https://x-access-token:{}@github.com/o/r.git", ghp());
    let (out, found) = scrub_with(&text, &[]).unwrap();
    assert_eq!(
        out,
        "clone https://x-access-token:[REDACTED:connection-string-password]@github.com/o/r.git"
    );
    assert_eq!(found, 1);
}

// ── The author denylist ──────────────────────────────────────────────────────

#[test]
fn the_denylist_catches_what_signatures_miss() {
    let secret = "hunter2-dev-pw";
    let text = format!("db password is {secret} ok");
    assert_eq!(
        scrub(&text).unwrap(),
        text,
        "precondition: no signature matches"
    );
    let (out, found) = scrub_with(&text, &dl(&[secret])).unwrap();
    assert_eq!(out, "db password is [REDACTED:denylist] ok");
    assert_eq!(found, 1);
}

#[test]
fn the_denylist_wins_over_a_signature_and_merges_overlaps() {
    let token = ghp();
    let (out, _) = scrub_with(&token, &dl(&[&token])).unwrap();
    assert_eq!(out, "[REDACTED:denylist]");
    let dl2 = dl(&["ABCDEFGHSECRET", "SECRETWXYZ1234"]);
    assert_eq!(
        scrub_with("x ABCDEFGHSECRETWXYZ1234 y", &dl2).unwrap().0,
        "x [REDACTED:denylist] y"
    );
}

#[test]
fn the_denylist_skips_short_literals_and_placeholders() {
    let text = "the cat sat on the mat";
    assert_eq!(
        scrub_with(text, &dl(&["", "a", "at"])).unwrap(),
        (text.to_owned(), 0)
    );
    let already = "x [REDACTED:denylist] y";
    assert_eq!(
        scrub_with(already, &dl(&["denylist"])).unwrap(),
        (already.to_owned(), 0)
    );
    let multibyte = "€€€"; // 3 characters, 9 bytes: honoured
    assert_eq!(
        scrub_with(&format!("pw={multibyte}"), &dl(&[multibyte]))
            .unwrap()
            .0,
        "pw=[REDACTED:denylist]"
    );
}

// ── JSON ─────────────────────────────────────────────────────────────────────

#[test]
fn scrub_json_redacts_leaves_and_keeps_clean_keys_and_non_strings() {
    let mut v = json!({
        "description": format!("leaked {} here", aws()),
        "nested": { "note": format!("token {}", ghp()) },
        "list": [machine_token(), "harmless"],
        "count": 7,
        "flag": true,
    });
    assert_eq!(scrub_json(&mut v, &[]).unwrap(), 3);
    assert_eq!(v["description"], "leaked [REDACTED:aws-access-key] here");
    assert_eq!(v["nested"]["note"], "token [REDACTED:github-pat]");
    assert_eq!(v["list"], json!(["[REDACTED:machine-token]", "harmless"]));
    assert_eq!(
        (v["count"].clone(), v["flag"].clone()),
        (json!(7), json!(true))
    );
}

/// An object's keys are content too: a key holding a known secret format or a denylist literal
/// is redacted like a string, and keys that redact to one text keep every value, the later ones
/// (in key order) under `~1`, `~2`, ….
#[test]
fn scrub_json_redacts_keys_and_keeps_every_member() {
    let other = format!("AKIA{}", "IOSFODNN7EXAMPLF");
    let object = |pairs: Vec<(String, Value)>| Value::Object(pairs.into_iter().collect());
    let mut v = object(vec![
        (ghp(), json!(1)),
        ("fixturehost-config".to_owned(), json!(2)),
        (aws(), json!(3)),
        (other.clone(), json!(4)),
        ("plain".to_owned(), object(vec![(aws(), json!(5))])),
    ]);

    let found = scrub_json(&mut v, &dl(&["fixturehost"])).unwrap();

    assert_eq!(found, 5);
    assert_eq!(
        v,
        json!({
            "[REDACTED:aws-access-key]": 3,
            "[REDACTED:aws-access-key]~1": 4,
            "[REDACTED:denylist]-config": 2,
            "[REDACTED:github-pat]": 1,
            "plain": { "[REDACTED:aws-access-key]": 5 },
        })
    );
    let mut payload = object(vec![
        ("input".to_owned(), object(vec![(other, json!("x"))])),
        ("machine".to_owned(), json!("laptop")),
    ]);
    assert_eq!(scrub_payload(&mut payload).unwrap(), 1);
    assert_eq!(
        payload,
        json!({ "input": { "[REDACTED:aws-access-key]": "x" }, "machine": "laptop" })
    );
}

#[test]
fn a_hex_leaf_is_redacted_when_its_key_is_a_context_word() {
    let hex = "0123456789abcdef".repeat(2);
    for key in ["api_key", "access-token", "X-Api-Key", "secret"] {
        let mut v = json!({ key: hex });
        assert_eq!(scrub_json(&mut v, &[]).unwrap(), 1, "{key}");
        assert_eq!(v[key], "[REDACTED:hex-secret]", "{key}");
    }
    for key in ["sha", "commit", "monkey", "keyboard"] {
        let mut v = json!({ key: hex });
        assert_eq!(scrub_json(&mut v, &[]).unwrap(), 0, "{key}");
    }
}

/// The shared fixture's `json-` cases hold JSON documents: the walk turns each input into its
/// output, which knowledge-base's walk must match (dutiona/knowledge-base#626). Read here because
/// `scrub_json` is crate-private; `tests/redaction_conformance.rs` runs their one text pass.
#[test]
fn fixture_json_cases_pass_the_json_walk() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/redaction-cases.jsonl");
    let text = std::fs::read_to_string(path).unwrap();
    let mut seen = 0;
    for line in text.lines().skip(1) {
        let case: Value = serde_json::from_str(line).unwrap();
        let name = case["name"].as_str().unwrap();
        if !name.starts_with("json-") {
            continue;
        }
        let parse =
            |key: &str| -> Value { serde_json::from_str(case[key].as_str().unwrap()).unwrap() };
        let mut value = parse("input");
        scrub_json(&mut value, &[]).unwrap();
        assert_eq!(value, parse("output"), "{name}");
        seen += 1;
    }
    assert!(seen > 0, "the fixture holds no json- case");
}

// ── Provenance ───────────────────────────────────────────────────────────────

fn provenance(root: &str, key: &Value) -> Value {
    json!({ "facts": { "root": root, "branch": "main" }, "dropped": {}, "key": key, "rules": 1 })
}

#[test]
fn genuine_provenance_keeps_its_paths_and_loses_machine_tokens() {
    let uuid_root = "/srv/work/9932fd5f-1a85-4237-809b-5e6ae329dda1/scratchpad/cache";
    let mut payload =
        json!({ PROVENANCE_KEY: provenance(uuid_root, &json!(null)), "error": uuid_root });
    assert_eq!(
        scrub_payload(&mut payload).unwrap(),
        1,
        "only the error is redacted"
    );
    assert_eq!(payload[PROVENANCE_KEY]["facts"]["root"], uuid_root);
    assert_eq!(payload["error"], "[REDACTED:high-entropy]");

    let mut forged = json!({ PROVENANCE_KEY: provenance("/dev/sk-learn-tutorial-2024", &json!(machine_token())) });
    assert_eq!(scrub_payload(&mut forged).unwrap(), 1);
    assert_eq!(
        forged[PROVENANCE_KEY]["facts"]["root"],
        "/dev/sk-learn-tutorial-2024"
    );
    assert_eq!(forged[PROVENANCE_KEY]["key"], "[REDACTED:machine-token]");
}

#[test]
fn content_cannot_hide_under_the_provenance_key() {
    let mut payload = json!({ PROVENANCE_KEY: { "note": ghp() } });
    assert_eq!(scrub_payload(&mut payload).unwrap(), 1);
    assert_eq!(payload[PROVENANCE_KEY]["note"], "[REDACTED:github-pat]");
    let mut long_key =
        json!({ PROVENANCE_KEY: provenance("/r", &json!("k".repeat(MAX_KEY_BYTES + 1))) });
    assert!(!is_provenance(&long_key[PROVENANCE_KEY]));
    assert_eq!(scrub_payload(&mut long_key).unwrap(), 0);
}

// ── The author denylist on the capture path (#1026) ─────────────────────────

/// The backfill route's denylist reaches every content member a capture stores: a prompt, a tool
/// input, a result and a stub token lose the literal, while the provenance and the identifiers
/// keep it, so a host name in the denylist never hides its own rows.
#[test]
fn the_capture_path_takes_the_denylist() {
    let host = "fixturehost";
    let denylist = dl(&[host]);
    let mut payload = json!({
        "machine": host,
        "event_id": format!("{host}-1"),
        "agent_id": host,
        "prompt": format!("deploy to {host} now"),
        PROVENANCE_KEY: provenance(&format!("/srv/{host}/p"), &json!(null)),
    });
    assert_eq!(scrub_capture_payload(&mut payload, &denylist).unwrap(), 1);
    assert_eq!(payload["prompt"], "deploy to [REDACTED:denylist] now");
    for identifier in ["machine", "agent_id"] {
        assert_eq!(payload[identifier], host, "{identifier}");
    }
    assert_eq!(payload["event_id"], format!("{host}-1"));
    assert_eq!(
        payload[PROVENANCE_KEY]["facts"]["root"],
        format!("/srv/{host}/p")
    );

    let call = capture_tool_use(
        &json!({ "command": format!("ssh {host} uptime") }),
        Some(&format!("{host}: up 3 days")),
        OutcomeClass::Success,
        &[],
        None,
        &denylist,
    )
    .unwrap();
    assert_eq!(
        call.args,
        json!({ "command": "ssh [REDACTED:denylist] uptime" })
    );
    assert_eq!(
        call.result.as_deref(),
        Some("[REDACTED:denylist]: up 3 days")
    );
    assert_eq!((call.input_found, call.result_found), (1, 1));

    let path = format!("/srv/{host}/.env");
    let stub = capture_tool_use(
        &json!({ "file_path": path }),
        Some("X=1"),
        OutcomeClass::Success,
        std::slice::from_ref(&path),
        None,
        &denylist,
    )
    .unwrap();
    assert!(stub.stub);
    assert_eq!(
        stub.args,
        json!({ "sensitive_path": "/srv/[REDACTED:denylist]/.env" })
    );

    // An identifier gets the machine-token rule alone: a name that reads as high-entropy, or a
    // hex run after a key word, is a validated identifier, never redacted.
    let agent = format!("{}-{}", "api_key", "a3f1b2c4d5e6f7a8".repeat(2));
    let mut ids = json!({
        "machine": "q7m2x9k4v8w3z6p1r5t0y8u2",
        "agent_id": agent,
        "event_id": format!("bf.{}.p", machine_token()),
    });
    assert_eq!(scrub_capture_payload(&mut ids, &denylist).unwrap(), 1);
    assert_eq!(ids["machine"], "q7m2x9k4v8w3z6p1r5t0y8u2");
    assert_eq!(ids["agent_id"], agent);
    assert_eq!(ids["event_id"], "bf.[REDACTED:machine-token].p");
    // Any other event payload is a caller's: no member of it is a validated identifier.
    let mut ingested = json!({ "machine": "q7m2x9k4v8w3z6p1r5t0y8u2", "kind": aws() });
    assert_eq!(scrub_payload(&mut ingested).unwrap(), 2);

    let mut live = json!({ "prompt": format!("deploy to {host} now") });
    assert_eq!(
        scrub_capture_payload(&mut live, &[]).unwrap(),
        0,
        "an empty denylist removes nothing"
    );
}

// ── The cut ──────────────────────────────────────────────────────────────────

#[test]
fn the_cut_keeps_whole_characters() {
    assert_eq!(cut("short"), "short");
    let long = "é".repeat(400); // 800 bytes
    let c = cut(&long);
    assert!(
        c.len() <= CAPTURE_CAP && c.len() >= CAPTURE_CAP - 1,
        "{}",
        c.len()
    );
    assert!(c.chars().all(|ch| ch == 'é'));
}

#[test]
fn a_long_tool_input_becomes_a_string_of_its_first_bytes() {
    let small = json!({ "path": "src/lib.rs" });
    assert_eq!(cut_json(small.clone(), &small.to_string()).unwrap(), small);
    let big = json!({ "content": "x".repeat(600) });
    let compact = big.to_string();
    let Value::String(s) = cut_json(big, &compact).unwrap() else {
        panic!("expected a string")
    };
    assert_eq!(s.len(), CAPTURE_CAP);
    assert!(s.starts_with("{\"content\":\"xxx"));
}

/// A word whose whole entropy is low but whose first 20 characters are all distinct (4.32
/// bits): a cut inside it exposes a match the uncut text does not hold.
fn entropy_trap() -> String {
    format!("{}{}", "Ab3dE5fG7hI9jK1lM2nO", "a".repeat(300))
}

#[test]
fn an_excerpt_is_settled_and_within_the_cap() {
    let text = format!("{} {}", "w".repeat(491), entropy_trap());
    assert_eq!(
        scrub(&text).unwrap(),
        text,
        "precondition: the whole text is clean"
    );
    let out = excerpt(&text).unwrap();
    assert!(out.len() <= CAPTURE_CAP, "{}", out.len());
    assert_eq!(
        scrub(&out).unwrap(),
        out,
        "a later scrub of the excerpt changes nothing"
    );
}

#[test]
fn every_placeholder_is_a_fixpoint() {
    let rules = SECRET_PREFIXES
        .iter()
        .map(|&(rule, _, _)| rule)
        .chain(FIXED_RULES.iter().copied());
    for rule in rules {
        let text = format!("x [REDACTED:{rule}] y");
        assert_eq!(scrub_with(&text, &[]).unwrap(), (text.clone(), 0), "{rule}");
    }
}

proptest::proptest! {
    #[test]
    fn an_excerpt_is_a_settled_short_prefix_of_the_scrubbed_text(
        text in proptest::string::string_regex(&format!("(.|{}){{0,700}}", ghp())).unwrap()
    ) {
        let out = excerpt(&text).unwrap();
        let settled = scrub(&text).unwrap();
        proptest::prop_assert!(out.len() <= CAPTURE_CAP);
        let rescrubbed = scrub(&out).unwrap();
        proptest::prop_assert_eq!(rescrubbed.as_str(), out.as_str());
        // It is the scrubbed text's prefix, but where settling the cut placed a placeholder.
        let same = out.bytes().zip(settled.bytes()).take_while(|(a, b)| a == b).count();
        proptest::prop_assert!(
            same == out.len() || out[same..].starts_with("[REDACTED:") || out[..same].ends_with('['),
            "{out:?} vs {settled:?}"
        );
    }
}
