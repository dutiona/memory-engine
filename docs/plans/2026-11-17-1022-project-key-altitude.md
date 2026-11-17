Plan SHA-256: dd796ce3cabe0e3b24f7bc1bcfae4989b96dbad6b29417f2b7e1d19181628400

# Altitude checkpoint: docs/plans/2026-11-17-1022-project-key.md

## Measurements

- Plan path: `docs/plans/2026-11-17-1022-project-key.md`; 605 physical lines, 55,550 bytes (`wc -l -c`).
- Top-level implementation tasks: 17 (`grep -c '^- \[ \] '`, items 0 to 16; no task line sits inside a fence).
- Fenced code: one block (lines 583-601), 17 body lines, 1,040 bytes: the Verification command list, a command list, not
  a canonical file. Other literal volume: inline exact strings (receipts, hint lines, tool descriptions, one expected
  record JSON object in item 4, about 300 bytes) and test-name lists; none is a complete product file, validator,
  generator, manifest or transcript.
- Named target files: `crates/memory-engine-mcp/src/{project.rs (new), home.rs, auth.rs, server.rs, lib.rs,
  tools/mod.rs, tools/recall.rs, tools/remember.rs, tools/placement.rs (new), tools/set_level.rs (new)}`;
  `crates/memory-engine-mcp/tests/{common/mod.rs, lean_surface.rs, project_scope.rs (new)}`;
  `crates/me-storage/src/{graph.rs, event_log.rs, lib.rs}`; `crates/me-index/src/scope/mod.rs`; `crates/me-backend-sqlite/src/{store/facts.rs, sqlite/graph.rs, sqlite/event_log.rs}`; `crates/me-ingest/src/lib.rs`;
  `crates/me-test-support/src/{factory.rs, graph.rs, event.rs, atomic.rs, lib.rs}`;
  `crates/memory-engine/src/engine/{ingest.rs, mod.rs, tests.rs}`;
  `fuzz/{fuzz_targets/dispatch.rs, Cargo.toml}`; docs `docs/reference/mcp-server.md`, `README.md`, `AGENTS.md`,
  `CLAUDE.md`, `GEMINI.md`, `docs/design/adr/0021-memory-scope-model.md` (one sentence, gated).
- Measured existing surface: `crates/memory-engine-mcp/src` 2,038 lines (`wc -l`), `tests/lean_surface.rs` 831,
  `tests/common/mod.rs` 479.
- Estimated new or changed surface (estimate, uncertainty about ±40%): `project.rs` about 350 lines of code plus 300 of
  unit tests; `home.rs`, `placement.rs`, `set_level.rs`, `tools/mod.rs`, `server.rs` about 350 changed or new; port,
  SQLite body, facade and conformance bodies about 300; `tests/project_scope.rs` about 900; fuzz about 50; docs about
  150. Roughly 2,400 lines in all. No generated render is counted.

## Required fallback / provenance inputs

- `plan-context.md`, the shared planning context the lens drafts were given (issue text, contract pointers, owner
  rulings, verified code facts). Source: the worker's private artifact scratch directory (outside the repository).
  Review copy: `.review/inputs/plan-context.md`. SHA-256:
  `99a92955987c75409a58185782875cf8c813a9ba3392a8d9330ab50fd5f8c814`.
- The binding contracts are committed at the reviewed HEAD and need no copy:
  `docs/design/adr/0020-harness-neutral-lifecycle-api.md`, `docs/design/adr/0021-memory-scope-model.md`.
- Not inputs: the three lens drafts (ephemeral, gitignored, superseded by the synthesis).

## Calibration and disposition

The plan assigns coordinated work across four layers (MCP tools, storage port, SQLite backend, facade) plus docs, and
its "Key rules, version 1" section is normative for two items: it fixes the decoding, normalization and path rules
that are the ticket's acceptance and that are stored permanently in the log under rules version 1, so they must be
stated once, exactly, before implementation. That section is about 55 lines and is not a second copy of a product
file: the code will implement it and the unit tables test it. The test-name lists are the plan-structure's required
"failing test" per item. No validator, generator, manifest or near-copy successor is embedded. The plan is shorter
than the code it describes (605 lines against an estimated 2,450).

Disposition: **proceed**. No design split, amendment or extraction is needed.

## Revision 2 (after round 1)

Predecessor: plan SHA-256 `576e640aa93bd683194e81631a40ba7c5e54c462098daf1120dde8e113ef6e69` (538 lines), retained in
the review tree as `.review/last-reviewed-plan.md`. The delta (`.review/plan-delta-r2.diff`) answers the round-1
findings of Codex and the clean-slate subagent: D1 (a valid `remember`'s record filed under its target), D4 and item 3
(liveness at `now`; the port returns the scope chain so nothing fallible runs after the commit), D5 and D7 wording, the
flaw name `not_plain_absolute`, item 2's caller list, item 0's public-repository scan, item 9's drift check, item 10's
wider scope, new tests (a failed fact write, future invalidation, C5's bare `x`, `.GIT`), and the `h` function. It adds
32 lines, no fenced literal and no new embedded artifact; the role is unchanged. Disposition: **proceed**.

## Revision 3 (after round 2)

Predecessor: plan SHA-256 `223edc02864136079c6f672bc4b40b6be7919d1feed2df95b39bd7ebefc34f90` (570 lines), retained as
`.review/last-reviewed-plan.md`; delta `.review/plan-delta-r3.diff`. It answers Codex's round-2 HIGH (node and record
now commit together: a second atomic port method, `EventLog::insert_event_at`, and the facade's `ingest_at`, which
#1025's event route will reuse) and the MEDIUM both reviewers raised (item 4's test no longer expects item 6's
behaviour), plus the clean-slate LOWs (rule 10's `:` guard and its test row, the body count, the `insert_scope_chain`
helper beside `cache_scope_chain`, item 10's commit subject, item 13's hand-offs and rescan, the D1 consequence in the
docs). About 20 lines added, no fenced literal, no embedded artifact; the role is unchanged. Disposition: **proceed**.

## Revision 4 (after round 3)

Predecessor: plan SHA-256 `f177c17c4e24db58abde3127000e7b4e69acd5ccaa8093218bf57851a52776c6` (590 lines), retained as
`.review/last-reviewed-plan.md`; its delta is part of `.review/plan-delta-r5.diff` (revision 5 below). It answers the clean-slate round-3 MEDIUM
(`set_level`'s record had no scope path: `move_fact_atomic` now takes `record_at` and creates both paths in the move's
own transaction; new test `set_level_right_after_a_machines_first_remember`) and its LOWs (the facade tests in
`mod scope_writes`, the `refuse_event_inserts` factory fault in place of SQL in a body, the counts, D1's "an event or a
memory", `ingest_at` and `move_fact` moved into `me-ingest` beside `ingest`). Item 3 is rewritten in place; about 9
lines added, no fenced literal, no embedded artifact; the role is unchanged. Disposition: **proceed**.

## Revision 5 (after the Vibe one-shot on revision 3)

Predecessor for the clean-slate delta: revision 3, `f177c17c4e24db58abde3127000e7b4e69acd5ccaa8093218bf57851a52776c6`
(the last revision every active reviewer read), retained as `.review/last-reviewed-plan.md`; delta
`.review/plan-delta-r5.diff` (revisions 4 and 5 together). Revision 5 adds Vibe's two plan fixes: item 7 lands in the
same PR as item 8 or is dropped, and item 10's check also catches comma-separated line references
(`crates/me-storage/src/schema.rs`). Three lines added; the role is unchanged. Disposition: **proceed**.

## Revision 6 (after round 4: clean-slate LGTM, Vibe LGTM, LOWs only)

Predecessor: revision 5, `e9eaeb83f5fc84be8b34adc55f4e0ce2229d9dd341e02bd7e168e3c533e678eb`, retained as
`.review/last-reviewed-plan.md`; delta `.review/plan-delta-r6.diff`. Four LOW fixes: item 3 lists `engine/mod.rs` for
the "single home" comment; `refuse_event_inserts` is a required factory method (`PgFactory`: `todo!("#635")`); item
10's check catches single-line references too; revision 4 above points at the r5 diff. A few lines; role unchanged.
Disposition: **proceed**.
