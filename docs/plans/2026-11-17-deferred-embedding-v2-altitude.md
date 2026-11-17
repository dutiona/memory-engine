Plan SHA-256: 29441a37ea5919d78a66fd5bb3710bee52de2d0a4a7d143a010cfad9aaacbdde

# Altitude checkpoint: 2026-11-17-deferred-embedding-v2 (successor)

## Regeneration

- Immutable predecessor: `docs/plans/2026-11-17-deferred-embedding.md`, SHA-256
  `248b4a410b09f0631ad4264e92ea58b0170adafbea503840136a5785af2806a9`, approved on #1079 (comment by the owner,
  2026-11-17T19:15:55Z). Its altitude report `…-altitude.md` (SHA-256 `3e4141ad…b1a`) stays as it is.
- Bounded delta, inspected before copying: the owner's answer 2 replaces truncation with chunking for every
  provider. It changes the Outcome sentence, adds decision D1 (chunk-to-vector mapping), rewrites item 5, renames
  item 10's over-length test, and updates the two test tables, one Documentation bullet and Open questions 7, 10
  and 13. Two Constraints record the owner's answers and the successor brief verbatim.
- Assembled two-file diff (`diff` predecessor successor): 174 changed lines (round 1 reviewed 142); every other line is retained byte for
  byte, including items 0–4 and 6–18, the guard design, the queue, the drain, recall and `status`.
- Divergence: Modified 1 item (5) + small edits in item 10; Dropped 0; Added 0 items. Under the 30 % successor
  boundary, but approved intent changed (truncation → chunking), so this is a complete reviewed successor, not an
  amendment, per the altitude checkpoint.

## Measurements

- Plan: `docs/plans/2026-11-17-deferred-embedding-v2.md`, 755 physical lines, 69,918 bytes (round-2 candidate;
  round 1 reviewed `4ff4ce8a…`, 729 lines).
- Top-level implementation tasks: 19 (`grep -c '^- \[ \] \*\*'`).
- Fenced literals (awk between fences, indented included): 78 lines, 5,126 bytes, 8 blocks: the v1 seven (DDL,
  signatures, status output, the command matrix) plus item 5's one `split_to_limit` signature. No complete product
  file, validator, generator or frozen manifest.
- Tables: the v1 tables plus D1's two-option table (consequences for the owner's decision).
- Target surface: as v1, plus `crates/memory-engine-embed/src/chunk.rs` (new). Estimate (labelled estimate):
  chunking and counting about 150 lines of production code and 200 of tests, replacing v1 item 5's truncation
  estimate of about 150; the rest unchanged.

## Inputs

Required provenance inputs: the predecessor plan above (in the repository working tree beside the successor).
The KB sources the port follows are cited by repository and PR (knowledge-base `embeddings.py` at PR #615, merge
`e95f0620`); no copy is required to review the plan's rules, which state the behaviour in full.

## Calibration and disposition

Same purpose and calibration as the predecessor: a cross-cutting implementation plan whose length comes from
19 items in the required shape. The successor adds one decision table and one rewritten item, no design prose.
Disposition: **proceed** (complete successor; no fallback, no override).
