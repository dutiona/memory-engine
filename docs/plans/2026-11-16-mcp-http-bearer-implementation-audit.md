# Implementation audit (pre-review): 2026-11-16-mcp-http-bearer

- Approved plan: `docs/plans/2026-11-16-mcp-http-bearer.md`, SHA-256
  `611a7deb309d1f0d722cba35bb23920187f2d0c975792994c5c7646e53261f74` (approved on plan issue #1054).
- Scope: pre-review. Audited head: `1fe87373f92b4360527ecfe30a88671ca3cad1ca` (the branch rebased onto origin/main
  `93bcb5d`); this audit's own commit follows it.
- Plan items: 11 (0-10).

| Item | Status | Evidence |
|---|---|---|
| 0. Baseline | Implemented | `cargo build --workspace --locked` rc 0; `cargo test -p memory-engine-mcp --all-features` 0 failed; `cargo update -p rmcp --dry-run` locks 0 packages (no newer 1.x within the requirement) |
| 1. Token-hash file and check | Implemented | `src/auth.rs`; 9 unit tests red on `todo!()`, then green |
| 2. HTTP behind the auth layer, stdio removed | Implemented | `src/http.rs`, `tests/common/mod.rs`, `tests/http_auth.rs`, `tests/server_process.rs`; with the layer weakened away the 401 test went red (200), restored by byte copy |
| 3. Calling machine | Implemented | `server.rs` `calling_machine`/`record_machine`; HTTP test red on `"forged"`, unit tests red on compile, then green |
| 4. Drain and flush on SIGTERM | Implemented | flush alone left `shutdown_waits_for_an_abandoned_call` red (0 facts), the fence made it green; SIGTERM process test exits 0 with the sidecar. Intent-preserving variation: the write guard is `mem::forget`-ed as soon as it is taken, before the flush, rather than after it: the lock is held for good either way, and it avoids a `significant_drop_tightening` false positive |
| 5. Configured Hosts | Implemented | process test red on clap's exit 2, then green; `parse_allowed_host` unit tests |
| 6. `engine.backup_dir` | Implemented | config test red on compile; three process tests red, then green |
| 7. Blocking clients outside the runtime + doc comment | Implemented | process test reproduced the debug panic ("Cannot drop a runtime…"), green with `block_in_place`; separate doc commit |
| 8. `flush_snapshot` doc | Implemented | doc commit; `cargo doc -p memory-engine` with the intra-doc flags passes |
| 9. Documentation | Implemented | `docs/reference/mcp-server.md`, `docs/getting-started/installation.md` |
| 10. Rebase, gate, PR | Implemented (pre-review part) | rebased onto origin/main (#1017 has not landed, so no #1017 test exists to reroute); full gate green at `1fe8737`; PR opened next |
| Added: release-checklist precondition | Added | Michaël's decision on plan question 1 (2026-11-16): one line in `.claude/skills/release-checklist/SKILL.md`, its own commit, besides the PR-body note |

Counts: Implemented 11, Modified 0, Dropped 0, Incomplete 0, Added 1. Divergence (Modified + Dropped + Added) / 11 =
9%, below the 30% threshold; no successor plan.
