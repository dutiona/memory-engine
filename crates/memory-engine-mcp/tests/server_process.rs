//! What only `main.rs` owns, driven through the built binary: flags, environment
//! variables, startup order and signals.

mod common;

use std::time::Duration;

use common::{ServerProcess, token_file_line};

#[test]
fn a_bad_token_file_stops_startup_before_the_store_opens() {
    let dir = tempfile::tempdir().unwrap();
    let tokens = dir.path().join("tokens");
    let bad = token_file_line("desktop")
        .to_uppercase()
        .replace("DESKTOP", "desktop");
    std::fs::write(&tokens, token_file_line("laptop") + &bad).unwrap();
    let mut server = ServerProcess::spawn_with_token_file(dir.path(), &tokens, &[], &[]);
    let status = server.wait_exit(Duration::from_secs(10));
    assert_eq!(status.code(), Some(1), "stderr:\n{}", server.stderr());
    assert!(
        server.stderr().contains("line 2"),
        "stderr:\n{}",
        server.stderr()
    );
    assert!(!dir.path().join("memory.db").exists());

    let missing = dir.path().join("no-such-tokens");
    let mut server = ServerProcess::spawn_with_token_file(dir.path(), &missing, &[], &[]);
    let status = server.wait_exit(Duration::from_secs(10));
    assert_eq!(status.code(), Some(1), "stderr:\n{}", server.stderr());
    assert!(
        server.stderr().contains("no-such-tokens"),
        "stderr:\n{}",
        server.stderr()
    );
}

#[test]
fn sigterm_flushes_the_snapshot_and_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = ServerProcess::spawn(dir.path(), &[], &[]);
    server.wait_ready().unwrap();
    let snapshot = dir.path().join("memory.db.snapshot");
    assert!(!snapshot.exists());

    let status = server.terminate();

    assert!(status.success(), "{status}; stderr:\n{}", server.stderr());
    assert!(snapshot.is_file());
}

fn status_for_host(server: &ServerProcess, host: &str) -> u16 {
    reqwest::blocking::Client::new()
        .post(format!("{}/mcp", server.url()))
        .bearer_auth(common::token("laptop"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("Host", host)
        .body(common::jsonrpc("tools/list", &serde_json::json!({})).to_string())
        .send()
        .unwrap()
        .status()
        .as_u16()
}

#[test]
fn allowed_host_from_flag_and_environment() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = ServerProcess::spawn(dir.path(), &["--allowed-host", "memory.example"], &[]);
    server.wait_ready().unwrap();
    assert_eq!(status_for_host(&server, "memory.example"), 200);
    assert_eq!(status_for_host(&server, "third.example"), 403);
    drop(server);

    let mut server = ServerProcess::spawn(
        dir.path(),
        &[],
        &[("MEMORY_MCP_ALLOWED_HOSTS", "memory.example,other.example")],
    );
    server.wait_ready().unwrap();
    assert_eq!(status_for_host(&server, "memory.example"), 200);
    assert_eq!(status_for_host(&server, "other.example"), 200);
    assert_eq!(status_for_host(&server, "third.example"), 403);
    drop(server);

    let mut server = ServerProcess::spawn(dir.path(), &[], &[("MEMORY_MCP_ALLOWED_HOSTS", "*")]);
    let status = server.wait_exit(Duration::from_secs(10));
    assert_eq!(status.code(), Some(2), "stderr:\n{}", server.stderr());
    assert!(
        server.stderr().contains("an allowed host is a host name"),
        "stderr:\n{}",
        server.stderr()
    );
}
