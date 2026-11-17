Plan SHA-256: 248b4a410b09f0631ad4264e92ea58b0170adafbea503840136a5785af2806a9

# Altitude checkpoint: 2026-11-17-deferred-embedding

## Measurements

- Plan: `docs/plans/2026-11-17-deferred-embedding.md`, 697 physical lines, 63,094 bytes (round-7 candidate).
  Provenance: round 1 Codex reviewed `012fa70a…49d2` (598 lines, 52,190 bytes); the clean-slate reviewer read
  `399d10f1…` (615 lines), the same plan with Codex's first fixes already in; round 2 reviewed `53481e13…` (653
  lines) and `0fbe6246…` (662 lines); round 3 reviewed `aeafa241…` (670 lines); round 4 reviewed `571a7b19…` (678
  lines); round 5 reviewed `be716ed9…` (693 lines); round 6 reviewed `89bdce7c…` (696 lines). The growth is the reviewers' fixes, no new scope.
- Top-level implementation tasks: 19 (`grep -c '^- \[ \] \*\*'`, items 0 to 18; items 0 and 14 to 18 are
  operational, no code).
- Fenced literals (counted with `awk` over lines between fences, indented fences included): 75 lines,
  4,843 bytes, in 7 blocks:
  - item 1: the `embed_queue` DDL (10 lines), the one schema the migration must freeze verbatim;
  - item 4: the `EmbedderError` enum and its `MemoryError` variant (signatures, 15 lines);
  - item 6: the recall limit, `Degraded` and `embed_query_bounded` signatures (5 lines);
  - item 7: four signatures (8 lines);
  - item 8: the queue types, port and facade signatures (17 lines);
  - item 9: the `status` table output (4 lines);
  - Verification: the command matrix (17 lines).
  None is a complete product file, validator, generator or frozen manifest. Each is a signature, a DDL that a
  migration freezes, an output contract or the command list.
- Other data: two decision tables (where each concern lives; settle's outcome map), two test tables (acceptance
  map; vector reader → guard → test) and the #634 type table. They assign work and name tests; they duplicate no
  code.

## Target surface

Measured existing surface of the main files (`wc -l`): `store/facts.rs` 4,274; `sqlite/graph.rs` 2,246;
`inspect/restore.rs` 2,491; `search/ann.rs` 1,489; `memory-engine-embed/src/http.rs` 1,234; `me-types/src/error.rs`
1,054; `store/fact_vectors.rs` 817; `me-ingest/src/lib.rs` 546; `me-storage/src/graph.rs` 525;
`search/vector.rs` 335; `mcp/src/main.rs` 248; `tools/recall.rs` 222; `consolidation/mod.rs` 197;
`mcp/src/http.rs` 179; `cli/src/main.rs` 144; `tools/remember.rs` 78.

Estimates (labelled estimates, not measurements; uncertainty about ±50 %): new production code about 900 to
1,300 lines (queue SQL and port methods ~300, settle rules and backoff ~150, embed classification and truncation
~150, drain loop ~120, recall bound ~60, status ~150, guards ~60, restore ~40, main runtime ~20); new tests about
1,500 to 2,500 lines. Generated renders (insta snapshots, dump goldens) are excluded; they change by a table, an
index and a version number.

## Inputs

Required fallback/provenance inputs: none. The plan's Constraints carry the issue's acceptance verbatim and the
dutiona/infra#77 numbers verbatim; everything else it cites is in the repository at the reviewed commit
(ADR-0015, ADR-0020, the code). The three lens drafts were ephemeral working input to the synthesis and are not
review inputs.

## Calibration and disposition

Purpose: an implementation plan for one feature PR that crosses eight crates (`me-types`, `me-storage`,
`me-backend-sqlite`, `me-ingest`, the facade, `memory-engine-embed`, `memory-engine-mcp`, `memory-engine-cli`)
because the issue's constraint is cross-cutting: one new representation (`X''`) must be guarded in every vector
reader, and the queue spans storage, engine policy, the HTTP client and the server loop. The detail budget goes to
what an implementer cannot know: the representation decision and the rejected alternatives, each reader's guard,
the classification table, the truncation arithmetic, the drain's sleep rule, and the test that proves each
acceptance item.

Signals checked: no complete product file or second validator is embedded; no frozen transcript or mutant
catalogue; the plan is roughly a third of the code it describes by the estimate above; the two test tables replace
prose, they do not restate code. Length is driven by the 19 items' required shape (files, red, change, green,
commit) times a cross-cutting change, not by a design specification.

Disposition: **proceed**. No fallback is needed; no override is claimed.
