# Clean-slate subagent review (round 1, plan SHA-256 3a438b9e284f0912d602d40174f14dfb4c62c0c8c1292f62b33c4e60d0d24897)

Reviewer: Claude general-purpose subagent, no prior context, read-only. Response recorded verbatim below.

**Verdict: approve with changes. There are no BLOCKER or HIGH findings. Three MEDIUM fixes should go into the plan before execution.** Confidence is high. I checked every Red/Green count, file location and API claim the plan makes against the snapshot and the rusqlite 0.34/0.43, libsqlite3-sys and rmcp 1.8.0 sources. They all hold, and every commit builds on its own in the stated order.

Altitude report SHA-256: 812499d868adf06a99492f592edb2263253d3ae33def5d1d855eabf5a7bb2fb9

## Altitude and duplication (before completeness)

The report's plan hash (`3a438b9e…`) matches `plan.md`.

**I accept the disposition: proceed.**
- The literal text is decision content, not a transcript:
  - The comment and doc wording in items 1, 3 and 5–10 is the change itself.
  - Item 5 specifies a signature and its behaviour, not code.
  - The ignore wording in items 6 and 7 is public security wording.
- "Evidence already measured" matches the research context (1997 passed, 81 ignored, one temp-dir failure). It is provenance, not a second spec.
- The only text that restates another owner's rules is in Delivery: finish-pr's Item 4 and Item 5 numbering, and the full archive-plan-issue invocation. That gives skill-owned procedure a second home. It is tolerable in an ephemeral plan and not a reason to reject.
- Nothing else is duplicated: no second validator, generator or manifest.

## Findings

**[MEDIUM] Delivery 2: the `pragmas.rs` collateral finding is half wrong, and the wrong half hides the real risk.**
- What the code does:
  - `set_pragmas` (`crates/me-backend-sqlite/src/store/schema/pragmas.rs:20-33`) prepares each PRAGMA and calls `stmt.query([])`.
  - In 0.43, `Statement::query` only binds and never steps (`statement.rs:223-226`).
- What SQLite does with that:
  - It applies `synchronous` during code generation, i.e. at prepare time. In the bundled `sqlite3.c:146448-146470`, `sqlite3Pragma` sets `pDb->safety_level` and calls `setAllPagerFlags`.
  - Flag pragmas such as `foreign_keys` are also applied at prepare time (146490/146493, `db->flags |= mask`).
  - Only `journal_mode` waits for a step: it emits `OP_JournalMode` (146083).
- So the real state is:
  - WAL is never enabled.
  - `synchronous = NORMAL` is enabled.
  - Every file database opened through `open_connection` (`schema/mod.rs:50-52`) therefore runs a rollback journal with `synchronous=NORMAL`. SQLite documents that pairing as able to corrupt the database on power loss.
  - No test checks `journal_mode`.
- Fix to the plan:
  - Observe it once: run `PRAGMA journal_mode` against a file database opened via `open_connection`, in a scratch directory, and log the result to `$LOG`.
  - State the mechanism correctly in the public PR body.
  - Recommend that Michaël file an issue. The fix would be `execute_batch` or `pragma_update`; 0.43's `execute_batch` accepts statements that return rows (`lib.rs:559-580`).
  - This is not this PR's fix, but "likely, no action" breaks the rule against silently deferring.

**[MEDIUM] Item 10 fixes 2 of the 6 places that still name the removed `async` feature.**
- `README.md:147` has `features = ["async"]`. That feature was removed in #702 (`crates/memory-engine/Cargo.toml:210`), so anyone who copies the snippet gets a cargo resolution error.
- README.md is already edited by item 8 (lines 4 and 157). That is exactly the reason item 10 gives for touching `installation.md`.
- The same stale text is also in:
  - `docs/reference/api.md:215-216` (the feature-list entry)
  - `docs/usage/inspection.md:267`
  - `docs/design/architecture-overview.md:105`
- Fix: extend item 10 to all of them, in one boy-scout commit.
  - Red: `git grep -nE '"async"|`async` feature' -- README.md docs` returns 6 lines.
  - Green: the same grep prints nothing.
  - `CLAUDE.md:40` and `GEMINI.md:70` already say correctly that the feature is gone.

**[MEDIUM] Documentation and the PR body leave out a third consumer-visible change: the libsqlite3-sys `links` conflict.**
- `libsqlite3-sys` declares `links = "sqlite3"` (0.40.0 `Cargo.toml:19`), so only one version of it can exist in a dependency graph.
- Any downstream crate that also depends on rusqlite outside 0.43.x will now fail to resolve.
- The plan says it records "the two consumer-visible changes". Add this as a third.

**[LOW] Item 4's commit body overstates its facts.**
- "The only LIMIT that errored past `i64::MAX`" is false: `store/fact_vectors.rs:61` (`next_backfill_window`) deliberately returns `MemoryError::Internal`. Say "the only one that leaked a `ToSql` conversion error".
- "rusqlite 0.41+ drops `usize: ToSql`" is not quite right. In 0.43 the impl sits behind the opt-in `fallible_uint` feature (`types/to_sql.rs:276-283`). Say "no longer implements it by default".
- The choice not to enable `fallible_uint` is still correct, because saturating the LIMIT is the crate's house rule.

**[LOW] Item 7's `cargo tree` check is ambiguous.**
- The inverted tree also lists me-backend-postgres's own dependents (memory-engine, me-test-support, and so on), so "shows only the me-backend-postgres dev edge" can be misread.
- A cleaner check: `cargo tree -i astral-tokio-tar --all-features -e normal,build` prints nothing, and in the `-e dev` output the only parent of testcontainers is `me-backend-postgres [dev-dependencies]`.

**[LOW] Verification step 3's "the two lists must be equal" is too strict.**
- A warning that disappears at HEAD is not a regression, but it would fail an equality check.
- Require instead: no entry at HEAD that is missing at base, and note any entry that disappeared.

**[LOW] Item 6: the `[[bans.features]]` comment should name its removal trigger.**
- Say it goes with the ignore under #1035.
- Otherwise, after the move to rmcp 3.9+, a leftover ban blocks a legitimate client feature for no reason.

**[LOW] Item 8 contradicts its own rule that "the docs give the value, `Cargo.toml` keeps the reason".**
- The new text for `CLAUDE.md:107` ("set by rusqlite 0.41+ … RUSTSEC-2026-0459") and for `SKILL.md:57` ("rusqlite 0.43") restates the reason in two more places.
- Either drop the reason from those two, or accept the drift on purpose.

**[LOW] The Delivery 2 collateral list needs a recommended action per item.**
- `docs/reference/api.md`'s `MemoryError` table is stale in many places, not only `Database(rusqlite::Error)`. About 12 current variants are missing, and `Conflict` and `Migration` have changed payloads.
- That is a reasonable candidate for a separate issue. Say so in the PR body, so it is not left as a bare mention.

## Verified (no finding)
- **Red/Green counts:** item 1 (1 line), item 3 (4 lines), item 8 (14 lines), item 9 (1 line), item 10 (2 lines) all match the snapshot. Six `rusqlite =` lines remain after item 2.
- **Item 2:** the trace dev-dependency really is dead; no `trace_v2` or `.trace(` call exists outside `ann.rs`.
- **Item 4:** goes red on 0.34, where `to_sql_self_fallible!(usize)` returns `ToSqlConversionFailure`. The existing test inserts 3 rows. The cited sibling LIMIT lines are correct.
- **Item 5, `trace_v2`:**
  - 0.43's signature and `TraceEvent::Stmt(StmtRef, &str)` match the plan.
  - `None::<fn(TraceEvent<'_>)>` is the form rusqlite's own test uses.
  - Clearing the callback drops it (via `set_clientdata`), and 0.43 has no `Connection::trace`.
  - `HnswStrategy` holds no borrow of `conn`, so `&mut conn` is fine.
- **Item 6:**
  - The advisory names only the `-reqwest` and `-unix-socket` clients, and both enable `transport-streamable-http-client`.
  - `client` does not enable that feature, so denying both is the right pair.
- **Multi-statement contingency:** a static scan of `prepare`/`execute`/`query_*` literals and SQL built with `format!` finds no multi-statement SQL. The `Error::MultipleStatement` contingency is unlikely to trigger.
- **Other inputs:**
  - The bundled build flags differ between 0.32 and 0.40 only in SQLCipher-related flags.
  - CI's `msrv` job reads the manifest, and `gate_matrix_parity` skips it.
  - `fuzz/` still resolves `rust-version.workspace = true` for path members after item 3.
- **Structure:** Documentation, Testing and Verification sections are all present and concrete.

---

# Round 2 (delta, plan SHA-256 6ba6ceca060d508187a4660c722fd408f80b574e9bd3859220c93a780707f095), same reviewer, verbatim

**Round 2 verdict: one new MEDIUM, otherwise LGTM.** Item 10's green check fails against item 10's own new wording. Fix that and the plan is ready. All nine round-1 findings are resolved correctly. Confidence is high: I confirmed that `plan-delta-r2.diff` matches `diff -u last-reviewed-plan.md plan.md` exactly, and I checked each changed claim against the snapshot.

Altitude report SHA-256: e6ce3857655094cd39dd5d440c5cc05e078486cfef384e2d42264cb13f6f4560

## Altitude (assessed first)

**I accept the disposition: proceed.**
- The report's plan hash `6ba6ceca…` matches `plan.md`, and its measurements match the file (276 lines, 25400 bytes).
- The 30 new lines are round-1 resolutions:
  - The Constraints line quotes the brief as provenance.
  - The pragmas paragraph is the PR wording itself, so it is decision content, not a transcript.
- One stale estimate: "~20 prose lines across the 7 doc files" still describes item 10 before it grew to 5 files. It does not affect the disposition.

## New findings

**[MEDIUM] Item 10's green check can never pass.**
- The new `api.md` entry reads "There is no `async` feature: …".
- That text matches the item's own grep alternative `` `async` feature ``, so "the grep prints nothing" cannot happen. The implementer would hit a false red.
- `api.md`'s "Feature Flags" section (212-216) has this single entry, so a section that only says what does not exist is noise anyway.
- Fix: reword it without the phrase, for example: "`tokio` is a non-optional dependency (#702), so no feature gates the runtime; `crates/memory-engine/Cargo.toml`'s `[features]` table lists the flags." The other option is to delete the entry and point to the table.

**[LOW] Item 10, README: dropping `features = ["async"]` leaves a duplicate snippet.**
- What remains is "For async support:" followed by a snippet identical to the one at `README.md:138-141`.
- Delete the whole block (143-148) instead.

**[LOW] Verification cadence: the expected result after item 5 is wrong for the deny row.**
- The full run after item 5 expects "every rc 0 except clippy".
- But `cargo deny check` is red at that point by design: item 5's own green lists RUSTSEC-2026-0440 and -0462 as the only findings.
- State that expectation, so the run is not read as a stop condition.

**[LOW] Delivery 2 copies "the Constraints' lines" into the public PR body.**
- That now includes the orchestrator-brief quote, "Never merge" and the tailnet rule, which is internal workflow text.
- The wording was already there in round 1, but the delta enlarges what gets pasted.
- Limit it to the PR-body line (`Closes`, `Refs`, `Classification`) plus the #1032 attribution in the gate table.

**[LOW] Delivery 2, pragmas: the corruption claim is stronger than SQLite's docs.**
- The phrase "a pairing SQLite documents as able to corrupt the database on power loss" comes from my round-1 wording, and it drops SQLite's qualifier.
- I believe the PRAGMA synchronous page says there is a "very small (though non-zero) chance … in journal_mode=DELETE on an older filesystem".
- I could not confirm that: two scrapes of sqlite.org/pragma.html returned no content.
- Check the page and quote it word for word, since this goes into a public PR.

**[LOW, optional] Item 7's second `cargo tree` check proves less than it seems.**
- `-e dev` hides normal edges. For example, testcontainers-modules → testcontainers is a normal edge and is never shown, so "one parent" is partly guaranteed by the filter itself.
- The first check (`-e normal,build --target all` → "nothing to print") is the one that matters, and it is sound.
- The manifests already show testcontainers is declared only in me-backend-postgres. The second check can stay as a label or be dropped.

## Resolutions verified
- **Item 4:** the new commit body is accurate. `fallible_uint` already gates `usize: ToSql` in 0.40.2 (`to_sql.rs:2,242,273`), so "0.41+ no longer implements it by default" is true. The "only LIMIT that leaked a `ToSql` error" wording correctly excludes `fact_vectors.rs:61`.
- **Item 10:** the red count of 6 lines across 5 files matches the snapshot. The `:!docs/plans` exclusion is needed because the committed plan contains these strings.
- **Item 6:**
  - The before/after status snapshot fixes Codex's clean-status point.
  - The bans comment names its #1035 removal.
- **Item 8:** the reason is no longer repeated in the docs.
- **Pragmas paragraph:** the mechanism matches the bundled `sqlite3.c`. `synchronous` and flag pragmas are applied at prepare time (146448-146470, 146490); `journal_mode` waits for `OP_JournalMode` at step (146083). The probe result is consistent with that. Recommending an issue and leaving the decision to Michaël is right.
- **Documentation:** the third consumer-visible change, the `links = "sqlite3"` conflict, is correct.
- **Cadence:** sound.
  - PRs are squash-merged, so the intermediate commits never reach main and do not need to bisect.
  - The full runs after items 5 and 7, at the final HEAD, after review fixes and after rebases catch anything the focused checks miss.
  - The plan narrows AGENTS.md's "after every change" openly, so Michaël's plan approval covers it.
- **Constraints line on the #1032 row:** it executes the brief's explicit allowance, and the READY report says so.

---

# Round 3 (delta, plan SHA-256 5cc6f15a732081493300ad2cc8fd81036550816bbb206ae07e04fc29e3d6d64d), same reviewer, verbatim

**Round 3 verdict: LGTM.** The delta resolves every round-2 finding and introduces no new one. Confidence is high: I confirmed that `plan-delta-r3.diff` matches `diff -u last-reviewed-plan.md plan.md` exactly (current SHA `5cc6f15a…`), and I checked each changed claim against the snapshot.

Altitude report SHA-256: 2e1eb9df6fa30b7d9c31a1cffc934d60033dc2fd66c6a6e9138671cd20300a8e

## Altitude (assessed first)

**I accept the disposition: proceed.**
- The report's plan hash and its measurements match the file (289 lines, 26422 bytes).
- The 13 new lines are resolutions: a verbatim quote, the expected intermediate results, and replacement wording. None of it is a transcript or a second copy of the product.
- The provenance paragraph names `.review/last-reviewed-plan.md` for both round 1 and round 2, which reads oddly but is accurate, since the file is overwritten each round. It is cosmetic and does not affect the disposition.

## Delta checked
- **Item 10, `api.md`:** the new sentence contains neither `"async"` nor `` `async` feature ``, so the green grep can now print nothing.
  - The path it names, `crates/memory-engine/Cargo.toml`'s `[features]` table, is where the flags actually live.
  - Collapsing the term and its definition fits a section whose only entry was `async` (212-216).
- **Item 10, README:** lines 143-148 are exactly the "For async support:" heading plus its snippet, and the snippet at 138-141 stays. Deleting the block removes the duplicate.
- **Item 7:** `testcontainers` matches only `crates/me-backend-postgres/Cargo.toml:31-32`, which sit under `[dev-dependencies]` (header at line 25).
  - Git pathspec `*` crosses `/`, so `'*Cargo.toml'` covers every member manifest.
  - Together with the `-e normal,build --target all` → "nothing to print" check, this proves the comment's claim with no filter-dependent step.
- **Verification:**
  - The expected result after item 5 (deny fails with exactly 0440 and 0462, `bans ok, licenses ok, sources ok`) matches item 5's own green.
  - "From item 7 on, rc 0" matches item 7.
  - The squash-merge rationale for the cadence is sound: only the squashed state reaches `main`, and the matrix runs on every state that can land.
- **Delivery 2:**
  - The public PR body now takes only `Closes`, `Refs` and `Classification` from Constraints.
  - The other required body content (the attributed #1032 clippy row with step 3's output, and the Collateral findings) is still listed separately.
- **Pragmas text:** it now quotes SQLite verbatim, with the "journal_mode=DELETE on an older filesystem" qualifier. That matches my round-2 recollection, and the orchestrator fetched the page on 2026-11-16.

---

# Round 4 (delta, plan SHA-256 cb4ed9008bdb0eb05feb7455efe513c9a6162ad72baee4b21a88164f28b3989b), same reviewer, verbatim

**Round 4 verdict: LGTM.** The only change is Delivery 1, which now points to finish-pr's `archive-plan-issue.sh` instead of spelling out a full invocation. It introduces no new finding. Confidence is high: I confirmed that `plan-delta-r4.diff` matches `diff -u last-reviewed-plan.md plan.md` exactly (current SHA `cb4ed900…`).

Altitude report SHA-256: 7ad9572577d491fe9392eaab08d656c8bff66ae410ed610070d71aa47dd1c8ae

## Altitude (assessed first)

**I accept the disposition: proceed.**
- The report's measurements match the file (288 lines, 26276 bytes).
- The only change is that Delivery 1 points to the script instead of restating it. That also resolves my round-1 remark that the plan restated skill-owned procedure.

## Delta checked
- **The refusal is fixed.**
  - The archiver's check (`archive-plan-issue.sh:184-235`) rejects the plan's own source path in its absolute, `file://`, `~/` and untracked repo-relative spellings, plus any linked-worktree paths.
  - A grep of the current plan finds none of these: no `docs/plans/2026-…-rustsec-advisories.md`, no `.worktrees`, no `/home/`, no `file://`.
  - The two remaining mentions of `docs/plans` (line 182's `:!docs/plans` exclusion and line 286) are bare directory names. The check's pattern needs the full path spelling, so they cannot trigger it.
- **"No `--pr`" matches the script's rule.** Its header says `--pr` may be omitted "only for a plan no PR carries", and the issue is published before the PR exists, with "/finish-pr then links the issue". The round-1 invocation also omitted `--pr`, so behaviour is unchanged.
- **Dropping finish-pr's "Item 5" numbering removes a stale cross-reference.** The `--title` convention is left to the skill, which is the right place for it.

## Resolution

Round 1:
- [MEDIUM] pragmas collateral half wrong → Delivery 2 states the prepare-time vs step-time mechanism, records the observed probe (`journal_mode=delete synchronous=1 foreign_keys=1 busy_timeout=5000`, throwaway copy, rusqlite 0.43) and recommends an issue to Michaël.
- [MEDIUM] item 10 covered 2 of 6 removed-`async` lines → item 10 now covers all 6 lines in 5 files, one boy-scout commit.
- [MEDIUM] links conflict missing → Documentation lists it as the third consumer-visible change.
- [LOW] item 4 commit body → reworded ("leaked a `ToSql` conversion error"; "no longer implements it by default", `fallible_uint`).
- [LOW] item 7 cargo-tree ambiguity → `-e normal,build --target all` "nothing to print" plus (round 3) a manifest grep.
- [LOW] step 3 equality too strict → "no new entry (or higher count) at HEAD; disappearances noted".
- [LOW] bans.features removal trigger → its comment names removal with the ignore under #1035.
- [LOW] item 8 restated the reason → CLAUDE.md:107 and SKILL.md:57 now point to the root `Cargo.toml`.
- [LOW] collateral needs actions → each collateral item carries its recommended action.

Round 2:
- [MEDIUM] item 10 green grep matched its replacement → api.md's single-entry section becomes one sentence without the pattern.
- [LOW] README duplicate snippet → the whole "For async support" block (143-148) goes.
- [LOW] deny after item 5 → Verification states the intermediate expectation (also Codex's MEDIUM).
- [LOW] public PR body copied internal Constraints → only Closes/Refs/Classification.
- [LOW] pragmas corruption claim → SQLite's PRAGMA page quoted verbatim (fetched 2026-11-16).
- [LOW, optional] item 7 second check → replaced by `git grep -n 'testcontainers' -- '*Cargo.toml'`.
- Stale altitude estimate → updated in the round-3 report.

Round 3: LGTM, no change required.

Round 4 (Delivery 1 no longer spells the archiver invocation, which the archiver refused as a local source reference): LGTM, no change required.
