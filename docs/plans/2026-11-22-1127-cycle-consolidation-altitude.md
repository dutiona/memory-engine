Plan SHA-256: 41d31a8b195f5cec674ba34beb5b1e63a4e61a05344f53ef773c72fbcd7440fc

# Altitude report — 2026-11-22-1127-cycle-consolidation

- Plan: `docs/plans/2026-11-22-1127-cycle-consolidation.md`, 535 physical lines, 50851 bytes.
  Top-level implementation tasks: 12 (`grep -c '^- \[ \]'`, items 0–11; none sit inside a fence).
- Literal volume: one fenced block (the Verification command list, lines 495–511 at this digest), 15 lines / 885 bytes;
  it is a command list copied from AGENTS.md "Verify", not a canonical file, validator or generated payload. Two
  markdown tables of decisions (where each rule lives; #1099's limits), one decision table (#1099 measurement), one
  rebase-surface table and one acceptance table. No code transcript: changes are given as signatures, rules or exact
  error text.
- Target surface (measured by grep at c103cfa, estimates labelled):
  - #1099: `crates/me-backend-postgres/src/tests.rs` (one file; estimate +60–120 lines depending on the variant item 1 selects).
  - #1080: about 12 files across me-types, me-backend-sqlite, me-consolidate, me-storage (docs), memory-engine
    (restore, tests); estimate +250–400 lines, most of it tests.
  - #1062 + promotion twin + CLI pre-check: about 12 files; estimate +200–300 lines, most of it tests.
  - #1095: 1 source file, 3 test files, 1 doc; estimate +40–80 lines.
  - #1089: rename over about 46 files (33 name `ForgetPolicy` today, counted with `grep -rln ForgetPolicy`), crate
    directory move; net line change near zero.
  - No generated renders are counted; `Cargo.lock` changes only by the crate rename.
- Regeneration: none (first candidate; no predecessor).
- Required provenance inputs (the member issues the Constraints quote, fetched 2026-11-22; review-copy paths are
  relative to the review tree):
  - `issue-1127.md` → `.review/inputs/issue-1127.md` sha256 66bb02a10b83b1bf7dbca64f97b7d3fa747bd4f5b98e9cc6c17cc99a5c811de4
  - `issue-1062.md` → `.review/inputs/issue-1062.md` sha256 17b8218703c7279d43875ef6eeb9a169145077e69cedde08df554fb81bf06632
  - `issue-1080.md` → `.review/inputs/issue-1080.md` sha256 064331d8c5fe495801fb299f646d9bdc9c7688fa4c24f6caf3675430074a94e8
  - `issue-1089.md` → `.review/inputs/issue-1089.md` sha256 8e4b6e16a04c4299b6d4fdc4849b9dc8479c24162d660c780b02f328d7647886
  - `issue-1095.md` → `.review/inputs/issue-1095.md` sha256 7cdad9be9ce0b394c2c57e5d76d84459c811980a0d77df09dc7080d5c08e2e24
  - `issue-1099.md` → `.review/inputs/issue-1099.md` sha256 db7a6c74cbd492ba7dbbbc1a1515656168a3f8e69a8b5310429c7f61d0149476
- Calibration: one PR closing five independent members across five subsystems (cycle apply, consolidation, naming,
  MCP answers, test harness), plus three boy-scout commits and four operational items. The plan's role is to fix every
  design choice (six owner points with defaults and alternatives), the rebase surface the owner asked for, and each
  item's red/green commands. Its length comes from that breadth and from the #1099 measurement protocol, which the owner's
  "measure first" rule makes a precondition, not from product text: no complete file, validator, manifest or mutant
  catalogue is embedded. The plan is shorter than the code it describes (estimated 600–900 changed lines plus a 46-file
  rename).
- Disposition: **proceed**. No design split, amendment or product extraction is warranted: the decisions are
  implementation choices inside settled issues, and each owner point is a bounded choice stated with its consequence.

- Revision 2 (after round 1): D3 gains a consolidation generation counter, an unpin rewind and stale-plan discard;
  item 1 gains owner point O7 and an inconclusive row. Growth is review-driven correctness, not new product text; the
  disposition stays **proceed**.
- Revision 3 (after round 2): a pin bumps the generation; the #1099 table's M row states its full conditions and a
  multi-row match stops. Disposition unchanged: **proceed**.
