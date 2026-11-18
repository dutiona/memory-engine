# Review — plan for dutiona/memory-engine#1088 follow-up group B

Altitude report SHA-256: 017b6e7018fdd3b0e259765b53983f6493dcc8e3db55a8a612bacd824ea13080

## Altitude / duplication (assessed first)

Verified against the snapshot: `.review/plan.md` is 254 lines (matches altitude.md:5), 21 top-level `- [ ]` tasks (altitude.md:6-7), and exactly one fenced block — `plan.md:194-200`, the literal `podman` block that *is* the product text copied into four instruction files. The #1067 and #1041 designs are stated as predicates, not embedded code; no product file, validator, generator or manifest is embedded. Duplication is justified, not ownership-ambiguous. Altitude disposition **proceed** is upheld; no justified override. `altitude.md:20-22` ("Required fallback/provenance inputs: none") is consistent with what I needed to read. **No altitude finding.**

## Findings on the specifically weighed points

**#1067 two-condition rule** — the design is correct and better than a code-only rule: `fts.rs:91-112` currently folds both FTS5 syntax errors and real faults into the row-skip arm, and `fts.rs:154-158` into `Ok(0)`; a bare `extended_code == 1` cannot distinguish a malformed MATCH from `json_extract` on bad JSON, which `filter_sql.rs:29-34,75-77` deliberately passes through as an opaque fragment. The MATCH-only probe is the right discriminator.

- [MEDIUM] `plan.md:14` — "fails the same way" is under-specified. The same paragraph says messages vary and cannot decide, but does not state the probe's exact predicate. Concrete fix: require the probe to fail with `rusqlite::Error::SqliteFailure { extended_code: 1, .. }` on the same connection (ignore message and primary code); any other probe outcome, including `QueryReturnedNoRows` or a different extended code, counts as a fault → `StorageError::backend`. Without this, an implementer could accept "any probe Err" as no-match.
- [MEDIUM] `plan.md:49-55` — `corrupt_fts_index_is_a_storage_error` rests on the unverified claim that `DELETE FROM facts_fts_data` makes MATCH fail with 267 (`SQLITE_CORRUPT_VTAB`). On the bundled SQLite the index may simply read as empty (→ `Ok(empty)`), leaving the red test permanently red. The "stop and report" caveat prevents a silent false pass, but the fix should be specified: in step 0, run a one-off probe of the injected state, record the actual `extended_code`, and require `!= 1` (or pick a different injection, e.g. a corrupt `facts_fts_idx` row) before committing to that test.
- [LOW] `plan.md:56-58` — the characterization list (`nocol:rust`, `*`, `^`, `a + `) assumes all malformed strings yield code 1. The 60 s fuzz run backs this, but add an explicit early assertion in the red phase that each listed string's probe error is `extended_code == 1`, so a surprising code is reported rather than silently reclassified as a fault.

**#1041 text-derived drift test** — the rule works on the real `error.rs`. I verified all 20 variants of `MemoryError` (`error.rs:430-608`) are single-line or the two brace-forms (`EmbeddingModelMismatch` :455-458, `SnapshotTooLarge` :529-532) whose `///`-skipping field join yields the stated shape; `#[from]` dropping produces the plan's examples (`Serialization(serde_json::Error)`, `Storage(StorageError)`). `include_str!("error.rs")` and `include_str!("../../../docs/reference/api.md")` resolve correctly from `crates/me-types/src/error.rs`, and `me-types` is `publish = false` (`Cargo.toml:11`), so out-of-crate inclusion is safe. api.md:235-247 has 9 rows → 20 as planned. **Nothing adverse on layout.**
- [LOW] `plan.md:134-135` — "rows after `MemoryError` variants: up to the first non-table line" includes the header and `|---|` separator, which contain no backtick-quoted cell. State explicitly that rows without a backtick-quoted first cell are skipped, or the test never goes green.
- [LOW] `plan.md:138-139` — multi-line struct variants derive full types (`Box<crate::types::EmbeddingFingerprint>`), not the short examples shown; make it explicit that the api.md cells carry the derived strings verbatim.

**Sphinx / `docs/plans` exclusion** — the exclusion is right. `conf.py:20,22,23-28,31` matches the plan's inventory; `.readthedocs.yaml:10` already builds with `-W`, so the new local line is consistent, not conflicting. `docs/design/index.md:8,13` are the two stale entries; 7 mermaid fences (5 in `associative-recall.md`), 7 `../../src/` links resolving to the 4 stated `crates/` destinations, two audit files, and missing `_static`/`_templates` all check out. Excluding `plans` is coherent with archiving branch plans to issues (and with B5 removing stale plan paths). **Nothing against the exclusion.**

**#1071 `--cpuset-cpus` deviation** — `Containerfile:26,31` confirms both binaries land in `/usr/local/bin` and the entrypoint is `memory-engine-mcp`, so both `--version` invocations work; `git archive` yields a clean tracked-only context. The rootless-podman rationale is external to the snapshot and I cannot verify it here — I accept it as documented and as the plan requires it be stated in the commit body.
- [LOW] `plan.md:188,203` — `git archive` ignores `.containerignore`, so triggering an image rebuild on a `.containerignore` change cannot validate that allowlist. Either drop `.containerignore` from the trigger or build from a clean checkout for that case.

**Six boy-scout items** — all six are real rot, not invented:
- B1: `fts.rs:393` cites `fts.rs:30-83` (actual mapping at 91-102); `fts.rs:405` cites `fts.rs:43-45` (logic lives in `filter_sql.rs:62-67`).
- B2: `scopes.rs:96-103` credits `validate_segment` with the whitespace rule that `scope.rs:43-46,132-140` explicitly assigns to `validate_label`.
- B3: `memory-engine-embed/Cargo.toml:36-39` indeed lacks `unwrap_used`; unwraps are confined to `src/*` `#[cfg(test)]` blocks/doctests and four `tests/*.rs` files (confirmed: 4 files).
- B4: api.md:222-225 omits the six typed errors that `lib.rs`'s `pub use error::*` exposes (comment at `lib.rs:296-297`).
- B5: `equivalence.rs:5` and `tests/eval/main.rs:7` name deleted plan files.
- B6: `conf.py:22` names an absent `_templates` (glob found none).

**Nothing** adverse among them; no collaterals need filing.

**#1077** — safe: `PgFactory` is `#[cfg(feature = "backend-postgres")]` and `todo!()`-bodied (`factory.rs:163-171`), so `backend-postgres = []` keeps it compiling.
- [LOW] `plan.md:17` — "No source names `me_backend_postgres`" is false workspace-wide (`memory-engine/src/storage/mod.rs:54` does, behind its own feature). Scope the claim to `me-test-support`, or the implementer's grep will find a hit and stall.

**Mandatory sections** — Documentation (`plan.md:215`), Testing (`:225`) and Verification (`:239`) are all present, with explicit "no unit test by design" justification. **Nothing.**

## Verdict

**LGTM with refinements.** No blocker: the layering, commit atomicity, red/green discipline and full-matrix verification are sound, and the two load-bearing designs (#1067 discriminator, #1041 derivation) are correct against the code. Fold the two MEDIUM clarifications (probe predicate; corrupt-index injection) into the plan text before implementation; the LOW items are wording/precision cleanups that do not change scope. Verification evidence remains the implementation owner's to produce — I ran no tests, builds or gates.

REVIEW COMPLETE

---
# Round 2, retry (plan 0925754c…7313, altitude 1c7769ef…b260; round 2 first attempt returned nothing: its globs went outside the working directory and were auto-rejected)

## Altitude report assessment

Assessed first. Its figures check out against `.review/plan.md`: 284 physical lines, 23 top-level `- [ ]` tasks (counted in order through "Merge"), and a single fenced block at lines 213–219 (the #1071 product command). The one-item/ +7-line growth over revision 2 matches B8 plus wording. "Disposition: proceed" is consistent with the content — it is still an implementation plan, no embedded validator or generated artifact. Nothing in the report overstates the delta.

Altitude report SHA-256: 1c7769ef9d21e1123af14973e8eb5ac9ea8903d40bf52e89857093d42897b260

## Round-2 delta review (r1 → r3)

**Findings: the delta raises nothing.** No [BLOCKER], [HIGH], [MEDIUM] or [LOW] finding. All round-1 points I can check are resolved, and the new material is factually grounded:

- **#1067** (`plan.md:14, 48–74`): the exact probe predicate is self-consistent with the source. `fts.rs:68-71` / `:135-137` share the `facts_fts` table, `:91`/`:147` are the `query_map`/`query_row` step points and `:105` is the row-skip arm the change removes. The corruption injection is verified against `facts.rs:1756-1774` (`PRAGMA ignore_check_constraints` ON/OFF around `UPDATE facts SET metadata = '{'`). The raw-MATCH injection check and the "pin the premise" characterization prevent a false red.
- **#1041** (`plan.md:134–149`): the parser's three layouts match the real enum (`error.rs:430-608` — 20 variants; multi-line struct variant `EmbeddingModelMismatch` at `:455-458` yields the exact example cell). The new "panic on unknown ending" and "skip non-backticked rows" fixes correctly handle the header/separator at `api.md:237-238`. `include_str!("../../../docs/reference/api.md")` resolves to the repo root from `error.rs`.
- **B4** (`plan.md:152-156`): substantiated. `lib.rs:187-190` does root-re-export `PersistenceClassifier`, so `api.md:223`'s note is wrong; `lib.rs:297` confirms `pub use error::*` supplies the six typed enums; `serialize_embedding`/`deserialize_embedding` appear only via `crate::store`, not at the root.
- **B3** (`plan.md:88-94`): grep confirms exactly three of four embed test files unwrap (`http_integration.rs`, `qwen_integration.rs`, `asymmetric_embedding.rs`); `proposer_integration.rs` does not.
- **B8** (`plan.md:197-199`): `api.md:36` and `api.md:189` are indeed the two `### Bootstrap` sections with the same two methods, the second fuller.
- **B2** (`plan.md:130-132`): `scopes.rs:94-108` credits `validate_segment` with the whitespace rule that is actually enforced only in `validate_label` (`:132-142`).
- **B7** (`plan.md:20, 162-166`): premises hold — `pyproject.toml:6-12` sets floors only and `.gitignore:46` ignores `uv.lock`; the tracked-lock green check and the whole-tree clean export (Verification 3) are now correct for `--locked`. The approval dependency stays explicit; the "if struck" clause covers all three `--locked` sites.
- **#1077** (`plan.md:17, 108-117`): no `me_backend_postgres` reference exists in `me-test-support/src`; `factory.rs:166-173` compiles `PgFactory` through `todo!()` under the feature gate, so `backend-postgres = []` is safe.
- **#1046** (`plan.md:168-195`): `conf.py` does contain `suppress_warnings` (`:20`), `templates_path` (`:22`), `exclude_patterns` (`:23`) and `html_static_path` (`:31`), and has no `myst_fence_as_directive` — every stated edit has a live target. The gate comment "local only, no CI job runs Sphinx" is precise (CI's docs job is rustdoc-only).
- **Surfaced follow-ups** (`plan.md:276-284`): `api.md:70` shows `execute_query` as `Result<Vec<SearchResult>>` and the tables show sync signatures while `:229` states the methods are `async` — the systemic drift is real and correctly left out of scope.

LGTM.

REVIEW COMPLETE
