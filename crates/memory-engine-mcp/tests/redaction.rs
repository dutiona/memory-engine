//! Redaction over HTTP (#1024, AC2): what `remember`, `forget`, `recall` and every call record
//! store, embed and search holds no secret. Every token-shaped value is built at run time.

mod common;

use common::{TestServer, holds, store_bytes};
use serde_json::{Value, json};

const ROOT: &str = "ME-Project-Root";
const DEV: &str = "ME-Project-Dev-Root";
const UPSTREAM: &str = "ME-Project-Upstream-Url";
const ORIGIN: &str = "ME-Project-Origin-Url";

const LAPTOP_DEV: &str = "/home/alice/perso/dev";
const P_KEY: &str = "github.com/example/project";
const P_HOME: &str = "user:alice/project:github.com%2Fexample%2Fproject";

/// AC2's new prefixes, with the rule each placeholder names.
const AC2: [(&str, &str); 16] = [
    ("hf_", "huggingface-token"),
    ("tskey-", "tailscale-key"),
    ("glpat-", "gitlab-pat"),
    ("gho_", "github-token"),
    ("ghu_", "github-token"),
    ("ghs_", "github-token"),
    ("ghr_", "github-token"),
    ("sk_live_", "stripe-key"),
    ("rk_live_", "stripe-key"),
    ("npm_", "npm-token"),
    ("pypi-", "pypi-token"),
    ("AGE-SECRET-KEY-1", "age-secret-key"),
    ("xapp-", "slack-token"),
    ("sbp_", "supabase-token"),
    ("dckr_pat_", "docker-pat"),
    ("dop_v1_", "digitalocean-token"),
];

/// A synthetic token for `prefix`, distinct per `i`.
fn ac2_token(i: usize, prefix: &str) -> String {
    format!("{prefix}{i:02}Zq7Xw3Rt9Lp2Vb6Nm4Kc8Hd")
}

fn machine_token() -> String {
    format!("me_{}", "Zq7Xw3Rt9Lp2Vb6Nm4Kc8Hd1Fs5Gj0Ay")
}

fn ghp() -> String {
    format!("ghp_{}", "0123456789abcdefghij".repeat(2))
}

/// No 8-byte window of `secret` appears in `text`.
fn holds_no_fragment(text: &str, secret: &str) -> bool {
    secret
        .as_bytes()
        .windows(8)
        .all(|w| !holds(text.as_bytes(), w))
}

fn stored_id(reply: &common::Reply) -> i64 {
    assert!(!reply.is_error, "{reply:?}");
    reply.body.split(' ').nth(1).unwrap().parse().unwrap()
}

async fn embedder_bodies(server: &TestServer) -> Vec<String> {
    server
        .embedder
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .collect()
}

#[tokio::test]
async fn every_ac2_prefix_and_a_machine_token_are_redacted_over_http() {
    let mut server = TestServer::start().await;
    let tokens: Vec<String> = AC2
        .iter()
        .enumerate()
        .map(|(i, (prefix, _))| ac2_token(i, prefix))
        .collect();
    let content = format!(
        "keys {} and {} in use me_types::Fact;",
        tokens.join(" "),
        machine_token()
    );
    let reply = server
        .tool("laptop", "remember", json!({ "content": content }))
        .await;
    let id = stored_id(&reply);
    let placeholders: Vec<String> = AC2
        .iter()
        .map(|(_, rule)| format!("[REDACTED:{rule}]"))
        .collect();
    let want = format!(
        "keys {} and [REDACTED:machine-token] in use me_types::Fact;",
        placeholders.join(" ")
    );
    assert_eq!(server.engine.get_fact(id).await.unwrap().content, want);
    server.wait_drained().await;
    let mut secrets = tokens;
    secrets.push(machine_token());
    for body in embedder_bodies(&server).await {
        for secret in &secrets {
            assert!(!body.contains(secret.as_str()), "the embedder saw {secret}");
        }
    }
    server.stop().await;
    for (name, bytes) in &store_bytes(&server) {
        for secret in &secrets {
            assert!(!holds(bytes, secret.as_bytes()), "{name} holds {secret}");
        }
    }
}

#[tokio::test]
async fn a_refusal_records_the_excerpt_of_its_redacted_message() {
    let server = TestServer::start().await;
    // serde quotes the bad level after "unknown variant `" (17 bytes): the token starts at byte
    // 498, so a cut before redaction would keep 14 bytes of it, too few for any rule.
    let token = ghp();
    let level = format!("{} {token}", "x".repeat(480));
    let reply = server
        .tool(
            "laptop",
            "remember",
            json!({ "content": "kestrel", "level": level }),
        )
        .await;
    assert!(reply.is_error, "{reply:?}");
    let records = server.records();
    let error = records.last().unwrap().1["error"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        error.len() <= 512 && holds_no_fragment(&error, &token),
        "{error}"
    );
}

#[tokio::test]
async fn a_forget_reason_is_redacted_then_cut() {
    let server = TestServer::start().await;
    let id = stored_id(
        &server
            .tool("laptop", "remember", json!({ "content": "kestrel kept" }))
            .await,
    );
    // The token starts at byte 498: a cut before redaction would keep 14 bytes of it, too few
    // for any rule; redacting first leaves the placeholder's start.
    let token = ghp();
    let reason = format!("{} {token} trailing words", "r".repeat(497));
    let reply = server
        .tool("laptop", "forget", json!({ "id": id, "reason": reason }))
        .await;
    assert!(!reply.is_error, "{reply:?}");
    let state = server.engine.explain_fact(id).await.unwrap().state;
    let memory_engine::inspect::FactState::Expired {
        reason: memory_engine::inspect::ExpiredReason::Forgotten { reason, .. },
    } = state
    else {
        panic!("explained as forgotten: {state:?}");
    };
    let reason = reason.unwrap();
    assert_eq!(
        reason,
        format!("{} {}", "r".repeat(497), &"[REDACTED:github-pat]"[..14])
    );
    assert!(holds_no_fragment(&reason, &token), "{reason}");
}

fn facts(pairs: &[(&'static str, String)]) -> Vec<(&'static str, Vec<u8>)> {
    pairs
        .iter()
        .map(|(n, v)| (*n, v.clone().into_bytes()))
        .collect()
}

/// `remember` in a session with `facts`; the call's record, by its call id.
async fn remember_with(
    server: &TestServer,
    facts: &[(&'static str, Vec<u8>)],
) -> (common::Reply, Value, String) {
    let borrowed: Vec<(&str, &[u8])> = facts.iter().map(|(n, v)| (*n, v.as_slice())).collect();
    let reply = server
        .tool_with(
            "laptop",
            &borrowed,
            "remember",
            json!({ "content": "kestrel kept" }),
        )
        .await;
    assert!(!reply.is_error, "{reply:?}");
    let (_, payload, scope) = server
        .records()
        .into_iter()
        .find(|(_, p, _)| p["call"] == reply.call.as_str())
        .expect("the call's record");
    (reply, payload, scope)
}

#[tokio::test]
async fn provenance_keeps_its_paths_and_drops_machine_tokens() {
    let mut server = TestServer::start().await;
    // A dot-free UUID root is kept byte for byte, with and without a key.
    let uuid_root = format!("{LAPTOP_DEV}/9932fd5f-1a85-4237-809b-5e6ae329dda1");
    for with_key in [true, false] {
        let mut pairs = vec![(ROOT, uuid_root.clone())];
        if with_key {
            pairs.push((DEV, LAPTOP_DEV.to_owned()));
        }
        let (_, payload, _) = remember_with(&server, &facts(&pairs)).await;
        assert_eq!(payload["project"]["facts"]["root"], uuid_root, "{payload}");
        assert_eq!(payload["project"]["dropped"], json!({}), "{payload}");
        let key = with_key.then_some("laptop:9932fd5f-1a85-4237-809b-5e6ae329dda1");
        assert_eq!(payload["project"]["key"], json!(key), "{payload}");
    }

    // A root holding a machine token is dropped and flagged; the key comes from what is left.
    let token_root = format!("{LAPTOP_DEV}/{}", machine_token());
    for (origin, key, home) in [
        (
            Some("https://github.com/example/project"),
            Some(P_KEY),
            P_HOME,
        ),
        (None, None, "unscoped:laptop"),
    ] {
        let mut pairs = vec![(ROOT, token_root.clone()), (DEV, LAPTOP_DEV.to_owned())];
        if let Some(origin) = origin {
            pairs.push((ORIGIN, origin.to_owned()));
        }
        let (_, payload, scope) = remember_with(&server, &facts(&pairs)).await;
        let project = &payload["project"];
        assert_eq!(project["dropped"]["root"], "machine_token", "{project}");
        assert!(project["facts"].get("root").is_none(), "{project}");
        assert_eq!(project["key"], json!(key), "{project}");
        assert_eq!(scope, home, "the record sits where the key put it");
    }

    // A checkout named like a token prefix keeps its root and key.
    let sk_root = format!("{LAPTOP_DEV}/sk-learn-tutorial-2024");
    let (_, payload, _) = remember_with(
        &server,
        &facts(&[(ROOT, sk_root.clone()), (DEV, LAPTOP_DEV.to_owned())]),
    )
    .await;
    assert_eq!(payload["project"]["facts"]["root"], sk_root);
    assert_eq!(payload["project"]["key"], "laptop:sk-learn-tutorial-2024");

    // A remote whose host keys to a machine token is dropped; the key falls through.
    let host = format!("ME_{}", "Zq7Xw3Rt9Lp2Vb6Nm4Kc8Hd1Fs5Gj0Ay");
    let remote = format!("https://{host}/example/project");
    let (_, payload, _) = remember_with(
        &server,
        &facts(&[
            (UPSTREAM, remote.clone()),
            (ORIGIN, "https://github.com/example/project".to_owned()),
        ]),
    )
    .await;
    assert_eq!(
        payload["project"]["dropped"]["upstream_url"],
        "machine_token"
    );
    assert_eq!(payload["project"]["key"], P_KEY);
    let (_, payload, _) = remember_with(
        &server,
        &facts(&[
            (ORIGIN, remote),
            (ROOT, format!("{LAPTOP_DEV}/notes")),
            (DEV, LAPTOP_DEV.to_owned()),
        ]),
    )
    .await;
    assert_eq!(payload["project"]["dropped"]["origin_url"], "machine_token");
    assert_eq!(payload["project"]["key"], "laptop:notes");

    server.stop().await;
    let lowered = host.to_lowercase();
    for (name, bytes) in &store_bytes(&server) {
        for secret in [machine_token(), host.clone(), lowered.clone()] {
            assert!(!holds(bytes, secret.as_bytes()), "{name} holds {secret}");
        }
    }
}

#[tokio::test]
async fn a_recall_quoting_a_token_searches_its_placeholder() {
    let server = TestServer::start().await;
    let token = ghp();
    let id = stored_id(
        &server
            .tool(
                "laptop",
                "remember",
                json!({ "content": format!("deploy key {token}") }),
            )
            .await,
    );
    server.wait_drained().await;
    let reply = server
        .tool(
            "laptop",
            "recall",
            json!({ "query": format!("which {token}") }),
        )
        .await;
    assert!(!reply.is_error, "{reply:?}");
    let found = reply.json()["results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["id"].as_i64() == Some(id));
    assert!(found, "{reply:?}");
    let bodies = embedder_bodies(&server).await;
    assert!(
        bodies.iter().all(|b| !b.contains(&token)),
        "the embedder saw the token"
    );
    assert!(
        bodies
            .iter()
            .any(|b| b.contains("which [REDACTED:github-pat]")),
        "the embedder saw the placeholder: {bodies:?}"
    );
}

/// A cut inside a long low-entropy word can expose a match the whole word does not hold; the
/// stored reason is settled, so the facade's own scrub never grows it past 512 bytes.
#[tokio::test]
async fn a_stored_reason_stays_within_the_cap() {
    let server = TestServer::start().await;
    let id = stored_id(
        &server
            .tool("laptop", "remember", json!({ "content": "kestrel kept" }))
            .await,
    );
    let reason = format!(
        "{} {}{}",
        " ".repeat(491),
        "Ab3dE5fG7hI9jK1lM2nO",
        "a".repeat(300)
    );
    let reply = server
        .tool("laptop", "forget", json!({ "id": id, "reason": reason }))
        .await;
    assert!(!reply.is_error, "{reply:?}");
    let state = server.engine.explain_fact(id).await.unwrap().state;
    let memory_engine::inspect::FactState::Expired {
        reason: memory_engine::inspect::ExpiredReason::Forgotten { reason, .. },
    } = state
    else {
        panic!("explained as forgotten: {state:?}");
    };
    let reason = reason.unwrap();
    assert!(reason.len() <= 512, "{} bytes: {reason}", reason.len());
}
