Plan SHA-256: cb4ed9008bdb0eb05feb7455efe513c9a6162ad72baee4b21a88164f28b3989b

# Altitude checkpoint: docs/plans/2026-11-16-rustsec-advisories.md (round 4 candidate)

## Measurements (method: `wc -l -c`, `grep -c '^- \[ \] \*\*'`, `grep -c '^```'`)

- Plan: 288 physical lines, 26276 bytes, 10 top-level implementation items (`- [ ] **N.`). Round 3 was 289 lines / 26422
  bytes (SHA-256 5cc6f15a732081493300ad2cc8fd81036550816bbb206ae07e04fc29e3d6d64d, all three reviewers LGTM); round 4
  only replaces Delivery 1's full archiver invocation, whose self-path the archiver rejects as a local source
  reference, by a pointer to finish-pr's script. Round 2 was 276 lines / 25400
  bytes (SHA-256 6ba6ceca060d508187a4660c722fd408f80b574e9bd3859220c93a780707f095); the round-3 growth is the
  round-2 findings (intermediate deny expectation, the squash-merge rationale, item 10 wording, the verbatim
  SQLite quote). Round 1 was 246 lines /
  22447 bytes (SHA-256 3a438b9e284f0912d602d40174f14dfb4c62c0c8c1292f62b33c4e60d0d24897); the +30 lines are round-1
  findings: a Constraints line quoting the brief on the #1032 row, item 10 widened from 1 to 5 doc files, the
  full-matrix cadence, a third consumer-visible change, and per-finding collateral actions.
- Fenced code: 0 blocks. Literal volume is inline code only: exact replacement wording for comments/docs (items 1, 3,
  5, 6, 7, 8, 9, 10), one test assertion (item 4), one helper signature (item 5) and the `deny.toml` entries
  (items 6, 7). None is a complete canonical file, validator, generator, manifest or mutation catalogue.
- Named target files and measured existing surface (lines): activity_filter.rs 276, crates/memory-engine/Cargo.toml
  303, crates/me-backend-sqlite/Cargo.toml 112, Cargo.toml 57, fuzz/Cargo.toml 115, activities.rs 508, ann.rs 1507,
  deny.toml 70, AGENTS.md 209, CLAUDE.md 249, GEMINI.md 214, README.md 194, CONTRIBUTING.md 70,
  docs/getting-started/installation.md 56, docs/reference/api.md, docs/usage/inspection.md and
  docs/design/architecture-overview.md (one line each, item 10), .claude/skills/qa-sweep/SKILL.md 220, me-consolidate/Cargo.toml 58,
  memory-engine-cli/Cargo.toml 46, memory-engine-embed/Cargo.toml 39, memory-engine-mcp/Cargo.toml 56.
- Estimated changed surface (an estimate, not measured): about 120-180 diff lines — ~25 manifest lines, ~15
  `deny.toml` lines, ~40-60 lines in ann.rs (two tests collapse onto one helper; net deletion likely), ~5 in
  activities.rs, ~20 prose lines across the 7 MSRV doc files, ~20 lines across item 10's 5 removed-`async` files, 2-3 comment lines. No generated
  renders.

## Required fallback/provenance inputs

- `~/.local/state/agent-work/2026-11-02-me-kb-claude-code/fleet/artifacts/memory-engine-1033/research-context.md`,
  SHA-256 `cc1974e0fcc850a1675f8baf0bfefedf0a6d23e0b9e65afd85ad72c62e6d69c8` (the issue's acceptance, the owner's
  decisions, and the spike's measurements the plan's "Evidence already measured" section summarizes). Review copy:
  `.review/inputs/research-context.md`.
- No predecessor design or amendment; the round-1 candidate is the review baseline (`.review/last-reviewed-plan.md`),
  the round-2 copy is `.review/last-reviewed-plan.md` from round 3 on, and each
  round's delta is `.review/plan-delta-r<N>.diff`.

## Calibration and disposition

The plan is longer than the diff it describes. The reason is not a change of role: about half of the change is
prose and comment wording, and for prose the exact text is the change, so the plan specifies it rather than
"update the docs". The rest is per-item red/green commands, which the plan structure requires. There is no
second copy of product (no validator, generator, frozen manifest or transcript), no duplicated ownership, and no
planning maintenance exceeding implementation. The "Evidence already measured" section records spike results the
sequencing depends on (stop conditions in items 5 and 6); it is provenance, not a spec.

Disposition: **proceed**. Reviewers: confirm or reject this calibration before completeness; flag any item whose
literal text you consider transcript rather than decision.
