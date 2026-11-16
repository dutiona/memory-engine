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

/// A store one schema version behind, so opening it runs a migration.
fn stale_store(dir: &std::path::Path) -> u32 {
    let db = dir.join("memory.db");
    drop(
        memory_engine::MemoryEngine::builder(common::DIM)
            .path(db.clone())
            .build()
            .unwrap(),
    );
    let previous = memory_engine::CURRENT_SCHEMA_VERSION - 1;
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute(
            "UPDATE config SET value = ?1 WHERE key = 'schema_version'",
            [previous.to_string()],
        )
        .unwrap();
    previous
}

fn schema_version(dir: &std::path::Path) -> String {
    rusqlite::Connection::open(dir.join("memory.db"))
        .unwrap()
        .query_row(
            "SELECT value FROM config WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn backup_dir_from_toml() {
    let dir = tempfile::tempdir().unwrap();
    let previous = stale_store(dir.path());
    let backups = dir.path().join("backups");
    std::fs::create_dir(&backups).unwrap();
    common::write_config(
        dir.path(),
        &format!("backup_dir = '{}'\n", backups.display()),
    );
    let mut server = ServerProcess::spawn(dir.path(), &[], &[]);
    // `migrate` writes the backup before any step runs, so it exists whether or not the
    // relabelled store's last migration replays cleanly.
    let _ = server.wait_ready();
    assert!(
        backups.join(format!("memory.db.v{previous}.bak")).is_file(),
        "stderr:\n{}",
        server.stderr()
    );
}

#[test]
fn backup_dir_from_environment() {
    let dir = tempfile::tempdir().unwrap();
    let previous = stale_store(dir.path());
    let backups = dir.path().join("backups");
    std::fs::create_dir(&backups).unwrap();
    let mut server = ServerProcess::spawn(
        dir.path(),
        &[],
        &[("MEMORY_MCP_BACKUP_DIR", backups.to_str().unwrap())],
    );
    let _ = server.wait_ready();
    assert!(
        backups.join(format!("memory.db.v{previous}.bak")).is_file(),
        "stderr:\n{}",
        server.stderr()
    );
}

#[test]
fn a_missing_backup_dir_stops_startup() {
    let dir = tempfile::tempdir().unwrap();
    let previous = stale_store(dir.path());
    let backups = dir.path().join("no-such-backups");
    let mut server = ServerProcess::spawn(
        dir.path(),
        &[],
        &[("MEMORY_MCP_BACKUP_DIR", backups.to_str().unwrap())],
    );
    let status = server.wait_exit(Duration::from_secs(10));
    assert_eq!(status.code(), Some(1), "stderr:\n{}", server.stderr());
    assert!(
        server.stderr().contains("no-such-backups"),
        "stderr:\n{}",
        server.stderr()
    );
    assert_eq!(schema_version(dir.path()), previous.to_string());
}

#[test]
fn starts_with_an_embedder_and_a_summary_endpoint() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mock = runtime.block_on(wiremock::MockServer::start());
    let dir = tempfile::tempdir().unwrap();
    let embed_url = format!("{}/v1/embeddings", mock.uri());
    let summary_url = format!("{}/v1/chat/completions", mock.uri());
    let mut server = ServerProcess::spawn(
        dir.path(),
        &[
            "--embed-url",
            &embed_url,
            "--embed-model",
            "m",
            "--summary-url",
            &summary_url,
            "--summary-model",
            "s",
        ],
        &[],
    );
    assert!(server.wait_ready().is_ok(), "stderr:\n{}", server.stderr());
    assert!(server.terminate().success(), "stderr:\n{}", server.stderr());
}

#[test]
fn the_backup_dir_flag_and_variable_win_over_toml() {
    let dir = tempfile::tempdir().unwrap();
    let previous = stale_store(dir.path());
    let from_toml = dir.path().join("from-toml");
    let from_env = dir.path().join("from-env");
    std::fs::create_dir(&from_toml).unwrap();
    std::fs::create_dir(&from_env).unwrap();
    common::write_config(
        dir.path(),
        &format!("backup_dir = '{}'\n", from_toml.display()),
    );
    let mut server = ServerProcess::spawn(
        dir.path(),
        &[],
        &[("MEMORY_MCP_BACKUP_DIR", from_env.to_str().unwrap())],
    );
    let _ = server.wait_ready();
    let backup = format!("memory.db.v{previous}.bak");
    assert!(from_env.join(&backup).is_file(), "stderr:\n{}", server.stderr());
    assert!(!from_toml.join(&backup).exists());
}
