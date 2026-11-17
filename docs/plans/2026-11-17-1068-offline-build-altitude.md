Plan SHA-256: 02021479fb0bd85dced236e4e75334d0c8030586f1a9d59570b8f4aeacf0e081

# Altitude checkpoint: 2026-11-17-1068-offline-build

## Measurements

- Plan: `docs/plans/2026-11-17-1068-offline-build.md`, 157 physical lines, 11336 bytes (round 2 revision; round 1 reviewed f83ed380…, 151 lines).
- Top-level implementation tasks: 2 (`grep -c '^- \[ \]'`, column-0 checkboxes; none sit inside a fence).
- Fenced literal volume: 4 fences, 10 body lines, 1009 bytes (awk over fence markers). Locations: Constraints (the
  ticket's four builder lines, verbatim), item 1 (the two new header comment lines), item 2 (the one-line archive
  command), Verification V2 (three commands). All are commands or the exact text the ticket fixes; none is a complete
  canonical file, validator or generated payload.
- Target files and measured surface: `Containerfile` (26 lines today; about 3 lines replaced and 4 added, estimate),
  `docs/getting-started/installation.md` (82 lines; about 9 lines added, estimate). No generated renders.
- Not a regeneration: no predecessor plan.

## Required inputs

- The ticket body, as fetched for the scope digest: source `issue-1068-body.md` (worker scratch), review copy
  `.review/inputs/issue-1068-body.md`, SHA-256 `46b38762bade40d3caf655d3b23f4338fa1b5da76bfa051eff16634ce15d8f0a`.
- The tar-context probe result: source `ctxprobe/RESULT.txt` (worker scratch), review copy
  `.review/inputs/ctxprobe-RESULT.txt`, SHA-256 `7e89bd21f9bd3eded03b76de38a4e6fc7eb6981cd51b0056d2693f1e7a480e2d`.

## Calibration and disposition

The plan (157 lines) is longer than the product change (about 16 changed lines). That is not a role change: it holds no second
copy of the product, no validator, no manifest. Its bulk is the behavioural evidence a container-only change needs
(an isolation probe, a fresh production-path build, a checkout build, the entry points, the cargo gate) and the
ticket's fixed lines copied verbatim into Constraints. A committed test would assert text, not isolation, so the
verification section carries the proof by necessity.

Disposition: **proceed**.
