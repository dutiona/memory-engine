# dutiona/memory-engine#1088: follow-up group B (eight members, one PR)

## Outcome

One PR on `chore/1088-followup-group-b` lands one commit per member (#1067, #1038, #1085, #1077, #1052, #1041,
#1046, #1071) plus eight boy-scout commits, each in its own commit. Each member's own acceptance holds at the final
HEAD, and the AGENTS.md matrix, both MSRV builds, the new Sphinx `-W` build (also from a clean export), a 60 s
`fts_query` fuzz run and the bounded image build pass there. The PR closes the eight members, #1088 and #960.

Decisions on the choices the issues left open:

| Choice | Decision | Reason |
| --- | --- | --- |
| #1067 rule | An error reads as "no match" only if it is `SqliteFailure` with `extended_code == rusqlite::ffi::SQLITE_ERROR` (1) **and** the MATCH string alone fails the same way: `SELECT rowid FROM facts_fts WHERE facts_fts MATCH ?1 LIMIT 1`, stepped, fails with `SqliteFailure` and extended code 1. The probe runs only after a failure. If the probe steps cleanly (a row or none) or fails any other way, the ORIGINAL error propagates as `StorageError::backend`; so does every other error, a row that fails to decode included. | Every malformed MATCH probed fails with a bare 1 under varying messages, so the message cannot decide. A corrupt FTS index fails with 267 (`SQLITE_CORRUPT_VTAB`). But a bare 1 also comes from the filter fragment's JSON functions: on a `metadata` value that is not valid JSON (the CHECK stops the library writing one; tampering, an outside writer or another backend can), or a consumer key that makes a bad JSON path. The probe attributes the error to the query string; a string that fails alone matches nothing, so "no match" is then exact. |
| #1041 drift check | A me-types unit test derives each variant's declaration from `error.rs`'s own text (`include_str!`) and requires the api.md table's first column to equal that list, in order. | One source (the enum), exact payloads, no second list to maintain. rustfmt, enforced by the fmt gate, fixes the layout the test reads, and the test panics on any layout it does not know. A macro with an exhaustive match was rejected: it adds a hand list beside the enum. |
| #1071 bound | `--memory=32g --cpu-period=100000 --cpu-quota=1600000` and `--layers=false`, not `--cpuset-cpus`. **Deviates from the issue's acceptance text; Michaël's plan approval covers it explicitly.** | `--cpuset-cpus` fails under rootless podman, which is delegated only the cpu, memory and pids controllers (measured, knowledge-base#600). Podman's default layer cache would keep the builder stage's layers after `podman rmi`. |
| #1077 | Remove the dependency; `backend-postgres = []` keeps compiling the inert `PgFactory`. | No source in me-test-support names `me_backend_postgres`; #635 adds it with the first code that does. |
| #1085 scope | The workspace table registers `cfg(fuzzing)`. The facade, me-backend-sqlite and memory-engine-embed inherit it. mcp and cli keep their own tables. | sqlite's and embed's tables duplicate the facade's for `cfg(fuzzing)` only, once embed gets its missing #725 gate (B3). The two binaries leave out `unwrap_used` deliberately (it guards a consumer's process), and Cargo cannot inherit a table minus one lint. |
| #1046 fixes | Each warning class at its cause (item 13). `docs/plans/` is excluded: a branch's plan is archived to an issue before merge, and without the exclusion this very branch fails `-W`. `suppress_warnings = ["myst.header"]` hides nothing (measured: 0 such warnings on a full export of 9b9d676), so it is deleted. | — |
| Docs lock (B7) | Commit `uv.lock`; the gate runs `uv run --locked`. **Reverses the `uv.lock` ignore added with the docs in #21 (no reason recorded); Michaël's approval covers it, or strike B7 and accept the risk in the PR.** | pyproject only sets floors, so a new Sphinx/MyST/theme release could turn `-W` red with no change here: a gate that breaks because upstream shipped. |
| #960 | `Closes #960`, if its acceptance (zero warnings) holds on the final build and its STOP trigger did not fire; else `Refs #960`. | Michaël on #1046: "link or close #960 when this lands". Its STOP trigger does not fire: `roadmap` is dropped, not re-pointed at the excluded `ROADMAP.md`. |

## Constraints

- One PR, one commit per member (Conventional Commits, imperative, body says why, no Co-Authored-By line),
  one `Closes #N` line per member plus `Closes #1088`; `Refs dutiona/infra#37` (never bare #37).
- Classification Bug fix / Standard path / Super planning / Full post-implementation review, Default severity.
- Each member's own acceptance is the bar. Michaël confirmed the lane card: #1038 = DELETE.
- CI is not used (no credit): the AGENTS.md Verify matrix run locally is the merge gate. Heavy commands run one
  at a time (the implementer's environment, not written into the repository).
- Every image build is bounded with `--memory=32g --cpu-period=100000 --cpu-quota=1600000` (16 CPUs) and removes its
  own image.
- Overlaps: ME#1021 may touch crates/me-backend-sqlite/src/search/fts.rs (#1067): the second PR to land rebases.
- Boy-scout rule: rot found on the way is fixed in its OWN commit, separate from the member commits.
- Less is more: smallest change that fully solves each item; no speculative options.
- TDD: failing test first where behaviour changes.

## Implementation sequence

Gates run unpiped, output redirected to a log outside the tree, exit code read. `cargo fmt --all` before each commit.

- [ ] **Worktree**: `.worktrees/chore-1088-followup-group-b` exists and is locked, on origin/main 9b9d676 or later.

- [ ] **0. Baseline (no commit).** `git fetch origin`, then `git log HEAD..origin/main --
  crates/me-backend-sqlite/src/search/fts.rs` is empty (else rebase). The Sphinx red log is saved. `cargo +nightly
  fuzz build` passes before any change (fuzz/ has no committed lock, so an upstream break is separated from this PR).

- [ ] **1. #1067** — crates/me-backend-sqlite/src/search/fts.rs; header comment of fuzz/fuzz_targets/fts_query.rs.
  - Red, in `fts.rs` tests, each seeding a fact containing "Rust":
    - `corrupt_fts_index_is_a_storage_error`: after `DELETE FROM facts_fts_data`, the test first checks its own
      injection (a raw `SELECT count(*) FROM facts_fts WHERE facts_fts MATCH 'Rust'` fails with an extended code other
      than 1), then asserts `fts_search(.., "Rust", ..)` and `fts_count_expired(.., "Rust", ..)` are
      `Err(MemoryError::Storage(StorageError::Backend(_)))`.
    - `malformed_metadata_under_a_valid_query_is_a_storage_error`: tamper as facts.rs:1756-1774 does
      (`PRAGMA ignore_check_constraints = ON`, `UPDATE facts SET metadata = '{'`, pragma back OFF), then
      `fts_search_filtered` with `FilterSql { where_clause: "json_extract(f.metadata, ?) IS NOT NULL", params: ["$.k"] }`
      is that `Err`.
    - `cargo test -p me-backend-sqlite --lib search::fts` fails on both assertions (today `Ok`).
  - Characterization, green before and after: `malformed_match_strings_read_as_no_match` loops `"unbalanced`,
    `foo AND`, `(`, `NEAR(a b`, `^`, `a + `, `"a" OR`, `nocol:rust`, `*`: each raw MATCH fails with extended code 1 on
    the bundled SQLite (pinning the premise), and `fts_search` / `fts_count_expired` return `Ok` empty / `Ok(0)`.
  - Change: one private `fn no_match_or_fault<T>(conn: &Connection, query: &str, err: rusqlite::Error, no_match: T)
    -> Result<T>` applying the rule above (one `tracing::warn!` on the no-match path). `fts_search_filtered` collects
    `stmt.query_map(..)` into `rusqlite::Result<Vec<_>>` (rusqlite steps, and FTS5 parses, on the first `next()`:
    today both a syntax error and a fault ride the row-skip arm) and sends an `Err` through the helper; the
    `filter_map` skip and the wrong comment at :87-90 go. `fts_count_expired` routes its `query_row` `Err` through
    it; the `QueryReturnedNoRows` arm goes (`COUNT(*)` always yields a row). The three fns' docs say a MATCH string
    FTS5 rejects yields empty/0 and every other failure is `MemoryError::Storage`. The fuzz seam keeps its `expect`s
    (on a healthy store every string must be `Ok`); its doc and the target header say the error surfaces at the step.
  - Green: `cargo test -p me-backend-sqlite --all-features` (incl. `lexical_malformed_query_is_empty_not_error` in
    sqlite/search_index.rs), `cargo test -p me-test-support --all-features` (battery
    `search_malformed_query_yields_empty`), `cargo +nightly fuzz build`, then `cargo +nightly fuzz run fts_query --
    -max_total_time=60` exits 0.
  - Commit: `fix(search): propagate FTS storage errors instead of returning an empty result`

- [ ] **2. Boy-scout B1** — fts.rs test comments cite `fts.rs:30-83` / `fts.rs:43-45`, lines that no longer hold that
  code; drop the line numbers. Commit: `test(search): drop stale line numbers from fts test comments`

- [ ] **3. #1038** — crates/me-types/src/types/cycle_report.rs (delete the module allow and its comment, :16-21, and
  the doc and enum, :228-232); drop `CycleAnomaly` from the re-export lists in crates/me-cognitive/src/lib.rs:117 and
  crates/memory-engine/src/lib.rs:310.
  - Red: none (pure deletion; every consumer compiling, fuzz/ included, is the check).
  - Green: `git grep -n -e CycleAnomaly -e empty_enums -- '*.rs'` empty; `cargo build --workspace --all-features`;
    clippy; `cargo +nightly fuzz build`.
  - Commit: `refactor(types): delete the unused CycleAnomaly placeholder`. Body: lane-card call, nothing names it,
    all three crates are `publish = false`.

- [ ] **4. Boy-scout B3** — memory-engine-embed is the only library crate without #725's `unwrap_used = "deny"`
  (AGENTS.md rule 9). Add it to its `[lints.clippy]`; `cargo clippy -p memory-engine-embed --all-targets -- -D
  warnings` fails on test code only (every `unwrap` under `src/` is in a `#[cfg(test)]` module or a doctest). Add
  `#![cfg_attr(test, allow(clippy::unwrap_used))]` to `src/lib.rs` (me-types' form) and the file-local
  `#![allow(clippy::unwrap_used)]` line the workspace `tests/` use to the three `tests/*.rs` that unwrap
  (`proposer_integration.rs` does not). Green: the same clippy exits 0. Commit: `chore(build): hold
  memory-engine-embed to the #725 unwrap gate`

- [ ] **5. #1085** — Cargo.toml; the `[lints.*]` tables of crates/memory-engine, crates/me-backend-sqlite,
  crates/memory-engine-embed.
  - Red: with the facade on `[lints] workspace = true` alone, `touch crates/memory-engine/src/lib.rs && cargo check -p
    memory-engine` warns `unexpected cfg condition name: fuzzing`.
  - Change: `[workspace.lints.rust]` gains `unexpected_cfgs = { level = "warn", check-cfg = ['cfg(fuzzing)'] }` with a
    one-line comment (set only by `cargo fuzz`). The three crates' tables and their justifying comments become
    `[lints]` / `workspace = true`. The root comment drops "migrates in a later slice" and says the two binaries keep
    their own table without `unwrap_used`, which guards a consumer's process; the `unwrap_used` comment gains the
    facade's note on file-local opt-outs for integration tests, benches and examples.
  - Green: the red command prints no warning; `cargo check -p me-backend-sqlite` likewise; clippy; fuzz build.
  - Commit: `refactor(build): move the facade, sqlite and embed crates onto the workspace lint table`

- [ ] **6. #1077** — crates/me-test-support/Cargo.toml, Cargo.lock.
  - Red (measured on cargo 1.100.0): `touch crates/me-test-support/src/lib.rs && cargo check -p me-test-support
    --all-features` prints `warning: unused dependency me-backend-postgres` (`cargo::unused_dependencies`).
  - Change: delete the dependency line; drop `"me-backend-postgres?/test-util"` from `test-util` and the weak-forward
    clause from its comment; `backend-postgres = []` under "compiles the inert, `#[ignore]`d `PgFactory` and postgres
    suites; #635 adds `dep:me-backend-postgres` with the first code that names `PgBackend`". One plain `cargo check`
    rewrites the lock; never `cargo update`.
  - Green: the red command is silent; `git diff Cargo.lock` removes exactly one line; `cargo build --workspace
    --locked`; `cargo test -p me-test-support --all-features`; `cargo deny check`; MSRV (below).
  - Commit: `chore(deps): remove me-test-support's unused me-backend-postgres dependency`

- [ ] **7. #1052** — crates/me-types/src/types/scope.rs, crates/me-backend-sqlite/src/store/scopes.rs.
  - Change: scope.rs gets its first `#[cfg(test)] mod tests` with `validate_segment_rejects_a_slash`:
    `assert_eq!(validate_segment("a/b"), Err("scope label must not contain '/'"))`, under a comment that
    `ensure_path` and `resolve_path` split on '/' first. In scopes.rs, `ensure_path_rejects_slash_in_label` becomes
    `ensure_path_rejects_leading_whitespace_label`, loses its excuse comment and asserts its reason
    (`ConflictError::ScopeLabel(m)` with `m == "scope label must not have leading/trailing whitespace"`).
  - Red (the rule exists, so by mutation): delete the `contains('/')` branch, `cargo test -p me-types
    validate_segment` fails, restore.
  - Green: `cargo test -p me-types validate_segment`; `cargo test -p me-backend-sqlite --lib store::scopes`.
  - Commit: `test(storage): rename the leading-whitespace scope test and test the slash rule`

- [ ] **8. Boy-scout B2** — the `ScopeStore::insert` doc in scopes.rs credits `validate_segment` with the whitespace
  rule, which only `validate_label` enforces; reword. Commit: `docs(storage): stop crediting validate_segment with the
  whitespace rule`

- [ ] **9. #1041** — crates/me-types/src/error.rs (`mod tests`), docs/reference/api.md.
  - Red: `api_md_error_table_matches_the_enum`. Enum side: the text of `include_str!("error.rs")` from
    `pub enum MemoryError {` to the next line `}`. A variant starts at a line of exactly four spaces then an ASCII
    uppercase letter. If it ends with `,`, its declaration is the line without the comma; if it ends with `{`, the
    following field lines (skipping `///` lines) up to `},` are joined as `Name { a: T, b: U }`; any other ending
    panics (a layout the test does not know must fail, not drop a variant). Every `#[from] ` is dropped. Table side:
    from `include_str!("../../../docs/reference/api.md")`, the lines after "`MemoryError` variants:", skipping blank
    lines and rows whose first cell is not backticked (header, separator), up to the first non-table line; each row's
    first cell between its backticks. `assert_eq!` on the two `Vec<String>`s (order, uniqueness, completeness and
    payloads at once). `cargo test -p me-types api_md_error_table` fails (9 rows against 20).
  - Change: the table becomes 20 rows in declaration order, each first cell the derived declaration verbatim (e.g.
    `Serialization(serde_json::Error)`, `EmbeddingDimension { expected: usize, actual: usize }`,
    `EmbeddingModelMismatch { expected: Box<crate::types::EmbeddingFingerprint>, actual: Box<crate::types::EmbeddingFingerprint> }`,
    `ReadOnly`), each Trigger one clause from the variant's rustdoc; `Internal` loses "I/O failure", now `Io`. One
    sentence says the enum is `#[non_exhaustive]`.
  - Green: the test passes; a one-off mutation (add `#[error("dummy")] Dummy,` to the enum) turns it red; revert.
  - Commit: `docs(reference): bring the api.md error table back in line with the error enum`

- [ ] **10. Boy-scout B4** — api.md's "Re-exported Types", corrected against crates/memory-engine/src/lib.rs:
  `PersistenceClassifier` IS re-exported at the root (the `pub use me_traits::{…}` block), so the "not re-exported" note goes;
  `serialize_embedding` / `deserialize_embedding` are NOT root re-exports (they live at `me_backend_sqlite::store`),
  so they leave the list; the Error line gains the six typed error enums; the Traits line lists the `me_traits` items
  the root re-exports. Commit: `docs(reference): correct api.md's list of crate-root re-exports`

- [ ] **11. Boy-scout B5** — tests/eval/main.rs and crates/memory-engine/src/engine/equivalence.rs name `docs/plans/`
  files that no longer exist; keep the issue/PR pointers, drop the paths. Green: `git grep -n 'docs/plans/' -- '*.rs'`
  empty. Commit: `docs: drop references to plan files removed from the repository`

- [ ] **12. Boy-scout B7 (pending Michaël's approval, see Decisions)** — commit `uv.lock` (drop it from .gitignore;
  generate with `uv lock`). Green: `uv lock --check` exits 0 and `git ls-files --error-unmatch uv.lock` succeeds (the
  lock is tracked, not just present). Commit: `build(docs): commit uv.lock so the docs build is reproducible`. If
  struck, `--locked` is dropped everywhere it appears (item 13's red command and gate line, Verification 3) and the PR
  records the accepted risk.

- [ ] **13. #1046** — docs/conf.py, docs/index.md, docs/advanced/index.md, docs/design/index.md,
  docs/advanced/hybrid-search.md, docs/design/architecture-overview.md, docs/design/geometric-associative-memory.md,
  AGENTS.md, CLAUDE.md, GEMINI.md, .claude/skills/qa-sweep/SKILL.md (the fourth gate copy `tests/gate_matrix_parity.rs`
  reads), CONTRIBUTING.md.
  - Red: `uv run --locked sphinx-build -b html -W --keep-going -E docs docs/_build` exits 1: 26 warnings on 9b9d676
    (#1046 counted 31; measured 29 on 7a22c07, the two not attributed; #1086 then listed three `reference/*` orphans), plus one orphan per file under docs/plans/.
  - Change, by cause:
    1. `html_static_path` names a missing `_static`: delete it.
    2. design/index.md names absent `roadmap` (ROADMAP.md is excluded on purpose as a frozen record) and
       `plans/2026-03-09-future-phases-design` (deleted by 7bddc48): delete both entries.
    3. Six orphans (#960 listed nine; #1022 took three): advanced/index.md gains `dream-cycle`, `associative-recall`,
       `migration-builder`; design/index.md gains `geometric-associative-memory`; docs/index.md gains a
       `:caption: Audits` toctree of the two audits.
    4. Unknown lexer `mermaid` (7 plain fences): `myst_fence_as_directive = ["mermaid"]`, so the fence GitHub renders
       natively also renders in Sphinx.
    5. Stale `../../src/…` links (7): repoint to `../../crates/…` (crates/me-traits/src/lib.rs,
       crates/memory-engine/src/engine/scheduling.rs, crates/me-types/src/types/facts.rs,
       crates/me-backend-sqlite/src/store/facts.rs), link text without `:line` (the line numbers are what rotted).
    6. Three links into a private sibling research repo (3): plain citations naming that repository, file names verbatim.
    7. `exclude_patterns` gains `"plans"` (comment: a branch's plan, archived to an issue before merge).
    8. `suppress_warnings = ["myst.header"]` hides nothing (measured): delete it.
    9. Gate: the Verify block of AGENTS.md, CLAUDE.md, GEMINI.md and SKILL.md gains, after `cargo deny check` and
       before the fuzz line (each file says the fuzz line is last), `uv run --locked sphinx-build -b html -W --keep-going
       -E docs docs/_build   # Sphinx site: a warning fails it (#1046); local only, no CI job runs Sphinx`. CLAUDE.md's
       Commands line and CONTRIBUTING.md:38 become the same command.
  - Green: the red command exits 0 with no warning; `grep -c 'class="mermaid' docs/_build/advanced/associative-recall.html`
    is 5; `cargo test -p memory-engine --all-features --test gate_matrix_parity`.
  - Commit: `docs(build): clear the Sphinx warnings so the docs build passes with -W`

- [ ] **14a. Boy-scout B8** — docs/reference/api.md carries two `### Bootstrap` sections (one near the top, one before
  `BootstrapConfig`); delete the first, whose rows the second repeats with fuller text. Green: one `### Bootstrap` and
  the Sphinx command exits 0. Commit: `docs(reference): drop the duplicated Bootstrap section from api.md`

- [ ] **14. Boy-scout B6** — `templates_path` names a missing `_templates`; silent, so dead config, not #1046. Delete.
  Green: the Sphinx command exits 0. Commit: `docs(build): drop the templates_path that names no directory`

- [ ] **15. #1071** — AGENTS.md, CLAUDE.md, GEMINI.md (after the MSRV paragraph), SKILL.md (after its MSRV paragraph),
  identical text:

  > **Image build**: when a change touches `Containerfile`, `.containerignore` or `Cargo.lock`, build the server image
  > from the committed tree (commit first: `git archive` reads `HEAD`) and run both binaries from it. It is heavy: it
  > compiles the workspace again, in release, inside the container. It is bounded to 16 CPUs and 32 GiB by a CPU quota
  > and a memory limit (`--cpuset-cpus` fails under rootless podman, which is usually delegated only the cpu, memory
  > and pids controllers); it keeps no intermediate layers, and it removes its image:

  ```bash
  git archive --format=tar HEAD | podman build --pull=newer --layers=false --memory=32g \
      --cpu-period=100000 --cpu-quota=1600000 -t memory-engine:verify -
  podman run --rm --network=none memory-engine:verify --version            # memory-engine-mcp, the entrypoint
  podman run --rm --network=none --entrypoint memory-engine-cli memory-engine:verify --version
  podman rmi memory-engine:verify
  ```

  - Red: none (instructions). Green: `gate_matrix_parity` passes (the block holds no `cargo fmt` anchor). The block runs
    in Verification, since item 6 changed `Cargo.lock`.
  - Commit: `chore(verify): build the image from an archive when Containerfile, .containerignore or Cargo.lock change`.
    Body: the `--cpuset-cpus` deviation and its measured reason.

- [ ] **Verification** (below) at the final HEAD. A fix goes in a new commit named for the item it amends.
- [ ] **Plan issue**: the archiver publishes the one plan issue (already at the approval handoff); never a second one.
- [ ] **PR**: one `Closes` line each for #1038, #1041, #1046, #1052, #1067, #1071, #1077, #1085, #1088, then
  `Closes #960` (or `Refs #960`), then `Refs dutiona/infra#37`; `Classification: Bug fix (user-confirmed)`; the
  decisions above, the #1071 deviation, the eight boy-scout commits, the 31→29→26 and 9→6 counts, that Read the Docs
  (`.readthedocs.yaml`) already builds with `-W` and turns green with this PR, and Verification output as received.
- [ ] **/super-review** at the confirmed Full gate; resolve findings; rerun affected gates.
- [ ] **Merge**: Michaël only, after `/finish-pr` READY.

## Documentation

- docs/reference/api.md: the error table (#1041), the re-export list (B4), the duplicated Bootstrap section (B8).
- Sphinx: conf.py and the three index files, four link fixes (#1046), `templates_path` (B6), `uv.lock` (B7).
- Verify blocks of AGENTS.md, CLAUDE.md, GEMINI.md, .claude/skills/qa-sweep/SKILL.md: the `-W` line (#1046), the image
  step (#1071); CLAUDE.md Commands and CONTRIBUTING.md docs command (#1046).
- In code: fts.rs and fuzz-target docs (#1067); root Cargo.toml lint comment (#1085); me-test-support feature comment
  (#1077); `ScopeStore::insert` doc (B2).
- No public API is added; no ADR (no decision reversed); no CHANGELOG exists.

## Testing

- New, red first: `corrupt_fts_index_is_a_storage_error`, `malformed_metadata_under_a_valid_query_is_a_storage_error`
  (#1067); `api_md_error_table_matches_the_enum` (#1041).
- New, proven by mutation: `validate_segment_rejects_a_slash` (#1052); the drift test against a dummy variant.
- Characterization: `malformed_match_strings_read_as_no_match`; must still pass: `fts5_syntax_error_returns_empty`,
  `fts_count_expired_syntax_error_returns_zero`, `lexical_malformed_query_is_empty_not_error`, the battery's
  `search_malformed_query_yields_empty`, `gate_matrix_parity`.
- Fuzz: 60 s of `fts_query`, whose `expect`s assert that every query string maps to `Ok` on a healthy store.
- No unit test, by design: #1038 (deletion: the compiler), #1085 and B3 (lint config: the warning/clippy red-green),
  #1077 (the cargo warning), #1071 (instructions; run in Verification), #1046 (the `-W` build is the test). No
  decode-error test: no row of this schema can fail to decode, and a decode error takes the same `Err` path as the
  injected fault.

## Verification

At the final HEAD, one heavy command at a time, unpiped, output to a log, exit code read:

1. The AGENTS.md matrix in its order, from `cargo build --workspace --locked` (first) to `cargo +nightly fuzz build`
   (last), with the new Sphinx line. The `cargo build --workspace --all-features` log has no `unused dependency` line.
2. MSRV (item 6 changes a dependency): `cargo +1.89 build --workspace --tests --examples`, then with `--all-features`.
3. Sphinx from a clean export of the whole tree: `git archive HEAD | tar -x -C <empty dir>`, then from that dir
   `uv run --locked sphinx-build -b html -W --keep-going -E docs _build` exits 0 (#1046's "on a clean checkout").
4. `cargo +nightly fuzz run fts_query -- -max_total_time=60` exits 0, no crash artifact.
5. Image: `podman images -a -q | sort` before and after the item-15 block. The build exits 0, both `--version` lines
   print, and after `podman rmi` no new ID remains except refreshed `rust`/`debian` bases. A leftover or exit 137 is a
   stop: report it; never prune other images, never drop the bound.
6. `git log --format=%B origin/main..HEAD | grep -c Co-Authored-By` is 0; `git log --oneline origin/main..HEAD` shows
   the items in order.
7. After the PR opens, its `closingIssuesReferences` are exactly the ten issues above (nine without #960).

## Surfaced, not done (follow-ups for the orchestrator)

- docs/reference/api.md's method tables drift from the code: `execute_query` is documented as returning
  `Result<Vec<SearchResult>>` but returns `Result<QueryResponse>`, and async methods are shown as sync. The page
  hand-copies rustdoc signatures, so this is systemic, not one-line rot; recommend one issue to shrink the page to a
  map onto rustdoc or extend the drift check to its method tables.
- Accepted: `git archive` packs tracked files, and podman applies `.containerignore` inside that tar context
  (`podman-build(1)`: an archive context is extracted and the context's `.containerignore` read; stdin `-` is not named
  explicitly), so a `.containerignore` change is exercised by the image build.
