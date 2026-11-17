# Plan review: lean MCP surface (#1020)

Altitude report SHA-256: 0fe1139efd9071601dd3249d19834efc0c2004e0807b745b9622787518021d46

**Verdict: not LGTM yet.** The plan is sound, can be built in the stated order, and matches ADR-0020 and ADR-0021 in nearly every claim I checked. Before approval it needs two HIGH fixes (the stale altitude binding and the live SQLite export) and two MEDIUM fixes (a test contradiction between items 8 and 9, and gaps in the D11 call-record spec). Confidence is high on everything marked verified.

**Which version this covers.** The plan file changed while I was reviewing it:
- I first read SHA `16f0898e…` (457 lines). That is the version the altitude report binds.
- At 13:37:31 the file on disk became SHA `0410ac2e4323419bc3874108fa76ce9d5e652b4757c83c3cae072dbdfbf76612` (487 lines).
- This review covers `0410ac2e…`. Six things I raised on the first pass were fixed by that edit and are not repeated below:
  - D1 now explains the HNSW widening and fallback.
  - D5 has the `vector only` label.
  - D12 handles a query with no words. Before, it would have fallen through to `execute_store_path` and listed facts by importance.
  - D11 now writes the call event before the fact.
  - D7 takes the lock before the dimension peek.
  - Item 8 now says "until item 11".

## Altitude and duplication (assessed first)

The altitude report's disposition, **proceed**, is sound on the substance:
- The decisions sit at contract level. No handler is transcribed.
- The exact strings (the two tool descriptions, the receipt, the instructions format) are what the model sees, so they belong in the plan.
- The breadth across five crates comes from the issue's acceptance list, not from the plan growing.
- There is one duplication: the verbatim ADR and issue quotes under Constraints, about 30 lines, restate binding documents. I accept it as justified. They anchor the acceptance criteria, and Michaël's rule keeps quotes verbatim.

One remaining altitude point is minor; see L1 (D12 puts SQLite FTS5 syntax in the MCP crate).

## Findings

### [HIGH] H1. The altitude report no longer certifies the plan on disk

- The report says `Plan SHA-256: 16f0898e…`, 457 lines, 41,932 bytes.
- The plan on disk is now `0410ac2e…`, 487 lines.
- The digest binding is broken, so the altitude gate is not satisfied for the version that will be implemented.
- **Fix:** regenerate the altitude checkpoint over the current plan before approval. I expect the content verdict to stay **proceed**, since the edits are local to D1, D5, D7, D9, D11, D12 and items 5, 8, 9 and 10.

### [HIGH] H2. "For Michaël" #4 and D7 leave out the option that keeps live SQLite backups

The plan presents a binary choice: `export --export-format sqlite` "now needs the unit stopped". That is not forced.

What I verified:
- `VACUUM INTO` succeeds on a connection opened read-only (`mode=ro`) while another connection holds the SQLite write lock. I ran this with Python's sqlite3 3.46.1 in the scratchpad.
- It fails only under `PRAGMA query_only=ON`.
- The writable pool's read connections set `query_only` (`crates/me-backend-sqlite/src/pool/connection_pool.rs:335`).
- The read-only pool opens with `SQLITE_OPEN_READ_ONLY` and sets no `query_only` (`connection_pool.rs:383-384`).

**Ideal fix:**
1. In `crates/me-backend-sqlite/src/sqlite/schema.rs:245`, run `DumpFormat::Sqlite` through `block_read` when the pool is read-only, and keep `block_write` otherwise.
2. Correct the `ReadOnly` claim at `crates/memory-engine/src/engine/inspect.rs:191`.
3. Have `export` open read-only for every format.
4. Move `export --export-format sqlite` to D7's no-lock list.
5. Add a test that exports SQLite while the store lock is held.
6. Delete item #4 from "For Michaël".

This belongs in this ticket: the lock is what introduces the downtime requirement for backups. Both the infra backup job and the item 5 tests depend on this choice.

### [MEDIUM] M1. Item 9's own design turns item 8's test red, and the plan does not say which side gives way

- Item 8, `an_unknown_project_is_an_error_that_names_it`, asserts: "with no `project` and no bucket yet the read is `user` only and creates no bucket".
- D11 files every `recall` event under the bucket, "created by `ensure_scope_path` if absent".
- So once item 9 lands, a keyless recall creates the bucket and item 8's assertion fails. Item 9 never says to amend it.
- **Recommendation:** change the assertion to "reads `user` only and creates no project node". A call record is a write that ADR-0020 §9 allows; only write paths mint *projects* (ADR-0021 §8).
- Item 9's Files list also omits `src/tools/recall.rs`, although item 9's Change ("D11's ordering in `remember` and `recall`") edits it.

### [MEDIUM] M2. The D11 call-record contract has four gaps that #1022, #1025 and #1029 will build on

- **(a) No `fact_ids` on remember records.** ADR-0020 §8 says the record keeps "the IDs of the facts returned or changed". The remember event cannot hold the fact's ID, because the event is written before the fact exists. The fact's `source_event_id` link stands in for it, which works for ADR-0021 §12 re-filing. D11 should state this deviation explicitly so downstream tickets don't expect `fact_ids` on remember events.
- **(b) Which failed calls get a record.** "One event per call" suggests argument refusals (unknown field, blank content) are recorded. "Ingests its call event first" leaves it open whether that happens before or after validation. Also say that `remember` without an embedder leaves an orphan event.
- **(c) The `project` swap.** ADR-0021 §5 says the swap replaces the context Home. D11 says "the call's context Home (the bucket)". Write "the session's own context Home (its bucket), even under the swap".
- **(d) The "drop item 9" option.** Item #1 of "For Michaël" offers to "give them to #1022", but #1022's acceptance doesn't list call records. Dropping item 9 leaves ADR-0020 §8 unmet with no owner. If Michaël drops it, name the ticket whose acceptance gains it. Also put in #1 that a failed record write now fails `recall`: availability is traded for complete logs.

### [LOW] L1. D12 puts SQLite's FTS5 syntax in the MCP crate, and the backend still swallows real faults

- `recall` building `"…" OR "…"` puts FTS5 dialect above the storage port. That is acceptable for now: the server is SQLite-only and Postgres lexical search isn't implemented. Say so in D12.
- Collateral bug: `fts.rs:97-100` catches every `query_map` error as "likely syntax error", and `fts.rs:108` skips rows that fail.
- After D12, `recall` can no longer send a syntax error, so the only errors left in those branches are real storage faults, reported as "no full-text results".
- **Recommend:** narrow the catch to FTS5 syntax errors, or file this as a collateral finding.

### [LOW] L2. The clippy guard doesn't cover `memory_flush_insights`

- That handler is `add_facts_batch` plus `INSIGHT_MARKER_KEY` (`p0.rs`), not one of the four methods D9 lists.
- So the Outcome's "a clippy guard keeps it so" overclaims for one of the four derived jobs.
- **Fix:** add `memory_engine::MemoryEngine::add_facts_batch` to `disallowed-methods` (`remember` uses `add_fact`), and add `INSIGHT_MARKER_KEY` to the derived-jobs `rg` check.

### [LOW] L3. The cross-binary smoke fails at its first step

- In a fresh temporary directory, `add-fact` fails with "database not found": `open_engine_writable_with_dim` requires the file to exist (`db.rs:171`).
- `stats` and `prune` also need a recorded embedding identity before their dimension peek succeeds.
- **Reorder:** start the server once (it creates the store), stop it, run `add-fact`, start it again, then run the checks.

### [LOW] L4. Taking the lock first leaves a stray lock file beside a mistyped path

- For opens of an existing store, acquiring the lock before anything else creates `<typo>.store-lock` next to a database that doesn't exist.
- `path.is_file()` only reads metadata, not the store. Doing it before `acquire` for those opens keeps `a_held_store_is_refused_before_it_is_read` green and leaves nothing behind.

### [LOW] L5. Dev-dependencies left unused after item 11

- `insta` is used only by `depth_shaping.rs`.
- `blake3` is used only by `query_mode_inference.rs`, `tool_roundtrip.rs` and `depth_shaping.rs`.
- `proptest` is used only by `depth.rs` and `tools/parse.rs`.
- All of those files are deleted, and no gate detects unused dependencies. Drop them in item 11, unless the new unit tests use `proptest`.

### [LOW] L6. Documentation gaps

- `ScopeQuery` variant lists also appear in `docs/reference/glossary.md:36` and `docs/getting-started/core-concepts.md:117-119`. Item 2 lists only two files.
- `GEMINI.md` has no crate tree. Its `memory-engine-mcp` mention is the prose line 92.
- `docs/advanced/dream-cycle.md:77` ("The harness/MCP entry") also names MCP, not only line 79.

### [LOW] L7. The Testing section claims more ADR-0021 coverage than the tests give

- C2 is covered on the read side only: seeded `user:` facts read by keyless sessions. The "in P, in Q" half needs #1022.
- `recall` never exercises C7's co-session-edge half.
- Qualify both the way "C6 (keyless)" already is.

### [LOW] L8. Item 12 and Verification disagree about the tool-name `rg`

- Item 12 says the check "prints nothing" after the change.
- Verification says it matches the refused list in `lean_surface.rs` and the module path `memory_query` in `me-types`. Align the two.

### [LOW] L9. Coverage of the pinned instruction may be lost in the move

- `embedding_integration.rs` also holds `query_path_applies_pinned_instruction_by_default` and `add_fact_path_never_applies_pinned_instruction` (the served-artifact pinned instruction, ADR-0015).
- Name them in `recall_embeds_with_the_query_instruction_and_remember_without`. They need a served-config `TestServer`. Alternatively, record that `memory-engine-embed/tests/asymmetric_embedding.rs:210` covers only the provider side.

### [LOW] L10. Collateral: `recall.query` has no size cap

- `input_caps.rs` is deleted, `http.rs` has no body limit, and the engine's 1 MiB `MAX_PAYLOAD_BYTES` applies to stored content, not to queries.
- This already held for `memory_query`, but this ticket is the one that publishes the engine on the tailnet.
- Capping `query` at `MAX_PAYLOAD_BYTES` is a one-line change.

## Claims checked against the source (all hold)

- **Retrieval scoping:**
  - `ScopeQuery`'s only exhaustive match is `tree.rs:189-192`.
  - `resolve_query_scope_ids` feeds `FactFilter.scope_ids` into the FTS and brute-force paths as a SQL predicate (`FilterSql`, where `Some(empty)` matches nothing).
  - The store path's `unwrap_or(&[])` means "all scopes", which is why D1's `None` matters.
  - HNSW widens and falls back to brute force (`ann.rs:393-472`).
  - The archive fallback runs only on opt-in.
  - No engine-internal timer runs a derived job.
- **FTS5 (Python sqlite3 probes):** the raw query `what's the lock file?` is an FTS5 syntax error, while quoted runs with no tokens (`"__"`, `"—"`) are harmless inside an `OR` chain.
- **Dependencies and config:** `getrandom` 0.3.4 is in `Cargo.lock`; rmcp is 1.8.0; `McpConfig` has `deny_unknown_fields`; there is no `clippy.toml` anywhere, so a crate-local one shadows nothing; `reqwest` is used only by `summary.rs`.
- **CLI:** the writable open sites outside `db.rs` (`batch_ingest.rs:419`, `bootstrap.rs:188`, `import.rs:31`) are as D7 says. `ForgetPolicy`, its validation and the case-insensitive `FactType::from_str` match D8. The CLI prints `error: {e:#}` and exits 1.
- **Event log:** `EventType::MemoryOp` exists and nothing parses its payload. `add_fact` writes no event, so D11's event is the remember's only log record.
- **Shutdown safety:** `serve` drains every in-flight call and flushes before it returns, so dropping the lock at the end of `main` is safe.
- **Other:**
  - `libc` is unused by the facade.
  - The README line numbers are right.
  - Every destination file in the Testing table exists.
  - `server_process.rs:106` already relabels a store one schema version behind.

## Structural completeness

The Documentation, Testing and Verification sections are all present and specific. The Verification block matches the AGENTS.md matrix plus the MSRV job.

## Resolution

- [HIGH] H1 stale altitude binding → the altitude report is regenerated over the revised plan before round 2; every reviewer re-acknowledges the new report digest.
- [HIGH] H2 live SQLite export → adopted as D13 and new item 5 (`block_read` for `DumpFormat::Sqlite` on a read-only pool, `export` opens read-only for every format and takes no lock); "For Michaël" #4 deleted; D7's lists and the docs table updated.
- [MEDIUM] M1 item 8 vs item 9 → item 9 (recall/remember) now asserts "creates no project node"; item 10 (call records) asserts the keyless recall creates the bucket and no project node; `recall.rs` added to item 10's files.
- [MEDIUM] M2 call-record gaps → D11 now states (a) remember records carry no `fact_ids`, the fact's `source_event_id` stands in; (b) every call of either tool is recorded, refusals included, once its outcome is known, except a valid remember, recorded just before its fact; an embed failure leaves an orphan record; (c) records file under the session's own context Home, even under the swap; (d) dropping item 10 moves the record to #1025's acceptance by Michaël's edit, and the availability trade is stated in "For Michaël" #1.
- [LOW] L1 → D12 states the FTS5-above-the-port trade; the `fts.rs` error swallowing is recorded as collateral ("For Michaël" #7, and the collateral ledger).
- [LOW] L2 → `add_facts_batch` joins `disallowed-methods`; `INSIGHT_MARKER_KEY` and `add_facts_batch` join the derived-jobs `rg`.
- [LOW] L3 → the cross-binary smoke now starts the server first to create the store.
- [LOW] L4 → an open of an existing store checks `path.is_file()` before acquiring; test `a_mistyped_path_leaves_no_lock_file`.
- [LOW] L5 → `insta`, `blake3`, `proptest` dropped from dev-dependencies in item 12.
- [LOW] L6 → glossary and core-concepts added to item 2; GEMINI's prose line 92 named; dream-cycle lines 77 and 79.
- [LOW] L7 → C2 and C7 qualified in Testing.
- [LOW] L8 → item 13 and Verification aligned ("matches only what Verification allows").
- [LOW] L9 → the pinned-instruction pair moves into item 9's embedder test on a served-config `TestServer`.
- [LOW] L10 → `recall.query` capped at `MAX_PAYLOAD_BYTES` (D10), test `an_oversized_query_is_refused`.

---

# Round 2 delta review: lean MCP surface plan (#1020)

(Revision 2, plan SHA-256 5e6f36189ac1c6ee5992079e4309656b866ef9d86908d51d6cd342a9f1972a08; the reviewer's final message, verbatim.)

**LGTM.** Every round-1 resolution is correct and complete in revision 2, and the delta adds no BLOCKER, HIGH or MEDIUM problem. I found six new LOW items; each can be folded in during implementation. Confidence is high: every claim below was checked against the source.

Altitude report SHA-256: 8b02f1364778780a93be48f540870450b4f68471a43341d226cc8b7948d6dade

## Altitude report (acknowledged first)

- The report binds `Plan SHA-256: 5e6f3618…`, which matches the file on disk.
- Its counts match the file: 531 lines, 49,948 bytes, 15 top-level tasks, and one 14-line fence at lines 500–515.
- The delta inventory is accurate. The sections it calls unchanged (Constraints, D2–D4, D6, D8) do not appear in the diff.
- The **proceed** disposition is sound: the plan's growth is bounded and comes entirely from round-1 findings.
- One wording slip that does not change the disposition: "deleted by items 9 and 12" should say item 12 only, since item 9 keeps the old tools.

## Round-1 resolutions verified

- **H1:** the altitude report is regenerated and bound to the new plan digest.
- **H2:** D13 and the new item 5 are correct.
  - `schema.rs:162` already calls `self.pool.is_read_only()`, so the branch is feasible.
  - No existing test asserts a `ReadOnly` error for a SQLite dump from a read-only engine.
  - The restore path's clean-backup check (`restore.rs:132-143`) is unaffected.
  - The CLI enables `compress-gzip` and `compress-zstd`, so "export in every format" can exit 0.
  - The rule D13 cites exists: infra D12 says "Status and `VACUUM INTO` backups are read-only, inside the running container through `podman exec`". The lock would have broken that rule.
- **M1:** resolved. Item 9 now asserts "creates no project node", item 10 asserts the bucket is created, and `recall.rs` is in item 10's file list.
- **M2 (a)–(d):** all four are stated in D11 and in "For Michaël" #1.
- **L1–L10:** each resolution is present.
  - The reordered smoke works. `add-fact` records an identity, and the restarted server does not check identity when no embedder is configured.
  - No file kept in the MCP crate calls a newly guarded method.

## New findings

### [LOW] Item 5's CLI test is not red before its change; item 6 has a stale phrase

- `export_sqlite_runs_while_the_store_lock_is_held` passes before item 5's change. Until item 6, no CLI command looks at `StoreLock`, so the old writable export ignores the held lock and exits 0.
- Item 5's only real red test is the engine test.
- **Fix:** drop the CLI test from item 5. Item 6's `read_only_commands_run_while_the_store_is_held` already covers it ("`export` in every format included"). Alternatively, label it as a guard that is green both before and after the change.
- Separately, item 6's `every_writable_command_refuses_while_the_store_is_held` still says "export target … unchanged". `export` has left the locked list, so that phrase is stale.

### [LOW] D10's query cap is not reachable from the MCP crate

- The facade re-exports `limits` only as `pub(crate)` (`crates/memory-engine/src/lib.rs:150`).
- The MCP crate depends only on the facade, so it cannot see `MAX_PAYLOAD_BYTES`.
- **Fix:** item 9 needs a facade re-export, `pub use me_types::limits::MAX_PAYLOAD_BYTES`, with an `api.md` row and `crates/memory-engine/src/lib.rs` added to its Files. Copying the constant instead would drift.
- D10 also says this is "the cap the deleted `input_caps.rs` gave the old tools". That is inaccurate: `input_caps.rs` capped the insight count, the bootstrap bytes and the replay limit, never query text.

### [LOW] D11 and C5: `project` has no size cap, but its error and record echo it

- An unknown `project` value is echoed in full into the tool result and into the record's `error` field.
- Past 1 MiB, the record write itself fails at `check_json_size` on the event payload. The model then sees a record-write error instead of the unknown-project error.
- Any `project` whose `project:<label>` exceeds 256 bytes is unknown by construction.
- **Fix:** bound the echoed value, or refuse `project` above a limit. ADR-0020's 4,096-byte header limit is a natural bound.

### [LOW] Item 5 misses two stale comments, and its engine test proves less than its name

- Two comments D13 makes false are not in item 5's Files:
  - `crates/memory-engine-cli/src/db.rs:147-149` lists "`export` with `SQLite` format" as a caller of the writable open.
  - `crates/me-backend-sqlite/src/sqlite/schema.rs:213` reads "WRITE (`VACUUM INTO`)".
- Engine test: `a_read_only_engine_exports_sqlite_while_a_writer_holds_the_store` only keeps a writable engine open. An open engine holds no SQLite write lock.
- **Fix:** to prove the property D13 rests on, hold `BEGIN IMMEDIATE` on a side `rusqlite` connection during the dump, as my round-1 probe did.

### [LOW] Item 9's "nothing stored" will clash with item 10's records

- Item 9's `unknown_arguments_are_refused` says "nothing stored". Item 10 then records every argument refusal.
- If item 9's assertion counts events (for example through `statistics()`), item 10 turns it red. This is the same pattern as round-1 M1.
- **Fix:** write "no fact stored".
- Related: the clippy guard applies to the crate's tests too (`--all-targets`). So `lean_surface.rs` must seed C16's 50 hidden facts with `add_fact`, not `add_facts_batch`.

### [LOW] Two wording overclaims

- D7 says "a held store is always refused as held". `migrate` is an exception by design: `migrate.rs` reads the schema version before the open, to decide whether it needs to write at all.
- D11 says "no answer without its record". It really means no *successful* answer: when the record write fails, the call answers an error that has no record. #1025 then treats that call line as unmatched, which is acceptable.

## Resolution (round 2)

- Item 5: the CLI test is dropped (item 6's read-only table covers `export` in every format); the engine test holds `BEGIN IMMEDIATE` on a side `rusqlite` connection; the two stale comments are in item 5's change. Item 6's "export target" phrase removed.
- D10: the facade re-exports `MAX_PAYLOAD_BYTES` (item 9 gains `crates/memory-engine/src/lib.rs` and an `api.md` row); the `input_caps.rs` claim removed; `project` is refused above 4,096 bytes without echo, test `an_oversized_project_is_refused_without_echo`.
- Item 9: "no fact stored"; seeding through `add_fact_precomputed`, never `add_facts_batch`.
- D7: the `migrate` exception stated. D11: now "no successful answer without its record", with the exact guarantee (also Codex round 2). Altitude wording fixed in revision 3's report.

---

# Round 3 delta review: lean MCP surface plan (#1020)

(Revision 3, plan SHA-256 304a3165e4d6f055d7efed39e96ec2d46ab775ecbefddd27b54d0ef5e24c072c; the reviewer's final message, verbatim.)

**LGTM.** Every round-2 resolution is in place, and so are the fixes for Codex's round-2 HIGH and MEDIUM. The delta adds nothing blocking. I found five new LOW items: four are wording or test-setup details, and one is a design note for #1023. Each can be folded in during implementation. Confidence is high; the evidence is cited under each finding.

Altitude report SHA-256: bf837855a816354f561857548b69a55ad4b4965697d436d84c6ae6b281034ffe

## Altitude report (acknowledged first)

- The report binds `Plan SHA-256: 304a3165…`, which matches the file on disk.
- Its counts match the file: 552 lines, 52,147 bytes, 15 top-level tasks, and one fence at lines 521–536.
- My round-2 wording slip is fixed: the report now says "deleted by item 12".
- The revision-3 delta inventory is accurate, and the **proceed** disposition is sound.

## Resolutions verified

- **Item 5:** the CLI test that could not go red is dropped, and the plan says so. The engine test now holds `BEGIN IMMEDIATE` on a side connection. `rusqlite` is already a dependency of `memory-engine` (`Cargo.toml:74`), so that test is feasible.
- **Item 5:** the two stale comments are now in its change list.
- **Item 6:** the "export target" phrase is removed.
- **D10:** the `MAX_PAYLOAD_BYTES` re-export is added, with `lib.rs` and an `api.md` row in item 9. The `input_caps.rs` claim is gone.
- **D10:** `project` is now refused above 4,096 bytes without being echoed.
- **Item 9:** the assertion now reads "no fact stored", and the seeding constraint is written down.
- **D7:** the `migrate` exception is stated.
- **D11:** the guarantee now reads "no successful answer without its record". A new test, `a_remember_whose_fact_write_fails_leaves_an_unnamed_record`, covers a fact write that fails after its record.

## New findings

### [LOW] D12: the 64-term cap is right, but its stated reason is not

- The plan says `MAX_FTS_TERMS` stops a long query from becoming "an expression FTS5 refuses", which the backend would then read as empty.
- My probe with Python's sqlite3 3.46.1 contradicts that. FTS5 accepted every `"…" OR "…"` chain I tried, from 64 terms up to 250,000.
- The real hazard is cost, and it grows superlinearly. On a 2,000-row table:
  - 20,000 terms took 0.23 s;
  - 100,000 terms (1.2 MB) took 5.9 s;
  - 250,000 terms took 69 s.
- A 1 MiB query can produce on the order of 100,000 terms, so the cap is still needed.
- I think the SQLite bundled with rusqlite behaves the same way, but I did not test it.
- **Fix:** restate the reason as bounding per-call cost.
- Optional nit: deduplicate terms case-insensitively. `unicode61` folds case, so `The` and `the` are one token yet would take two of the 64 slots.

### [LOW] D11: an unknown-argument error can still be unbounded

- serde's `unknown field` error echoes the key in full. A model can send any key, and the HTTP layer has no body limit.
- That full text goes into both the reply and the record's `error` field.
- Above 1 MiB, the record write fails at `check_json_size` while the log itself keeps working.
- So the sentence "the only way to lose a record is a log that refuses the write, and then nothing can be logged" is not exactly true.
- **Fix:** truncate the failure text in both the reply and the record, at the same 4,096-byte bound already used for `project`.
- Related wording: the payload sentence (`"error"` for "a call that failed") should say "a call refused before its record was written". A valid `remember` that fails afterwards carries no outcome field.

### [LOW] D11: the fact-side outcome assumes fact rows are never deleted

- D11 defines a valid `remember` as stored exactly when some fact's `source_event_id` names its record.
- Pruning sets `t_expired` and keeps the row, and ADR-0020 §8 keeps the row for `forget`. Both are safe.
- But if #1023's `erase` deletes rows, a `remember` that succeeded would later read as failed.
- **Fix:** add one line for #1023: keep a row or tombstone, or accept that the definition only holds until the fact is erased.

### [LOW] Item 9: seeding with `add_fact_precomputed` must declare the provider's own fingerprint

- On a fresh store, the first write stamps whatever fingerprint it declares (`record_if_absent`).
- If `seed` declares anything other than `provider.fingerprint()`, every later `remember` through the provider fails with `EmbeddingModelMismatch`. The full-tuple comparison is described at `ingest.rs:44-56`.
- **Fix:** the harness's `seed` should declare the configured provider's fingerprint. On `start_without_embedder`, any declared identity works.

### [LOW] C5's over-long key now overlaps the new no-echo rule

- `an_unknown_project_is_an_error_that_names_it` expects an over-long key to produce "an error naming the value".
- Since D10's change, that only holds if the key is longer than the 256-byte label bound but at most 4,096 bytes. Above 4,096 bytes, the error must not echo the value, which contradicts the assertion.
- **Fix:** give the C5 key a concrete length, for example 300 bytes, so it cannot clash with `an_oversized_project_is_refused_without_echo`.

## Resolution (round 3)

- 64-term cap reason → superseded in revision 4 (Codex round 3): no truncation; a query over 256 distinct terms is refused, the reason stated as per-call cost with the measured numbers. Terms are now deduplicated case-insensitively (revision 5).
- Unbounded unknown-argument error → every record's `error` is cut to 512 bytes at a character boundary (ADR-0020 §6's capture cap), so no client value can outgrow the payload limit; the reply keeps the full text (ADR-0020 §8 requires an unknown `project` be named; the 4,096-byte refusal was removed in revision 4). Payload wording now reads "a call that failed before its record was written".
- #1023 erase → D11 carries a hand-off line for #1023.
- `seed` fingerprint → item 9's harness `seed` declares the configured provider's own `fingerprint()`.
- C5 over-long key → a concrete 300-byte key; the no-echo test no longer exists (revision 4 names every unknown value).

---

# Round 5 delta review: lean MCP surface plan (#1020)

(Revision 5, plan SHA-256 c3f3ac9e30c20981bfd566cd8501fb0a5d87edbb48a1058eeb47f9252c8cab79; the reviewer's final message, verbatim.)

**Not LGTM yet: one new MEDIUM.** Revision 5 resolves every round-3 LOW, and the revision-4 changes are in place. But the new 256-term limit in D12 counts runs, not FTS tokens, so it does not bound the cost of one call, which is the reason D12 gives for it. Nothing else in either delta is a problem. Confidence is high: the finding rests on probes I ran.

Altitude report SHA-256: bce9d20ee5dcd0bdceaa98c61fd931e5ec1094c99834eb0afe1f06307d2ea2f4

## Altitude report (acknowledged first)

- The report binds `Plan SHA-256: c3f3ac9e…`, which matches the file on disk.
- Its counts match the file: 561 lines, 53,297 bytes, 15 top-level tasks, and one fence at lines 530–545.
- It describes the revision 3→4 and 4→5 deltas accurately. The **proceed** disposition is sound.

## Resolutions verified

- **Record `error` field:** cut to 512 bytes at a character boundary on every record. The other payload fields are small and bounded, so a record can no longer outgrow the 1 MiB payload limit.
- **Reply:** keeps the full failure text, as the revision-4 ruling requires.
- **Payload wording:** now reads "a call that failed before its record was written".
- **#1023 hand-off:** stated in D11.
- **Deduplication:** terms are now deduplicated case-insensitively.
- **C5:** uses a 300-byte key.
- **`seed`:** declares the configured provider's own `fingerprint()`.
- **Revision 4:** an unknown `project` is named in full (Codex's ruling, not re-raised), and the 256-term refusal and its tests are consistent with D12's text.

## New finding

### [MEDIUM] D12: a single "term" can hold thousands of FTS tokens, so the 256-term limit does not bound cost

**Why one term can be huge.** D12 defines a term as a maximal run of ASCII alphanumerics, `_` or non-ASCII characters. But `unicode61` splits tokens on `_` and on non-ASCII separators such as NBSP (U+00A0) and the em dash (U+2014). So one quoted run like `lock_lock_lock…` counts as one of the 256 terms, yet FTS5 evaluates it as a phrase with thousands of tokens.

**Measured** with Python's sqlite3 3.46.1 over 20,000 rows, each run counted as a single term:

| One quoted run | Size | Time |
|---|---|---|
| `lock_lock_…`, 2,000 tokens | 10 KB | 0.95 s |
| `lock_lock_…`, 10,000 tokens | 50 KB | 4.74 s |
| `lock` joined by NBSP, 20,000 tokens | 120 KB | 8.33 s |
| `lock` joined by NBSP, 100,000 tokens | about 600 KB | 135.65 s |

- All of these fit under the 1 MiB query cap.
- Runs of distinct unmatched words stay cheap (0.01 s at 100,000 tokens). The cost comes from tokens with long index lists.

**Impact:**
- One `recall` can hold a read connection and a blocking thread for minutes.
- A handful of such calls starves every other `recall`.
- The shutdown drain (`in_flight`) waits for them, so stopping the server can stall for minutes too.
- Only authenticated machines can call, so this is MEDIUM rather than HIGH.

**Recommended fix:** bound tokens, not runs. Make a run a maximal sequence of Unicode alphanumerics only (`char::is_alphanumeric`), dropping `_` and non-ASCII separators from the run class. Each term is then about one token, and 256 terms bound the call as D12 intends.

**Alternative**, if identifiers such as `memory_query` should stay phrases: keep runs as they are, but split any run of more than 8 segments into separate OR'd terms, and count every segment toward the 256.

**Add a test:** a 10,000-segment `lock_lock_…` query must be refused, or answered quickly, without an embedder.

## Resolution (round 5)

- [MEDIUM] D12 run class → adopted the recommended fix: a run is a maximal sequence of Unicode alphanumerics (`char::is_alphanumeric`), so each term is one FTS5 token and the 256-term limit bounds the call; D12 records the measurements and that identifiers split (`memory_query` → `"memory" OR "query"`). Item 11 gains unit cases (`été` one term, `memory_query` split, NBSP and em dash separate) and `a_long_underscore_run_is_cheap` (a 10,000-segment `lock_lock_…` query dedupes to one term and is answered without an embedder).

---

# Round 6 delta review: lean MCP surface plan (#1020)

(Revision 6, plan SHA-256 c589db1f3ba0a2757f31741b2b5ed588a1eb82d0c6b5b0edc8d044acf50f13e5; the reviewer's final message, verbatim.)

**Not LGTM yet: one MEDIUM remains.** Revision 6 adopts the run class I recommended in round 5, `char::is_alphanumeric`, but that fix is incomplete. Some characters count as alphanumeric to Rust yet split tokens in FTS5's `unicode61` tokenizer, so one term can still expand into a phrase of thousands of tokens. The rest of the 29-line delta is fine. Confidence is high; the evidence is below.

Altitude report SHA-256: 68ee8b89da133dc5fd87b627ef8dc7a6bb285f60df80eea5971c253c0f3b385e

## Altitude report (acknowledged first)

- The report binds `Plan SHA-256: c589db1f…`, which matches the file on disk.
- Its counts match the file: 567 lines, 53,947 bytes, 15 top-level tasks, and one fence at lines 536–551.
- It describes the revision-6 delta accurately (D12's run class and item 11's tests). The **proceed** disposition is sound.

## Delta verified

- The `_`, NBSP and em-dash cases are now closed: each of those characters ends a run.
- `memory_query` now splits into two terms, and D12 accepts that trade explicitly.
- Item 11 gains these tests:
  - `été` is one term;
  - `memory_query` splits into two;
  - NBSP and the em dash separate terms;
  - `a_long_underscore_run_is_cheap`.
- Repeats are deduplicated case-insensitively.

## New finding

### [MEDIUM] D12: `char::is_alphanumeric` is wider than `unicode61`'s token characters, so the term limit can still be bypassed

**Why the two classes differ.**
- Rust's `char::is_alphanumeric` means Alphabetic or Numeric. Alphabetic includes the `Other_Alphabetic` property, which covers some marks and symbols, not only letters.
- `unicode61` treats as token characters only the general categories `L*`, `N*` and `Co` (its default `categories` setting). Everything else splits tokens.

**Characters in the gap.** The system's `/usr/share/unicode/PropList.txt` lists these as `Other_Alphabetic`:
- U+0345 (`Mn`), a combining mark;
- U+24B6..24E9, circled letters such as `Ⓐ` (`So`);
- U+1F130..1F149, squared letters (`So`).

**Evidence** (Python's sqlite3 3.46.1, 20,000 rows; I did not compile Rust here):
- `"lockⒶfile"`, `"lockͅfile"`, `"lock\U0001F130file"` and `"lockिfile"` each match the stored phrase "lock file". So `unicode61` splits on all four characters.
- U+093F is `Mc`; I expect it is `Other_Alphabetic` too, but did not confirm that line in PropList.

**Cost.** Under revision 6 each of these is one term, yet FTS5 evaluates it as a phrase:

| One run | Size | Time |
|---|---|---|
| `lockⒶlockⒶ…`, 2,000 segments | 14 KB | 0.91 s |
| `lockⒶlockⒶ…`, 10,000 segments | 70 KB | 4.43 s |
| `lock` joined by U+0345, 10,000 segments | 60 KB | 4.67 s |

**What this means.**
- D12's sentence "a run holds no `_` and no separator, so each term is one FTS5 token" is false for these characters.
- A query near the 1 MiB cap would cost roughly a minute, so the round-5 impact (a held read connection, starved recalls, a stalled shutdown drain) still applies.
- One case I checked is not a problem: a letter added in a later Unicode version (Adlam U+1E900) is a token character in this SQLite, so version drift between Rust's and SQLite's tables is not a second hole.

**Fix (ideal):** make a run exactly `unicode61`'s token class, meaning general category `L*`, `N*` or `Co`. The standard library has no general-category API, so this needs a small Unicode-category crate.
- Then every term really is one token, and the 256-term limit bounds the call.
- Add unit cases for `lockⒶfile` and `lockͅfile`, each giving two terms, and a cheap-answer test for a 10,000-segment `lockⒶlock…` query.

**Fix (no new dependency):** keep the current run class and add a second limit that does not depend on character classes. For example, refuse a query whose full-text terms total more than 4 KB, with an error naming the size.
- By my measurements that caps one call at roughly 1,000 tokens, about 0.45 s over 20,000 rows, whatever the classification.
- Under this option, D12 should stop claiming that each term is one token.

## Resolution (round 6)

- [MEDIUM] D12 run class vs `unicode61` (also raised by Codex in its round 6, with U+0345) → the no-new-dependency fix, simplified: `recall.query` is refused above 4,096 bytes (`MAX_RECALL_QUERY_BYTES`, D10), replacing both the 1 MiB cap (and its facade re-export) and the 256-term limit, which the byte cap makes redundant. D12 no longer claims a term is one token; it states the run class is for syntax safety and that the cost bound rests on the byte cap plus deduplication, with measurements at the cap (worst phrase 0.21-0.34 s, 64 distinct every-row terms 0.08 s, 600 absent terms 0.001 s over 20,000 rows). A general-category crate was not added: the byte cap bounds cost whatever the classification, with one rule and no dependency. Item 11's tests now drive worst-case 4,096-byte queries (`lock_`, U+0345, a repeated word) and a last-word match; item 9's oversized-query test uses 4,097 bytes.

---

# Round 7 delta review: lean MCP surface plan (#1020)

(Revision 7, plan SHA-256 4c947d14cc67456f067d60ad3ff076b2f06baff3e619eb30ead470635de7d8ba; the reviewer's final message, verbatim.)

**LGTM.** The 4,096-byte query cap in D10 closes the round-6 MEDIUM with no new dependency. Re-measured, the worst case is now about one to two seconds instead of minutes. Two LOWs remain, neither blocking: D12's worst-case figures are 3–6× too low, and the model is never told about the limit.

Altitude report SHA-256: 31793d810bdbb85f996dc49c3f3be5fd6dea9fa3ce6e00396e0b745e4507852f

## Altitude report (acknowledged first)

- The report binds `Plan SHA-256: 4c947d14…`, which matches the file on disk.
- Its counts match the file: 565 lines, 53,740 bytes, 15 top-level tasks, and one fence at lines 534–549.
- It describes the revision-7 delta accurately: the plan shrank by two lines, and the 1 MiB cap, its facade re-export and the 256-term limit are gone.
- The **proceed** disposition is sound.

## Delta verified

- **D10** refuses a query over 4,096 bytes (`MAX_RECALL_QUERY_BYTES`) with an error naming the size and the limit. Without the 1 MiB cap, the facade re-export and its `api.md` row are no longer needed.
- No stale reference to the removed limits is left. The "1 MiB payload limit" at line 189 is the event-payload limit, which still holds.
- **D12** no longer claims that each term is one token, and it names U+0345 as the counterexample.
- **Item 9** tests both sides of the boundary: 4,097 bytes is refused, 4,096 is answered.
- **Item 11's** worst-case tests drive 4,096-byte queries.

## New findings

### [LOW] D12: the stated worst cases at the cap are too low, and diacritics get past the deduplication

The plan reports a worst single phrase of 0.21–0.34 s at the cap and says the cost bound rests on the cap plus deduplication. I re-measured with Python's sqlite3 3.46.1 over 20,000 rows, using `ORDER BY bm25 LIMIT 30`, which is how the backend queries:

| 4,096-byte query | Time | Why it costs more than D12 states |
|---|---|---|
| `a_a_a…` (2,048 segments, `a` in every row) | 0.99 s | Two bytes per segment packs four times the tokens of D12's `common0_` example |
| 516 diacritic spellings of one word (`lock`, `lóck`, `ļočķ`…) | 1.81 s | Shown below |

The diacritic spellings get past the deduplication:
- The plan deduplicates by comparing strings case-insensitively, so every spelling counts as a distinct term.
- `unicode61` strips diacritics by default, so all 516 are the same FTS5 token. I checked 50 of 50.
- This is the "repeated frequent term" case that deduplication was credited with removing.

**What still holds.** The cap bounds the cost: about 1–2 s at 20,000 rows, growing linearly with the number of rows. That is a real bound, not the minutes measured in round 5.

**What to change.** D12 should state the true worst case, so Michaël decides on real numbers. Then one of:
- accept it as is;
- lower the cap, which reduces cost proportionally;
- deduplicate on the folded form (diacritics stripped). This needs normalization, which the standard library does not provide.

The item 11 tests assert only that each query is answered, so they cannot catch any of this. Timing assertions would be flaky, so this belongs in the D12 text rather than in a test.

### [LOW] The model is never told the 4,096-byte limit

- The exact `recall` description in item 9 does not mention the limit, so the model learns it only from a refusal. Long pasted error messages and stack traces are the likely ones to hit it.
- **Fix:** add one sentence to the description, for example "Queries over 4,096 bytes are refused."
- A JSON Schema `maxLength: 4096` would only be a hint, because it counts characters while the cap counts bytes.

## Resolution (round 7)

- [LOW] D12 worst case → D12 now states the re-measured worst cases (about 1-2 s at 20,000 rows, linear in rows: `a_a_…` 0.99 s, 516 diacritic spellings 1.81 s), that string deduplication does not fold diacritics, and accepts the residual for authenticated callers; "For Michaël" #2 offers a lower cap. No normalization dependency added.
- [LOW] limit not told to the model → item 9's exact `recall` description ends with "Queries over 4,096 bytes are refused."

---

# Round 8 delta review: lean MCP surface plan (#1020)

(Revision 8, plan SHA-256 deb26a4be7e417062797a24937b2f1adcdaef5270cc9ae638184069d318d250d; the reviewer's final message, verbatim.)

**LGTM.** The 42-line delta applies both round-7 LOWs correctly and completely, and it introduces no new finding.

Altitude report SHA-256: b750a3ab4021eabd587f19c89f3f57408e67115f227825f101bfcc6b237f4038

## Altitude report (acknowledged first)

- The report binds `Plan SHA-256: deb26a4b…`, which matches the file on disk.
- Its counts match the file: 570 lines, 54,266 bytes, 15 top-level tasks, and one fence at lines 539–554.
- Its account of the revision-8 delta is accurate: D12's worst case, "For Michaël" #2, and the `recall` description.
- The **proceed** disposition is sound.

## Delta verified

**D12 now gives the real worst case.**
- The figures are mine, carried over unchanged: 0.99 s for a phrase of 2,048 one-letter segments (`a_a_…`) and 1.81 s for 516 diacritic spellings of one word. Both were measured over 20,000 rows with `ORDER BY bm25 LIMIT 30`, as the backend queries.
- It says the cost grows linearly with the number of rows. That is an inference from the bm25 scan; I measured only at 20,000 rows.
- It states that string deduplication does not fold diacritics.
- The cost bound now rests on the cap alone, which is the accurate claim.
- Case-insensitive deduplication is still in place (line 219).
- It accepts the residual, about 1–2 s per crafted call from an authenticated machine, explicitly.

**"For Michaël" #2** names that residual and offers a lower cap. The decision now rests on correct numbers.

**The `recall` description** now ends with "Queries over 4,096 bytes are refused." That matches D10's byte-based limit.

## Resolution (round 8)

No changes required: LGTM, no new finding. The "grows linearly" wording is the reviewer's own inference, kept as stated.

---

# Round 9 delta review: lean MCP surface plan (#1020)

(Revision 9, plan SHA-256 63615f8813ae874586fba9ae0be975f7d517ea2e5e6a61c78d4feaa4677b4dc1; the reviewer's final message, verbatim.)

**LGTM.** The one-line change to item 1 is correct and complete, and it introduces no new finding.

Altitude report SHA-256: 91d6ae10c23f1303faa5959c49aa691043368aa0ce10331b8a293fe8e7d0fcce

## Altitude report (acknowledged first)

- The report binds `Plan SHA-256: 63615f88…`, which matches the file on disk.
- Its counts match the file: 570 lines, 54,252 bytes, 15 top-level tasks, and one fence at lines 539–554.
- It accurately describes the revision-9 delta as one line in item 1.
- The **proceed** disposition is sound.

## Delta verified

- Item 1 now reads "Commit this plan and its review artifacts under `docs/plans/`". The meaning is unchanged: the plan and its review artifacts are committed, and `-drafts/` is deleted, never committed.
- The plan no longer names its own path anywhere. `grep` finds no `docs/plans/2026-11-17…` reference left.

## One note, not a finding

Item 15, line 474, still names the worktree `.worktrees/feat-1020-lean-mcp-surface`. I don't know the archiver's exact rule. If it also refuses repository-relative local paths, this is the only candidate left; if it only refuses the plan's own path, nothing more is needed.

## Resolution (round 9)

No changes required. The note was checked: the archiver's dry run over these exact bytes exits 0, so the worktree mention is not a refused source reference.
