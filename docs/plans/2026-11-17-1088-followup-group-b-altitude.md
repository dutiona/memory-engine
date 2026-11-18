Plan SHA-256: 0925754c41e6681f2ebcab8de9401ee19fe76a058a20d12bb10fbfb335aa7313

# Altitude checkpoint: 2026-11-17-1088-followup-group-b (revision 3)

- Plan: docs/plans/2026-11-17-1088-followup-group-b.md, 284 physical lines, 26199 bytes (`wc -l -c`).
- Top-level implementation tasks: 23 (`grep -c '^- \[ \]'`; revision 2's 22 plus boy-scout B8, item 14a). No
  `- [ ]` sits inside a fence.
- Fenced code: one block, lines 213-219, 5 body lines / 402 bytes, unchanged: the exact #1071 command block (product
  text). No complete product file, validator, generator, manifest, transcript or mutant catalogue is embedded.
- Named targets: as revision 2; B8 deletes one duplicated section of docs/reference/api.md (~8 lines).
- Regeneration: revision 3 after round 2. Predecessor: revision 2, SHA-256
  2e4e2b3993930e0f95eb594819fe9164421bd3d015a31055683e031f4031feb8 (the delta baseline). Delta: wording fixes from
  round 2 (gate-line comment, B7's "if struck" clause and tracked-lock check, the 31→29→26 count, B4's line
  reference, the `.containerignore` note) and one new boy-scout item (B8, the duplicated Bootstrap section); the
  method-table drift stays a surfaced follow-up. Everything else is retained unchanged.
- Required fallback/provenance inputs: none.
- Calibration: +7 lines for one item and wording; still an implementation plan, shorter than its change.
- Disposition: **proceed**.
