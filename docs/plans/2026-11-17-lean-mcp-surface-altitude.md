Plan SHA-256: 63615f8813ae874586fba9ae0be975f7d517ea2e5e6a61c78d4feaa4677b4dc1

# Altitude checkpoint: 2026-11-17-lean-mcp-surface

- Plan: `docs/plans/2026-11-17-lean-mcp-surface.md`, 570 physical lines, 54,252 bytes (`wc -l -c`).
- Top-level implementation tasks: 15 (`grep -c '^- \[ \] \*\*'`, counting only the Implementation sequence's
  top-level checkbox items; no checkbox appears inside a fence).
- Fenced / literal volume: one fence (lines 539-554), 14 lines, 648 bytes (`awk` over the fence markers): the
  repository's verification commands, copied from AGENTS.md's Verify block plus the MSRV job. It is a command list,
  not a canonical file, validator or generated payload. Other literal text is short: four exact model-facing strings
  (two tool descriptions, the receipt, the instructions format) and test names. No complete product file, no
  duplicated validator or generator, no frozen manifest, transcript or mutant catalogue.
- Target surface, measured: `crates/memory-engine-mcp/src` today 5,409 lines (`wc -l` over `src/*.rs`,
  `src/tools/*.rs`, `src/tools/handlers/*.rs`); `crates/memory-engine-mcp/tests/*.rs` 6,508 lines. Most of both is
  deleted by item 12.
- Target surface, estimated (uncertain, not measured): new or changed product code about 600-900 lines
  (`ExactSet` arm and builder, `resolve_scope_path`, `StoreLock`, CLI `Writable`/`create_engine`/`prune`, `home.rs`,
  `tools/{mod,recall,remember}.rs`, call records, `fts_terms`, wiring), plus about 1,000-1,500 lines of tests
  (`lean_surface.rs`, CLI `store_lock.rs` and `prune.rs`, engine-seam and unit tests), plus doc rewrites
  (`docs/reference/mcp-server.md` shrinks from 348 lines). No generated renders.
- Regeneration: revision 9 of the canonical candidate. Revision 9's delta over revision 8 (SHA-256
  `deb26a4be7e417062797a24937b2f1adcdaef5270cc9ae638184069d318d250d`) is one line of item 1: the plan no longer names
  its own local path ("Commit this plan and its review artifacts under `docs/plans/`"), which the plan-issue archiver
  refuses as a local source reference. Revision 8's delta over revision 7 (SHA-256
  `4c947d14cc67456f067d60ad3ff076b2f06baff3e619eb30ead470635de7d8ba`), from the Claude reviewer's round-7 LOWs only:
  D12 states the re-measured worst case and its accepted residual, "For Michaël" #2 offers a lower cap, and the
  `recall` description names the 4,096-byte limit. Revision 7's delta over revision 6 (SHA-256
  `c589db1f3ba0a2757f31741b2b5ed588a1eb82d0c6b5b0edc8d044acf50f13e5`), from round-6 findings (Codex and the Claude
  reviewer, the same MEDIUM): D10 caps `recall.query` at 4,096 bytes, replacing the 1 MiB cap, its facade re-export
  and the 256-term limit; D12 rests the cost bound on that cap with measurements; items 9 and 11 tests. The plan
  shrank by two lines. Revision 6's delta over revision 5 (SHA-256
  `c3f3ac9e30c20981bfd566cd8501fb0a5d87edbb48a1058eeb47f9252c8cab79`), from the Claude reviewer's round-5 MEDIUM only:
  D12's run class becomes Unicode alphanumerics, so each term is one FTS5 token; item 11 tests. Revision 5's delta over revision 4 (SHA-256
  `e9eacdf0b97d89a0e3e4ea4f152e9ed55d3c15d02826c258183a4b9e8892a0b3`), from the Claude reviewer's round-3 LOWs only:
  D11 (every record's `error` cut to 512 bytes, payload wording, a #1023 hand-off line), D12 (case-insensitive
  dedupe), item 9 (a 300-byte C5 key, the harness `seed` declares the provider's fingerprint). Revision 4's delta over revision 3 (SHA-256
  `304a3165e4d6f055d7efed39e96ec2d46ab775ecbefddd27b54d0ef5e24c072c`), from round-3 findings only: D10 (an unknown
  `project` is named in full; only the record's `error` is cut to 512 bytes), D12 (a 256-term refusal with its
  measured reason replaces the 64-term truncation), items 9, 10 and 11 tests. Revision 3's delta over revision 2 (SHA-256
  `5e6f36189ac1c6ee5992079e4309656b866ef9d86908d51d6cd342a9f1972a08`), from round-2 findings only: D7 (`migrate`
  exception), D10 (`MAX_PAYLOAD_BYTES` re-export, `project` cap), D11 (the exact guarantee, the remember outcome read
  from the fact side, one more test), D12 (`MAX_FTS_TERMS`), items 5, 6, 9, 10 and 11 test and file adjustments.
  No revision succeeds an approved plan. Revision 2's predecessor is revision 1, SHA-256
  `16f0898e19873fc9866722e373dac1b79e32f5415451da1f591bb090272ce4d2`; both are retained outside the review tree.
  Revision 2's bounded delta, all from round-1 findings: D1 (HNSW widening described exactly), D5 (startup RNG probe), D7 (lock before the
  peek, `is_file` first, `export` off the locked list), D9 (`add_facts_batch` guarded), D10 (dev-dependencies, query
  cap), D11 (record ordering and coverage rules), D12 (no-terms case, port note, collateral), new D13 and item 5 (live
  SQLite export), items renumbered 5-14 to 6-15, test, docs and Verification adjustments. Retained unchanged: the
  Constraints, D2-D4, D6, D8, the item structure and the test-file dispositions.
- Required fallback/provenance inputs: none. The plan cites contracts committed at HEAD
  (`docs/design/adr/0020-harness-neutral-lifecycle-api.md`, `docs/design/adr/0021-memory-scope-model.md`) and the
  public issue #1020; every reviewer reads those from the review tree.

## Calibration and disposition

Purpose: an implementation plan for one PR that replaces a 26-tool MCP surface with two tools, adds a shared store
lock, a CLI command, one engine scope variant and a read-only SQLite export, across six crates (me-types, me-index,
me-backend-sqlite, memory-engine, memory-engine-cli, memory-engine-mcp) plus the detached fuzz workspace and docs. That breadth is inherent in the
issue's acceptance (surface, call IDs, CLI lock refusal, derived-job fate, docs agreement), not plan growth.

Detail budget: each item names files, red tests, the rule or signature, the green command and the commit subject, as
the plan structure requires. The Decisions section (D1-D13) is the densest part; it fixes contract choices the
ADRs leave open (lock file name, response shape, result fields, call-record form) and lists the rejected
alternatives in one line each. It is not a specification of the product: no handler is transcribed, and the plan is
much shorter than the code it governs.

Structural signals checked: no complete product file, no second validator, no near-copy successor, no planning
maintenance exceeding implementation. Two items (10 call records, 11 full-text terms) are flagged as separable on
Michaël's decision; they are small and self-contained.

Disposition: **proceed**.
