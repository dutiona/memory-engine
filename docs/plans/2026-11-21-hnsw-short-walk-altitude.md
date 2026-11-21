Plan SHA-256: d3fe837fdd7e6ae9b6bd8461dd718571b4a8436aa0ef3dab79c85bb49410ae94

# Altitude checkpoint: 2026-11-21-hnsw-short-walk

- Plan: `docs/plans/2026-11-21-hnsw-short-walk.md`, 357 physical lines, 26732 bytes (`wc -l -c`).
- Top-level implementation tasks: 11 (`grep -c '^- \[ \]'`; none of them sit inside a fence), numbered 0–10.
- Fenced literal volume: 2 fences, 20 body lines, 1066 bytes (awk count), both in `## Verification` (lines 321–347).
  Both are the AGENTS.md Verify block and its MSRV/image addendum, copied as commands. They are not product files,
  validators or generated payloads.
- Other literal data: one inline Options table (6 rows) and inline one-line code spans. The plan carries no complete
  canonical file, manifest, transcript or mutant catalogue.
- Named target files, with their measured existing surface:
  - `crates/me-backend-sqlite/src/search/ann.rs` (1529 lines; the change touches one expression and one comment, and
    adds two tests);
  - `tests/eval/conformance/scope_isolation.rs` (one test added);
  - three manifests, `.containerignore`, `Cargo.lock`;
  - the vendored `vendor/hnsw/src` (756 lines copied byte for byte, 2-line diff);
  - five docs pages (a few sentences each).
- Estimate (labelled): about 120–200 changed lines of ME code and tests, plus the verbatim vendored copy. The plan is
  prose that assigns that work; it does not transcribe it.
- Regeneration: N/A (revision 2 of the first candidate, after round 1: Outcome wording, D1 per-option consequences,
  fully specified T1/T2 fixtures, the rustfmt remedy's evidence; +19 lines, no new literal volume). Revision 3 after round 2:
  (b″) fork mechanics in D1, T2 described as a result check; no new literal volume. Revision 4: upstream issue
  references qualified as `rust-cv/hnsw#N` so the published plan issue does not link unrelated memory-engine issues.
- Required provenance inputs: one.
  - Source: `docs/plans/2026-11-21-hnsw-short-walk-drafts/research.md`.
  - Review copy: `.review/research.md`.
  - SHA-256: `d2a9e1a870c7637d8fc15b6ad265dbbf7f399e0d720667de705631e41e578220`.
  - It holds the measured probe and the crate survey with sources, which the plan's Root-cause and Options sections
    condense.
- Calibration. The plan's role is an implementation plan whose core is a decision record. The issue requires the
  (a)/(b)/(c) comparison, and Michaël picks at approval. That is why the Options table and the D1/D2 paragraphs make
  up about a third of the bytes. Each implementation item names its files, red/green commands and commit, with no
  code transcript. No duplicated ownership: the vendored crate is referenced, not reproduced, and the tests are
  specified by fixture and assertion, not by body.
- Disposition: **proceed**.
