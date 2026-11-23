//! What only `main.rs` owns, driven through the built binary: flags, environment
//! variables, startup order and signals.

mod common;

use std::time::{Duration, Instant};

use common::{ServerProcess, token_file_line};
use serde_json::{Value, json};

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
        server
            .stderr()
            .contains("the backup directory is not a directory"),
        "stderr:\n{}",
        server.stderr()
    );
    assert!(
        server.stderr().contains("no-such-backups"),
        "stderr:\n{}",
        server.stderr()
    );
    assert_eq!(schema_version(dir.path()), previous.to_string());
}

#[test]
fn starts_with_an_embedder() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mock = runtime.block_on(wiremock::MockServer::start());
    let dir = tempfile::tempdir().unwrap();
    let embed_url = format!("{}/v1/embeddings", mock.uri());
    let mut server = ServerProcess::spawn(
        dir.path(),
        &["--embed-url", &embed_url, "--embed-model", "m"],
        &[],
    );
    assert!(server.wait_ready().is_ok(), "stderr:\n{}", server.stderr());
    assert!(server.terminate().success(), "stderr:\n{}", server.stderr());
}

/// #1101: the key and the key file at once refuse to start, before the store opens, naming
/// both settings and neither secret.
#[test]
fn a_key_and_a_key_file_at_once_stop_startup_before_the_store_opens() {
    let dir = tempfile::tempdir().unwrap();
    let key_file = dir.path().join("embed-key");
    let file_key = format!("file-key-{}", std::process::id());
    let env_key = format!("env-key-{}", std::process::id());
    std::fs::write(&key_file, &file_key).unwrap();
    let mut server = ServerProcess::spawn(
        dir.path(),
        &[],
        &[
            ("MEMORY_MCP_EMBED_URL", "http://127.0.0.1:9/v1/embeddings"),
            ("MEMORY_MCP_EMBED_MODEL", "m"),
            ("MEMORY_MCP_EMBED_API_KEY", &env_key),
            ("MEMORY_MCP_EMBED_API_KEY_FILE", key_file.to_str().unwrap()),
        ],
    );
    let status = server.wait_exit(Duration::from_secs(20));
    let stderr = server.stderr();
    assert_eq!(status.code(), Some(1), "stderr:\n{stderr}");
    assert!(
        stderr.contains("MEMORY_MCP_EMBED_API_KEY_FILE"),
        "stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("MEMORY_MCP_EMBED_API_KEY;"),
        "stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains(&file_key) && !stderr.contains(&env_key),
        "stderr:\n{stderr}"
    );
    assert!(
        !dir.path().join("memory.db").exists(),
        "refused before the store opens"
    );
}

/// #1101: a key read from a file is held like a key given directly: never logged. Every crate
/// logs at trace while the key is sent to the embedder and a machine token authenticates a call
/// (#1149), so neither may reach the log through a dependency's trace output.
#[test]
fn a_key_file_never_reaches_the_log() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mock = runtime.block_on(wiremock::MockServer::start());
    runtime.block_on(
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1/embeddings"))
            .respond_with(common::an_embedding())
            .mount(&mock),
    );
    let dir = tempfile::tempdir().unwrap();
    let key_file = dir.path().join("embed-key");
    let key = format!("file-key-{}", std::process::id());
    std::fs::write(&key_file, format!("{key}\n")).unwrap();
    let embed_url = format!("{}/v1/embeddings", mock.uri());
    let mut server = ServerProcess::spawn(
        dir.path(),
        &[
            "--embed-url",
            &embed_url,
            "--embed-model",
            "m",
            "--embed-api-key-file",
            key_file.to_str().unwrap(),
        ],
        &[("RUST_LOG", "trace")],
    );
    assert!(server.wait_ready().is_ok(), "stderr:\n{}", server.stderr());
    // An authenticated call that embeds its query with the key.
    let recalled = answer(&server.tool("laptop", "recall", &json!({ "query": "kestrel" })));
    assert!(recalled.starts_with('{'), "{recalled}");
    let sent = runtime.block_on(mock.received_requests()).unwrap();
    assert!(
        sent.iter().any(|r| r
            .headers
            .get("authorization")
            .is_some_and(|v| v.as_bytes() == format!("Bearer {key}").as_bytes())),
        "the key reached the embedder"
    );
    assert!(server.terminate().success(), "stderr:\n{}", server.stderr());
    let log = server.stderr();
    assert!(
        log.contains(" TRACE "),
        "nothing was logged at trace:\n{log}"
    );
    assert!(!log.contains(&key), "the key is in the log:\n{log}");
    assert!(
        !log.contains(&common::token("laptop")),
        "a machine token is in the log:\n{log}"
    );
}

/// #1149: a bare level applies to every crate, so readiness' `listening on` line is logged.
#[test]
fn a_bare_rust_log_level_logs_the_listening_line() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = ServerProcess::spawn(dir.path(), &[], &[("RUST_LOG", "info")]);
    assert!(server.wait_ready().is_ok(), "stderr:\n{}", server.stderr());
    assert!(server.terminate().success(), "stderr:\n{}", server.stderr());
}

/// The text of a model tool's answer, its call line removed.
fn answer(response: &Value) -> String {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no tool result in {response}"));
    text.split_once('\n').unwrap().1.to_owned()
}

#[test]
fn starts_serves_and_exits_beside_a_silent_embedder() {
    // Bound and never accepted: the kernel takes each connection and nothing ever answers.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dir = tempfile::tempdir().unwrap();
    common::write_config(
        dir.path(),
        &format!(
            "[embedding]\nendpoint = 'http://{}/v1/embeddings'\nmodel = 'm'\nprovider = 'tei'\n\
             dimensions = {}\ntimeout_secs = 30\n",
            silent.local_addr().unwrap(),
            common::DIM
        ),
    );
    let mut server = ServerProcess::spawn(dir.path(), &[], &[]);
    assert!(server.wait_ready().is_ok(), "stderr:\n{}", server.stderr());

    let stored = answer(&server.tool(
        "laptop",
        "remember",
        &json!({ "content": "the kestrel nests on the cliff" }),
    ));
    assert!(stored.starts_with("stored "), "{stored}");

    let started = Instant::now();
    let recalled = answer(&server.tool("laptop", "recall", &json!({ "query": "kestrel" })));
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "{:?}",
        started.elapsed()
    );
    let recalled: Value = serde_json::from_str(&recalled).unwrap();
    assert_eq!(recalled["degraded"], "embedder slow", "{recalled}");

    // The drain's embed call and the recall's abandoned one both still wait on the embedder.
    let status = server.terminate();
    assert!(status.success(), "{status}; stderr:\n{}", server.stderr());
    drop(silent);
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
    assert!(
        from_env.join(&backup).is_file(),
        "stderr:\n{}",
        server.stderr()
    );
    assert!(!from_toml.join(&backup).exists());
    drop(server);

    // The flag, over the same TOML, with the store relabelled again.
    let from_flag = dir.path().join("from-flag");
    std::fs::create_dir(&from_flag).unwrap();
    stale_store(dir.path());
    let mut server = ServerProcess::spawn(
        dir.path(),
        &["--backup-dir", from_flag.to_str().unwrap()],
        &[],
    );
    let _ = server.wait_ready();
    assert!(
        from_flag.join(&backup).is_file(),
        "stderr:\n{}",
        server.stderr()
    );
    assert!(!from_toml.join(&backup).exists());
}

#[test]
fn help_never_prints_an_api_key_from_the_environment() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_memory-engine-mcp"))
        .arg("--help")
        .env("MEMORY_MCP_EMBED_API_KEY", "embed-key-sentinel")
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("MEMORY_MCP_EMBED_API_KEY"), "{help}");
    assert!(!help.contains("embed-key-sentinel"), "{help}");
}

#[test]
fn the_summary_settings_are_refused_before_the_store_opens() {
    // ADR-0020 §8: the derived jobs are off in the server, so it has no summary generator.
    // A leftover flag or section refuses to start instead of being silently ignored.
    let dir = tempfile::tempdir().unwrap();
    let mut server = ServerProcess::spawn(
        dir.path(),
        &["--summary-url", "http://127.0.0.1:9/v1/chat/completions"],
        &[],
    );
    let status = server.wait_exit(Duration::from_secs(20));
    assert_eq!(status.code(), Some(2), "stderr:\n{}", server.stderr());
    assert!(
        server.stderr().contains("--summary-url"),
        "stderr:\n{}",
        server.stderr()
    );

    common::write_config(
        dir.path(),
        "[summary]\nendpoint = 'http://127.0.0.1:9'\nmodel = 's'\n",
    );
    let mut server = ServerProcess::spawn(dir.path(), &[], &[]);
    let status = server.wait_exit(Duration::from_secs(20));
    assert_eq!(status.code(), Some(1), "stderr:\n{}", server.stderr());
    assert!(
        server.stderr().contains("unknown field"),
        "stderr:\n{}",
        server.stderr()
    );
    assert!(
        !dir.path().join("memory.db").exists(),
        "refused before the store opens"
    );
}

#[test]
fn the_running_server_holds_the_store_lock() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("memory.db");
    let mut server = ServerProcess::spawn(dir.path(), &[], &[]);
    server.wait_ready().unwrap();
    assert!(matches!(
        memory_engine::StoreLock::acquire(&db),
        Err(memory_engine::StoreLockError::Held { .. })
    ));
    assert!(server.terminate().success(), "stderr:\n{}", server.stderr());
    assert!(memory_engine::StoreLock::acquire(&db).is_ok());
}

#[test]
fn a_held_store_lock_stops_startup_before_the_store_migrates() {
    let dir = tempfile::tempdir().unwrap();
    let previous = stale_store(dir.path());
    let _held = memory_engine::StoreLock::acquire(&dir.path().join("memory.db")).unwrap();
    let mut server = ServerProcess::spawn(dir.path(), &[], &[]);
    let status = server.wait_exit(Duration::from_secs(10));
    assert_eq!(status.code(), Some(1), "stderr:\n{}", server.stderr());
    assert!(
        server.stderr().contains("memory.db.store-lock"),
        "stderr:\n{}",
        server.stderr()
    );
    assert!(!server.stderr().contains("listening on"));
    assert_eq!(schema_version(dir.path()), previous.to_string());
}

#[test]
fn a_second_server_on_the_same_store_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut first = ServerProcess::spawn(dir.path(), &[], &[]);
    first.wait_ready().unwrap();
    let mut second = ServerProcess::spawn(dir.path(), &[], &[]);
    let status = second.wait_exit(Duration::from_secs(10));
    assert_eq!(status.code(), Some(1), "stderr:\n{}", second.stderr());
    assert!(
        second.stderr().contains("memory.db.store-lock"),
        "stderr:\n{}",
        second.stderr()
    );
    first.wait_ready().unwrap();
}

#[test]
fn the_user_setting_is_required_and_validated() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::write_config(dir.path(), "");
    let config = config.to_str().unwrap();
    let mut missing = ServerProcess::spawn_without_user(dir.path(), &["--config", config]);
    let status = missing.wait_exit(Duration::from_secs(10));
    assert_eq!(status.code(), Some(2), "stderr:\n{}", missing.stderr());
    assert!(
        missing.stderr().contains("--user"),
        "stderr:\n{}",
        missing.stderr()
    );

    for (args, envs) in [
        (vec!["--user", "Alice"], vec![]),
        (vec![], vec![("MEMORY_MCP_USER", "a/b")]),
    ] {
        let mut server = ServerProcess::spawn(dir.path(), &args, &envs);
        let status = server.wait_exit(Duration::from_secs(10));
        assert_eq!(status.code(), Some(2), "stderr:\n{}", server.stderr());
        assert!(
            server.stderr().contains("--user"),
            "stderr:\n{}",
            server.stderr()
        );
    }
    assert!(!dir.path().join("memory.db").exists());

    let mut server = ServerProcess::spawn(dir.path(), &[], &[("MEMORY_MCP_USER", "bob")]);
    server.wait_ready().unwrap();
}
/// SIGKILL after the 204: the server dies with no chance to flush or close, the client resends
/// (it never saw the answer it needs, or saw it and the outbox is replayed), and the store holds
/// the event once.
#[test]
fn a_killed_server_stores_a_resent_event_once() {
    let dir = tempfile::tempdir().unwrap();
    let event = common::fixture("stop");
    let mut server = ServerProcess::spawn(dir.path(), &[], &[]);
    server.wait_ready().unwrap();
    let posted = server.post_event("laptop", &event);
    assert!(posted.is(204, Some("stored")), "{posted:?}");

    server.child.kill().unwrap();
    server.child.wait().unwrap();
    drop(server);
    let mut server = ServerProcess::spawn(dir.path(), &[], &[]);
    server.wait_ready().unwrap();
    let resent = server.post_event("laptop", &event);

    assert!(resent.is(204, Some("duplicate")), "{resent:?}");
    let count = |sql: &str| -> i64 {
        rusqlite::Connection::open(dir.path().join("memory.db"))
            .unwrap()
            .query_row(sql, [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(
        count("SELECT COUNT(*) FROM events WHERE source = 'lifecycle'"),
        1
    );
    assert_eq!(count("SELECT COUNT(*) FROM lifecycle_receipts"), 1);
}

/// A quarantined body is metadata only, on disk and in the log: the server logs the machine, the
/// event ID and the status, never a byte of what it could not parse.
#[test]
fn a_quarantine_logs_no_body_byte() {
    let dir = tempfile::tempdir().unwrap();
    let mut server =
        ServerProcess::spawn(dir.path(), &[], &[("RUST_LOG", "memory_engine_mcp=debug")]);
    server.wait_ready().unwrap();
    let token = common::ghp();
    let path = format!("/work/fixture/{}", ".env");
    let body = format!(r#"{{"session_id":"s","note":"{token} {path}"#);
    let event = common::fixture("stop")
        .header("ME-Event-Id", "quarantine-1")
        .body(body);

    let posted = server.post_event("laptop", &event);

    assert!(posted.is(204, Some("quarantined")), "{posted:?}");
    server.terminate();
    let log = server.stderr();
    assert!(
        log.contains("quarantine-1") && log.contains("laptop"),
        "the log names the event:\n{log}"
    );
    for secret in [token.as_str(), path.as_str(), "session_id"] {
        assert!(!log.contains(secret), "the log holds {secret:?}:\n{log}");
    }
}
/// An event ID is a client's string, and `me_` followed by 32 base62 characters is within its
/// grammar: a machine token pasted there must not reach the log, on any path that names the ID.
#[test]
fn an_event_id_shaped_like_a_token_never_reaches_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = ServerProcess::spawn(dir.path(), &[], &[]);
    server.wait_ready().unwrap();
    let token = common::machine_token();
    let event = common::fixture("stop").header("ME-Event-Id", &token);

    // The engine refuses the identifier: a 422, logged.
    let refused = server.post_event("laptop", &event);
    assert_eq!(refused.status, 422, "{refused:?}");
    // A body-read refusal, logged before the engine is asked: a 413 from the headers alone.
    let over = u64::try_from(memory_engine_mcp::lifecycle::BODY_LIMIT).unwrap() + 1;
    assert_eq!(server.status_from_headers("laptop", &event, over), 413);

    server.terminate();
    let log = server.stderr();
    assert!(
        log.contains("lifecycle event refused") && log.contains("lifecycle body refused"),
        "both refusals are logged:\n{log}"
    );
    assert!(
        log.contains("[REDACTED:machine-token]"),
        "the ID is logged in its redacted form:\n{log}"
    );
    assert!(!log.contains(&token), "the log holds the token:\n{log}");
}
