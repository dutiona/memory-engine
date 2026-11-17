# Clean-slate subagent review: docs/plans/2026-11-17-forget-retraction.md

A fresh general-purpose subagent (Claude Opus 5.5, no conversation context), resumed across four rounds; read-only, no build or test run. Each round reviewed the copied plan in a disposable snapshot of main at 7a22c07. Rounds 2 to 4 reviewed only the delta.


---

<!-- round 1 -->

# Review r1: feat(forgetting) forget as a reversible retraction (#1023)

Altitude report SHA-256: ff394e193f0758560547301731114fd6d6161283b36531b7fed32bfae43cce01

Plan reviewed: `.review/plan.md`, SHA-256 `037f4a8742a4b99320cd864fab52ce21bc5fd88b98137677118917d44f63645b` (matches the
altitude report's header). Checkout: memory-engine main at 7a22c07. Read-only review; nothing was built or run.

**Verdict: not LGTM yet. 0 BLOCKER, 4 HIGH, 7 MEDIUM, 12 LOW.** The design is sound and close to the contracts:
ADR-0020 §8's three answers and write authority, ADR-0021 §5's by-ID authority and C17, and §12's call record as an
appended event. Three things are wrong. Two steps cannot be done as written: item 10 breaks the archive-OFF gate, and
items 6 and 8 need item 9's API. The erasure privacy guarantee is false in two ways (H3, H4). And several write and
erase scopes are decided without an Owner mark (M1–M5).

## Altitude and duplication (assessed first)

I agree with the report's **proceed**. The detail goes into decisions (D1–D18), the classification of write paths
(D3), the one schema literal (the DDL the migration freezes) and one failing test per item. Items name files,
signatures and tests, and transcribe no code. Each rule is owned once: the gate in D2/D3, selection in D9, purge in
D10, records in D12. Items point back to those decisions and do not restate them.

**Override acknowledged as justified.** The report's signal (18 items, about 47 KB, one PR) holds up. The acceptance
ties together the tool, the tombstone every import and promotion path must honour, unforget and erase with its #1020
linkage. Splitting the PR would ship a tombstone without its erasure semantics. Items 1, 2 and 16 are separable
boy-scout commits.

One small duplication: the Verification block copies AGENTS.md's matrix verbatim. It is acceptable, because the plan
then states exactly what evidence it expects.

## HIGH

**H1. Item 10 does not compile under the archive-OFF gate.**
- `facade erase` calls `MemoryGraph::remove_node` "as `me_archive::archive` does". That method is
  `#[cfg(any(feature = "archive", test))]` (`crates/me-index/src/graph/memory_graph.rs:143`).
- memory-engine's default features are `["backend-sqlite"]` (`crates/memory-engine/Cargo.toml:205`). Only the facade's
  `archive` feature forwards `me-index/archive` (`Cargo.toml:219-235`).
- So `cargo check -p memory-engine` fails, and that is the archive-OFF gate in Verification. Erase is unconditional.
- Fix: ungate `remove_node`. Erase becomes its second production caller, which also removes the dead-code reason given
  in its doc and in the Cargo comment at `:219-220`. Add `crates/me-index/src/graph/memory_graph.rs` (and that Cargo
  comment) to item 10's files.

**H2. Items 6 and 8 cannot reach green in the stated order.**
- Item 8's tests live in `tests/archive_test.rs` and `tests/restore_roundtrip_test.rs`. These are facade integration
  tests, so they cannot reach the port: `storage()` is `#[cfg(test)] pub(crate)` (`engine/mod.rs:733-734`).
  - `archive_leaves_a_forgotten_fact_in_place` needs `forget_fact` and `unforget_fact`.
  - `export_and_import_carry_tombstones` needs `unforget_fact` and the `explain` change.
  - All of these arrive in item 9.
- Item 6's CLI unit test `a_rerun_over_a_forgotten_memory_reports_it_blocked` (`commands/bootstrap.rs`) has the same
  problem: the CLI crate can only forget through the facade.
- A compile failure is an acceptable red step, but these tests cannot turn green until item 9 lands.
- Fix: move item 9 (the facade's `forget_fact`/`unforget_fact`/`explain`) to directly after item 5, or move these
  tests into item 9.

**H3. D10's purge does not remove earlier copies, so `erase` of a forgotten fact still leaves its content in
`memory.db`.**
- `PRAGMA secure_delete` zeroes only the space freed while it is on. Earlier writes leave copies behind:
  - **The forget itself.** It updates `t_expired`/`t_invalid` from NULL to TEXT, which changes the cell size. SQLite
    then drops the old cell into a freeblock unzeroed, and frees its overflow pages unzeroed. An embedding plus content
    often overflows a 4 KiB page.
  - **Every later UPDATE of the row**, such as access or surfaced stamps.
  - **The FTS trigger.** `facts_fts_au` fires on every UPDATE of `facts` (`store/schema/ddl.rs:199-202`) and adds
    segments holding the tokens. FTS5 automerge later frees the old segments, unzeroed.
- The diagram's headline path, `Forgotten --> [*]: erase`, is therefore the one most likely to leave the text in free
  pages.
- The item 13 test (insert, then erase) cannot see this. Its sentinel can also be altered by the `porter` tokenizer
  (`ddl.rs:186`), so a stemmed token could survive a raw byte search undetected.
- Fix:
  - After the erase transaction commits, run `VACUUM` and then `PRAGMA wal_checkpoint(TRUNCATE)`. The store is in WAL
    mode (`store/schema/pragmas.rs:22`) and the CLI holds the lock. Keep `secure_delete` and `optimize`.
  - Name VACUUM's cost (time, about 2× disk) in the docs.
  - Rewrite `erase_fact_leaves_no_byte_of_its_content`: forget the fact and recall it (which updates the row) before
    erasing, and pick a sentinel the porter stemmer leaves unchanged.

**H4. The Outcome's claim "leaves no copy of the erased content in the live store's files" does not hold for
`--fact`.**
- `--fact` deletes no event (D9), and for activities it only nulls `activities.promoted_fact_id`. Two kept rows can
  still hold the text:
  - The source activity of an activity-promoted fact. Its `args`/`result_summary` hold the content that was promoted
    (`engine/activity.rs:75-110`).
  - Any ingested event that carries the text and that a fact's `source_event_id` names.
- Today's server sources carry no content: the `remember` record, and the bootstrap marker
  (`bootstrap/mod.rs:131-134`). But #1025 projects final messages from lifecycle events, and those do carry the text.
- This is an owner decision. Either:
  - scope the guarantee: the report and D12 record name the kept source event and activity, and D10's residuals list
    them; or
  - have `--fact` also purge a content-bearing source that the #1020 linkage does not need. A `remember` record holds no
    content and stays.
- Either way, add a sibling-coordination line for #1025 and a test with an activity-promoted fact.

## MEDIUM

**M1. An identical twin of a forgotten fact still comes up in the same session's recall.**
- `forget` retracts one row. But `add_fact` has no dedup (`remember` twice gives two rows), and `insert_or_reinforce`
  dedups per scope only (`store/facts.rs:236-262`). So a memory imported into both the bucket and the user level has
  two rows. Item 9's own test imports into both.
- The session that forgot fact 412 can therefore get 413 back from `recall` with the same text, against "this fact
  won't come up again". D1 already treats content as the identity for imports.
- Owner decision: either retract the active twins inside the own Homes in the same transaction (one tombstone each,
  and `unforget` restores the named one), or document that forget acts per row.

**M2. The lineage closure in D9 can lose other facts with no way to restore them.**
- Erasing a synthesized or wisdom fact W deletes W. But the `Synthesize` arm expired W's other sources when it created
  W (`sqlite/consolidation.rs:502-530`, `store.expire(*src, now)`).
- Those sources are possibly another machine's facts. After the erase they stay expired, their only successor and their
  `supersedes` edges are gone, so they are out of recall for good. No command lifts that.
- Fix, at least: report them (an `orphaned_sources` entry in `EraseReport` and in the D12 record) and say so in
  cli-inspector. Make the larger question an Owner point: should erase restore their validity?

**M3. D9's `--machine` scope has two unrecorded gaps.**
- (a) `activities` and `session_checkpoints` rows filed in the bucket hold that machine's text (`args`,
  `result_summary`, `summary`; `ddl.rs:98-133`), and erase leaves them. They are not among the tables "#1025, #1026,
  #1029 add", so the sibling note will not catch them either.
- (b) User-level facts stamped with the machine are erased. That is an irreversible reading of the issue's "that
  machine's facts", decided without an Owner mark.
- Fix: decide and record both (include them by scope, or exclude them with a reason), and mark D9's user-level choice
  **Owner**.

**M4. Two hard deletes remain on the port with no callers.**
- `FactGraph::hard_delete_facts` (`crates/me-storage/src/graph.rs:97`; `sqlite/graph.rs:283-297`) and
  `hard_delete_edges_by_facts` (`me-storage/src/graph.rs:242`; `sqlite/graph.rs:662-666`) have no caller in `crates`,
  `tests`, `fuzz`, `examples` or `benches`.
- Both bypass erase's FK cleanup, the clearing of tombstone reasons, and the maintenance record. They contradict the
  "store's one hard delete" claim the docs will make.
- Delete them in item 2, for the same reason given for `insert_or_reinforce_fact`.

**M5. Two paths left ungated may not be explicit assertions.**
- `add_facts_batch(_partial)`: the server guard names it the write of the insight flush, a derived job whose return the
  follow-up decides (`crates/memory-engine-mcp/clippy.toml:8-9`). Classifying it as an assertion (D3) lets a derived
  job bypass the tombstone.
- `resolve_conflict` Add/Update (`sqlite/graph.rs:941-966`) is fed by a consumer's arbiter pipeline, which is
  extraction rather than an explicit assertion. It is left ungated without an Owner mark.
- Fix: keep `batch-ingest` an assertion, but give the facade batch methods a gated default (or write the constraint
  into the clippy reason and the sibling notes). Mark the conflict-resolution choice **Owner** in D2.

**M6. Two of item 5's conformance assertions cannot be written in the conformance battery.**
- The battery is generic over `ConformanceBackend` and must "seed ONLY through the port" (`me-test-support/src/lib.rs:15-17`).
- `PRAGMA foreign_key_check` is not a port call.
- An event with an invalid JSON payload cannot be seeded through the port, because `NewEvent.payload` is a
  `serde_json::Value` (`me-types/src/types/events.rs:192`).
- Fix: move those two checks into a SQLite-only test in `sqlite/retraction.rs`.

**M7. Portable paths (ADR-0021 §9) will slip past the gate.**
- The gate hashes the stored form. Once tokenization lands, re-importing a first-release literal-path memory that was
  forgotten stores it tokenized. Its hash differs, so it is not blocked.
- Add a sibling-coordination line: the issue that implements §9 must make `blocks` match both forms, or tombstone both
  hashes.

## LOW

- **L1. D3 is missing `FactGraph::insert_fact`** (`sqlite/graph.rs:159-171`). It becomes gated through D2's `insert`.
  List it in D3, update its `# Errors`, and note that the battery seeds through it.
- **L2. The HNSW reasoning in "How a forgotten fact is seen" is wrong.** The sidecar's HNSW is rebuilt from the DB at
  close (`to_snapshot` selects `t_expired IS NULL`, `search/ann.rs:319-321`). A missed `notify_insert` does not freeze
  into the sidecar; it leaves the live index stale until the next open. Keep the notify and fix the wording.
- **L3. `Restored { active }` should also require `prior_t_invalid` to be NULL or in the future.** `assemble_results`
  drops `t_invalid <= now` (`me-query/src/hybrid.rs:84`), so without this the CLI prints "back in recall" for a fact
  that is not.
- **L4. Item 7's comment on the activity check is inaccurate.** The `is_forgotten` read runs on the read pool, outside
  the write, so it races a concurrent forget; it is not "harmless under the single writer". It is not reachable from
  the server, so this is minor. Fix the comment, or do the check in the insert transaction (a gated `insert_fact_atomic`
  variant).
- **L5. D6's record names a fact for every answer.** `{"fact_id": id}` is written even for `not found` and
  `already forgotten`, where ADR-0020 §8 says "IDs of the facts returned or changed". Consider naming the key for the
  requested id, or aligning with recall's `fact_ids`.
- **L6. `validate_snapshot` should check tombstones** (`inspect/restore.rs:193`): `fact_id` and `record_event_id` must
  be present in the snapshot or null, and the hash must be 32 hex characters, as the #257 edge checks do. Otherwise a
  bad snapshot fails late as a raw FK error.
- **L7. Validate `erase --machine` against the DNS-label grammar** (`memory-engine-mcp/src/auth.rs`,
  `is_dns_label`). A value such as `a/b` makes `machine_bucket` a two-segment path, a child of `unscoped:a`. D14's move
  to me-types is the place to move the grammar too.
- **L8. A large `linked` can make an erase impossible.** D12's erase record is size-checked inside the transaction, so
  an oversized record rolls back the whole erase. Unlikely; cap it or document it.
- **L9. A related, pre-existing defect in archival.** It hard-deletes facts without unlinking
  `activities.promoted_fact_id` (`sqlite/cold_storage.rs:125-126`). That column has no ON DELETE action
  (`ddl.rs:112`) and `foreign_keys` is ON (`pragmas.rs:13`), so archiving an expired activity-promoted fact aborts the
  archive transaction. Apply item 5's `unlink_promoted_facts` in `commit_archive_atomic` as its own boy-scout commit, or
  file it. Items 1 and 3's new indexes already help archival.
- **L10. Two small wording and safety points in D12 and item 5.**
  - D12 says the record is appended "in their own transaction", while item 5 says one transaction. Say "in the
    operation's transaction".
  - Restore `secure_delete` on the error path too (an RAII guard), since the write connection is pooled.
- **L11. Documentation gaps.**
  - `docs/usage/bootstrapping-sessions.md:132` has a `BootstrapReport` field table that needs `facts_blocked`.
  - `docs/getting-started/installation.md:93` lists the writable commands under the lock and needs `unforget` and
    `erase`.
  - AGENTS.md rule 10 asks for `# Examples` on all public items, while the plan limits them to the three facade
    methods. Say why, or add them.
  - Beside rule 7's exception, note that the tombstone's reason cannot be rebuilt from the log (D6).
- **L12. Nits.**
  - D16 could also disallow `MemoryEngine::archive`, another hard delete, in server code.
  - `own_homes -> [String; 2]` forces a type change in #1022; a slice or `Vec` avoids it.
  - The opt-in archive recall fallback (`include_archives`) ignores tombstones for an archived twin.
  - "the batch savepoint" is now an RAII transaction (#1000).

## Over-engineering

Nothing material.
- `EraseReport.deleted: BTreeMap<String, usize>` is justified, because named siblings (#1025, #1026, #1029) extend
  erase.
- D18 (no `--dry-run`) and D13 (no L3 crate) are the right size.
- `is_forgotten` is a port method with one caller. That is acceptable, and the alternative is the gated
  `insert_fact_atomic` variant mentioned in L4.

## Structural completeness

Documentation, Testing and Verification sections are all present. Verification is the full AGENTS.md matrix plus
MSRV, a check that the new `[[test]]` target actually runs, the `insert_asserted` caller grep and the plugin-gate
exemption. With H3 fixed, Testing must also cover the forget-then-erase purge (see H3), and an activity-promoted erase
if H4 keeps the guarantee.

## Plan claims checked against the source (accurate)

- **Hash and gate:** `content_hash` is blake3 truncated to 32 hex characters (`store/facts.rs:90-93`), and the DDL
  comment's `hex[:16]` is stale (`ddl.rs:27`).
- **Insert sites:** every production fact insert is accounted for:
  - `FactStore::insert` (`facts.rs:154`) and the raw restore INSERT (`inspect/restore.rs:366`);
  - its callers `insert_fact_atomic` (`sqlite/graph.rs:757`), the batch (`:118`), resolve Add/Update (`:946`,
    `:966`), bootstrap's `insert_or_reinforce` (`:1132`), the cycle arms AddFact/Promote/Synthesize
    (`consolidation.rs:367`, `:453`, `:504`), `promote_atomic` (`:701`);
  - activity promotion through `add_fact` (`engine/activity.rs:100`).
  - All other raw INSERTs are in test modules.
- **Dead method:** `insert_or_reinforce_fact` has no caller.
- **Recall filtering:**
  - Both channels filter Active (`me-query/src/hybrid.rs:136-141`), and `assemble_results` drops `t_invalid <= now`.
  - No recall channel follows edges.
  - Every FactStore list query filters `t_expired IS NULL`, and no path un-expires a fact.
- **Foreign keys erase must handle:**
  - edges, both ends, no action;
  - `activities.promoted_fact_id`, no action;
  - `facts.source_event_id`, no action;
  - `lineage.wisdom_fact_id` and `fact_vectors`, which cascade.
  - `hard_delete_incident_to_facts` vs `hard_delete_by_facts` is as D5 says, and `hard_delete_ids` binds one variable
    per id (`facts.rs:1558-1573`).
- **Storage port:** six bounded traits plus `ColdStorage` (`me-storage/src/backend.rs:50-61`); `SqliteBackend` is the
  only implementor; Postgres has its own decoupled version chain.
- **MCP server:** `home::bucket` and `user_home` (`home.rs:43-50`); `record` writes `machine` and files the record in
  the bucket; `capture_cut` and `refuse` (`tools/mod.rs`); the guard exists (`clippy.toml`); `TOOL_NAMES`
  (`fuzz/fuzz_targets/dispatch.rs:27`); the lean-surface tests.
- **CLI and schema:** `StoreLock` via `open_engine_writable` (`memory-engine-cli/src/db.rs`); `MIGRATIONS` and v15 with
  its `simulate_vN` test pattern; `<db>.vN.bak` in the DB's directory (`backup.rs:45`, `db.rs`); `ExpiredReason::Forgotten`
  exists but is never emitted; `EngineSnapshot` and `ConflictError` (`#[non_exhaustive]`).

---

<!-- round 2 -->

# Review r2: feat(forgetting) forget as a reversible retraction (#1023)

Altitude report SHA-256: 986fca38a4c3848b8cda191a32606fc20b8bf3c283408682aab371cb50e8f229

This round reviews the delta only, read-only. The plan reviewed has SHA-256
`77af1ac52b194750c844645334a88d958153ec638793ce82cce40f1074a347bc`, which matches the altitude report's header. The
code is the same checkout at 7a22c07.

**Verdict: not LGTM yet. 0 BLOCKER, 1 HIGH, 3 MEDIUM, 6 LOW (all new).**
- All 23 round-1 findings are resolved, or accepted as Owner points that are now explicit. Two of those fixes brought
  in new problems: N2 (the `archive` guard) and N3 (VACUUM after the commit).
- The rest of the reorder and the new decisions check out against the code.
- One new D3 promise cannot be delivered with the mechanism item 7 describes: per-item failure in a partial batch (N1).

## Altitude (assessed first)

I agree with **proceed**. The plan grew by 13 lines and about 7 KB. Each addition is a changed decision, a required
test or a recorded residual; none is transcribed code. Ownership stays single: gates in D2/D3, selection in D10,
purge in D11, records in D13. Item 10's change list repeats D11's purge order, but only as the place where the
implementation goes; that is acceptable.

The override from round 1 still holds: 19 items in one PR, with items 1, 2, 3 and 17 as separable boy-scout commits.
The report's measurements match what I see: 326 lines, the same three fenced blocks, and the D3 table now 11 rows.

## Round-1 findings: disposition

| # | Status | Where |
|---|---|---|
| H1 `remove_node` cfg gate | Resolved | item 11 ungates it; `cargo check -p memory-engine` added to its Green (see N6 on the now-empty feature) |
| H2 sequence | Resolved | the facade's forget, unforget and explain are now item 6, so items 7, 8 and 9 only use APIs that already exist; erase is items 10–11 |
| H3 purge mechanism | Resolved, new issue N3 | D11 uses `optimize`, then a post-commit `VACUUM` and `wal_checkpoint(TRUNCATE)`; the sentinel `zqxjvkpwqq` is left whole by the stemmer; the test recalls and forgets before erasing |
| H4 guarantee scope | Resolved | D11 scopes the guarantee to the rows erase deletes; `linked` names a kept event; `--fact` takes the promoting activity; #1025 sibling line (see N4 on identical copies) |
| M1 identical twins | Resolved (Owner) | D5; tests in items 5 and 12 (see N4) |
| M2 orphaned sources | Resolved (Owner: reported, not restored) | D10, D13 `orphaned` (see L-a on its definition) |
| M3 `--machine` scope | Resolved | D10 takes the bucket's activities and checkpoints; user-level erasure is marked Owner (see L-d) |
| M4 dead hard deletes | Resolved | item 2 |
| M5 ungated batch and conflict | Resolved in decision, new issue N1 | D2/D3 |
| M6 conformance feasibility | Resolved | item 10 moves both checks into SQLite-only tests |
| M7 portable paths | Resolved | sibling line |
| L1–L12 | Resolved | D3 `insert_fact` row; HNSW wording; D4 `active`; item 8 comment; D7; item 9 validation; D15; D13 refusal; item 3; D13 wording; Documentation; D17, D9 `Vec`, the residual line. Exception: L12's `archive` guard needs a fix (N2) |

## New findings

**N1 [HIGH] D3's partial-batch row cannot be delivered as item 7 describes it.**
- `add_facts_batch_partial` makes its per-record split in the facade by validation only (`me-ingest/src/lib.rs:329-340`).
  It then sends the whole valid partition through **one** `insert_facts_batch_atomic` call (`:346-349` → `:449`).
- Its own doc says a failure inside that insert "rolls the whole valid set back together" and comes back as the
  **outer** `Err` (`:301-310`).
- With D2's gate inside `insert`, a single forgotten row therefore rolls back its whole chunk. The plan promises
  "that item fails as `Conflict(Forgotten)`; the others land" (plan line 73).
- The consequence is in `batch-ingest`, which is the partial caller (`memory-engine-cli/src/commands/batch_ingest.rs:346-371`).
  The whole chunk goes to `skipped += chunk_size; failed_batches += 1`.
- D3 also places `batch-ingest` in the atomic row (line 72), but it uses the partial path.
- Item 7's test `batch_and_conflict_inserts_refuse_forgotten_content` ("partial batch fails that item only") is placed
  in `sqlite/graph.rs`. The partial semantics do not exist at that layer.
- Smallest fix:
  - In `add_facts_batch_partial`, check `is_forgotten` per entry next to `validate_add_fact_request`, before embedding.
    Blocked entries become per-record `Err(Conflict(Forgotten))` and are never embedded.
  - The in-transaction gate stays as the fail-closed backstop. Under the CLI's store lock it cannot race.
  - Fix D3 so `batch-ingest` is listed with the partial row.
  - Move the partial assertion to a facade test (`tests/retraction_test.rs`), and add a CLI assertion that a forgotten
    line is reported `skipped (engine)` while its chunk lands.

**N2 [MEDIUM] D17's new `MemoryEngine::archive` entry will make item 12's clippy run fail.**
- `MemoryEngine::archive` exists only with memory-engine's `archive` feature (`engine/mod.rs:56-57`,
  `#[cfg(feature = "archive")] mod archive;`).
- `memory-engine-mcp` depends on `memory-engine` with default features (`memory-engine-mcp/Cargo.toml:23`), and so
  does `memory-engine-embed`. The MCP crate has no `[features]` section.
- So item 12's Green, `cargo clippy -p memory-engine-mcp --all-targets --all-features -- -D warnings` (plan line 213),
  builds the facade without `archive`, and the path does not resolve.
- The Clippy lint-configuration docs say an unresolvable `disallowed-methods` path is an error unless the entry sets
  `allow-invalid = true`. The workspace-wide `--all-features` clippy unifies `archive` on, which is why the problem
  shows only per crate.
- Fix: add `allow-invalid = true` to that one entry, with a comment that the method exists only under `archive`. The
  workspace `--all-features` build turns it on for the server. This was my round-1 suggestion (L12); I am confident
  about the mechanism from the docs, but the implementer should confirm it with that clippy command.

**N3 [MEDIUM] The plan does not say what happens when VACUUM or the checkpoint fails after the commit.**
- D11 and item 10 run `VACUUM` and `PRAGMA wal_checkpoint(TRUNCATE)` after the erase has committed. The port's stated
  contract is "`Ok` ⟹ committed, `Err` ⟹ store unchanged", and its only listed exception is unforget's post-commit
  `IndexInconsistent` (item 5).
- Two failures are realistic:
  - `VACUUM` running out of disk, since D11 itself warns it needs up to about 2× the store's size free.
  - `wal_checkpoint(TRUNCATE)` reporting `busy = 1` in its result row when a reader still holds the WAL. That is not
    an error, so unchecked it fails silently. This matters for the library `erase`; the CLI's `close()` deletes the WAL
    anyway.
- Either way the rows are deleted but the copies remain, and nothing re-runs the purge: a second `erase --fact N` is
  `NotFound`, and the CLI has no `vacuum` command.
- Fix:
  - Make the post-commit purge an explicit carve-out, like `IndexInconsistent`: a typed error, or an
    `EraseReport.purged: bool`.
  - Check the checkpoint's `busy` column.
  - Have the CLI exit non-zero with "erased, not purged: free disk space and run …".
  - Give the operator a retry path, for example erase re-running the purge on an empty target, or a documented
    `sqlite3 … VACUUM` step.
- Nit: skip `VACUUM` when `deleted` is all zeros (the idempotent rerun in `a_second_machine_erasure_deletes_nothing`).

**N4 [MEDIUM] With D5, unforget and erase cannot find a forget's other rows.**
- D5 writes one tombstone per retracted copy, all naming the same record.
- `unforget <id>` restores only the named row (item 5's test asserts "a copy stays forgotten"). The copies' tombstones
  keep the content blocked from imports, and the copies stay out of recall. Nothing tells the admin: the CLI output
  (item 13) shows one row, and the call record holds only the requested id (D7). Reversing the whole forget therefore
  needs raw SQL on `record_event_id`.
- The same gap affects erasure. `erase --fact N` on a forgotten fact leaves its forgotten copies, which hold
  byte-identical text. The scoped guarantee (D11) still holds, but an operator erasing in order to purge text will find
  it in the copies, and the report does not mention them.
- Fix (Owner chooses):
  - either `unforget` restores every row that shares the forget's `record_event_id`;
  - or, keeping per-row unforget, `UnforgetOutcome::Restored` and the CLI list the remaining rows forgotten by the same
    record.
  - In both cases, `EraseReport` lists the identical-content rows that remain (`copies`), so the operator can erase
    them too.

## LOW

- **L-a. `orphaned` (D10) is defined too narrowly.** It covers only "the facts a taken `Synthesize` result had
  expired". A conflict-resolution Update (`sqlite/graph.rs:924`, `:966`, with a `contradicts` edge new→old) and a dream
  `Supersede` (`consolidation.rs:484`) also expire a predecessor whose successor erase can delete. Define it as
  non-taken expired facts whose `supersedes` or `contradicts` source edge comes from a taken fact.
- **L-b. Look up D5's identical copies by content, not by the stored `content_hash` column.** Restore copies a
  snapshot's stored hash verbatim (`inspect/restore.rs:384`), and D1 already says the hash is computed from content and
  never taken from the column. Inside the homes, `scope_id IN homes AND t_expired IS NULL AND content = ?` is cheap and
  exact.
- **L-c. Item 7 does not list the docs of the public methods that now return `Conflict(Forgotten)`.**
  `resolve_conflict` (`engine/conflict.rs`, `me-resolve`), `add_facts_batch(_partial)` (`engine/ingest.rs`,
  `me-ingest`), `FactGraph::insert_fact`, and `CycleCtx::promote` (`me-traits`) all need their `# Errors` updated.
- **L-d. D10 selects checkpoints by `scope_path`, which ADR-0021 §3 calls "a rendering".** A checkpoint with a NULL
  `scope_path`, or an activity of the machine's sessions filed outside the bucket, is not taken. Say so in the docs'
  selection paragraph, since there is no machine column to do better.
- **L-e. Item 11 leaves the `me-index` `archive` feature empty.** Once `remove_node` is ungated, the feature gates
  nothing (`me-index/Cargo.toml:36-39`, plus the facade's forwarding at `memory-engine/Cargo.toml:235`). Remove both in
  item 11, per "less is more", rather than keep a marker that gates nothing.
- **L-f. D13 should say how `orphaned` and `copies` count toward the payload limit.** They do not affect the #1020
  link, so if the record nears the limit they could be capped (with a total) before the whole erase is refused.

## Checked and correct

- **Reorder:** each item's tests now use only APIs that land in earlier items (6 → 7, 8, 9; 10 → 11 → 12–14).
- **D2/D3:** `insert_asserted` has the single caller `insert_fact_atomic`. Conflict Update rolls back the expiry of the
  old fact together with the refused insert, in one transaction (`sqlite/graph.rs:924-966`), and the graph is mirrored
  only after `Ok` (`me-resolve/src/lib.rs:85-131`).
- **D10 deletion order:** activities before facts. `session_checkpoints.last_activity_id` is `ON DELETE SET NULL`.
  Facts go before events, and the tombstones' `SET NULL` columns cover the rest.
- **D11:** `VACUUM` preserves the `INTEGER PRIMARY KEY` rowids that FTS5 external content, the HNSW index and the graph
  key on, and `TRUNCATE` removes the WAL frames that still hold the pre-erase page images. Dropping `secure_delete` is
  therefore sound.
- **D15:** `is_dns_label` (`memory-engine-mcp/src/auth.rs:126`) is self-contained and moves cleanly to me-types.
- **D4 `active`, D7 record key, D9 `Vec`, item 3's archival unlink** (`sqlite/cold_storage.rs:125-126`) are all
  consistent with the code.

---

<!-- round 3 -->

# Review r3: feat(forgetting) forget as a reversible retraction (#1023)

Altitude report SHA-256: c9f1aeb227f3f1b4cb833061f25eae0cb14f48009e9f062357ff06e82daf6d77

This round reviews the delta only, read-only. The plan reviewed has SHA-256
`1e7578b3ddcb821a3075834c69f67f7de9dc0724ec418ad4dd95259281bccee6`, which matches the altitude report's header. The
code is the same checkout at 7a22c07.

**Verdict: 0 BLOCKER, 0 HIGH, 1 MEDIUM, 3 LOW (all new).** All of round 2's N1–N4 and L-a–L-f are resolved. Once
N5 is fixed, the plan is LGTM. The three LOWs are wording fixes that can be folded in during implementation.

## Altitude (assessed first)

I agree with **proceed**. The measurements check out: 326 lines, 58,267 bytes and 19 top-level items. The delta is 27
lines changed in place. Each change follows a finding and adds a decision, a field or a test; none transcribes code.
Ownership stays single: gates in D2/D3, selection in D10, purge in D11, records in D13. The round-1 size override still
holds.

## Round-2 findings: disposition

| # | Status | Where |
|---|---|---|
| N1 partial batch | Resolved | D3 now checks each entry against the tombstones in `add_facts_batch_partial`, beside `validate_add_fact_request` and before embedding (`me-ingest/src/lib.rs:329-340`), with the in-transaction gate as backstop. `batch-ingest` is on the partial row. Facade test and a `cli_integration.rs` test added |
| N2 clippy `archive` path | Resolved | D17 and item 12 set `allow-invalid = true` |
| N3 post-commit purge | Resolved (see N5 on the retry) | D11: `purged` and `purge_error`; the checkpoint's `busy` column is checked; CLI exits 1; the contract names the carve-out; a busy-checkpoint test is added |
| N4 a forget's other rows | Resolved | D5: `unforget` reverses the whole forget through the shared `record_event_id`, and `record` is now required; `EraseReport::copies`; the CLI lists every restored row (see L-g) |
| L-a `orphaned` | Resolved | supersedes and contradicts targets (see L-i) |
| L-b copies by content | Resolved | matched on `content = ?` inside the homes, never on the stored hash column |
| L-c `# Errors` docs | Resolved | item 7 names each method |
| L-d checkpoint caveat | Resolved | D10 and the cli-inspector docs |
| L-e empty `me-index` feature | Resolved in intent (see L-h) | item 11 |
| L-f record size | Resolved | D13 carries counts; ids stay in the report |

## New findings

**N5 [MEDIUM] The purge retry tells the operator to run an erase that can delete another machine's data.**
- D11 (plan line 89) and item 14 (line 226) make the retry `memory-engine-cli erase --machine <name>`, relying on "a
  machine erase that deletes nothing still purges".
- After a failed `erase --machine M`, rerunning the same command is safe: with the server stopped, M is now empty.
- After a failed `erase --fact N`, the operator has to invent a `<name>`. The obvious choice is a real machine name,
  such as their laptop, and that would irreversibly erase all of that machine's facts and events.
- This is an irreversible command, so the recovery instruction must not depend on the operator picking a name that
  owns nothing.
- Fix: make the retry the same command in every case.
  - An erase whose target is already gone still runs the purge. `erase --fact N` on a missing fact purges, then
    reports "fact N not found; purged" (or the purge error).
  - The CLI's "erased, not purged" message prints the exact command that was run, not a `<name>` placeholder.
  - This keeps the no-op-purges design the plan chose when it declined my "skip VACUUM on zeros" nit, and removes the
    footgun.

## LOW

- **L-g. Unforget could lift the permanent block on an erased row.**
  - "For every row of the same forget, restore both priors and delete the tombstone" (item 5) also matches a tombstone
    whose fact was later erased: `fact_id` NULL, `record_event_id` kept, because `erase --fact` keeps the record.
  - Deleting that tombstone lifts the permanent block that D1 and D12 accept as Owner points ("a tombstone whose fact
    was erased has no command that lifts it").
  - Fix: say `fact_id IS NOT NULL` in the unforget selection. Extend `unforget_reverses_the_whole_forget` with an
    erased copy whose tombstone stays.
- **L-h. Item 11's discovery grep misses one place that enables the feature.**
  - `grep -rn 'me-index/archive' crates` finds the facade's forwarding (`crates/memory-engine/Cargo.toml:235`).
  - It misses `crates/me-archive/Cargo.toml:36`: `me-index = { path = "../me-index", features = ["archive"] }`.
  - Once the feature is removed from `me-index`, Cargo rejects that dependency line when it resolves the workspace.
    Every cargo command then fails, so it is caught at once.
  - Fix: name that line in item 11's Files and drop `features = ["archive"]` there.
- **L-i. D10 should say "any edge state" when it finds orphans.**
  - The orphan rule follows `supersedes` and `contradicts` edges from a taken fact.
  - A pruned successor has its edges expired by the decay prune: `prune_atomic` cascades edge expiry in its
    transaction (`me-forget/src/policy.rs:248-251`). An active-edge helper would therefore miss its predecessor.
  - Fix: state "edges in any state", and compute orphans before the incident edges are deleted.

## Checked and correct in the delta

- **N1:** `add_facts_batch_partial` is in me-ingest (L3), which reaches the port through `ctx.storage`, so
  `is_forgotten` is available there without a layer violation. Blocked entries stay out of the embed call. The atomic
  `add_facts_batch` refusing as a whole matches its documented all-or-nothing contract (`me-ingest/src/lib.rs:264`).
- **N3, busy-checkpoint test:** with a read transaction held open on another connection, `VACUUM` still succeeds as
  the one WAL writer, and `wal_checkpoint(TRUNCATE)` waits out `busy_timeout = 5000` (`pragmas.rs:13`) and then
  reports `busy = 1`. The test is therefore achievable; it takes about 5 s. In the CLI, `close()` deletes the WAL in
  any case.
- **N4:** a required `record: i64` is sound, because the `record_event_id` foreign key catches a fabricated id.
  Conformance tests can seed the event through the port. The D13 unforget record (`fact_id`, `forget_record`) still
  names every restored row through the forget's record.
- **D13:** `copies` and `orphaned` counts keep `linked` as the record's only list, and that list is the #1020 link.

---

<!-- round 4 -->

# Review r4: feat(forgetting) forget as a reversible retraction (#1023)

Altitude report SHA-256: 186b3324f71e419edcbd68f91366321cc55d276d51e9678d3932ba4b2df24748

This round reviews the delta only, read-only. The plan reviewed has SHA-256
`756ffce9c47c3256a32cf8403d72323667c1e773f0661028eea794b233ff52cc`, which matches the altitude report's header. The
code is the same checkout at 7a22c07.

**Verdict: LGTM. 0 BLOCKER, 0 HIGH, 0 MEDIUM, 0 LOW.** The four round-3 findings are resolved, and the delta adds
nothing new.

## Altitude

I agree with **proceed**. The delta is 10 lines changed in place, each one a round-3 fix, with no transcription. The
plan has 326 lines and 58,970 bytes, which matches the report. The round-1 size override still holds.

## Round-3 findings: disposition

| # | Status | Where |
|---|---|---|
| N5 dangerous purge retry | Resolved | The retry is now the same command that ran. D11 says so. In item 10, the port returns an empty report for a missing fact, still purges and appends no record. In item 14, the CLI prints "fact N not found; purged" or the purge error, and "run this same command again" with the exact command. A test covers the path (`erase_fact_of_a_missing_fact_exits_1_and_purges`). |
| L-g unforget lifting an erased copy's block | Resolved | Item 5 restores only rows whose fact still exists (`fact_id IS NOT NULL`). The conformance case now asserts that an erased copy's tombstone stays. |
| L-h missed feature enable | Resolved | Item 11 now names `crates/me-archive/Cargo.toml` and `crates/memory-engine/Cargo.toml`. Its search command, `grep -rnE 'me-index/archive\|me-index.*features' crates`, matches both `me-archive/Cargo.toml:36` and `memory-engine/Cargo.toml:235`. |
| L-i edges in any state | Resolved | D10 now reads orphans over supersedes and contradicts edges "in any state, read before the incident edges are deleted". That covers successors whose edges the decay prune expired (`me-forget/src/policy.rs:248-251`) and the conflict cascade (`sqlite/graph.rs:925`). |

## Consistency of the delta

The switch from `NotFound` to an empty report is applied everywhere it appears:
- the port contract (item 10);
- the facade's `# Errors` list, which drops `NotFound` (item 11);
- the conformance case, renamed `erase_of_a_missing_fact_deletes_nothing`;
- the CLI's not-found message (item 14);
- D11.

No stale `NotFound` wording remains for erase. The CLI can tell a missing fact apart reliably: an existing `--fact N`
target always erases at least one fact, so an empty `erased_facts` means N was not there.

One optional note, not a finding. A retry that succeeds ("not found; purged") and one that fails ("erased, not
purged") both exit 1. A script can only tell them apart from the message or from `purged` in the JSON output. If
scripts are expected to loop on the retry, a distinct exit code for "purge failed" would help.

---

## Resolution

Round 1 (0 BLOCKER, 4 HIGH, 7 MEDIUM, 12 LOW), all addressed in the round-2 candidate:
- [HIGH] H1 `remove_node` archive-gated → item 11 ungates it, removes the then-empty `me-index` `archive` feature, and adds `cargo check -p memory-engine` to its Green.
- [HIGH] H2 sequence → the facade's forget/unforget/explain moved to item 6, before the gates (7, 8), archive/export (9) and erase (10, 11).
- [HIGH] H3 purge → D11: FTS `optimize` in the transaction, post-commit `VACUUM` + `wal_checkpoint(TRUNCATE)`, sidecar removed; the sentinel test forgets and recalls before erasing, with a stemmer-stable word.
- [HIGH] H4 guarantee scope → D11 scopes it to the rows erase deletes; `--fact` takes the promoting activity; a kept source event is named in `linked`; #1025 sibling line (Owner point).
- [MEDIUM] M1 identical twins → D5: forget retracts active identical copies in the Homes (Owner point).
- [MEDIUM] M2 orphaned sources → D10 reports `orphaned`, not restored (Owner point).
- [MEDIUM] M3 `--machine` scope → D10 takes the bucket's activities and checkpoints; user-level stamped facts marked Owner.
- [MEDIUM] M4 dead hard deletes → item 2 drops them.
- [MEDIUM] M5 ungated batch/conflict → gated (D2/D3), `insert_asserted` has one caller (Owner point).
- [MEDIUM] M6 conformance feasibility → the raw-SQL checks moved to SQLite-only tests.
- [MEDIUM] M7 portable paths → sibling line.
- [LOW] L1–L12 → D3 `insert_fact` row; HNSW wording; D4 `active`; item 8 comment; D7 key; item 9 snapshot validation; D15 DNS label; D13 size refusal; item 3 archival unlink fix; D13 wording; Documentation gaps; D17 `archive` guard, D9 `Vec`, `include_archives` residual line.

Round 2 (0 BLOCKER, 1 HIGH, 3 MEDIUM, 6 LOW), all addressed in the round-3 candidate:
- [HIGH] N1 partial batch → per-entry `is_forgotten` in `add_facts_batch_partial` before embedding; `batch-ingest` on the partial row; facade and CLI tests.
- [MEDIUM] N2 → `allow-invalid = true` on the `archive` clippy entry (item 12's clippy run confirms it).
- [MEDIUM] N3 → post-commit purge carve-out: `purged`/`purge_error`, checkpoint `busy` checked, CLI exit 1, a retry path. The "skip VACUUM on zeros" nit is declined: the empty erase is the retry path.
- [MEDIUM] N4 → unforget reverses the whole forget (rows sharing `record_event_id`; `record` required); `EraseReport::copies`.
- [LOW] L-a..L-f → orphans widened; copies matched on content; `# Errors` docs; checkpoint caveat; `me-index` feature removed; the record carries counts.

Round 3 (0 BLOCKER, 0 HIGH, 1 MEDIUM, 3 LOW), all addressed in the round-4 candidate:
- [MEDIUM] N5 retry footgun → the retry is the same command; an erase whose target is gone still purges; the CLI prints the exact command.
- [LOW] L-g → unforget selects `fact_id IS NOT NULL`, so an erased copy's block stays.
- [LOW] L-h → item 11 names `crates/me-archive/Cargo.toml`'s feature enable.
- [LOW] L-i → orphans read over edges in any state, before deletion.

Round 4: LGTM, no finding. Its optional point (a distinct exit code for "purge failed again") is declined: the JSON `purged` field already tells a script apart, and no consumer scripts retries today.
