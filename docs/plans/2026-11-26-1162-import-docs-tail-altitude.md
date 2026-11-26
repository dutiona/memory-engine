Plan SHA-256: b4ce7ce8e7acf0aea7e8520fc04ec2492519e52c4b7b7aa1fa0ccd025f865421

# Altitude checkpoint: #1162 plan

- Plan: docs/plans/2026-11-26-1162-import-docs-tail.md, 393 physical lines, 36341 bytes, 11 top-level `- [ ]`
  implementation/operational items (counted with `grep -c '^- \[ \]'`; none inside fences).
- Literal volume: one fenced block (Verification, 15 lines, 864 bytes): the repository's own gate commands with the
  host's heavy-lock wrapper. No complete product file, validator, generator, manifest or transcript is inlined. Item 3's
  case table (7 rows) is test input specification, not a copy of test code.
- Target surface, measured: crates/memory-engine/src/bootstrap/filter.rs (1074 lines), parse.rs (959), tests/api_doc_parity.rs
  (458), tests/bootstrap_test.rs, crates/memory-engine-mcp/src/adapters/claude_code/transcript.rs (1235), two JSONL
  fixtures, the six files of the fuzz run line, api.md (334) and five-to-seven guides. Estimated change (uncertain,
  author's estimate): ~150 changed lines in filter.rs's reconstruction, ~350 lines of new tests in filter.rs and
  transcript.rs, ~300 lines added to api_doc_parity.rs, ~80 lines of doc edits, six one-token edits for #1161.
- Regeneration: N/A (first plan for this issue; no predecessor).
- Fallback/provenance inputs: none required. The research context the drafts used is summarized in the plan's
  "Ground truth" section with path:line citations into the repository under review.
- Calibration: a packed PR of three issues, one load-bearing (#1159). The plan assigns decisions (rules, names,
  signatures, test cases, commands) and does not transcribe code; at roughly a third of the estimated changed lines it
  stays shorter than the code it describes. No duplicated ownership: the pairing rule is stated once (item 3) and the
  checker rules once (item 4).
- Disposition: proceed.
- Revision 2 (after round 1): +~20 lines resolving the agy and Claude findings (raw result-block reading, owner
  seeding and sort order, the fuzz artifact prefix decided, risk 1's re-bootstrap wording, the field rule scoped to
  api.md). No change of role; disposition unchanged: proceed.
- Revision 3 (after round 2): one wording fix in item 3 (risk 1 owns the re-bootstrap effect). Disposition: proceed.
- Revision 4 (Michaël: "Approve + fold C1 in", relayed 2026-11-26): +~50 lines, one new item (4, C1: visible text,
  never thinking, with its stored-fact and --reprocess consequences), later items renumbered, risk 1b, Documentation and
  Testing lines. Changed approved intent is not involved: the earlier sha was never approved, and this is the complete
  successor for approval. Estimated added code: ~20 changed lines in filter.rs/parse.rs, ~80 lines of tests. No change
  of role; disposition: proceed.
- Revision 5 (after round 4): item 4 skips Claude Code's own rows for text, documents the 2000-character cut, and puts the erase question in the PR body. Disposition: proceed.
- Revision 6 (after round 5): item 4 names the struct-literal edits its two new fields need. Disposition: proceed.
