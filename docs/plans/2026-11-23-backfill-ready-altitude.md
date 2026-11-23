Plan SHA-256: 578afdc576bb0c1cb6f2b730141f58d639de6cf1f44bd34ad93b16173c157aec

# Altitude checkpoint: docs/plans/2026-11-23-backfill-ready.md

## Measurements

- Plan: 292 physical lines, 27,478 bytes (`wc -l -c`); revision 3, after round 2.
- Top-level implementation tasks: 6 (`- [ ]` lines outside fences, counted with `awk` toggling on lines starting
  with three backticks).
- Fenced literal volume: 1 block, 18 lines, 988 bytes: the repository's verification command list (AGENTS.md's
  matrix plus the new fetch row, the two MSRV rows and a `git status` check). A command list, not a canonical file, validator or payload.
- No other literal/data volume: no full source file, no generated payload, no manifest, no mutant catalogue. Inline
  code spans are single signatures, one comment line and short doc sentences.

## Target surface

- Existing files the plan edits (measured lines): `crates/memory-engine/src/engine/backfill.rs` 615,
  `tests/backfill_import_test.rs` 1048, `crates/memory-engine/src/bootstrap/parse.rs` 913,
  `crates/memory-engine/src/bootstrap/filter.rs` 1063,
  `crates/memory-engine-mcp/src/adapters/claude_code/transcript.rs` 1122, `docs/reference/api.md` 328,
  `fuzz/fuzz_targets/backfill_upload.rs` 167; plus small edits to `bootstrap/{mod,outcome}.rs`,
  `crates/memory-engine/Cargo.toml`, `docs/reference/mcp-server.md`, `docs/backfill.md`, `README.md`,
  `fuzz/{.gitignore,Cargo.toml}`, `fuzz/seeds/backfill_upload/README.md`, `.github/workflows/ci.yml`, the four gate
  matrices, `justfile`.
- Estimates (labelled estimates, not measurements): #1153 about 60-90 changed lines (one hashed member removed, a
  helper's second receipt, one new test, one fuzz assertion, docs); #1154 about 120-180 (one predicate with doctest,
  three call sites, one six-shape test with an embedder stub); #1092 about 120-180 (a `syn`-based parity test with three guards, two dev-dependency lines and two `Cargo.lock`
  edges, plus about 55 rewritten table cells); #1093 about 25 hand-written lines
  plus the generated `fuzz/Cargo.lock` (about 3,000 lines, generated, excluded from the product estimate) and one
  generated seed file. Uncertainty: the count of drifted api.md rows is known only from the parity test's red run.

## Required fallback/provenance inputs

None. The plan cites repository files at main 9ed349a, which the reviewers read in the review tree. The shared draft
context and the three lens drafts are ephemeral working artifacts, not inputs the plan depends on.

## Calibration and disposition

The plan assigns four small defects and two doc fixes across interacting subsystems (engine preview, store outcomes,
two transcript readers, docs, fuzz gate). Its detail is decision records with cited evidence, a failing test and
green command per item, and the exact rule or signature per change; it does not transcribe code. The plan (292 lines)
is shorter than the code it governs (estimated 350-500 changed hand-written lines). Revision 2 grew by 26 lines,
  all of them resolutions of round-1 findings (test fixture fields, the predicate's one widening, the parity check's
  scoping, the deploy note, two verification rows); no new item. Revision 3 adds 7 lines, all round-2 resolutions (the builder rows' qualification,
  table header/separator skipping, unparseable-file failure, owner naming); no new item. No duplicated validator or
generator: the parity test's rules are stated once, as rules, for the implementer to write once.

Disposition: **proceed**.
