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
