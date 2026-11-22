Plan SHA-256: ae4aacb0746360665d0812f95d16d19fae0ece9694ce9252b99d3d2af633a24e

# Altitude report — 2026-11-22-1127-cycle-consolidation-v2 (successor)

- Plan: `docs/plans/2026-11-22-1127-cycle-consolidation-v2.md`, 596 physical lines, 65150 bytes. Top-level implementation tasks: 12
  (`grep -c '^- \[ \]'`, items 0–11, none inside a fence).
- Literal volume: one fenced block (the Verification command list, lines 512–528), unchanged from the predecessor.
- Regeneration: predecessor `docs/plans/2026-11-22-1127-cycle-consolidation.md`, sha256 41d31a8b195f5cec674ba34beb5b1e63a4e61a05344f53ef773c72fbcd7440fc, approved on
  plan issue #1129. Bounded delta: `diff` reports 55 removed and 116 added lines (171 changed of 535). Changed: the
  title line and a successor paragraph; one Constraints line (Michaël's approval decisions, verbatim); D5 replaced by
  the audited surface table and the outside-repository result; the owner-points heading (settled); item 9 (audit rerun,
  `git mv`, ADR amendments); the #1089 Documentation bullet; the #1089 sweep pattern; the Collateral section (filed and
  relayed). Retained unchanged: Outcome, D1–D4, D6, items 0–8 and 10–11, Testing, Verification, the rebase surface.
  Approved intent is unchanged (the rename was approved as O4); the delta widens #1089's audited surface inside the
  same acceptance, as the owner directed.
- Target surface for item 9: 52 files name an identifier (the audit's `rg -l`), plus prose-only docs; crate directory
  move; net line change near zero.
- Required provenance inputs: the six member issues, unchanged from the predecessor's report (same review copies and
  digests under `.review/inputs/`), plus the predecessor plan at `.review/predecessor-plan.md` sha256
  41d31a8b195f5cec674ba34beb5b1e63a4e61a05344f53ef773c72fbcd7440fc.
- Calibration: the successor adds an audit table the owner asked for; no product text, validator or catalogue.
- Disposition: **proceed** (complete successor; review of the delta only, against the predecessor).
- Successor revision 2 (after its round 1): D5 gains ADRs 0005, 0008, 0009 and 0014, `hybrid-search.md`, a method-list row and an
  "unforgettable" row (the CLI `--pinned` help included); the sweep adds `forget()` and "unforgettable". Disposition: **proceed**.
- Successor revision 3: one word in the #1089 Documentation bullet (four → eight ADRs). Disposition: **proceed**.
- Successor revision 4 (after the Claude stand-in's review): D5 gains the redaction tripwire and benchmark rows, a
  sharper rule for "forgetting", `std::mem::forget` kept; item 9 writes out the audit command and the ADR-0005 header
  rule; the #1089 Documentation bullet points to D5; the sweep adds the qualified method names. **proceed**.
- Successor revision 5 (after the stand-in's second review): D5 gains a code-prose row, the historical/literature/English
  keep classes and the ADR amendment criterion; ADR amendments use ADR-0015's Status-line form; item 9 names its four
  `git mv`s; the rebase surface covers the rename; the sweep is one explicit command set over the whole tree. **proceed**.
- Successor revision 6: the sweep becomes a closed keep-list over every word containing "forget" (hidden files
  included); `forget_policy` joins D5; the alias-convention comments are renamed; the ADR check covers the Status entry; D3
  states why restore still excludes the watermark after #1131; the rebase surface names #1026's merge (7ba301e) and the
  `promote_space` hotspot with #1126. **proceed**.
- Successor revision 7 (after the Sonnet review): a per-line review restored beside the word list; item 9's Green
  compiles `benches/` (clippy --all-targets); the ADR check is wrap-proof; the gitleaks history allowlist, the pak-manifest
  "prune" and the forgetting.md title are stated as kept. **proceed**.
