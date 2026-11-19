Plan SHA-256: 27da3da724cf0c8603096ce1844ca56f43ddebfaffd7165ecef6f110e16fe162

# Altitude checkpoint: ADR-0015 §5/§6 lockstep plan (dutiona/memory-engine#1078, dutiona/knowledge-base#607)

Revision 5 (after review round 4). Predecessor candidates: round 1
`5b442d860b3b771b28c31691efbd533b43cfcd085cada759bb5c8f2c03957bbe` (report
`2506ba2d21eda083c9ed53ca1b8aabb7d05d13bcd34b58b69d68b5c745ae185f`), round 2
`0f4b532e4ffaebb2e947e860432d80b5798feb112c522090403e3de99182e8a6` (report
`a84db723747e6f2a7f13ae98fc945cdc4c1495ea5d7b02053b15abccd5e8e207`), round 3
`f7a6ccfd3ee390620f3b05ab72ae4f64f61b25064b26922c7bfdbd9f34854ba0` (report
`366ec3030d08ee599c2aa168f0409a184e72ce4b9876ff83f555d05718429525`), round 4
`95be6d15ea6b885e2e87e09ceccfd3db644db0ea4b24f99a1dacdff2191f015b` (report
`a461be532dd33e060ec99f25cad48ff055090837d6e5e4c3305bdcb6c1e5aabd`, all three LGTM). Revision 5 only qualifies three
bare cross-repository issue references in prose, so neither plan issue links the wrong repository's issue. This is an in-place revision of an unapproved
candidate under review, not a successor of an approved plan.

## Measurements

- Plan: `docs/plans/2026-11-19-adr-0015-measured-artifact-per-space-instruction.md`, 340 physical lines, 25,734 bytes
  (`wc -l -c`); round 1 was 291 lines and 20,461 bytes, round 2 324 and 24,219, round 3 332 and 25,219, round 4 339
  and 25,695.
- Top-level implementation tasks: 9 (`grep -c '^- \[ \] \*\*'`, lines outside fences; no fenced line starts that
  way). Item 6 (an ME-only design-doc note) is new; items 3 and 7 merged their old gates/PR steps.
- Fenced literal volume: 7 fences, 71 body lines, 4,667 bytes (awk over fence markers):
  - 3 `text` fences (lines 51-53, 63-75, 83-106; 1 + 11 + 22 lines): the exact replacement ADR text the owner
    approves. These are the deliverable's bytes: 34 lines of a 197-line ADR.
  - 1 `markdown` fence (lines 220-224, 3 lines): the exact ME design-doc note of item 6.
  - 2 `bash` fences (lines 287-301, 307-315): the two repositories' existing merge-gate command lists, copied from
    each AGENTS.md "Verify" block (13 and 7 lines).
  - 1 `bash` fence (lines 321-336): an 8-line approved-text check (Python, standard library), a one-line leak grep and
    the parity commands. It is a verification command, not a second validator of product behaviour.
- Target files: `docs/design/adr/0015-cross-layer-embedding-identity-policy.md` in each of two repositories (identical
  bytes), plus ME's `docs/design/embedding-identity-and-tei-qwen-migration.md`. Measured from a trial application to
  scratch copies: ADR 31 added, 9 deleted, SHA-256 2a13502a... (pinned in the plan); design doc 4 added (`git diff --no-index --numstat`). Plan files under
  `docs/plans/` are committed and later removed by `/finish-pr` Item 5. No generated renders.
- Required fallback/provenance inputs: none. The plan is self-contained. The review tree holds both repositories'
  tracked sources at the plan's commits (`git archive`: `memory-engine/` at e14b188, `knowledge-base/` at 2ccc5d3), so
  reviewers check every cited code fact in both layers. The round-4 plan is at `.review/last-reviewed-plan.md` and the
  delta at `.review/plan-delta-r5.diff`.

## Calibration

The plan is about 10x longer than the ADR change (340 plan lines vs 31 added ADR lines). Growth since round 1 (49
lines) comes from review findings: two evidence rows, the approval and successor mechanics, the digest-bound
post-merge check and pinned ADR digest, the boy-scout item and its exact text. Signals and necessity:

- The owner's approval of this plan is the approval of the ADR text, so the plan must carry the exact new text; that is
  34 of its lines by design, not duplicated ownership.
- The claim-to-code table (9 rows) exists because the brief requires checking that ME's code contradicts nothing the
  text says; it records evidence for review, not a second specification.
- *Reported to Michaël* is a decision record the owner needs at approval (a divergence between his ruling and ME's
  code); it goes verbatim into both PR bodies.
- The gate lists are copies of two AGENTS.md blocks that the brief makes the merge gate even for docs.
- No complete product file, no generated payload, no frozen manifest, no mutation catalogue, no near-copy successor.

## Disposition

`proceed`. The length is coordination and decision evidence for a two-repository lockstep with an owner-approved text,
not a design document displacing a product file. No fallback selected.
