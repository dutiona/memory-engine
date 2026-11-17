Plan SHA-256: 756ffce9c47c3256a32cf8403d72323667c1e773f0661028eea794b233ff52cc

# Altitude report: docs/plans/2026-11-17-forget-retraction.md (round 4)

## Measurements (ordinary inspection: `wc`, `grep`, `awk`, `diff`)

- Physical lines 326; raw bytes 58,970.
- Top-level implementation tasks: 19 (`grep -c '^- \[[ x]\] '`), items 0 to 18; item 0 (worktree) is done, item 18
  groups the operational steps (parity note, follow-up hand-off, plan issue, PR, review, merge).
- Fenced literals: 3 blocks, 36 lines, 1,720 bytes (awk count between ``` fences, indented fences included): the
  10-line Mermaid state diagram (lines 19–30), the 14-line v16 DDL (lines 133–148, the schema the migration freezes),
  the 12-line AGENTS.md verification matrix (lines 304–317). No complete product file, validator, generator,
  manifest, transcript or mutant catalogue. The DDL is the one product literal, unchanged from round 1.
- Tables: the write-path table (D3, 11 rows) and the acceptance-to-test table (15 rows).

## Target surface

Unchanged from round 1 in kind (measured: `store/facts.rs` 4,274 lines, `sqlite/graph.rs` 2,246,
`sqlite/consolidation.rs` 1,143, `me-storage/src/graph.rs` 525, `engine/bootstrap.rs` 458, `schema/ddl.rs` 224,
`tools/mod.rs` 184, `me-test-support/src/lib.rs` 169, `memory-engine-cli/src/main.rs` 144), plus
`crates/me-index/src/graph/memory_graph.rs`, `store/checkpoints.rs`, `sqlite/cold_storage.rs` and
`memory-engine-mcp/src/auth.rs` added by the revision. Estimate (labelled, uncertain): new code about 1,700–2,800
lines including tests. The plan stays well under the code it describes.

## Regeneration (revision of the round-3 candidate)

- Predecessor: the round-3 candidate, SHA-256 `1e7578b3ddcb821a3075834c69f67f7de9dc0724ec418ad4dd95259281bccee6`
  (round 2 `77af1ac5…`, round 1 `037f4a87…`), retained in the review tree as `.review/last-reviewed-plan.md`.
- Delta (`diff` line count): 10 lines changed in place. Changed: D10 (orphans read over edges in any state, before
  deletion), D11 (the purge retry is the same command; an erase whose target is gone still purges), item 5 (unforget
  skips erased copies, `fact_id IS NOT NULL`), item 10 (a missing fact is an empty report), item 11 (`me-archive`'s
  feature enable named, `# Errors` list), item 14 (the CLI's not-found and not-purged messages, a retry test).
  Retained: everything else.
- Approved intent: none yet, so no successor transition applies.

## Required fallback/provenance inputs

None. The plan's Constraints quote the issue, the owner's binding comment and the contracts verbatim; ADR-0020 and
ADR-0021 are in the reviewed tree at `docs/design/adr/`.

## Calibration and disposition

Same purpose and budget as round 1: a cross-cutting single-PR plan whose detail goes to decisions, the write-path
classification, the schema and one failing test per item. The growth over round 1 (+13 lines, +12 KB) comes from review findings
that each changed a decision or added a required test, not from transcription; no rule is stated twice (gates in
D2/D3, selection in D10, purge in D11, records in D13).

Signal: 19 items and about 59 KB for one PR. Necessity as in round 1: the acceptance couples the tool, a tombstone
every write path honours, two maintenance commands and a hard delete with FK and privacy obligations. Items 1, 2, 3
and 17 are separable boy-scout commits.

Disposition: **proceed**.
