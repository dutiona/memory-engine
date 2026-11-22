Plan SHA-256: c264349119dc7facc565398b83cc7bd2306c43cef8899dfb5a43aeae1e456299

# Altitude report: 2026-11-22-1126-backfill-identity

## Measurements

- Plan: `docs/plans/2026-11-22-1126-backfill-identity.md`, 503 physical lines, 49,858 bytes (`wc -l -c`).
- Top-level implementation tasks: 18 (items 0–17), counted with `grep -c '^- \[ \] \*\*'`; none sits inside a fence.
- Fenced code: one block (lines 480–498), 17 body lines, 1,061 bytes: the Verification command list (commands, not
  a canonical file, validator or generated payload). No other literal or data volume; inline code spans are
  identifiers, signatures, error texts and commands.
- Named target files: about 55 repository files across 11 crates plus 6 docs pages (crates: me-types, me-storage,
  me-backend-sqlite, me-backend-postgres, me-archive, memory-engine, memory-engine-embed, memory-engine-mcp,
  memory-engine-cli; workspace `tests/`). Measured existing surface of the hottest files (`wc -l` at c103cfa):
  `engine/reconstruct.rs` 991, `store/fact_vectors.rs` 851, `store/embedding_meta.rs` ~330, `engine/restore.rs` 192,
  `me-archive/src/search.rs` 756, `memory-engine-mcp/src/config.rs` ~500, `embedding_args.rs` ~350.
- Estimate (labelled, uncertain): new or changed product code ~700–1,100 lines and tests ~1,000–1,600 lines; deletions
  ~150 lines (the two dimension probes and their tests, the duplicated `ensure_match`, the restore probe, the
  post-open check). The plan is shorter than the code it assigns.

## Inputs

Required fallback/provenance inputs: none. The plan is a first candidate (no predecessor, no regeneration, no design
split). The research context and source plans it cites are background read by the drafters, not inputs this report
binds; reviewers verify the plan against the code at the worktree's HEAD.

## Revision 2

Round-1 findings added rules and tests (dimension-only refusal, the archive identity read in one
transaction, the backfill cursor, the CLI key-setting checks); growth of 39 lines, all assigned work, no new literal
or duplicated structure.

## Revision 3

Round 2: one red command's filters moved after `--`, and Decision 8 records why a promote between the
archive's read and its commit is accepted. Five lines of rationale; no new structure.

## Calibration and disposition

The plan coordinates eight independent issues in one PR, each with its own acceptance, plus five boy-scout commits
and one refactor; its length comes from that breadth (one item per commit, each naming files, functions, red and
green commands), not from transcribed code, duplicated validators, frozen manifests or mutation catalogues. No item
carries a complete product file. The one cross-cutting structure (the shared reader and the open-seam check) is
assigned once (items 3–5) and referenced by later items rather than restated.

Disposition: **proceed**.
