Plan SHA-256: 4cff7c9059b9a3ab9c4fcd295f3fdfe6b4e657b0404b8b50432921c5220131e0

# Altitude report: 2026-11-22-1126-backfill-identity-v2 (successor)

## Measurements

- Plan: `docs/plans/2026-11-22-1126-backfill-identity-v2.md`, 583 physical lines, 59147 bytes (`wc -l -c`).
- Top-level implementation tasks: 19 (items 0–18), counted with `grep -c '^- \[ \] \*\*'`; none inside a fence.
- Fenced code: one block (lines 560–578), 17 body lines: the unchanged Verification command list. One new table
  (item 16's rebase surface, 6 rows) is data the rebase needs, not a product payload.

## Regeneration

- Predecessor: `docs/plans/2026-11-22-1126-backfill-identity.md`, SHA-256 `c264349119dc7facc565398b83cc7bd2306c43cef8899dfb5a43aeae1e456299`, approved on #1130.
- Bounded delta (`diff` of the two files, 116 changed lines): a provenance header; the Outcome sentence and
  two Constraints lines for #1131 and Michaël's confirmation of Decision 7; Decision 13 now names the filed issues;
  new Decision 14 (#1131's design); new item 15 (#1131); the rebase item renumbered 16 with A2's shared-function
  table; the PR item renumbered 17 with `Closes #1131` and the collateral line; item 18 renumbered; one Testing row
  and one Documentation clause. Retained unchanged: items 0–14, every other Decision, Testing, Verification.
- Modified + Dropped + Added is well under 30% of the predecessor; intent of every approved item is unchanged.

## Inputs

Required provenance input: the predecessor plan, copied into the review tree as `.review/predecessor-plan.md`,
SHA-256 `c264349119dc7facc565398b83cc7bd2306c43cef8899dfb5a43aeae1e456299`.

## Revision 2 of the successor

Codex's round-1 findings tightened item 15's tests (named call records, all six marks, both compatibility
directions) and stated Decision 14's compatibility limit; about 25 lines, no new structure.

## Revision 3 of the successor

The Claude stand-in's findings (Codex out of quota) corrected item 15's call-record fixture, added the second golden
and the struct literals, dropped the negative-mark check, and named the new field among the breaking changes; a
few lines each, no new structure.

## Calibration and disposition

The successor adds one collateral fix (one item, one decision) and coordination data; nothing is transcribed or
duplicated. Disposition: **proceed**.
