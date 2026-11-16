# fix(build): clear the three RustSec advisories that fail cargo deny check (#1033)

## Outcome

`cargo deny check` passes on the all-features graph. RUSTSEC-2026-0459 is fixed, not ignored: every `rusqlite`
requirement is 0.43 (libsqlite3-sys 0.40, bundled SQLite 3.54.1), whose `rust-version = "1.89.0"` raises the MSRV
to 1.89. The MSRV has one manifest home, `[workspace.package]` in the root `Cargo.toml`, which every member
inherits; the CI `msrv` job reads it, and only the detached `fuzz/` repeats it. RUSTSEC-2026-0462 and
RUSTSEC-2026-0440 are documented `ignore`s carrying their reason and removal issue (#1035, #1036). A cargo-deny
feature ban enforces 0462's premise that rmcp's client code is not compiled. Every other gate row passes; the
clippy row is red only with #1032's lint drift, shown identical at the merge-base and at HEAD.

## Constraints

- `cargo deny check` passes.
- Every `ignore` added to `deny.toml` carries its reason and a removal issue, following the existing policy comment.
- Every other matrix command still passes.
- rusqlite: take **0.43** (not 0.41) and raise the MSRV to **1.89**. Update every place that names 1.88 (Cargo `rust-version`, AGENTS.md/CLAUDE.md/GEMINI.md, the CI MSRV job, and the extra places found). The stale "1.85" comment goes in its own commit.
- rmcp RUSTSEC-2026-0462: a documented `deny.toml` exception. Reason: client-only, not compiled into our build. Removal tracked by dutiona/memory-engine#1035.
- astral-tokio-tar RUSTSEC-2026-0440: a documented `ignore` in the same shape as the RUSTSEC-2026-0173 one. Reason: unmaintained notice (not a vulnerability), reached only through `me-backend-postgres` dev-dependencies; upstream fix testcontainers/testcontainers-rs#1006 (open); removal tracked by dutiona/memory-engine#1036 (already filed; file no other).
- Branch commits: Conventional Commits, imperative, the body explains why, no Co-Authored-By line.
- PR body: `Closes #1033`, `Refs dutiona/infra#37` (never a bare #37), `Classification: Security patch (user-confirmed)`, a Collateral findings section, and the red gate row that belongs to #1032 with its output.
- Never write to issue #1033. Never merge. The repository is public: no tailnet name, address or token anywhere.
- #1032 may touch `Cargo.toml` and `AGENTS.md`; the first to land wins, the second rebases.
- From the orchestrator's brief: "Your own gate run must be green except for the other ticket's failure (#1032
  fixes clippy, #1033 fixes cargo deny). Say in the PR which red row belongs to the sibling ticket, with its
  output." Clippy already fails on main, so "every other matrix command still passes" cannot cover it; READY with
  that one attributed red row is the brief's explicit allowance, and the merge order is the orchestrator's.

## Evidence already measured (2026-11-16, throwaway copy of b59f9ee; nothing committed)

- No testcontainers release drops astral-tokio-tar (0.27.3, 0.28.0 and upstream main all depend on it).
- rmcp features in our graph: base64, default, macros, server, transport-async-rw, transport-io; no `client`.
- rusqlite 0.43 + 1.89: `cargo check --workspace --all-targets --all-features` breaks only at
  `crates/me-backend-sqlite/src/store/activities.rs:184` (`usize: ToSql`) and the two `trace_v2` tests in
  `crates/me-backend-sqlite/src/search/ann.rs`. With those fixed: the full suite passed (1997 passed, 81 ignored;
  1 failure that is an artifact of the copy living under the temp dir: `validate_dump_path_handles_bare_relative_leaf`);
  `cargo +1.89 build --workspace --tests --examples` passed, default and all-features; `cargo deny check` reported
  only 0440 and 0462, with `bans ok, licenses ok, sources ok`; a `[[bans.features]]` entry denying rmcp's
  `transport-streamable-http-client` failed `bans` when the `-reqwest` variant was added.
- Clippy, with #1032's two me-types lints allowed and `--keep-going`: the same findings at main and in the copy.

## Implementation sequence

Work in the existing, locked worktree of `fix/1033-rustsec-advisories`. Keep logs in
`~/.local/state/agent-work/2026-11-02-me-kb-claude-code/fleet/artifacts/memory-engine-1033/logs/` (`$LOG`), never
in the tree. Run cargo unpiped into `$LOG/<row>.log` and record its rc. `cargo fmt --all` before each commit.

- [ ] **1. Correct the stale 1.85 comment** (its own commit, as decided). File:
  `crates/memory-engine/src/engine/activity_filter.rs` (150-152). Red: `git grep -n 'MSRV is 1.85' -- crates` → 1
  line; a comment has no behaviour. Change: "`str::floor_char_boundary` is stable only since Rust 1.91, above the
  workspace MSRV, so walk back manually via the long-stable `is_char_boundary`." (no number left to drift). Green:
  the grep prints nothing. Commit: `docs(core): stop naming a stale MSRV in the char-boundary comment`.

- [ ] **2. Drop the facade's dead rusqlite `trace` dev-dependency** (boy-scout). Files:
  `crates/memory-engine/Cargo.toml` (`[dev-dependencies]` `rusqlite … ["trace"]` and its 4-line comment),
  `crates/me-backend-sqlite/Cargo.toml` (drop "Mirrors the facade's own dev-dependency exactly."). Red: none, dead
  code: `git grep -nE 'rusqlite::trace|trace_v2|\.trace\(' -- crates/memory-engine tests benches examples` prints
  nothing (the only user moved with `search::ann`). Green: `cargo test -p memory-engine --all-features --no-run`
  rc 0. Commit: `chore(build): drop the facade's unused rusqlite trace dev-dependency`.

- [ ] **3. Every member inherits the MSRV.** Files: `crates/{memory-engine,memory-engine-cli,memory-engine-embed,memory-engine-mcp}/Cargo.toml`,
  `Cargo.toml` (comment 27-30). Red: `git grep -n '^rust-version = ' -- 'crates/*/Cargo.toml'` → 4 lines, against a
  root comment that calls the root the single source of truth. Change: the four lines become
  `rust-version.workspace = true`; the root comment becomes "Workspace MSRV — the single source of truth: every
  member inherits it (`rust-version.workspace = true`) and the CI "verify rust-version" job greps it from here
  (`grep '^rust-version' Cargo.toml`). Only the detached `fuzz/` workspace repeats it." Green: the grep prints
  nothing; `cargo metadata --format-version 1 --no-deps | jq -r '[.packages[].rust_version] | unique | .[]'` prints
  only `1.88`. Commit: `refactor(build): inherit rust-version from the workspace in every member`.

- [ ] **4. Saturate `list_recent_by_scope`'s LIMIT, still on 0.34.** File:
  `crates/me-backend-sqlite/src/store/activities.rs`. Test first, appended to `tests::list_recent_by_scope`:
  `assert_eq!(store.list_recent_by_scope(&[1], usize::MAX).unwrap().len(), 3, "a limit past i64::MAX saturates, as in list_by_session");`.
  Red: `cargo test -p me-backend-sqlite --all-features --lib store::activities::tests::list_recent_by_scope` → 1
  failed (0.34 binds `usize` through a fallible conversion: `ToSqlConversionFailure`). Change:
  `let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);` bound as `params![ids_json, limit_i64]`, the rule of the
  crate's other LIMITs (`activities.rs:148`, `checkpoints.rs:116`, `events.rs:360`, `fts.rs:78`); never `as i64`
  (`cast_possible_wrap`). Green: the same command → 1 passed. Commit:
  `fix(storage): saturate list_recent_by_scope's LIMIT like its siblings` (body: the only LIMIT that leaked a
  `ToSql` conversion error past `i64::MAX`; rusqlite 0.41+ no longer implements `usize: ToSql` by default — it sits
  behind the opt-in `fallible_uint` feature, which would make this one site error where its siblings saturate — so
  the conversion is needed for item 5 anyway).

- [ ] **5. Take rusqlite 0.43 and raise the MSRV to 1.89 (RUSTSEC-2026-0459).** Files: the six `rusqlite`
  requirements `"0.34"` → `"0.43"`, features unchanged (`crates/me-backend-sqlite/Cargo.toml` ×2,
  `crates/me-consolidate/Cargo.toml`, `crates/memory-engine/Cargo.toml`, `crates/memory-engine-cli/Cargo.toml`,
  `crates/memory-engine-mcp/Cargo.toml`); `Cargo.toml` and `fuzz/Cargo.toml` `rust-version = "1.89"`;
  `deny.toml`; `crates/me-backend-sqlite/src/search/ann.rs`.
  - Red: `cargo deny check advisories` lists RUSTSEC-2026-0459; with the manifests edited,
    `cargo check --workspace --all-targets --all-features` fails at the `trace_v2` calls in `ann.rs` only. An error
    anywhere else is an unplanned break: stop and re-plan.
  - Change, manifests: the root comment gains "1.89 is rusqlite 0.41+'s `rust-version`, taken for RUSTSEC-2026-0459."
    `fuzz/Cargo.toml` gains above its line "# The root workspace MSRV, repeated: a detached workspace cannot inherit
    it." me-backend-sqlite's `trace` comment says `Connection::trace_v2` (0.43 has no `trace`). `deny.toml`, after
    the RUSTSEC-2026-0009 lines: "RUSTSEC-2026-0459 (bundled SQLite before 3.54.0) was *fixed* — rusqlite bumped to
    0.43 (SQLite 3.54.1), which requires Rust 1.89, so the MSRV was raised again — not ignored." No workflow edit:
    the `msrv` job reads the root manifest.
  - Change, `ann.rs` (`hnsw_strategy_tests`): 0.43's signature is
    `trace_v2<F: FnMut(TraceEvent<'_>) + Send + 'static>(&mut self, TraceEventCodes, Option<F>) -> Result<()>`, so a
    capturing closure replaces the thread-local. One test helper,
    `fn count_batch_queries<R>(conn: &mut Connection, f: impl FnOnce(&Connection) -> R) -> (R, usize)`, moves an
    `Arc<AtomicUsize>` clone into a `move |ev: TraceEvent<'_>|` closure that counts each `TraceEvent::Stmt` whose SQL
    contains `json_each`, calls `trace_v2(SQLITE_TRACE_STMT, Some(..)).unwrap()`, runs `f(&*conn)`, clears with
    `trace_v2(TraceEventCodes::empty(), None::<fn(TraceEvent<'_>)>).unwrap()`, and returns the result with the count.
    Both `hnsw_strategy_one_batched_query_per_widening_attempt*` tests call it on a `let mut` connection and keep
    their names, assertions and expected counts (1 and 2). Delete from each: the `thread_local!`, `fn on_stmt` with
    its `#[allow(clippy::needless_pass_by_value)]` (its reason, "bare `fn(TraceEvent)`", is now false) and the "bare
    `fn` pointer" comment. Fallback if the helper fights a lint: keep `fn on_stmt`, make `conn` `mut`, unwrap both
    calls, type the `None`, and rewrite the `reason` to "`trace_v2` passes `TraceEvent` by value".
  - Green: the check → rc 0; `cargo test -p me-backend-sqlite --all-features --lib hnsw_strategy_one_batched` → 2
    passed; `cargo tree -i libsqlite3-sys --all-features --depth 0` → `libsqlite3-sys v0.40.x`; `cargo deny check`
    reports exactly RUSTSEC-2026-0440 and -0462, with `bans ok, licenses ok, sources ok`;
    `cargo +1.89 build --workspace --tests --examples` and the same with `--all-features` → rc 0;
    `cargo test --workspace --all-features` → rc 0. Any other deny finding, an MSRV failure, or a test failing for a
    reason other than a mechanical API change: stop and report with its output.
  - Commit: `fix(build): take rusqlite 0.43 and MSRV 1.89 for RUSTSEC-2026-0459` (body: 0.41+ declares
    rust-version 1.89, so the MSRV moves with it; closed by the requirement, nothing pinned, `Cargo.lock` stays
    untracked; `trace_v2` now takes a capturing closure).

- [ ] **6. Except RUSTSEC-2026-0462 and enforce its premise.** File: `deny.toml`. Red: item 5's deny run lists it.
  Change: the policy sentence after "Policy: action every advisory; never silently suppress." becomes "Each exception
  below carries its reason and a removal issue. An *unmaintained* notice may be ignored; a vulnerability only while
  its code is not compiled, a premise a `[[bans.features]]` entry enforces." `ignore` becomes a multi-line array, each
  ID under its own comment, 0173's moved verbatim, then:
  `# RUSTSEC-2026-0462: rmcp 1.8's streamable-HTTP *client* is vulnerable (fixed only in 3.9.0, a major bump).`
  `# memory-engine-mcp builds rmcp server-only (server + transport-io), so the client is not compiled; the`
  `# rmcp entry under [bans] keeps it that way. Documented exception; removal tracked by #1035.` then
  `"RUSTSEC-2026-0462",`. After `allow-wildcard-paths = true`:
  `# Keeps the RUSTSEC-2026-0462 exception honest; every client variant enables transport-streamable-http-client.`
  `# Remove together with that ignore under #1035, or it blocks a legitimate client feature after rmcp 3.9+.`
  `[[bans.features]]` / `crate = "rmcp"` / `deny = ["client", "transport-streamable-http-client"]`.
  Green: `cargo deny check` reports RUSTSEC-2026-0440 alone and `bans ok`. Mutation proof, by
  `$LOG/../weaken-rmcp.sh <feature>` written from super-plan's weaken template (backup in the git dir, inject, run,
  restore by `cmp`-proven copy, never `git checkout`): SUBJ `crates/memory-engine-mcp/Cargo.toml`, inject the feature
  into rmcp's `features`, gate `cargo deny check bans`. Run it for `transport-streamable-http-client-reqwest` and for
  `client`: each run reports `feature-banned` for rmcp, gate rc ≠ 0, `RESTORE PROVEN`, and `git status --porcelain`
  after the run equals the snapshot taken before it (the uncommitted `deny.toml` edit is expected in both). If
  cargo-deny rejected the key, stop: an unenforced vulnerability exception is Michaël's call. Commit:
  `fix(build): except RUSTSEC-2026-0462 while rmcp's client stays uncompiled`.

- [ ] **7. Except RUSTSEC-2026-0440.** File: `deny.toml`. Red: item 6's green lists it. Change, appended to the
  array: `# RUSTSEC-2026-0440: astral-tokio-tar is unmaintained (not a vuln), pulled via testcontainers, a`
  `# dev-dependency of me-backend-postgres only (the live-PG test harness). No testcontainers release drops it yet:`
  `# testcontainers/testcontainers-rs#1006 is open. Documented exception; removal tracked by #1036.` then
  `"RUSTSEC-2026-0440",`. Green: `cargo deny check` rc 0, `advisories ok, bans ok, licenses ok, sources ok`, no
  `advisory-not-detected`; `cargo tree -i astral-tokio-tar --all-features --target all -e normal,build` prints
  `warning: nothing to print` (measured on main), and `git grep -n 'testcontainers' -- '*Cargo.toml'` shows the
  two declarations only in `crates/me-backend-postgres/Cargo.toml`'s `[dev-dependencies]`. Commit: `fix(build): except RUSTSEC-2026-0440 until testcontainers drops astral-tokio-tar`.

- [ ] **8. State the 1.89 MSRV in the prose.** Files: `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, `README.md`,
  `CONTRIBUTING.md`, `docs/getting-started/installation.md`, `.claude/skills/qa-sweep/SKILL.md`. Red:
  `git grep -n '1\.88' -- <those files>` → 14 lines. Change: the docs give the value, `Cargo.toml` keeps the reason.
  - `AGENTS.md:7`, `GEMINI.md:7`: "**Rust 1.88+** — the workspace uses let-chains" → "**Rust 1.89+** — the MSRV;
    the root `Cargo.toml` records what sets it".
  - `AGENTS.md:38`, `CLAUDE.md:61`, `GEMINI.md:42`: `cargo +1.88 build` → `cargo +1.89 build`; "if you touch
    let-chains or edition-sensitive code" → "if you change a dependency or edition-sensitive code".
  - `CLAUDE.md:107`: "It fails on RustSec security advisories" gains "(minus the documented exceptions in
    `deny.toml`)"; from "Note: the MSRV is **Rust 1.88**" to the paragraph's end → "Note: the MSRV is **Rust 1.89**;
    the root `Cargo.toml` records what sets it." (This drops the false claim that `time` left the graph; 0.3.55 is
    in it.)
  - `README.md:4` badge `rust-1.88%2B` → `rust-1.89%2B`; `README.md:157`, `CONTRIBUTING.md:12`,
    `docs/getting-started/installation.md:5`: `1.88+` → `1.89+`.
  - `SKILL.md:57-59`: "MSRV is **1.88** (let-chains)" → "MSRV is **1.89** (the root `Cargo.toml` records what sets
    it)"; "on exactly 1.88" → "on exactly that version"; "a dev-dep needing" → "a dependency or dev-dep
    needing"; the rerun condition as above.
  Kept, as true history: `deny.toml`'s RUSTSEC-2026-0009 lines, `SKILL.md:94` and `:138`, `utils/qa/**`. Green: the
  grep prints only `SKILL.md:94` and `:138`; `git grep -n 'let-chain' -- AGENTS.md CLAUDE.md GEMINI.md` prints
  nothing (all 6 of today's lines state the old MSRV reason); `cargo test -p memory-engine --all-features --test gate_matrix_parity` passes.
  Commit: `docs: state the 1.89 MSRV and where it is set`.

- [ ] **9. Correct `deny.toml`'s stale lists** (boy-scout). File: `deny.toml`. Red: `git grep -n 'async, ann' --
  deny.toml` → 1 line (`async` was removed in #702), and the `[bans]` comment's "(currently getrandom, hashbrown,
  r-efi)" matches neither main's nor HEAD's duplicate warnings. Change: the `[graph]` comment → "Check every workspace
  feature, mirroring CI's `--all-features` gates, so a vuln behind a feature flag cannot hide."; drop the
  "(currently …)" parenthesis. Green: the grep prints nothing; `cargo deny check` rc 0. Commit:
  `docs(build): drop deny.toml's stale feature and duplicate lists`.

- [ ] **10. Stop documenting the removed `async` feature** (boy-scout, one commit; `async` left in #702, and
  `README.md` and `installation.md` are already touched by item 8). Files: `README.md` (147),
  `docs/getting-started/installation.md` ("Async support"), `docs/reference/api.md` (215-216, "Feature Flags"),
  `docs/usage/inspection.md` (267), `docs/design/architecture-overview.md` (105). Red:
  ``git grep -nE '"async"|`async` feature' -- README.md docs ':!docs/plans'`` → 6 lines; a copied snippet fails to
  resolve. Change: README's whole "For async support:" block (143-148) goes, since without the feature it would
  repeat the snippet at 138-141; `installation.md` keeps the async-native paragraph,
  then "`tokio` is a non-optional dependency, so there is no feature to enable. If you disable default features,
  re-enable a storage backend (at least one is required):" with `default-features = false, features =
  ["backend-sqlite"]`; `api.md`'s "Feature Flags" section (212-216) holds only the `async` entry, so the entry
  (term and definition) becomes one sentence: "`crates/memory-engine/Cargo.toml`'s `[features]` table lists the
  flags; no flag gates the runtime, since `tokio` is a non-optional dependency (#702)." (worded so the green grep
  cannot match its own replacement); the other two sentences lose the claim
  that a default-on `async` feature supplies the runtime (the runtime is the caller's tokio runtime). Green: the grep
  prints nothing; the installation snippet matches the gate's `--no-default-features --features backend-sqlite`
  row. Commit: `docs: stop documenting the removed async feature`.

## Documentation

- Changed: MSRV prose (8), comments (1, 2, 3, 5), `deny.toml` policy, exceptions and lists (5, 6, 7, 9), the
  removed-`async` docs (10). User-facing: the README badge, snippet and requirement lines; consumers need Rust 1.89.
- No CHANGELOG exists. The PR body records the three consumer-visible changes: MSRV 1.89;
  `list_recent_by_scope` with a limit past `i64::MAX` returns every row instead of a storage error; and, because
  libsqlite3-sys declares `links = "sqlite3"`, a downstream crate that also depends on rusqlite outside 0.43.x no
  longer resolves alongside memory-engine.
- Not edited: `.github/workflows/ci.yml` (its `msrv` job reads the manifest), `tests/gate_matrix_parity.rs` (skips
  `msrv`).

## Testing

- Red then green: item 4's `usize::MAX` assertion, failing on main today.
- Migrated, expectations unchanged: the two HNSW trace tests (counts 1 and 2, so a constant would fail one).
- Config mutations: item 6's two `rmcp` feature injections, proving the ban is enforced and not only parsed.
- Regression: `cargo test --workspace --all-features`, the first run of the whole suite on 0.43 in the real tree.
  rusqlite 0.43 rejects multi-statement SQL in `prepare`/`execute` (`Error::MultipleStatement`); a failure of that
  kind is fixed in item 5 by moving the SQL to `execute_batch`. Any other new failure: diagnose before editing.
- Not added, by Michaël's rule (tests check behaviour, never a dependency's version): a bundled-SQLite version
  test; `cargo deny check` is that guard. The `#[ignore]` Postgres tests are not run: nothing on that side changes,
  and they still compile in the test row.

## Verification

AGENTS.md says to run the whole matrix "after every change". Each item's focused red/green is the TDD loop; the
whole of steps 1-2 below also runs after item 5 (the dependency change) and after item 7 (the last code or config
change), at the final clean HEAD after a rebase onto the current `origin/main`, and again after every review-fix
commit set and every rebase. The matrix therefore runs on every state that can land: the PR is squash-merged, so
no intermediate commit ever reaches `main` alone, and per-commit evidence would have no consumer. Items 1-4 and
8-10 change no behaviour the next full run would not catch.

Expected intermediate results: after item 5, `cargo deny check` fails with exactly RUSTSEC-2026-0440 and
RUSTSEC-2026-0462 (`bans ok, licenses ok, sources ok`) and every other row except clippy is rc 0; from item 7 on,
`cargo deny check` is rc 0. The expectations below are the final ones.

1. Prepare: `git status --porcelain` empty; `cargo install cargo-fuzz --locked` (absent on this desktop);
   `cargo generate-lockfile` and `cargo generate-lockfile --manifest-path fuzz/Cargo.toml` (both gitignored; this is
   CI's fresh resolution).
2. The AGENTS.md matrix plus the MSRV job, each unpiped into `$LOG`: `cargo fmt --all --check`;
   `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `cargo build --workspace`;
   `cargo build --workspace --all-features`; `cargo test --workspace --all-features`; `cargo check -p memory-engine`;
   `cargo check -p memory-engine --no-default-features --features backend-sqlite`; with
   `RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links"`,
   `cargo doc --no-deps -p memory-engine` and `… --all-features`; `cargo deny check`; `cargo +nightly fuzz build`;
   `cargo +1.89 build --workspace --tests --examples` and `… --all-features …`. Expected: every rc 0 except clippy
   (unless #1032 has landed, when clippy must be rc 0 too).
3. Clippy attribution. Base: `git archive "$(git merge-base HEAD origin/main)"` extracted into a scratch dir;
   head: the worktree. In each, `cargo generate-lockfile` then
   `cargo clippy --workspace --all-targets --all-features --message-format=short -- --cap-lints warn`
   (caps every lint at warn, so no crate hides behind a failed dependency; rc must be 0), keep
   `grep -E '^[^ ]+:[0-9]+:[0-9]+: warning'`, strip the line and column, `sort | uniq -c`. HEAD's list must hold no
   entry (or higher count) absent from base's; an entry that disappears at HEAD is noted, not a failure. Every error
   site of row 2's clippy log must appear in HEAD's list. Expected entries, all #1032's: me-types
   `assert_is_empty` and `empty_enums`, `crates/me-backend-sqlite/src/store/mod.rs` (`chunks_exact` → `as_chunks`),
   `crates/me-ingest/src/lib.rs` (redundant clone). A difference is this PR's to fix; an entry #1032 does not list
   goes to Collateral findings.
4. Facts: `grep -m1 '^rust-version' Cargo.toml` → `rust-version = "1.89"`; `cargo metadata` gives 1.89 for every
   package; `git grep -n 'rusqlite = ' -- '*Cargo.toml'` → six lines, all `"0.43"`; the bundled `sqlite3.h` of
   libsqlite3-sys 0.40 defines `3.54.1`; `git log --format=%B origin/main..HEAD` has no Co-Authored-By and no
   closing keyword before any issue number.
5. Acceptance map: deny passes → items 5-7, step 2; ignores documented → items 6, 7; other rows pass → steps 2-3.

## Delivery

1. Commit this plan and its review files (`docs(plans): add the #1033 plan`) before item 1; the lens drafts move
   to the private artifact directory. The plan issue is published once, before approval, by finish-pr's
   `archive-plan-issue.sh` (no `--pr`); `/finish-pr` reuses it and never publishes a second.
2. `git push -u origin fix/1033-rustsec-advisories`; `gh pr create --repo dutiona/memory-engine --base main` with the
   issue's title and a body holding: `Closes #1033`, `Refs dutiona/infra#37` and
   `Classification: Security patch (user-confirmed)` (no other Constraints text: the rest is internal workflow);
   one line per advisory and its disposition; the gate
   table (row → rc) at the head SHA; the clippy row as #1032's with step 3's output; "RUSTSEC-2026-0459 is closed by
   the requirement bump; `Cargo.lock` is gitignored, so nothing is pinned"; the three behaviour changes; Collateral
   findings, each with its recommended action:
   - fixed here as boy-scout commits: items 2, 9, 10, and CLAUDE.md's false `time` claim (item 8);
   - **recommend an issue**: `crates/me-backend-sqlite/src/store/schema/pragmas.rs` prepares each PRAGMA and drops
     the unstepped `Rows`. SQLite applies `synchronous` and flag pragmas at prepare time, but `journal_mode` only
     when stepped, so file databases opened through `open_connection` never enter WAL and run a rollback journal
     with `synchronous=NORMAL`. SQLite's PRAGMA page, verbatim: "There is a very small (though non-zero) chance
     that a power failure at just the wrong time could corrupt the database in journal_mode=DELETE on an older
     filesystem." Observed
     2026-11-16 (probe test in a throwaway copy, rusqlite 0.43): `journal_mode=delete synchronous=1 foreign_keys=1
     busy_timeout=5000`; the code path is the same on 0.34. No test checks `journal_mode`. Fix sketch:
     `execute_batch` or `pragma_update`, plus a test asserting `wal` on a file-backed connection;
   - **recommend an issue**: `docs/reference/api.md`'s `MemoryError` table is stale beyond the removed
     `Database(rusqlite::Error)` (about 12 variants missing; `Conflict` and `Migration` payloads changed);
   - any clippy entry step 3 finds that #1032 does not list.
   Michaël decides which become issues. Then check the PR's `closingIssuesReferences` is [1033].
3. `/super-review` at the Full gate through the persisted route; each fix is a new commit, then Verification again.
4. `/finish-pr` to READY with a fresh Item-4 re-poll of 0; it archives the plan on the plan issue and removes
   `docs/plans/` in one commit. READY carries the one attributed #1032 clippy row (Constraints); the READY report
   says so. Stop at READY: the merge is Michaël's. CI (`workflow_dispatch` only, #989) is dispatched only if
   Michaël asks.
