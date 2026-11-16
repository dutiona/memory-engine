# feat(mcp): serve MCP over streamable HTTP behind per-machine Bearer auth (#1018)

## Outcome

`memory-engine-mcp` answers MCP only through rmcp 1.8's streamable-HTTP service on `POST /mcp`, stateless, with
JSON responses. One axum layer wraps the whole router: every request without exactly one `Authorization: Bearer
<token>` whose SHA-256 is in the ADR-0020 §4 token-hash file gets 401, on any method and any path, before rmcp reads
it. A valid token puts its `Machine` on the request; `call_tool` refuses to run without one, and `memory_add_fact`
records it as a server-set `metadata.machine`. Configured `Host` values are added to rmcp's loopback list. SIGTERM or
SIGINT stops accepting, waits for every tool call already started, flushes the `<db>.snapshot` sidecar and exits 0.
`engine.backup_dir` reaches the engine builder from TOML or the environment. stdio is gone. `tests/common/` is the
harness that #1020, #1021, #1022 and #1025 build on.

## Constraints

- Issue #1018 acceptance, verbatim scope; ADR-0020 (merged 6a03fb1) and ADR-0021 (merged 0bf36a3) win over any
  restatement in it.
- Token: `me_` + 32 `[0-9A-Za-z]` (35 bytes). Header exactly `Authorization: Bearer ` + token. A value off that shape
  gets 401 without being hashed; otherwise its SHA-256 is compared in constant time (ADR-0020 §4).
- Token-hash file: one `<machine> <sha256-hex>\n` per machine, last line included; machine a DNS label
  `[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?`; 64 lowercase hex; exactly one space; no comment, blank line, CR or other
  whitespace. Missing/unreadable file, malformed line, repeated machine or hash refuses to start and names the line.
  An empty file is valid and admits no one. Read at startup only. Test vector: `me_` + 32 × `0` →
  `f7ca4ebc8eb419b37dea09411716d9aa5506b9e5572f1a5e8b3170e815ee13aa`.
- 401: no token, a malformed one, or one whose hash is not in the file (ADR-0020 §3). No unauthenticated route; the
  container health probe expects 401.
- rmcp stays on `1` (1.8.0 locked), server features only. `deny.toml` keeps banning `client` and
  `transport-streamable-http-client` (RUSTSEC-2026-0462); tests drive HTTP with `reqwest`, never rmcp's client.
- `allowed_hosts` configurable; a configured non-loopback Host is served; never `*`, never empty, never
  `disable_allowed_hosts`.
- `engine.backup_dir` configurable in TOML and through an environment variable, passed to the builder.
- SIGTERM: stop accepting, flush the snapshot, exit, so a container stop is not a kill plus a projection rebuild.
- Public repository: no tailnet name, address, deploy port, token or hook payload anywhere. Example host
  `memory.example`; test tokens are generated at run time.
- No new SQL schema. #1017 edits `main.rs` and `config.rs` in parallel: keep those diffs focused; the second to land
  rebases.
- Commits: Conventional Commits, imperative, body says why, no co-author line. TDD: watch each test fail first.

### Decisions

1. **Modules.** `src/auth.rs` (no axum): `Machine`, `TokenTable`, `TokenFileError`. `src/http.rs`:
   `pub async fn serve(..)`, the private `router()`, the `require_machine` middleware and `parse_allowed_host`.
   `main.rs` is the composition root. `main.rs` and `tests/common` call the same `serve`, so no test passes on a
   router the binary does not run.
2. **Check order.** Auth layer (401, any path, 404 fallback included: axum 0.8 `Router::layer` wraps the fallback)
   → `/mcp` only: rmcp Host check (403) → POST only (405) → handler. An unauthenticated caller learns nothing about
   hosts, methods or paths. A 403 can appear only on `/mcp` for an authenticated request; ADR-0020's "never 403" row
   governs the lifecycle routes, which #1025 adds as axum routes outside rmcp. `.layer(..)` stays the last call in
   `router()`, with one comment saying why (a route added after it is uncovered). GET and DELETE with a valid token
   get 405 (stateless mode serves POST only, rmcp `tower.rs` `handle`): the acceptance's "with a valid token, 200"
   is asserted on `initialize`, `tools/list` and `tools/call`.
3. **The middleware.** Exactly one `Authorization` value, else 401. On success it inserts the `Machine` into the
   request extensions and removes `Authorization`, so no handler or log downstream ever holds the token. The 401 has
   an empty body and `WWW-Authenticate: Bearer`. Refusals are not logged (the health probe draws one by design).
4. **The machine reaches the fact.** rmcp moves the request's `http::request::Parts`, extensions included, into the
   MCP request extensions (stateless POST path). `MemoryMcpServer::call_tool` reads
   `context.extensions.get::<Parts>()?.extensions.get::<Machine>()`; if absent it returns `internal_error` and runs
   nothing (fail closed for every tool). For `memory_add_fact` it then sets `metadata.machine`, overwriting a client
   value; absent or null `metadata` becomes `{"machine": ..}`; non-object `metadata` gets `invalid_params`.
   `tools::dispatch` keeps its signature: it already has 7 parameters (clippy's `too_many_arguments` limit, denied),
   ~115 test call sites and the fuzz target call it, and #1020 replaces the dispatcher. No schema column: ADR-0020
   §8's call record arrives with #1020. Other fact writers (`memory_flush_insights`, activity promotion, bootstrap)
   keep client metadata until #1020 removes them from the surface; nothing is published before #1020.
   `memory_flush_insights` has a similar per-entry metadata rule, but it reports into a `failed` list rather than
   an `invalid_params` error, and #1020 removes it, so `record_machine` does not share a helper with it.
5. **Stateless JSON.** `StreamableHttpServerConfig::default().with_stateful_mode(false).with_json_response(true)`,
   `NeverSessionManager`, factory `move || Ok(server.clone())` (`MemoryMcpServer` derives `Clone`; every field is an
   `Arc` or a `usize`). rmcp's cancellation token is never cancelled: cancelling turns in-flight JSON calls into 500s.
6. **Drain before flush.** rmcp runs each stateless call in a detached task, so axum's graceful shutdown returns
   once connections close even when a client hung up mid-call. A flush concurrent with a write can record new-DB
   fingerprint with old projections, a sidecar the next open trusts. Fence: `in_flight: Arc<tokio::sync::RwLock<()>>`
   on `MemoryMcpServer`; `call_tool` holds a read guard for the whole call; `serve`'s tail takes the write guard,
   flushes (best effort, warn on error, as today), then `std::mem::forget`s the guard, with one comment why: a call
   queued behind the writer must never commit after the flush, and the process exits right after `serve` returns.
   No server-side deadline: the container stop timeout bounds it, and a kill before the flush leaves no new sidecar
   (full rebuild, never a stale valid one).
7. **Hosts.** `--allowed-host` (repeatable) / `MEMORY_MCP_ALLOWED_HOSTS` (comma list) are appended to rmcp's
   loopback default, so loopback (the in-container probe) keeps working and the list is never empty.
   `parse_allowed_host` (the clap value parser) refuses an empty value, any `*`, any `@`, surrounding whitespace, and
   anything that is not an `axum::http::uri::Authority`. An entry without a port admits any port (rmcp's rule).
8. **Configuration.** `--listen <SocketAddr>` / `MEMORY_MCP_LISTEN` and `--token-file <path>` /
   `MEMORY_MCP_TOKEN_FILE` are required, CLI or env only, no default: deploy parameters the quadlet owns, and no
   deploy port appears in public code. `[engine] backup_dir` in TOML, overridden by `--backup-dir` /
   `MEMORY_MCP_BACKUP_DIR` the way `db_path` already is. No default backup dir (a default would copy the whole store
   on every upgrade unasked).
9. **Startup order.** Parse CLI → register SIGTERM and SIGINT (a stop during startup is never the default kill) →
   load the token file (a bad file never migrates the store) → config → engine with `backup_dir` → embedder, identity
   check, summary generator (unchanged) → bind (an open port means ready) → `serve`.
10. **Crates.** rmcp features `server` + `transport-streamable-http-server` (drop `transport-io`);
    `axum = { version = "0.8", default-features = false, features = ["http1", "tokio"] }` (0.8.9 already locked via
    tonic); `sha2 = "0.11"`, `subtle = "2"` (both locked); tokio gains `net` and `sync`. `Parts` and `Authority` come
    from `axum::http`; no direct `http` or `hex` dependency (the lowercase-only nibble decode is the validation).
    New lock entries come only from rmcp's server transport (`sse-stream`, `uuid`, ...); no existing version changes.

### Questions for Michaël (defaulted; the plan proceeds on the stated default)

- **Promoted stdio installs.** `.claude/skills/release-checklist` promotes `memory-engine-mcp` into
  `~/.local/opt/memory-engine/bin`, "the only artifacts the harness consumes". After this merge that binary speaks
  HTTP only and needs `--listen` and `--token-file`. On this desktop that directory does not exist. Default: no
  change to the skill; the PR body says not to promote a post-#1018 release where a harness still launches it over
  stdio, until the plugin's HTTP channel replaces it. The durable alternative is one precondition line in that skill;
  say so and item 9 adds it.
- **Breaking-change marker.** Default: the stdio-removal commit and the squash title use `feat(mcp)!:` with a
  `BREAKING CHANGE:` footer.

## Implementation sequence

- [ ] **0. Baseline (no commit).** On the untouched branch: `cargo build --workspace --locked` and
  `cargo test -p memory-engine-mcp --all-features` green (record counts); `cargo update -p rmcp --dry-run` output
  recorded for the PR's rmcp 1.x note.

- [ ] **1. Token-hash file and token check.**
  - Files: `crates/memory-engine-mcp/{Cargo.toml (sha2, subtle), src/auth.rs (new), src/lib.rs}`, `Cargo.lock`.
  - Red: `auth::tests` written against `todo!()` stubs; `cargo test -p memory-engine-mcp --lib auth::` panics.
  - Change:
    - `pub struct Machine(String)` (private field, `Clone, Debug, PartialEq, Eq`, `as_str()`): only the parser builds
      one, so a `Machine` on a request is proof the token was checked.
    - `pub struct TokenTable { entries: Vec<([u8; 32], Machine)> }`, no `Debug`; `pub fn load(&Path)` and
      `pub(crate) fn parse(&[u8])` (item 3's unit tests build a `Machine` through it), both
      `-> Result<Self, TokenFileError>`.
    - `pub enum TokenFileError { Read { path, source: io::Error }, Line { line: usize, reason: &'static str } }`
      (thiserror). Lines numbered from 1; a message never quotes the line's bytes.
    - Parse: empty input → empty table; `split_inclusive(b'\n')`, a chunk without `\n` is an error at its line;
      exactly two fields on one space; machine matches the DNS label; hash is 64 chars decoded by a lowercase-only
      nibble map; a repeated machine or hash fails at the repeating line.
    - `#[must_use] pub fn authenticate(&self, authorization: &[u8]) -> Option<&Machine>` (`auth.rs` stays
      axum-free; the middleware owns the one-header rule): strip exactly `Bearer `; check the 35-byte shape before
      hashing; `Sha256::digest`; compare with every entry through `subtle::ConstantTimeEq`, folding with no early
      exit.
  - Tests: the ADR vector; a two-machine file admits each token; empty file admits no one; missing file → `Read`;
    rejected with the expected line number: uppercase hex, uppercase machine, two spaces, CR before LF, last line
    without LF, repeated machine, repeated hash, blank line, `#` line, tab, leading/trailing `-` label, 64-char label,
    63- and 65-digit hash; a line holding a raw token is rejected and the error text does not contain it; values
    refused: `bearer `/`Basic ` scheme, double space, trailing space, 34 and 36 bytes, `ME_` prefix, non-ASCII,
    unknown well-shaped token; an off-shape value whose own SHA-256 is listed is still refused (never hashed).
  - Green: same command passes; `cargo clippy -p memory-engine-mcp --all-targets --all-features -- -D warnings`.
  - Commit: `feat(mcp): read the machine token-hash file and check Bearer tokens`.

- [ ] **2. Serve over HTTP behind the auth layer; remove stdio.**
  - Files: `crates/memory-engine-mcp/{Cargo.toml, src/http.rs (new), src/server.rs (derive Clone), src/lib.rs,
    src/main.rs, tests/common/mod.rs (new), tests/http_auth.rs (new), tests/server_process.rs (new)}`,
    `Cargo.lock`, `deny.toml` (the RUSTSEC-2026-0462 comment says "server features only"; the ban stays).
  - Red: `cargo test -p memory-engine-mcp --all-features --test http_auth --test server_process` fails to compile
    (no `http::serve`); then, with `router()` built without `.layer(..)`, `refuses_every_request_without_a_valid_token`
    gets 200/404/405 where it expects 401 (proves the test detects a missing layer).
  - Change:
    - `http.rs`: `pub async fn serve(listener: TcpListener, server: MemoryMcpServer, tokens: TokenTable,
      allowed_hosts: Vec<String>, shutdown: impl Future<Output = ()> + Send + 'static) -> std::io::Result<()>` =
      `axum::serve(listener, router(..)).with_graceful_shutdown(shutdown)`; `router()` = `Router::new()
      .route_service("/mcp", rmcp service per Decision 5).layer(from_fn_with_state(Arc::new(tokens),
      require_machine))`; `require_machine` per Decision 3. `allowed_hosts` is accepted and appended here
      (Decision 7) so the signature does not change in item 5; item 5 adds the CLI and its validation.
    - `main.rs`: `Cli` gains `listen: SocketAddr` and `token_file: PathBuf` (Decision 8); doc/about says "over
      streamable HTTP"; token file loaded before `load_config`, error names path and line; bind, then
      `http::serve(.., std::future::pending())` (signals arrive in item 4). Delete `rmcp::ServiceExt`,
      `transport::io::stdio()`, `engine_for_flush` and the "stdout carries JSON-RPC" comment; log `listening on
      {addr}` at info.
    - `Cargo.toml`: axum, rmcp features (Decision 10), tokio `net` and `sync` (the harness oneshot; item 4's fence
      reuses it), dev tokio `time`; description "over streamable HTTP".
    - `lib.rs`: `pub mod auth; pub mod http;`, crate doc and module-visibility list updated.
  - Harness `tests/common/mod.rs` (`#![allow(dead_code, reason = "each test binary uses part of the harness")]`;
    `allow`, because an `expect` is unfulfilled in a binary that uses everything):
    - `DIM = 4`; `token(machine) -> String` = `format!("me_{machine:0>32}")`; `write_token_file(dir, &["laptop",
      "desktop"]) -> PathBuf`.
    - `TestServer::start()` / `start_with_hosts(&[&str])`: tempdir; wiremock `POST /v1/embeddings` answering a
      `DIM` vector; `HttpEmbeddingProvider` and the file-backed engine built in `spawn_blocking` (the blocking
      reqwest client must not be built on the runtime); real `http::serve` on `127.0.0.1:0` with a oneshot shutdown.
      Fields `url`, `engine: Arc<MemoryEngine>`, `embedder: MockServer`, `dir: TempDir`. Methods `rpc(token, method,
      params) -> RequestBuilder` (POST `/mcp`, `Accept: application/json, text/event-stream`, JSON body),
      `call_tool(machine, tool, args) -> Value` (asserts 200, returns the tool's JSON), `fact_count()`,
      `stop().await` (fires the oneshot, awaits `serve`), `snapshot_path()`. Drop aborts the task.
    - `ServerProcess::spawn(dir, args, envs)`: `CARGO_BIN_EXE_memory-engine-mcp`, `--listen 127.0.0.1:<port freed
      just before>`, `--token-file`, `--config` (TOML with a literal-string `db_path` and `embed_dim = 4`); removes
      inherited `MEMORY_MCP_*`; stdout null, stderr to a file in `dir`. `wait_ready() -> Result<(), ExitStatus>`
      polls `GET /mcp` for 401 against a deadline and returns the exit status if the child dies first;
      `terminate() -> ExitStatus` runs `kill -TERM <pid>` and waits at most 10 s (podman's default stop timeout);
      Drop kills and reaps.
  - Tests (`#[tokio::test(flavor = "multi_thread")]`):
    - `refuses_every_request_without_a_valid_token`: {no header, off-shape value, `bearer` scheme, unknown
      well-shaped token, two `Authorization` headers both holding the valid `laptop` token, two headers holding the
      `laptop` and `desktop` tokens} × {POST `initialize`, POST `tools/list`, POST `tools/call memory_add_fact`, GET
      `/mcp`, DELETE `/mcp`, GET `/`, POST `/lifecycle/events`} → 401 with `WWW-Authenticate: Bearer`; afterwards
      `fact_count() == 0` and the embedder received no request. (A trailing-space value stays a unit case only:
      httparse trims trailing header whitespace on the wire.)
    - `a_valid_token_is_served_statelessly`: the first request is a `tools/call memory_statistics` with no
      `initialize` → 200 `application/json`, no `Mcp-Session-Id`; `tools/list` (lists `memory_add_fact`) and
      `initialize` (`serverInfo.name`) → 200; a stale `Mcp-Session-Id` header is ignored (200).
    - `get_and_delete_answer_405_with_a_token`; `unknown_path_is_404_with_a_token`.
    - `the_host_check_runs_after_authentication`: valid token + `Host: other.example` → 403; no token, same Host →
      401.
    - `a_configured_host_is_served` (`start_with_hosts(&["memory.example"])`): `memory.example` and
      `memory.example:8443` → 200, `other.example` → 403, `127.0.0.1` → 200 (loopback kept).
      `no_configured_host_admits_only_loopback` (`start()`): `memory.example` → 403. These belong here because
      `serve` appends `allowed_hosts` from this item on; item 5 only adds the CLI and its validation.
    - `server_process::a_bad_token_file_stops_startup_before_the_store_opens`: line 2 with uppercase hex → exit code
      1 (not clap's 2) within 10 s, stderr names `line 2`, `memory.db` was never created; a missing file → exit code
      1 and stderr names the path.
  - Green: `cargo test -p memory-engine-mcp --all-features` (all old tests unchanged and green); `cargo deny check`;
    `target/debug/memory-engine-mcp --version` exits 0 with no other argument (the Containerfile check).
  - Commit: `feat(mcp)!: serve MCP over streamable HTTP behind per-machine Bearer auth`, footer
    `BREAKING CHANGE: no stdio transport; --listen and --token-file are required`.

- [ ] **3. Record the calling machine.**
  - Files: `crates/memory-engine-mcp/{src/server.rs, src/tools/definitions.rs, tests/http_auth.rs}`.
  - Red: `cargo test -p memory-engine-mcp --all-features --test http_auth a_remembered_fact` fails: stored
    `metadata.machine` is the client's `"forged"`; `cargo test -p memory-engine-mcp --all-features --lib
    server::tests` fails to compile (no `calling_machine`).
  - Change: Decision 4 in `call_tool`. Private `fn calling_machine(extensions: &rmcp::model::Extensions) ->
    Result<&Machine, ErrorData>` is `call_tool`'s first statement (with `?`), before any argument parsing, embedding
    or dispatch; private `fn record_machine(&mut Map<String, Value>, &Machine) -> Result<(), ErrorData>`.
    `memory_add_fact`'s `metadata` schema gets `"type": "object"` and says the server sets `machine`.
  - Tests:
    - `a_remembered_fact_records_the_calling_machine`: `laptop` sends `{"machine": "forged", "note": 1}` → stored
      `{"machine": "laptop", "note": 1}`; `desktop` with no metadata → `{"machine": "desktop"}`; `"metadata": "x"`
      → JSON-RPC `-32602` and no fact written. Facts read through `engine.get_fact`.
    - `server::tests` (unit, because rmcp's `Peer` cannot be built outside rmcp, so `call_tool` cannot be driven
      without the HTTP stack): `calling_machine` on empty `Extensions` → `internal_error`; on `Extensions` holding
      `Parts` without a `Machine` → `internal_error`; with both → the machine. The `Machine` comes from
      `TokenTable::parse` plus `authenticate`, so no test-only constructor exists. Because the guard is
      `call_tool`'s first `?`, a refused call reaches neither the embedder nor the store.
  - Green: same commands, then `cargo test -p memory-engine-mcp --all-features`.
  - Commit: `feat(mcp): record the calling machine on a remembered fact`.

- [ ] **4. Drain and flush on SIGTERM.**
  - Files: `crates/memory-engine-mcp/{src/server.rs (in_flight), src/http.rs (serve tail), src/main.rs (signals),
    tests/http_shutdown.rs (new), tests/server_process.rs}`.
  - Red: `cargo test -p memory-engine-mcp --all-features --test http_shutdown` (no filter) and
    `cargo test -p memory-engine-mcp --all-features --test server_process sigterm`: no sidecar; the process is
    killed by the signal instead of exiting 0.
  - Change: Decision 6. `main` registers `signal(SignalKind::terminate())?` and `interrupt()` before loading tokens;
    the shutdown future is `select!` over both `recv()`s. Order within the item: add the flush alone first and run
    `cargo test -p memory-engine-mcp --all-features --test http_shutdown shutdown_waits_for_an_abandoned_call`; it
    must stay red (`fact_count() == 0` when `stop` returns), then add the fence and see it green. The hazard is
    structural (rmcp spawns the call, `tower.rs` stateless path; hyper closes a busy HTTP/1 connection on EOF), so a
    green run on the flush alone means the test did not really abandon the request: fix the test until it is red,
    and keep the fence.
  - Tests:
    - `shutdown_lets_an_in_flight_call_finish`: embedder delays 1.5 s; once wiremock received the embed request,
      `stop()`; the call gets 200, then `stop` returns, the sidecar exists, a new connection is refused.
    - `shutdown_waits_for_an_abandoned_call`: embedder delays 2 s; once the embed request arrived, abort the client
      request and `stop()`; when `stop` returns, `fact_count() == 1` and the sidecar exists.
    - `server_process::sigterm_flushes_the_snapshot_and_exits_zero`: ready (401), no `<db>.snapshot` yet;
      `terminate()` → success within 10 s; `<db>.snapshot` is a file.
  - Green: same command, then `cargo test -p memory-engine-mcp --all-features`.
  - Commit: `feat(mcp): drain tool calls and flush the snapshot on SIGTERM`.

- [ ] **5. Admit configured Hosts besides loopback.**
  - Files: `crates/memory-engine-mcp/{src/http.rs (parse_allowed_host), src/main.rs, tests/server_process.rs}`.
  - Red: `cargo test -p memory-engine-mcp --all-features --lib http::tests` fails to compile;
    `cargo test -p memory-engine-mcp --all-features --test server_process allowed_host`: clap rejects the unknown
    `--allowed-host` flag, so `wait_ready()` returns `Err` (exit 2).
  - Change: Decision 7; `Cli.allowed_hosts: Vec<String>` with `long = "allowed-host"`, `env =
    "MEMORY_MCP_ALLOWED_HOSTS"`, `value_delimiter = ','`, `value_parser = http::parse_allowed_host`.
  - Tests: `parse_allowed_host` accepts `memory.example`, `memory.example:8443`, `[::1]`; refuses ``, `*`,
    `*.example`, `user@memory.example` (userinfo would silently reduce to the host), ` memory.example` and
    `memory.example ` (so a comma list holds no spaces). Process `allowed_host_from_flag_and_environment`:
    `--allowed-host memory.example`, and separately `MEMORY_MCP_ALLOWED_HOSTS=memory.example,other.example`, then
    `tools/list` with each named Host → 200 and `Host: third.example` → 403 (the values reach `serve`);
    `MEMORY_MCP_ALLOWED_HOSTS='*'` → exit code 2, and stderr holds `parse_allowed_host`'s own message for it.
  - Green: both commands pass.
  - Commit: `feat(mcp): admit configured Host values besides loopback`.

- [ ] **6. Pass `engine.backup_dir` to the engine builder.**
  - Files: `crates/memory-engine-mcp/{src/config.rs, src/main.rs, tests/server_process.rs}`.
  - Red: `cargo test -p memory-engine-mcp --lib config::tests::deserialize_full_config` fails to compile once it
    asserts `config.engine.backup_dir`; `--test server_process backup_dir` finds no `.bak`.
  - Change: `EngineSection.backup_dir: Option<PathBuf>` (doc: an existing directory; a migration writes
    `<db>.v<N>.bak` there first; unset means no backup); `Cli.backup_dir` (`--backup-dir`,
    `MEMORY_MCP_BACKUP_DIR`) overrides TOML in `load_config`; the minimal-config literal gets `backup_dir: None`;
    a set value that is not a directory refuses to start, naming it (a typo would otherwise surface only at the next
    upgrade, when `VACUUM INTO` fails); the builder gets `.backup_dir(dir)` when set.
  - Tests: setup builds a store with `MemoryEngine::builder(4).path(db)`, drops it, and relabels `schema_version`
    to `memory_engine::CURRENT_SCHEMA_VERSION - 1` through rusqlite (the v13→v14 step is idempotent today).
    `backup_dir_from_toml` and `backup_dir_from_environment`: after `wait_ready()` returns either way, `<backups>/
    memory.db.v{N-1}.bak` is a file (`migrate` writes it before any step runs, so a later non-replayable migration
    from #1017 cannot hide it); terminate if running. `a_missing_backup_dir_stops_startup`: exit code 1, stderr
    names the directory, no migration ran (`schema_version` still `N-1`).
  - Green: both commands pass.
  - Commit: `feat(mcp): pass engine.backup_dir to the engine builder`.

- [ ] **7. Boy-scout: build the blocking HTTP clients outside the async runtime.** In debug builds reqwest's
  blocking client builds and drops a shell runtime (`reqwest-0.12.28/src/blocking/wait.rs` `enter()`), which panics
  inside `#[tokio::main]` whenever an embedder or summary endpoint is configured. Release builds are unaffected;
  #1017's process tests would be.
  - Files: `crates/memory-engine-mcp/{src/main.rs, tests/server_process.rs}`.
  - Red: `starts_with_an_embedder_and_a_summary_endpoint` (`--embed-url`, `--embed-model`, `--summary-url`,
    `--summary-model` at a wiremock server answering both) → `wait_ready()` returns `Err` (exit 101).
  - Change: wrap `build_embedder` and `build_summary_generator` calls in `tokio::task::block_in_place`, one comment
    why.
  - Green: same test passes.
  - Commit: `fix(mcp): build the blocking HTTP clients outside the async runtime`.
  - Second boy-scout commit in the same file: `build_embedder`'s doc comment sits on `KNOWN_PROVIDERS` (the
    "Build the embedding provider…" paragraph precedes the constant's own doc); move it onto `build_embedder`. No
    test (doc only). If #1017 already fixed it when this branch rebases, drop the commit.
    Commit: `docs(mcp): move build_embedder's doc comment back onto build_embedder`.

- [ ] **8. Boy-scout: `flush_snapshot`'s doc.** `crates/memory-engine/src/engine/mod.rs` says a shared owner can flush
  "on shutdown — or periodically"; a flush concurrent with a write can pair new-DB fingerprint with old
  projections. Doc now requires no write in flight, with that reason. Doc only.
  - Commit: `docs(engine): require no write in flight for flush_snapshot`.

- [ ] **9. Document the HTTP surface** (Documentation below). The stale pre-#631 claims in
  `docs/reference/mcp-server.md` ("spawn_blocking", "18 handlers", "the engine is sync") all sit inside the
  Architecture section this item rewrites, so they go with the rewrite and its commit body names them.
  - Commit: `docs(mcp): document the HTTP transport, machine tokens and shutdown`.

- [ ] **10. Rebase, gate, PR.** If #1017 merged first, rebase; keep both sides' `Cli` fields; route any #1017
  process test through `common::ServerProcess` with `--listen`/`--token-file` and a positive control (with a matching
  identity the server reaches 401), so a clap exit 2 cannot satisfy an "exits non-zero" assertion. Run Verification
  unpiped. PR body: `Closes #1018`, `Refs dutiona/infra#37`, `Refs #1035`, `Classification: Feature (researched)
  (user-confirmed)` and the route lines, the rmcp 1.x note (RUSTSEC-2026-0462 is fixed only in 3.9, so 1.x gets no
  transport fixes; the upgrade is #1035, not folded in), the questions above, the accepted residuals (no body limit on
  `/mcp` for token holders: rmcp 1.8's `expect_json` collects the whole body, and #1025's limit layer must exceed
  `memory_bootstrap_session`'s 50 MB cap or wait for #1020 to remove that tool; every token holder reaches all 26
  tools until #1020, including `memory_dump_state`, which now lets any token holder on the network make the server
  write into its temp dir, bounded by `validate_dump_path`; no drain deadline), collateral findings, gate output.
  Run the private leak patterns over the PR body before posting it.

## Documentation

- `docs/reference/mcp-server.md`: What (HTTP-only, stateless `POST /mcp`, per-machine tokens); Usage (`--config`,
  `--listen 127.0.0.1:<port>`, `--token-file`, `--allowed-host`, with their env vars); Configuration (`[engine]
  backup_dir`, `MEMORY_MCP_BACKUP_DIR`); new "Transport and authentication": the header, the token-hash file by
  reference to ADR-0020 §4 plus one example line and a minting recipe that keeps the token off argv and hashes no
  stray line feed (`printf '%s %s\n' laptop "$(printf '%s' "$token" | sha256sum | cut -d' ' -f1)"`), the status table
  (401 any request without a valid token; 403 Host not admitted; 405 GET/DELETE; 406/415 wrong `Accept` or
  `Content-Type`), Hosts (added to loopback, `*` refused), `metadata.machine` on `memory_add_fact`; Shutdown (SIGTERM
  and SIGINT drain, flush, exit 0; a stop timeout shorter than the slowest call becomes a kill and a rebuild); the
  stdio `settings.json` client snippet replaced by one sentence (the Claude Code plugin sends the token through its
  `headersHelper`, ADR-0020 §12) and a `curl -H @-` check against `https://memory.example/mcp`; Architecture line
  `HTTP POST /mcp → auth layer → rmcp (stateless) → server`, with `auth.rs` and `http.rs`.
- `docs/getting-started/installation.md` "Server image": the entry point serves HTTP and needs `MEMORY_MCP_LISTEN`
  (`0.0.0.0:<port>` inside a container) and `MEMORY_MCP_TOKEN_FILE`; the health probe expects 401 (`curl -s -o
  /dev/null -w '%{http_code}' http://127.0.0.1:<port>/mcp` prints `401`); `podman stop` drains and flushes. The
  "deployment lives outside this repository" sentence stays.
- Crate docs (`lib.rs`, clap `about`, `Cargo.toml` description) change in item 2, where they become false.
- Unchanged: `Containerfile` (`--version` short-circuits required args; the probe belongs to the infra quadlet),
  README, ADRs.

## Testing

- Unit: `src/auth.rs` (item 1), `src/server.rs` `calling_machine` (item 3), `src/http.rs` `parse_allowed_host`
  (item 5), `src/config.rs` (item 6).
- Integration, in-process (`tests/http_auth.rs`, `tests/http_shutdown.rs`) through the real `http::serve`.
- Integration, built binary (`tests/server_process.rs`): only what `main.rs` owns — flags and env, startup order,
  SIGTERM, backup plumbing, the debug-runtime fix.
- "No handler runs" rests on: rmcp never answers 401 (no `UNAUTHORIZED` under its `streamable_http_server/`), so a
  401 is our layer; a refused `tools/call memory_add_fact` leaves the embedder untouched and writes no fact.
- Assertions check statuses, presence/absence, counts and stored values; never dependency versions or log text.
  The stderr assertions check only our own error messages naming the offending input (the line number ADR-0020
  requires, the token-file path, the backup directory, and `parse_allowed_host`'s own message for `*`), never
  clap's or a dependency's wording.
- No token literal in any test: the ADR vector and every token-shaped value are built at run time
  (`format!("me_{}", "0".repeat(32))`, `token(machine)`), so the leak guard's token shape never matches a source
  file.
- Flake guards: in-process binds `:0`; the binary's freed-port race is accepted; every wait has a deadline; wiremock
  delays (≤ 2 s) sit far below the harness provider timeout (10 s); child stderr goes to a file.
- Not tested: compare timing (reviewed: `ct_eq` fold, no early exit); SIGINT separately (same future as SIGTERM);
  `tailscale serve` Host behaviour and `podman stop` (INF-6's smoke).
- Acceptance map: `--listen`/stdio removed → 2 (+ process tests); stateless JSON → 2; layer, hashed file,
  constant time, machine via `Parts` → 1, 2, 3; 401 matrix and valid 200 → 2; machine on the fact → 3;
  `allowed_hosts` → 2, 5; `backup_dir` TOML + env + `.bak` → 6; rmcp 1.x → 0, 10; no unauthenticated route, probe
  expects 401 → 2 (`/`, `/lifecycle/events`, readiness probe); SIGTERM → 4; `tests/common/` → 2.

## Verification

Run unpiped at the final clean HEAD; paste the results into the PR:

```bash
cargo build --workspace --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --all-features
cargo test  --workspace --all-features
cargo check -p memory-engine
cargo check -p memory-engine --no-default-features --features backend-sqlite
export RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links"
cargo doc --no-deps -p memory-engine
cargo doc --no-deps -p memory-engine --all-features
cargo deny check
cargo +nightly fuzz build          # fuzz/ depends on memory-engine-mcp and item 8 touches the facade docs
cargo +1.89 build --workspace --tests --examples
cargo +1.89 build --workspace --tests --examples --all-features
```

Then:

- `cargo tree -p memory-engine-mcp --all-features -e features -i rmcp` shows no `client`, `transport-io` or
  `transport-streamable-http-client`; `git diff origin/main -- Cargo.lock` only adds;
  `rg -n 'stdio|transport-io|ServiceExt' crates/memory-engine-mcp` prints nothing.
- Leak guard, generic shapes only (this plan is public):
  `rg -n -e '\.ts\.net\b' -e '\b100\.(6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7])\.' -e 'me_[0-9A-Za-z]{32}'
  $(git diff --name-only origin/main | grep -v -e '-review\.md$' -e '-external-')` prints nothing (the excluded
  review artifacts quote the guard's own CGNAT range; the private guard still covers them, and `/finish-pr` removes
  `docs/plans/` before merge), and so does the same over `git log --format=%B origin/main..HEAD`. The deployment-specific
  patterns live in a private file outside the repository and run the same way:
  `rg -n -f ~/.local/state/agent-work/2026-11-02-me-kb-claude-code/fleet/artifacts/memory-engine-1018/private-leak-patterns.txt`
  over the same files and messages, and over the PR body before it is posted.
- Smoke run of the debug binary with a throwaway token file: `curl` without a token prints `401`, with the token
  (`-H @-`) `initialize` answers 200 `application/json`, `kill -TERM` exits 0 and leaves `<db>.snapshot`.

Operational steps: the worktree exists and is locked (`.worktrees/feat-1018-mcp-http-bearer`); the plan issue is
published by `archive-plan-issue.sh` before approval is asked (fleet handoff) and reused by `/finish-pr` Item 5; PR
from this branch; `/super-review` at the confirmed Full gate; merge by Michaël through `/squash-merge`.
