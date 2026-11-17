# External review: Vibe (Mistral Large 4, headless one-shot, agent ask, read-only review copy)

Codex hit its usage limit at round 3; per the fleet rule, Vibe stands in as the external voice. Review-tree paths are written as `<review-tree>`.

## Round 1 (full review of plan SHA-256 f177c17c4e24db58abde3127000e7b4e69acd5ccaa8093218bf57851a52776c6), verbatim

Review complete. I read the plan (`.review/plan.md`), the altitude report and its provenance input, both binding ADRs in full, and the code the plan touches: `crates/memory-engine-mcp` (home/auth/server/tools/tests), `crates/me-storage` (graph/event_log/backend), `crates/me-index` (scope tree), `crates/me-backend-sqlite` (store/scopes, store/facts, sqlite/graph, sqlite/event_log, search), `crates/me-backend-postgres`, `crates/me-test-support`, `crates/memory-engine` (engine/mod, ingest, inspect), `crates/me-types` (limits, scope, events), `fuzz/`, and `plugins/memory-engine/bin/me-hook`.

**Altitude / duplication (assessed first).** The plan is 590 lines against an estimated ~2,400 lines of new/changed code; its single fenced block is the Verification command list, not a product artifact. The normative "Key rules, version 1" section (~55 lines) is not duplication: those rules are persisted in the event log under `rules: 1` and must be fixed once before implementation, and the unit tables test them. Test-name lists are the TDD scaffolding. I agree with the report's disposition (proceed); no split or extraction needed. Provenance note: `.review/altitude.md` records Plan SHA-256 `f177c17c…` and plan-context SHA-256 `99a92955…`; the altitude report's own digest is not recorded inside the file and cannot be recomputed in this read-only review, so the line below is included verbatim as instructed, unverified.

Altitude report SHA-256: ae7969dc465f01ebbdd394188fd88cbcc7c6ce28f1d76b1c9788832acdb28869

**Contract and code validation (what I checked and found consistent).** Decoding rules 1–10 match ADR-0020 §3 exactly (4,096-byte cap, raw bytes outside 0x21–0x7E, single-pass `%HH` in either case, UTF-8, C0/DEL/C1 control, dev root plain-ASCII sent undecoded, bad fact dropped and flagged — never the call). Rule 10's two-step userinfo strip matches the plugin's `strip_userinfo` in `plugins/memory-engine/bin/me-hook` and adds the `token@host:a://b` case the plugin keeps (verified against the hook's `*://*` arm). Key rules R1–R6/P1–P4 match ADR-0020 §7 and ADR-0021 §8, including upstream-over-origin, `file:`/local-path remotes ignored as inputs, the 256-byte final-key rejection with no fall-through (D6 matches ADR-0021 §8's "only a final key ADR-0020 rejects"), and path-key/remote-key non-collision; the label encoder is unchanged from `home.rs`. Scope paths `user:<u>/project:<label>` and `unscoped:<m>` match ADR-0020 §7. Fail-closed reads verified in code: `ScopeQuery::ExactSet` filters missing paths and returns `None` on empty (`me-index/src/scope/tree.rs:193`), and `me-query` turns `None` into no results; both search channels filter on `facts.scope_id` in SQL (`search/fts.rs`, `search/vector.rs`, `search/ann.rs`), so a scope move needs no HNSW notification. Only `SqliteBackend` implements `FactGraph`/`EventLog`, so the two new port methods ripple safely and the Postgres conformance arm stays `#[ignore]`d as the plan claims; `all_behaviors_into!` and the `event.rs`/`graph.rs`/`atomic.rs` modules exist as named. The facade pieces the plan builds on all exist: `ensure_open`, `ensure_writable`, `cache_scope_chain`, `resolve_scope_path`, `ensure_scope_path`, `me_types::limits::check_json_size`, `MAX_SEGMENT_LEN = 256`, `EmbeddingFingerprint::new(model, provider, dim)`. `dispatch`'s only callers are `server.rs` and the fuzz target, both updated by the plan; axum 0.8 justifies `http = "1"` in `fuzz/Cargo.toml`. Every `lean_surface.rs` test the plan names exists; the docs drift targets (mcp-server.md:6/185/193, README:84/161, AGENTS.md:93, CLAUDE.md:155, GEMINI.md:92) and the stale `rs:N–M` comments item 10 cites are real. Documentation, Testing and Verification sections are all present, and the Verification block covers the full AGENTS.md matrix unpiped plus MSRV and Sphinx.

**Findings.**

[Vibe Review] [MEDIUM] Decisions D2, items 7–8: the default makes `set_level(id, "project")` mint the session's project, which deviates from ADR-0021 §8's binding "Only write paths mint a project: the event endpoint and `remember`" until the item-7 amendment lands. The plan flags this (D2\*), gates it on Michaël's approval, records the hand-off to infra#37, and specifies the fallback (item 8's `error: project <key> holds no memory yet…`). Not a silent violation, but the ADR text and the code must not diverge at merge: item 7 must land in the same PR or D2 must be declined.

[Vibe Review] [LOW] Decision D3, item 8: the move receipt `stored <id> in <home>, visible to all agents` is a literal reading of ADR-0020 §8's "the same receipt as `remember`"; the alternative (`moved …`) is noted but unresolved. Owner-gated (D3\*), low risk either way.

[Vibe Review] [LOW] Item 4, test `every_record_keeps_the_project_facts_key_and_rules`: it asserts every record — including the valid `remember`'s — sits under `unscoped:laptop`, which item 6 (D1) then moves to the project. The parenthetical says item 6 updates it; the implementer must not treat the item-4 assertion as final or the item-6 red will be misread.

[Vibe Review] [LOW] Item 10: the drift check `rg -n 'rs:[0-9]+[–-][0-9]+'` misses comma-separated refs — `crates/me-storage/src/schema.rs:61` cites `engine/mod.rs:649,659` and is outside item 10's file list, so the check can pass with a stale ref remaining. Widen the regex or the file list.

[Vibe Review] [LOW] Item 8, `set_level`'s record: it is filed under the bucket inside `move_fact`'s transaction, which ensures only the `to` path; if the bucket node does not exist yet (e.g. the only prior write was a `user`-level fact seeded by another writer), the record insert FK-fails and the call errors fail-closed. In the MCP flow every prior call creates the bucket, so this is unreachable in practice — worth one comment or test, not a change.

[Vibe Review] [LOW] Security focus items are all covered by design and tests: userinfo is stripped before anything is stored (`no_file_of_the_store_holds_userinfo` scans db/-wal/-shm/snapshot), no fail-open scope read exists (`scope_exact_set` fail-closed; Verification greps `scope: None` in the MCP crate; `remember` never passes `None`), no read mints a project (D1 plus `a_read_never_mints_a_project`, `a_remember_whose_record_fails_mints_nothing`), and no recorded move can fail to have happened (move + record in one `unchecked_transaction`; refusal records carry `error`, never `from`/`to`; covered by `a_move_whose_record_fails_does_not_happen`, `a_record_never_names_a_move_that_failed`, `concurrent_moves_leave_a_replayable_log`).

[Vibe Review] LGTM — no [BLOCKER] and no [HIGH]; the two MEDIUM/LOW decision points (D2, D3) are explicitly owner-gated by the plan itself with fallbacks, and every code-level assumption I could check against the snapshot (port implementors, conformance registry, facade helpers, fuzz seam, test names, doc targets, plugin stripping rule, fail-closed query resolution) held.

REVIEW COMPLETE

## Round 2 (delta to plan SHA-256 e9eaeb83f5fc84be8b34adc55f4e0ce2229d9dd341e02bd7e168e3c533e678eb, from f177c17c), verbatim

Round-2 review. I read `.review/plan-delta-r5.diff`, `.review/altitude-delta-r5.diff`, the new `.review/altitude.md` and the new `.review/plan.md`, and re-checked the delta's new claims against the code (`me-ingest/src/lib.rs`, `me-test-support/src/factory.rs`, `me-storage/src/ctx.rs`, line-ref patterns in the item-10 target dirs).

**Altitude disposition first.** The new altitude report (plan SHA-256 `e9eaeb83…`, 602 lines / 55,222 bytes vs an estimated ~2,450 lines of code) keeps disposition **proceed**: still one fenced block (the Verification command list only), no new embedded artifact, revisions 4–5 documented with predecessors and deltas. No duplication or altitude concern; the normative key-rules section remains justified as log-persisted rules version 1. Provenance note: the altitude report's own digest is not recorded inside the file (it records the plan and plan-context digests), and this review is read-only, so the line below is included verbatim as instructed, unverified.

Altitude report SHA-256: 52338efe43e49d3c33ed882fddb7ce20363e8c97e9393c69a4ab4c0cd62e72f9

**Delta validation.** All five changelog items check out against the code:

- `record_at` on `move_fact_atomic` (both `to` and `record_at` ensured in the one `unchecked_transaction`; `ScopedWrite.chain` now covers every named path) directly answers my round-1 LOW on the bucket FK edge — D1 is reworded ("the bucket, which its move creates when absent, with the target node, in the move's own transaction") and the new test `set_level_right_after_a_machines_first_remember` covers exactly the fresh-store case I flagged. Item 8's call site is consistent: `engine.move_fact(id, fact.scope_id, &target.path(user), &bucket.path(user), &call_record(…))` matches the new 6-arg facade signature, and item 4's `record()` still goes through `engine.ingest_at`.
- Item 7's "lands in the same PR as item 8 or is dropped" answers my round-1 MEDIUM: the ADR-0021 §8 text and the minting behavior can no longer diverge at merge.
- Moving `ingest_at`/`move_fact` into `me-ingest` beside `ingest` is sound: `me_ingest::ingest` already runs `ensure_open`, `ensure_writable`, `check_json_size(&event.payload, "event payload")` (`me-ingest/src/lib.rs:27-35`), `MemoryCtx` exposes both gates (`me-storage/src/ctx.rs:47,59`), and me-ingest already imports `check_json_size`, `Utc`, and `ScopeTree` — the "one home for event-append validation" claim holds, and the facade delegates in `engine/ingest.rs` match the existing one-line-delegate pattern.
- `ConformanceBackend::refuse_event_inserts` beside `break_facts_table` fits the factory pattern exactly (test-util-gated default via `raw_exec`, PgFactory inherits the default while its arm stays `#[ignore]`d), so no conformance body holds backend SQL.
- `mod scope_writes` in `engine/tests.rs` plus "none reports 0 tests" addresses the dark-test trap from AGENTS.md.
- Item 10's widened file list (adds `me-storage/src/schema.rs`) and regex `rs:[0-9]+([–,-][0-9]+)+` catch the comma-separated `engine/mod.rs:649,659` I flagged.

**Findings.**

[Vibe Review] [LOW] Item 10 residual: the widened regex still requires a range/comma after the first line number, so single-line references slip through; one remains in scope — `crates/me-backend-sqlite/src/sqlite/cold_storage.rs:73` cites `archive.rs:239` (a body that moved in Wave 2). Either widen the check to `rs:[0-9]+` or fix that line in the same commit.

[Vibe Review] [LOW] Provenance: the altitude-report SHA-256 line above is not self-recorded in `.review/altitude.md` and cannot be recomputed in this read-only review; included verbatim per instructions.

[Vibe Review] LGTM — the delta resolves my round-1 MEDIUM (item 7 same-PR rule) and both LOWs (bucket FK edge via `record_at` + the new fresh-store test; comma-separated line refs via schema.rs and the widened regex), the clean-slate MEDIUM is answered by `record_at` creating both paths in the move's transaction, and every new code claim I could check (me-ingest validation home, MemoryCtx gates, factory fault pattern, signature consistency across items 3/4/8, altitude measurements) held. No [BLOCKER], [HIGH], or [MEDIUM] remains; the one residual LOW is a one-line regex widening in item 10.

REVIEW COMPLETE

## Round 3 (delta to plan SHA-256 dd796ce3cabe0e3b24f7bc1bcfae4989b96dbad6b29417f2b7e1d19181628400, from e9eaeb83), verbatim

Round-3 confirmation. I read `.review/plan-delta-r6.diff` and `.review/altitude-delta-r6.diff`, spot-checked the new `.review/plan.md` and `.review/altitude.md`, and re-ran the enumeration of line references in the item-10 target directories against the new check.

**Altitude disposition first.** The new report (plan SHA-256 `dd796ce3…`, 605 lines / 55,550 bytes vs an estimated ~2,450 lines of code) keeps disposition **proceed**: still exactly one fenced block (the Verification command list), no new embedded artifact, revision 6 documented with its predecessor and delta, and the revision-4 pointer now correctly names `.review/plan-delta-r5.diff`. No altitude or duplication concern. Provenance note, same as prior rounds: the altitude report's own digest is not recorded inside the file (it records the plan and plan-context digests) and cannot be recomputed in this read-only review, so the line below is included verbatim as instructed, unverified.

Altitude report SHA-256: aee3488a8f2ed9a949af8aca4d2341659415012ea57df9c1d3c36cc92a171595

**The four LOW fixes, confirmed.**

[Vibe Review] [LOW → fixed] Item 10's check is now `rg -n '\.rs:[0-9]+' crates/me-storage/src crates/me-backend-sqlite/src/sqlite` (plan.md:501). I enumerated every `.rs:<digit>` match in those two directories (21 hits, including the single-line `archive.rs:239` at `sqlite/cold_storage.rs:73` that my round-2 LOW flagged) and confirmed no other `rs:` form (space-separated, non-digit) exists that the regex would miss — the check is now total over both directories.

[Vibe Review] [LOW → fixed] Item 3's file list now includes `crates/memory-engine/src/engine/mod.rs` for the "single home" comment of the private `cache_scope_chain` wrapper (plan.md:236); that wrapper and its `#973` "single home" doc do live in `engine/mod.rs`, so the pointer is correct.

[Vibe Review] [LOW → fixed] `refuse_event_inserts` is a required `ConformanceBackend` method with no default body (a trigger is not portable SQL), SQLite factory installs the `BEFORE INSERT ON events` trigger, and `PgFactory` gets `todo!("#635")` (plan.md:255-258) — consistent with `factory.rs`'s existing pattern, where PgFactory's `todo!()`s are filled by #635. (The `#[cfg(feature = "test-util")]` gate is implied by "sits beside `break_facts_table`", which carries it.)

[Vibe Review] [LOW → fixed] The altitude report's revision-4 entry now points at `.review/plan-delta-r5.diff` (altitude.md:73), and the revision-6 entry records the four fixes with predecessor `e9eaeb83…`; measurements updated (605 lines, fenced block still lines 583-601, 1,040 bytes).

[Vibe Review] [LOW] Standing provenance note: the altitude-report SHA-256 line above is not self-recorded in `.review/altitude.md` and is unverifiable in this read-only review; included verbatim per instructions.

[Vibe Review] LGTM — all four round-3 LOW fixes are present in the delta and verified against the code and the report; no [BLOCKER], [HIGH], or [MEDIUM] findings remain open from any round. The plan's altitude is unchanged in role (602→605 lines against ~2,450 estimated), its disposition stays proceed, and the binding-contract checks from round 1 (ADR-0020 §3/§7/§8, ADR-0021 §5/§8/§12, fail-closed reads, userinfo stripping, minting rules, atomic move+record) still hold in the revised text.

REVIEW COMPLETE

## Resolution

Converged: LGTM at round 3, no open finding. Round 1 [MEDIUM] D2 → item 7 lands in the same PR as item 8 or is dropped;
[LOW] item 10's comma-separated reference → `schema.rs` added; [LOW] `set_level`'s bucket → `record_at` in
`move_fact_atomic` (already in revision 4); [LOW] D3 and the item-4 note → owner-gated / already handled. Round 2
[LOW] single-line references → item 10's check is `\.rs:[0-9]+`. The standing provenance note (the report's own
digest is not inside the report) is by design: the digest binds the report from outside, and each reviewer quotes it.
