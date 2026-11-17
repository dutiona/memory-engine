# Clean-slate subagent review (Claude Opus 5.5, general-purpose, no prior context)

## Round 1 (plan SHA-256 576e640aa93bd683194e81631a40ba7c5e54c462098daf1120dde8e113ef6e69), verbatim

Altitude report SHA-256: 378409c50b3e9819fc052e4ca2144e76b8a2ad50fe0e23368bb759b3b7c7d81f

# Plan review: feat(scope), project key, levels and set_level (#1022)

**Verdict: the plan is sound, but not LGTM yet.** It has no blocker and no high finding. Three findings are MEDIUM: two break a check or a commit the plan itself states, and one is a privacy hazard. The rest are LOW. I have high confidence in the code facts, because I checked each one against the snapshot. My confidence is medium on the userinfo-shape judgment in L1.

All paths are relative to the root of the reviewed snapshot. The repository is public, so this review contains no host paths.

## Altitude and duplication (assessed first)

- **Digests and measurements check out.** `plan.md` hashes to the `576e640a…` the altitude report names, and `inputs/plan-context.md` hashes to `99a92955…`. The report's measurements reproduce: 538 lines and 48,072 bytes, 17 tasks, 2,038 lines under `src`, 831 lines in `lean_surface.rs` and 479 in `common/mod.rs`.
- **I accept the report's justified override.** The "Key rules, version 1" block is about 40 lines and restates and refines ADR-0020 §3 and §7. It is still not a copy of a product file. It states exactly the semantics the log will keep for good under `rules: 1`: the rule order, R1 (remote helpers), the port grammar, IPv6 brackets, D6 (no fall-through) and D7. Its unit tables pin those semantics. Nothing else is embedded, so no split is needed.
- **One duplication remains to settle [LOW].** Once `/finish-pr` archives the plan, version 1 will live in two durable places:
  - `docs/reference/mcp-server.md`, which the plan says will hold "rules version 1 as in this plan";
  - the rustdoc of `project.rs`.

  Name one of them canonical so a later edit cannot change one copy silently. I would make the reference doc canonical, and have the rustdoc link to it and state `KEY_RULES_VERSION`.

## Findings

### MEDIUM

**M1. Item 2 does not compile at its own Green.**
- Item 2 removes `user_home`, `bucket`, `project_home` and `label` from `home.rs`. Its Files list names only "`tools/recall.rs` (its `project_home` call only)".
- Other code still calls the removed functions:
  - `tools/mod.rs:86` (in `record`) calls `home::bucket`;
  - `tools/remember.rs:48` calls `home::bucket`;
  - `tools/recall.rs` calls `home::user_home` (line 79) and `home::bucket` (line 89).
- So `cargo test -p memory-engine-mcp --lib` cannot pass at item 2.
- **Fix:**
  - In item 2, move every caller to `Home::{User, Bucket}.path(user)` with behaviour unchanged, and add `tools/mod.rs` and `tools/remember.rs` to the item's Files.
  - Add `Home::own`, `words` and `scope_label` in the items that first call them (items 4 to 6). Otherwise the item 2 and item 3 commits carry dead-code warnings that `-D warnings` rejects.

**M2. Item 9's drift check matches the plan itself.**
- The check runs `rg … 'two tools|exactly two|Until #1022|#1022 adds|arrive with #1022' docs …`. That scans `docs/plans/2026-11-17-1022-project-key.md`, which item 0 commits and which stays until item 15.
- The plan's own text contains "Until #1022" (item 2), "#1022 adds" (item 4) and the pattern itself, so "prints nothing" can never hold.
- **Fix:** add `-g '!docs/plans/**'`.
- Today the check's other hits are `README.md:84`, `docs/reference/mcp-server.md:6/185/193`, `home.rs:3`, `tools/mod.rs:78`, `remember.rs:43` and `tests/lean_surface.rs:2`. Each one is already slated for change, so they are not a problem.

**M3. Item 0 commits "review artifacts" to a public repository.**
- `plan-context.md` states that the repository is public and must hold no real home paths or host names.
- Reviews are produced against a snapshot inside the operator's home directory, and they routinely cite absolute paths.
- PR branch commits stay visible after the squash merge.
- **Fix, either of:**
  - keep the review artifacts out of the tree (the plan issue archive already keeps them);
  - before the commit, scan `.review/` with `rg` for the home prefix and the host names, and block the commit on any hit.

### LOW

**L1. D7 is not "stricter" than the plugin; it is laxer on one shape.**
- Rule 10 skips the scheme branch when the text before `://` holds a `/`. The scp branch then refuses too, because a `/` comes before the first `:`.
- So `a/b@c://d@e/f` is stored unchanged. The plugin's `strip_userinfo` in `plugins/memory-engine/bin/me-hook` turns it into `a/b@c://e/f`.
- ADR-0020 §1 expects future harnesses that carry no client logic, and §3 says the server "strips again". The server's rule should therefore remove at least everything the plugin's rule removes.
- **Fix:**
  - apply the plugin's scheme rule whenever `://` occurs, then the scp rule to its result (this also covers `token@host:a://b`);
  - add `a/b@c://d@e/f` to `userinfo_is_stripped_from_every_remote_shape`.
- Git reads such strings as local paths, which is why this is only LOW. Correct the word "stricter" in D7 either way.

**L2. The `DevRoot` flaw name stores a pair that says nothing.**
- The flaw name is the field name, so the log would keep `{"dropped":{"dev_root":"dev_root"}}`.
- Item 1 freezes flaw names for good. Name the defect instead, for example `not_plain_absolute`.

**L3. Item 7's provenance marker is off-legend.**
- "*(settled in #1022)*" is not one of ADR-0021's markers.
- That ADR files a decision Michaël makes at plan approval as *(owner)*, recorded verbatim on dutiona/infra#37. Use that marker and record his words there.

**L4. D4's "live" predicate differs from recall's.**
- The port treats a fact as live when `t_invalid IS NULL`.
- Recall's temporal filter is `t_invalid IS NULL OR t_invalid > now` (`crates/me-query/src/execute.rs`, `hybrid.rs`).
- So a fact with a future `t_invalid` is recallable, yet `set_level` answers `not found` for it, and D4's rationale "it is no memory `recall` returns" holds only for a past `t_invalid`.
- No service path writes a future `t_invalid` today: `remember` sets no validity. Either say so in the port rustdoc or align the two predicates.

**L5. `MemoryEngine::move_fact` can fail after the port has committed.**
- `cache_scope_chain(to_id)` runs after the commit, so an `Err` from it means the fact did move. Item 8, however, maps `NotFound` to "nothing was changed".
- **Fix:**
  - document this post-commit exception under `# Errors`, as the other atomic methods do for `IndexInconsistent`;
  - map only the port's own `NotFound` to the retry message.
- Also call `ensure_open()` first, as `me_ingest::ingest` does.

**L6. The empty-node residual has a realistic cause, not only a storage fault.**
- `me_ingest::insert_fact_with_embedding` commits `ensure_scope_path` before `insert_fact_atomic`.
- The #614 fingerprint guard rolls `insert_fact_atomic` back on an embedder identity mismatch. So an embedder model change without `reconstruct` would mint an empty node for every new project it is asked to remember in.
- Nothing leaks either way. Name this cause in the residual.
- The ideal fix is to ensure the scope inside the insert transaction, the split that `insert_facts_batch_atomic` already uses. Whether to fold that in or file it is Michaël's call.

**L7. Item 10 narrows the boy-scout fix to two files.**
- The same stale `file.rs:N–M` references also appear in:
  - `crates/me-storage/src/cold_storage.rs:51`
  - `crates/me-backend-sqlite/src/sqlite/cold_storage.rs:70`
  - `crates/me-backend-sqlite/src/sqlite/mod.rs:149, 252, 308, 337`
  - `crates/me-backend-sqlite/src/sqlite/consolidation.rs:205, 347, 408, 579`
- Either widen item 10 to the port and backend files, or name the rest and file one issue. As written, the narrowed check suggests the rot is gone when it is not.

**L8. C5's second case has no test.**
- C5's second case is a bare `x` when only `laptop:x` exists, which must be a typed error.
- No named test covers it, although the Testing table claims C5. One assert in `another_machines_path_key_is_read_only_when_named` would do.

**L9. Test and doc bookkeeping.**
- In item 8, `tools_list_is_exactly_the_model_tools` is a rename of the existing `tools_list_is_exactly_recall_and_remember`; say so.
- The module doc of `fuzz/fuzz_targets/dispatch.rs` ("no embedder, so `remember` refuses … Neither tool writes outside the store") goes stale once item 8 adds the seeded fact and `set_level`. Item 9's `rg` does not catch it.

**L10. Write down the re-filing rule (ADR-0021 §12).**
- A `set_level` record is a move only when it carries `from` and `to`. Refusals carry `error` and no `level`.
- The concurrency test already folds on this rule. State it in the call-records paragraph of `docs/reference/mcp-server.md`, so the later re-filer never replays a refusal.

**L11. R5's ".git (case kept)" is ambiguous about `.GIT`.**
- Version 1 freezes this choice. State it, and add a `.GIT` row to `remote_keys_follow_rules_v1`.

**L12. `$H` breaks in zsh.**
- `$H` is an unquoted multi-word variable. zsh does not word-split it, so `$H cargo …` fails with "command not found" in this harness's shell.
- It fails loudly, never as a false pass. Define a shell function instead, or run the block under `bash`.

**L13. Minor wording and residuals.**
- D5 says stage 1 "moves it as it is". But the derive half imports `MAX_SEGMENT_LEN` from `memory_engine::types`, and ADR-0021 §8's registry crate "depends on no memory crate". Stage 1 has to swap that import, so reword.
- A dev root behind a symlink gives remote-less projects no path key. That is plausible on a real machine. The fix belongs on the client: resolve the configured dev root with `pwd -P` before sending it. ADR-0020 §3's "sent as configured" would need one more sentence. Raise it with #1028 rather than only documenting it.

## Plan claims verified against the code

- **Request headers reach the tools.** `server.rs` reaches `Parts` (and so the headers) through `context.extensions`, and `http.rs` strips `Authorization` before rmcp. rmcp 1.8 logs requests at debug level, but its `Extensions` Debug output is opaque, so no header value reaches a log.
- **The fuzz target reaches the new port method.** `MemoryEngine::builder(DIM).build()` is an in-memory SQLite engine.
- **The storage layering matches the plan.**
  - `FactGraph` has one implementation, `SqliteBackend`. `PgBackend` implements only `SchemaManager`, and its conformance arm is `#[ignore]`d.
  - `all_behaviors_into!` and `fixtures::snapshot` exist; the snapshot covers facts, scopes, events and more.
  - Facade write methods call the port directly, as `pin_fact` does.
- **A scope move needs no index notification.**
  - Both vector paths filter on `facts.scope_id` at query time: HNSW post-filters with widening, then falls back to brute force.
  - FTS is external-content over `content` only.
  - The snapshot's `DbFingerprint` counts scopes, so a newly minted node invalidates an older snapshot.
- **The rollback test and record sizes hold.** `foreign_keys = ON`, so the test with a dangling-scope record is valid. `MAX_PAYLOAD_BYTES` is 1 MiB, so five 4 KiB facts can never make a record fail.
- **An unminted own project reads `user` only.** `ScopeQuery::ExactSet` drops missing paths; an empty set resolves to `None`, which means no results (fail closed).
- **Remaining references are correct.**
  - The doc line references: `README.md` 84 and 161, `AGENTS.md` 93, `CLAUDE.md` 155, `GEMINI.md` 92.
  - The `lean_surface.rs` test names and the `refuse_event_writes` trigger pattern.
  - `MAX_SEGMENT_LEN` is 256.
  - The Verification block is AGENTS.md "Verify", plus the MSRV builds, a fuzz run and Sphinx.

## Structure, feasibility, over-engineering, decisions

- **Structure.** The Documentation, Testing and Verification sections are all present and specific. The acceptance-to-test table is complete apart from L8. The deferrals (C21's alias hint, C33's `${PROJECT_ROOT}` hint, C34 and the later stages) are explicit, and ADR-0021 §11 justifies them.
- **Order.** The sequence works apart from M1. Items 3 and 4 are independent of each other. Item 7 waits on D2 and has a stated fallback.
- **Over-engineering.** Nothing material.
  - The exhaustive decoder tests are cheap (about 2,300 and 65,536 cases) and guard a security boundary.
  - `Home`, `ProjectKey` and `placement.rs` each have callers.
  - The new port method is the only way to commit a move and its record together.
- **D2 to D4 (Michaël's call).**
  - **D2:** I agree. ADR-0020 §8 already lets a session of the other project bring a memory down from `user`; refusing would force a throwaway `remember` before the owner's rehoming case can work.
  - **D4:** I agree, subject to L4.
  - **D3:** the literal reading of "the same receipt as `remember`" is defensible.

## Round 2 (delta to plan SHA-256 223edc02864136079c6f672bc4b40b6be7919d1feed2df95b39bd7ebefc34f90), verbatim

Altitude report SHA-256: 6aa214ddd654d3198bf6eddb4f33a30e7d4854efcddc276cc19ea2c0eb7a719f

# Round-2 review (delta only): #1022 plan, project key, levels and set_level

**Verdict: not LGTM yet, but close.** One MEDIUM finding remains: item 4's stated Green cannot pass as written. The rest are LOW. Once M1 is fixed, I would approve.

The round-1 fixes hold. My confidence is high: I checked each new claim below against the snapshot. Paths are repository-relative.

## Altitude (assessed first)

- **The report binds to the plan.** `altitude.md` names plan `223edc02…`, which is the current `plan.md`, and its predecessor `576e640a…`, which is `last-reviewed-plan.md`.
- **The delta's size and content match the report.** 570 − 538 = 32 lines added, no new fenced literal, no new embedded artifact.
- **I agree with "proceed".** The canonical home of rules v1 is now `docs/reference/mcp-server.md`, with the `project.rs` rustdoc linking to it. That closes the duplication I raised in round 1.

## Findings

### MEDIUM

**M1. Item 4's test asserts behaviour that only item 6 implements.**
- `every_record_keeps_the_project_facts_key_and_rules` (plan line 288) asserts "the `remember` record sits under the project (D1)".
- But item 4's own Change says "Every caller passes the bucket except a valid `remember` (item 6)". At item 4, `remember` still files its fact and its record in `unscoped:laptop`.
- So `h cargo test -p memory-engine-mcp` fails at item 4's Green.
- Routing the record to `own_home()` already at item 4 is not a fix: it would mint the project while the fact still lands in the bucket.
- **Fix:**
  - in item 4, assert that the `remember` record sits under `unscoped:laptop`;
  - move the D1 assertion to item 6, where `a_failed_fact_write_leaves_a_known_project` already covers the failure path. Add a success-path check, for example in `remember_records_its_level`.

### LOW

**L1. D7's motivating shape has no test, and step 2 lacks the plugin's guard.**
- The new two-step rule exists because of `a/b@c://d@e/f`, but that string is not in `userinfo_is_stripped_from_every_remote_shape`. Add it, expecting `a/b@c://e/f`.
- Step 2 of rule 10 reads "when the text before the first `:` holds no `/`", which is undefined when no `:` exists. The plugin's `*:*` branch requires one. Write "when a `:` exists and the text before the first one holds no `/`", as R3 already does.

**L2. Item 3 says "All four registered in `all_behaviors_into!`", but there are now five conformance bodies.**
- The new one is `move_fact_atomic_moves_a_fact_invalid_only_later`.
- An unregistered body in the private `cfg(test)` module would trip `dead_code` under `-D warnings`, so this catches itself. Fix the wording anyway.

**L3. Two follow-ups the plan names have no step that does them.**
- ADR-0021's rule for *(owner)* requires the approval "recorded verbatim on dutiona/infra#37". Item 7 now uses the marker but never records Michaël's D2 approval there.
- "Raised as a follow-up for #1028" (the `pwd -P` dev-root note) has no owning step either.
- Put both in item 13 as issue comments.

**L4. A second path now hydrates the scope tree.**
- `MemoryEngine::move_fact` now inserts `FactMove.chain` into `ScopeTree` itself. The rustdoc of `cache_scope_chain` calls `me_index::cache_scope_chain` "the single home for the port walk + `ScopeTree` hydration (#973)".
- Either put the infallible loop beside it in `me-index` (for example `insert_scope_chain(&tree, chain)`) or update that comment.
- Also say whether `chain` includes root. `cache_scope_chain` excludes it, and `ScopeTree::insert` is idempotent by id, so either choice works.

**L5. The residual and the D1 consequence need fuller wording.**
- **A second cause of an empty node with no event:** `me_ingest::ingest` calls `ensure_open()`, the #742 reopen fence, while `ensure_scope_path` does not. A fenced server therefore leaves one empty node per new-project `remember`, not only after "a storage fault".
- **A failed remember now mints its project for good.** Under D1, a `remember` that fails after its record (an embedder failure, the common case before #1021) leaves its project minted. That is consistent with ADR-0020 §8's definition of "known", but say it in the Homes paragraph of `mcp-server.md`.

**L6. Item 10's commit message is narrower than the change.**
- `docs(storage): drop stale line references from the atomic FactGraph methods` no longer covers the change, which now also touches `cold_storage.rs`, `sqlite/mod.rs` and `sqlite/consolidation.rs`.
- Retitle it, for example "…from the storage port and the SQLite backend".

**L7. Nits.**
- Plan line 287 ends in a stray `,` before "The `recall` records…".
- Item 0's public-repository scan runs once. Rerun it before any later push that adds review artifacts under `docs/plans/`.

## Resolutions verified (accepted)

- **Post-commit failure (Codex's HIGH, my round-1 L5).** The chain is read inside the transaction, before the commit. `ScopeTree::insert` returns `()`, is idempotent by id, and links children by `parent_id`, so insert order does not matter. A failed commit is a rollback. So "an `Err` always means nothing moved" holds.
- **Unknown project after a failed remember (Codex's HIGH, my L6).**
  - D1 is consistent with ADR-0020 §8: a project is "known once an event … was filed under it".
  - Refusals and `recall` records stay in the bucket, and `a_read_never_mints_a_project` now includes a refused `remember`.
  - `set_level`'s record stays in the bucket while its move mints the target inside the transaction.
  - The new test `a_failed_fact_write_leaves_a_known_project` is sound: a `BEFORE INSERT ON facts` trigger blocks only `insert_fact_atomic`.
- **Future `t_invalid` (L4).** `t_invalid IS NULL OR t_invalid > ?4` matches recall's cutoff. It also follows the established RFC 3339 TEXT comparison in `FactStore::list_active_at`. Both new tests (live an hour ahead, refused in the past) are in place.
- **D7.** Step 1 is exactly `strip_userinfo`'s scheme branch: authority after the first `://`, up to the next `/`, cut through its last `@`. Step 2 then also covers `token@host:a://b`. Every existing expectation in the unit test still holds under the new rule.
- **Items 2, 9 and 10.**
  - Item 2's caller list is complete: `tools/mod.rs:86`, `tools/remember.rs:48` and `tools/recall.rs:79/82/89`.
  - Item 2's "each method lands with its first caller" removes the dead-code risk.
  - Item 9's `-g '!docs/plans/**'` fixes the self-match.
  - Item 10's widened `rg` covers exactly the 19 hits in the six named files, and no others.
- **The rest.**
  - Item 0's scan targets `docs/plans/`, where super-plan writes its review artifacts.
  - `h()` works in zsh.
  - Also applied as proposed: `not_plain_absolute`, `.GIT` kept with its test row, C5's bare `x`, the test rename note, the rewritten fuzz module doc, the re-filing rule in the docs, and the D5 wording.

## Round 3 (delta to plan SHA-256 f177c17c4e24db58abde3127000e7b4e69acd5ccaa8093218bf57851a52776c6), verbatim

Altitude report SHA-256: ae7969dc465f01ebbdd394188fd88cbcc7c6ce28f1d76b1c9788832acdb28869

# Round-3 review (delta only): #1022 plan

**Verdict: not LGTM yet.** One MEDIUM finding remains: the new `ingest_at` design left out `set_level`'s own record. Everything else is LOW. Round 2's M1 and LOWs are resolved. Confidence is high; I checked each claim below against the snapshot.

## Altitude (assessed first)

- **The report binds to the plan.** `altitude.md` names plan `f177c17c…`, which is the current `plan.md`, and its predecessor `223edc02…`, which is `last-reviewed-plan.md`.
- **The size matches.** 590 − 570 = 20 lines added, no fenced literal, no embedded artifact.
- **I agree with "proceed".**
- **Nit:** the report's list of named target files misses two files that item 3 now touches: `crates/me-backend-sqlite/src/sqlite/event_log.rs` and `crates/me-test-support/src/event.rs`. The disposition is unaffected. [LOW]

## Findings

### MEDIUM

**M1. `set_level`'s move record has no valid scope, and nothing creates its bucket.**
- **The record's scope is never set.**
  - In item 4, `call_record(ctx, call, tool, detail) -> NewEvent` now only builds the payload. Resolving and creating the Home moved into `record` → `ingest_at`.
  - Item 8 still passes `&call_record(…)?` to `move_fact`: a `?` on a function that no longer returns a `Result`.
  - Item 3 says `move_fact_atomic` keeps "the record's own `scope_id`". Nothing in the plan ever sets that `scope_id` to the bucket's id, or creates the bucket.
- **D1 now creates a gap.** A keyed session's valid `remember` files its record under the project, so a machine can hold a fact and still have no `unscoped:<machine>` node.
- **The primary flow then fails.** The hint invites exactly "remember, then `set_level` the id you just got". On a machine whose first calls are those two, the move record names a missing scope. That is an FK error, so the move fails.
- **The plan's own test would hit it.** `set_level_moves_a_memory_and_back` (remember from P, then `set_level` from P on laptop) goes red unless an earlier `recall` happened to create the bucket.
- **D1's wording is now wrong too.** "Every record is appended by `MemoryEngine::ingest_at`" is false for the one record that commits inside `move_fact_atomic`.
- **Recommended fix (zero-compromise):** give `move_fact_atomic` the record's path, as `insert_event_at` takes it, for example `record_at: &str` with `scope_id` replaced by its leaf.
  - Both paths are then created in the move's own transaction.
  - `ScopedWrite.chain` carries the nodes of both paths; `insert_scope_chain` is idempotent by id, so overlap is harmless.
  - D1's "every record is a scoped write, a failed record leaves no node" then holds literally.
- **Cheaper alternative:** `set_level` calls `ensure_scope_path(bucket)` before the move. A leftover bucket is not a project, so ADR-0020 §8's "known" is not at stake. State it in D1 if you choose this.

### LOW

**L1. Item 3's facade Green runs zero tests.**
- `h cargo test -p memory-engine --all-features scope_writes` filters on a substring that neither facade test name contains (`ingest_at_and_move_fact_cache_the_new_scope`, `ingest_at_and_move_fact_refuse_read_only_and_oversized_records`).
- The Red still fails, because the crate does not compile. The Green, however, reports "0 tests" and exits 0: the false pass AGENTS.md warns about.
- **Fix:** put the tests in `mod scope_writes`, or filter on `ingest_at_and_move_fact`. The full Verification run still covers them meanwhile.

**L2. `atomic::insert_event_at_rolls_back_its_path` puts SQLite trigger SQL inside a conformance body.**
- The battery's rule, in the rustdoc of `ConformanceBackend::break_facts_table` (`crates/me-test-support/src/factory.rs`), is to keep crash-injection out of the bodies "so #635 never edits a body".
- **Fix:** add a sibling factory method, for example `refuse_event_inserts`, and override it per backend.

**L3. Counts and wording.**
- The Testing table still says "four `move_fact_atomic_*` bodies". There are five; with the two `insert_event_at_*` bodies that makes item 3's seven.
- D1 says "a project node exists only with an event filed under it". `set_level`'s mint files a memory, not an event, so write "an event or a memory", as ADR-0020 §8 does.

**L4. `ingest_at` repeats `me_ingest::ingest`'s guard sequence in the facade.**
- That sequence is `ensure_open`, `ensure_writable`, `check_json_size`.
- **Fix:** put `ingest_at` in `me-ingest`, beside `ingest`, taking the scope tree as `add_fact` already does, and let the facade delegate. Event-append validation then keeps one home.

## Resolutions verified (accepted)

- **Codex's round-2 HIGH (node and record were two autocommit writes).**
  - `insert_event_at` creates the path and inserts the event in one transaction. `EventLog` has one implementor, `SqliteBackend`, so the new required method breaks no other backend.
  - A failed `remember` record now leaves no node, and the residual is correctly gone.
  - `ingest_at` runs `ensure_open`, which also removes the fenced-server cause I raised in round 2 (L5).
- **`a_remember_whose_record_fails_mints_nothing` is feasible.** Under `refuse_event_writes`, the follow-up `recall(project: K)` also fails to write its own record, so its body reads `unknown project: "K" (cannot record this call: …)`. Assert with `contains`.
- **Round 2's M1 is fixed.**
  - Item 4 now expects every record under `unscoped:laptop`.
  - Item 6's `remember_files_its_record_under_its_target` covers the project, `user:alice` and refusal cases.
- **Round 2's LOWs are applied.**
  - Rule 10's `:` guard, and the `a/b@c://d@e/f` → `a/b@c://e/f` row.
  - "All seven" (two `insert_event_at` bodies plus five `move_fact_atomic` bodies).
  - `me_index::insert_scope_chain`, with the chain excluding root, as `cache_scope_chain` does; `ScopeTree::insert` is order-independent.
  - Item 10's commit subject.
  - The rescan before each push.
  - The D1 consequence in the Homes docs.
  - The hand-offs through the worker report, which are the right channel since the worker does not write to dutiona/infra#37 or #1028.
  - `raw_exec` is available to the battery under `test-util`.

## Round 4 (delta to plan SHA-256 e9eaeb83f5fc84be8b34adc55f4e0ce2229d9dd341e02bd7e168e3c533e678eb, from f177c17c), verbatim

Altitude report SHA-256: 52338efe43e49d3c33ed882fddb7ce20363e8c97e9393c69a4ab4c0cd62e72f9

# Round-4 review (delta r5 only): #1022 plan

**Verdict: LGTM.** The fix for my round-3 MEDIUM is correct and complete, and each LOW was applied as proposed. The three LOW findings below are wording and file-list nits that need no new review round. Confidence is high; I checked each claim against the snapshot.

## Altitude (assessed first)

- **The report binds to the plan.** `altitude.md` names plan `e9eaeb83…`, which is the current `plan.md`, and its baseline `f177c17c…`, which is `last-reviewed-plan.md`.
- **The size matches.** 602 − 590 = 12 lines, which is revision 4's "about 9" plus revision 5's 3.
- **I agree with "proceed"** for both revisions. No fenced literal or embedded artifact was added, and the plan's role is unchanged.
- **The file list now includes the files I flagged** in round 3, plus `me-ingest` and `factory.rs`. For one omission, see L1.

## Findings

All three are LOW.

**L1. The "single home" comment sits in a file item 3 no longer lists.**
- The comment lives at `crates/memory-engine/src/engine/mod.rs:631`, on the facade's private `cache_scope_chain` wrapper. Nothing in `me-index` carries it.
- Item 3's Files dropped `engine/mod.rs` (it now lists `engine/{ingest.rs,tests.rs}`), and so did the altitude report's file list.
- **Fix:** add the file back, so that "the comment names both" has a place to happen. Otherwise the comment goes stale once `insert_scope_chain` exists.

**L2. Make `ConformanceBackend::refuse_event_inserts` a required method.**
- A trigger is not portable SQL, so the method should have no default. `PgFactory` then gets `todo!("#635")`, like its other methods.
- `--all-features` compiles `PgFactory` under `backend-postgres`, so a missing impl would fail loudly rather than pass silently.
- State it so the implementer does not put SQLite syntax in a default body. That would break the very rule this change exists to keep.

**L3. A provenance pointer is dangling.**
- The altitude report's "Revision 4" paragraph cites `.review/plan-delta-r4.diff`, which is not in the review tree.
- "Revision 5" explains that `plan-delta-r5.diff` covers revisions 4 and 5 together. Point revision 4 at r5 too, or keep the r4 file.

## Resolutions verified

- **My round-3 M1 is resolved.**
  - `move_fact_atomic` now takes `record_at` and creates both paths, the target and the record's, in the move's own transaction, with `record.scope_id` replaced by the leaf of `record_at`.
  - Item 8 passes `&bucket.path(user)` and drops the stale `?`.
  - `set_level_right_after_a_machines_first_remember` covers exactly the flow that failed.
  - D1's "every record is a scoped write" and "an event or a memory" now hold literally.
  - The stale-source and rollback bodies assert that neither path is created.
  - When `to` and `record_at` coincide (a keyless session moving into its bucket), the double creation is idempotent, and so is the chain insert.
- **L1 from round 3 is resolved.** The facade tests sit in `mod scope_writes`, and the Green now requires the new tests to be listed by name, never "0 tests".
- **L2 from round 3 is resolved.**
  - `refuse_event_inserts` sits beside `break_facts_table`.
  - Using a trigger, rather than dropping a table, keeps `fixtures::snapshot` readable, so the "`snapshot` identical" assertions stay meaningful.
- **L3 from round 3 is resolved.** The bodies are two `insert_event_at_*` plus five `move_fact_atomic_*`, which is item 3's "seven", and the Testing table now agrees.
- **L4 from round 3 is resolved.** `ingest_at` and `move_fact` now live in `me-ingest`, beside `ingest`.
  - `MemoryCtx` exposes `ensure_open` and `ensure_writable` (`crates/me-storage/src/ctx.rs:47,59`).
  - `me-ingest` already takes `scope_tree` and calls `me_index` through `add_fact`, so no dependency is added.
  - The facade keeps one-line delegates.
- **Vibe's two fixes are applied correctly.**
  - Item 7 is now gated with item 8, and is dropped if D2 is declined.
  - Item 10's new pattern `rs:[0-9]+([–,-][0-9]+)+` also catches `crates/me-storage/src/schema.rs:61` (`engine/mod.rs:649,659`) and `crates/me-backend-sqlite/src/sqlite/cold_storage.rs:98` (`archive.rs:239,253-279`). Every hit falls in item 10's listed files.

## Round 5 (delta to plan SHA-256 dd796ce3cabe0e3b24f7bc1bcfae4989b96dbad6b29417f2b7e1d19181628400, from e9eaeb83), verbatim

Altitude report SHA-256: aee3488a8f2ed9a949af8aca4d2341659415012ea57df9c1d3c36cc92a171595

# Round-5 review (delta r6 only): #1022 plan

**Verdict: LGTM.** All four LOW fixes are applied correctly, and the delta introduces nothing new. Confidence is high: I checked the item 10 pattern against the snapshot.

## Altitude (assessed first)

- **The report binds to the plan.** `altitude.md` names plan `dd796ce3…`, which is the current `plan.md`, and its baseline `e9eaeb83…`, which is `last-reviewed-plan.md`.
- **The size matches.** 605 − 602 = 3 lines, with no fenced literal and no embedded artifact.
- **I agree with "proceed".**
- **The revision-4 pointer is fixed.** It now names `plan-delta-r5.diff`, which exists in the review tree.

## Fixes confirmed

- **L1 (the "single home" comment).** Item 3's Files and the altitude file list both name `crates/memory-engine/src/engine/mod.rs` again, with the reason: the comment on the private `cache_scope_chain` wrapper at line 631.
- **L2 (`refuse_event_inserts`).** It is now a required method with no default body. The SQLite factory installs the trigger, and `PgFactory` gets `todo!("#635")`, which typechecks through the never type like its other methods.
  - One optional note for the implementer: give it the same `#[cfg(feature = "test-util")]` gate as `break_facts_table`, on the trait and on both impls, since the SQLite body needs `raw_exec`. A mismatch would fail to compile, so it cannot slip through.
- **L3 (dangling pointer).** Fixed, as above.
- **Vibe's change to item 10.** The new check `rg -n '\.rs:[0-9]+'` over `crates/me-storage/src` and `crates/me-backend-sqlite/src/sqlite` matches 23 lines, every one a history reference in one of item 10's seven listed files. It has no false positive in code or strings. It also catches the single-line reference at `crates/me-backend-sqlite/src/sqlite/cold_storage.rs:73` (`archive.rs:239`), which the previous range-only pattern missed.

## Resolution

Converged at round 5 (LGTM, no open finding). Every finding was addressed in the plan; none was declined.

- Round 1: [MEDIUM] M1 → item 2 moves every caller of the removed `home.rs` helpers and adds each `Home` method with its first caller. [MEDIUM] M2 → item 9's check excludes `docs/plans/**`. [MEDIUM] M3 → item 0 writes review artifacts with `<review-tree>` and scans `docs/plans/` for home paths and host names before every push (item 13). [LOW] altitude duplication → `docs/reference/mcp-server.md` holds rules v1 canonically. L1 → D7 is the plugin's scheme rule then its scp rule. L2 → flaw `not_plain_absolute`. L3 → *(owner)* marker; the orchestrator records Michaël's words. L4, L5 → liveness at `now`; nothing fallible after commit (`ScopedWrite.chain`). L6 → D1 and `ingest_at` remove the empty-node residual. L7 → item 10 widened. L8 → C5's bare `x`. L9 → rename note, fuzz module doc. L10 → re-filing rule in the docs. L11 → `.GIT` kept. L12 → `h()` function. L13 → D5 wording; `pwd -P` follow-up for #1028 relayed through the worker report.
- Round 2: [MEDIUM] M1 (also Codex) → item 4 asserts the bucket; item 6 adds `remember_files_its_record_under_its_target`. L1-L7 → applied (rule 10's `:` guard and row, counts, hand-offs through the worker report, `insert_scope_chain`, D1 consequence in docs, item 10's subject, rescan before each push).
- Round 3: [MEDIUM] M1 → `move_fact_atomic` takes `record_at` and creates both paths in its transaction; test `set_level_right_after_a_machines_first_remember`. L1 → `mod scope_writes`, Green lists tests by name. L2 → `refuse_event_inserts` factory fault. L3 → counts, "an event or a memory". L4 → `ingest_at` and `move_fact` in `me-ingest`.
- Round 4: L1 → item 3 lists `engine/mod.rs`. L2 → `refuse_event_inserts` is required, `PgFactory` `todo!("#635")`. L3 → revision-4 pointer fixed.
- Round 5: the optional `#[cfg(feature = "test-util")]` note is adopted for implementation (it fails to compile if missed).
