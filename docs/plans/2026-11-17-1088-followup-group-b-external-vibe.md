# Review — dutiona/memory-engine #1088 follow-up group B (`.review/plan.md`)

## Altitude and duplication

The plan is 254 lines / 22365 bytes, exactly as `.review/altitude.md` records, with 21 top-level tasks and a single fenced block (lines 194–200) that is itself the product text (the #1071 command block), not an embedded file. No validator, manifest, generator or predecessor is embedded; no fallback/provenance inputs are named, and none are needed — every factual claim I checked was verifiable in the tree. The plan is shorter than the ~350–450 lines of change it describes and assigns one owner, one red/green pair and one commit subject per item, so it does not duplicate any file's ownership. Disposition "proceed" is justified; I take no override.

Altitude report SHA-256: 017b6e7018fdd3b0e259765b53983f6493dcc8e3db55a8a612bacd824ea13080

## The four weighed questions

**#1067 two-condition rule vs code-only — the two-condition rule is right.** Verified against `crates/me-backend-sqlite/src/search/fts.rs`: today `fts_search_filtered` maps a `query_map` `Err` to `Ok(vec![])` (fts.rs:96–101) and skips row errors via `filter_map` (fts.rs:103–111); `fts_count_expired` has a catch-all `Err → Ok(0)` plus a dead `QueryReturnedNoRows` arm (fts.rs:150–157). A code-only rule (`extended_code == SQLITE_ERROR(1)` ⇒ no-match) would misclassify the malformed-`metadata` case the plan's red test injects (`json_extract(f.metadata, …)` on `metadata = '{'` also fails with bare 1), because `FilterSql` fragments are caller-supplied SQL (`filter_sql.rs:31-35`) and the backend's metadata translation runs per-row. The MATCH-only probe (`SELECT rowid FROM facts_fts WHERE facts_fts MATCH ?1 LIMIT 1`) attributes the bare 1 to the query string; corrupt index fails with 267 (`SQLITE_CORRUPT_VTAB`) and never reaches the probe branch. The plan's contingency ("if the shadow-table delete does not fail the MATCH on the bundled SQLite, stop and report") correctly hedges the one unverifiable-in-review behavior.

**#1041 text-derived drift test — works on the real `error.rs` layout.** `crates/me-types/src/error.rs` has exactly 20 `MemoryError` variants, each on a line of exactly four spaces + uppercase; single-line struct variants end with `},` (handled by the "ends with `,`" branch), multi-line ones (`EmbeddingModelMismatch`, `EmbeddingReopenRequired` is single-line, `SnapshotTooLarge`) end with `{` and their field lines carry only `///` docs to skip; `#[from] ` appears only inside tuple payloads (`Serialization(#[from] serde_json::Error)` → `Serialization(serde_json::Error)`). The slice `pub enum MemoryError {` → next column-0 `}` contains no false-positive lines. `include_str!("error.rs")` and `include_str!("../../../docs/reference/api.md")` resolve correctly from `crates/me-types/src/error.rs`. The api.md table indeed has 9 rows (including the deleted `Database(rusqlite::Error)`), so "9 rows against 20" is accurate. The rustfmt-layout dependency is real but guarded by the fmt gate.

**Excluding `docs/plans` from Sphinx — right.** `docs/conf.py:22-27` `exclude_patterns` is the only exclusion lever; `.readthedocs.yaml` runs `sphinx-build -W -b html docs …` with no excludes of its own, so a conf.py entry covers RTD too. `docs/plans/` does not exist at the branch base (confirmed by listing `docs/`), but the work branch itself commits a plan there (per AGENTS.md), which is exactly why the exclusion is needed for this branch's own `-W` build; `ROADMAP.md` is the existing precedent. The plan also removes the one known toctree entry into `plans/` (`docs/design/index.md:13`).

**#1071 deviation from `--cpuset-cpus` — justified.** `--cpu-period=100000 --cpu-quota=1600000` (16 CPUs) plus `--memory=32g` use only the cpu/memory controllers that rootless podman is typically delegated, and `--layers=false` avoids the layer cache retaining builder stages; `Containerfile`/`.containerignore` exist at the root. The knowledge-base#600 measurement is external and I cannot re-verify it read-only — accepted on the plan's stated evidence. All four target files (AGENTS.md, CLAUDE.md, GEMINI.md, `.claude/skills/qa-sweep/SKILL.md`) have the MSRV paragraph and the gate block with `cargo deny check` immediately before the fuzz line, so the insertion anchors exist; `tests/gate_matrix_parity.rs` only collects `cargo `-prefixed lines from the single `cargo fmt --all --check`-anchored fence, so the new `uv run sphinx-build` line and the separate podman fence cannot break it.

## Boy-scout items — all six are real rot

- B1: fts.rs test comments cite `fts.rs:30-83` / `fts.rs:43-45`; the empty-query mapping actually lives at fts.rs:96–111 and the empty-scope behavior in `filter_sql.rs:60-66`. Stale. Confirmed.
- B2: the doc crediting `validate_segment` with the whitespace rule is `ScopeStore::insert`'s doc (scopes.rs:96–99), while `scope.rs:44-47` explicitly says whitespace is *not* enforced there (only `validate_label`, scopes.rs:127–135). Real.
- B3: `crates/memory-engine-embed/Cargo.toml` `[lints.clippy]` lacks `unwrap_used = "deny"`; me-types (workspace table + `#![cfg_attr(test, allow(clippy::unwrap_used))]` at `src/lib.rs:21`), me-backend-sqlite, memory-engine and me-test-support all have it. Embed has exactly four `tests/*.rs`. Confirmed.
- B4: api.md's "Re-exported Types" Error line lists only `MemoryError`, `Result`, while `crates/memory-engine/src/lib.rs` flat-exports `pub use error::*` (comment at lib.rs:295) — the six typed enums are missing. Real.
- B5: `tests/eval/main.rs:7` and `crates/memory-engine/src/engine/equivalence.rs:6` name `docs/plans/…` files; `docs/plans/` does not exist at base. Real.
- B6: `docs/conf.py:21` `templates_path = ["_templates"]` names no directory (docs/ listing has none); `html_static_path = ["_static"]` likewise (conf.py:32). Real.

## Findings

- [LOW] plan item 6 (#1077) red step: `cargo check -p me-test-support --all-features` is expected to print `warning: unused dependency me-backend-postgres`, but nothing in the repo enables `unused_crate_dependencies` (root `Cargo.toml` `[workspace.lints]` has only `unsafe_code`/`clippy`; `me-test-support/src/lib.rs:29` has only the cfg_attr allow). If the pinned cargo does not emit that warning natively, the red command is silent. The change itself is correct — I read all of `factory.rs`; `PgFactory` (factory.rs:148–196) is `#[cfg(feature = "backend-postgres")]` with `todo!()` bodies that never name `me_backend_postgres`. Fix: implementer confirms the warning text on the first run; if absent, use the `git diff Cargo.lock` one-line removal plus `cargo build --workspace --locked` as the evidence and say the red was silent.
- [LOW] plan item 8 (B2) says "the `ScopeStore` doc"; the offending doc is on `ScopeStore::insert` (scopes.rs:96–99), not the struct doc (scopes.rs:7). Point the commit at the right doc.
- [LOW] #1067 helper spec: state explicitly that the probe is *executed* (stepped), not just prepared — FTS5 parses at the first step — and that a probe `QueryReturnedNoRows` means "query valid" (then the original error is a fault). The plan's "fails the same way" implies this but the helper signature alone doesn't.
- [LOW] Testing section says the fuzz `expect`s "now also check that the probe covers every error a query string causes"; the probe is internal, so the `expect`s can only pin Ok-on-a-healthy-store (as item 1 says, "keeps its `expect`s"). Align the wording.
- [LOW] `lexical_malformed_query_is_empty_not_error` is not in `search/fts.rs`, `search/mod.rs` or `search/strategy.rs`; presumably in `sqlite/search_index.rs`. Confirm the `cargo test -p me-backend-sqlite --all-features` filter actually selects it.
- [LOW] Collateral rot adjacent to B4: `docs/reference/api.md` has a duplicated `### Bootstrap` section (api.md ~:50 and ~:120). Per the repo's collateral-issue rule, file it separately; don't fold it into the #1041 commit.
- [LOW] Counts I could not verify read-only and the implementer owns as evidence: "26 warnings on 9b9d676", the `class="mermaid"` count of 5, the cli/mcp crates keeping their own lint tables without `unwrap_used`, and the knowledge-base#600 podman measurement.

Nothing [BLOCKER], nothing [HIGH]. The plan has explicit **Documentation**, **Testing** and **Verification** sections (all three present and populated; no N/A needed).

## Verdict

LGTM. The two load-bearing designs (#1067's probe rule, #1041's text-derived drift test) check out against the real code, the docs/plans exclusion is the correct single lever for both local and Read the Docs builds, the #1071 flag deviation is sound, and all six boy-scout items are genuine rot at the cited locations. The [LOW] items above are wording/evidence notes for the implementation owner, who runs the gates.

REVIEW COMPLETE

---
# Round 2 (plan 2e4e2b39…, altitude 0267ae88…)

# Round-2 review — #1088 plan revision 2 (delta only)

## Altitude and duplication

Revision 2 is 277 lines / 25438 bytes with 22 top-level tasks; the single fenced block (lines 207–213) is unchanged from r1 and is itself the #1071 product text. The delta is 157 changed lines, every one addressing a round-1 finding; the predecessor (r1, SHA-256 7f1e3bae…) is recorded as the delta baseline and no fallback/provenance inputs are required. Growth of 23 lines buys precision (exact probe predicate, panic-on-unknown-layout, B3/B4 specifics) plus one new item (B7); the plan remains shorter than the change it describes and duplicates no ownership. Disposition "proceed" is justified; I take no override.

Altitude report SHA-256: 0267ae88f8b54011fb88fc3cd56af47cc26bfc81fbbad31e6f1c50c0017b9b4a

## Delta assessment against the code

- **#1067 predicate — now exact and verified.** "Stepped probe; `SqliteFailure` extended code 1 ⇒ no match; probe stepping cleanly or failing any other way ⇒ the *original* error propagates as `StorageError::backend`" closes my round-1 stepping note. The malformed-metadata setup is sound: `ddl.rs:38` has `metadata TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(metadata))`, so a plain `UPDATE` is blocked, and the cited bypass pattern exists verbatim at `facts.rs:1759-1774` (`PRAGMA ignore_check_constraints = ON` → `UPDATE` → OFF). The corrupt-index test now self-checks its injection (raw MATCH fails with extended code ≠ 1) before asserting the `Err`, and the characterization test pins extended code 1 per malformed string on the bundled SQLite. `search_index.rs:31-44` confirms `lexical_search` routes through `fts_search_filtered`, so the battery's `search_malformed_query_yields_empty` exercises the new rule.
- **#1041 parse rule — hardened correctly.** Panic on any unknown variant-line ending, skip blank and non-backticked rows (header/separator), verbatim derived cells, mutation `#[error("dummy")] Dummy,`. This resolves my round-1 separator-row note; the r1 layout analysis (20 variants, `#[from]` drop, `include_str!` paths) still stands.
- **B7 (uv.lock) — internally consistent and properly gated.** `.gitignore:47` does ignore `uv.lock`; `pyproject.toml` sets floors only (`sphinx>=8.0`, …), so the reproducibility rationale is accurate. The clean-export step now archives the whole tree, which is exactly what makes `uv run --locked` work there once the lock is committed; `uv lock --check` is the green. The "pending Michaël's approval, or strike B7 and record the accepted risk" fallback is the right governance for reversing the #21 ignore. Bonus consistency: `.readthedocs.yaml` runs `uv sync --no-dev` + `sphinx-build -W`, so a committed lock also makes RTD reproducible — matching the PR-body note.
- **`suppress_warnings` deletion** — `docs/conf.py:19` confirmed present at base; deleting it on a measured 0 is consistent, and step 0 no longer needs the count-then-restore dance.
- **B3 specifics — verified exactly.** Full reads of all four embed test files: `asymmetric_embedding.rs`, `qwen_integration.rs` and `http_integration.rs` contain `.unwrap()`; `proposer_integration.rs` (112 lines, read whole) uses only `expect`/`expect_err`, which stay allowed. "Three files that unwrap, proposer does not" is correct.
- **B4 specifics — verified.** `lib.rs:180-184` re-exports `PersistenceClassifier` via `pub use me_traits::{…}` (so the api.md "not re-exported" note is indeed wrong); `lib.rs:186-188` re-exports the six typed error enums; `lib.rs:154` is `pub(crate) use me_backend_sqlite::{pool, store}`, so `serialize_embedding`/`deserialize_embedding` are indeed not public root re-exports. `CONTRIBUTING.md:38` is the `uv run sphinx-build -b html docs docs/_build` line.
- **Fuzz wording** now says the `expect`s assert Ok-on-a-healthy-store — my round-1 wording note is resolved. **#1077 red** now cites a measured cargo 1.100.0 `cargo::unused_dependencies` warning — my round-1 mechanism note is resolved. **B2** now names `ScopeStore::insert` — resolved.
- **"Surfaced, not done"** is honest: the api.md claims check out on the doc side (`execute_query` documented as `Result<Vec<SearchResult>>`, duplicated `### Bootstrap`, sync-looking signatures — all confirmed in r1); the `QueryResponse` return type and the `.containerignore`-in-`git archive` point are flagged as unverified ("I think; not measured"), which is the right epistemics for a surfaced-not-done section.

## Findings

- [LOW] plan item 13, gate-line comment: "local only, ci.yml has no docs job" — `.github/workflows/ci.yml:44-58` *does* have a job named `docs:`, but it runs `cargo doc` (rustdoc intra-doc links), not Sphinx. Reword to "ci.yml's docs job is rustdoc-only; no CI job runs the Sphinx build" so a reader grepping ci.yml for `docs` isn't misled. (`gate_matrix_parity` is unaffected either way: it only collects `cargo `-prefixed lines, and the new `uv run --locked …` line doesn't start with `cargo `.)
- [LOW] B4 cites `lib.rs:190` for `PersistenceClassifier`; the `pub use me_traits::{…}` block is at `lib.rs:180-184` (PersistenceClassifier on :183). Line drift only; the substance is right.
- [LOW] `lexical_malformed_query_is_empty_not_error` in `sqlite/search_index.rs`: the file (1144 lines) is the lexical dispatch, but I read only :1-120 and :950-1144, so I did not see the test body itself. The implementer's `cargo test -p me-backend-sqlite --all-features` run confirms selection.
- [LOW] Counts I cannot verify read-only and the implementer owns as evidence: 26 warnings on 9b9d676 (31→26, 9→6), 0 hidden `myst.header` warnings, mermaid `class="mermaid"` count 5, the cargo 1.100.0 warning text, and the #21 provenance of the `uv.lock` ignore.

Nothing [BLOCKER], nothing [HIGH], nothing [MEDIUM] — the delta raises nothing at those levels. Documentation, Testing and Verification sections remain present (plus the new "Surfaced, not done"); no N/A needed. All round-1 points I raised are either resolved by the delta or carried as the [LOW] items above.

## Verdict

LGTM. The revision converts every round-1 precision gap into an exact, code-verified rule (probe predicate, pragma-based tamper, panic-on-unknown-layout, three-file B3 list, B4 re-export facts), adds B7 with proper approval gating and a strike fallback, and keeps the plan at the right altitude with no duplication. The remaining [LOW] items are wording and implementer-owned evidence, not design risks.

REVIEW COMPLETE

---
# Round 3 (plan 0925754c…7313, altitude 1c7769ef…b260)

# Round-3 review — #1088 plan revision 3 (delta only)

## Altitude and duplication

Revision 3 is 284 lines / 26199 bytes with 23 top-level tasks; the single fenced block (lines 213–219) is the unchanged #1071 product text. The delta is small (+7 net lines): five wording fixes from round 2 plus one new boy-scout item (B8). The predecessor (r2, SHA-256 2e4e2b39…) is recorded as the delta baseline; no fallback/provenance inputs are required. The plan remains an implementation plan, shorter than its change, duplicating no ownership. Disposition "proceed" is justified; I take no override.

Altitude report SHA-256: 1c7769ef9d21e1123af14973e8eb5ac9ea8903d40bf52e89857093d42897b260

## Delta assessment against the code

- **Gate-line comment** now reads "local only, no CI job runs Sphinx" (plan.md:189) — accurate: ci.yml's `docs:` job (ci.yml:44-58) is rustdoc-only, verified in round 2. Round-2 LOW resolved.
- **B7** green now also runs `git ls-files --error-unmatch uv.lock` (tracked, not merely present), and the "if struck" clause drops `--locked` in all three places it appears (item 13's red command plan.md:171, the gate line plan.md:189, Verification 3). Internally consistent.
- **B4** now cites "the `pub use me_traits::{…}` block" instead of a line number — the block is at lib.rs:180-184 (verified round 2). Resolved.
- **B8 (new, item 14a)** — real rot, correctly scoped: api.md carries two `### Bootstrap` sections (api.md:50 and api.md:120, confirmed in round 1); the second repeats the two method rows and adds the `BootstrapConfig` fields, so deleting the first is the right cut, with a concrete green (one `### Bootstrap`, Sphinx exits 0) and its own commit. The "Surfaced, not done" bullet correctly drops the Bootstrap duplication from the systemic-drift list since B8 now fixes it.
- **Warning-count chain** 31 (issue) → 29 (7a22c07, two not attributed) → 26 (9b9d676), with the PR body updated to "31→29→26 and 9→6" and "eight boy-scout commits" — internally consistent with B8 added.
- **`.containerignore` note** now cites `podman-build(1)` and keeps the honest hedge ("stdin `-` is not named explicitly"); it sits in "Surfaced, not done" as an accepted risk, which is the right place for an unverified-but-hedged claim.

## Findings

- [LOW] plan.md:172 vs plan.md:178 — the red line attributes the three `reference/*` orphans to **#1086** ("#1086 then listed three `reference/*` orphans") while item 13.3 still says "**#1022** took three". Both cannot be right; r2 said #1022 in both places, so r3 introduced the mismatch. Fix: `gh issue view 1022` and `gh issue view 1086`, keep the issue that actually listed the orphans in both spots (and in the PR body if it cites the counts).
- [LOW] Unverifiable read-only, owned by the implementer as evidence: the 31/29/26 warning counts and the 7a22c07 attribution, the mermaid `class="mermaid"` count of 5, and the podman-build(1) archive-context claim (hedged in the plan, acceptable as an accepted risk).

Nothing [BLOCKER], nothing [HIGH], nothing [MEDIUM] — the delta raises nothing at those levels. Documentation, Testing and Verification sections remain present (Documentation updated for B8; the other two unchanged). All round-2 LOWs are either resolved by this delta (gate comment, B7 clause, B4 citation) or carried as the [LOW] items above; no resolved point is re-raised.

## Verdict

LGTM. Revision 3 is a clean wording-and-one-boy-scout pass: B8 is genuine, correctly scoped rot with its own commit, the B7 strike-fallback is now complete across all three `--locked` sites, and the only real defect is the #1022/#1086 attribution mismatch, which is a one-line citation fix for the implementer to confirm against GitHub.

REVIEW COMPLETE
