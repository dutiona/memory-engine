# Super QA Findings — memory-engine

**Generated:** 2026-06-01 23:34:26
**Scope:** Full 3-phase workflow (`/super-qa`)
**Language:** Rust (4-crate workspace: `memory-engine`, `memory-engine-cli`, `memory-engine-mcp`, `memory-engine-embed`)
**Status:** Phase 1 complete; pre-agent findings recorded. Phase 2 deep-dive pending checkpoint.

---

## Executive Summary

Phase 1 discovery (cheap static signals + compile/lint baseline) surfaced **9 findings before any analysis agent ran**, including **1 blocker** and **1 critical** that break documented build commands.

Compile baseline:

| Build                                                             | Result                                                                    |
| ----------------------------------------------------------------- | ------------------------------------------------------------------------- |
| `cargo check --workspace` (default features)                      | **PASS** (2 lib warnings)                                                 |
| `cargo check --workspace --all-features`                          | **FAIL** — 17 errors                                                      |
| `cargo clippy --workspace --all-targets -- -D warnings` (default) | **FAIL** — 2 denied-lint errors + 226 warnings                            |
| `cargo test --workspace` (default features)                       | **PASS** — 13+ tests, 0 failures; `--all-features` matrix unrunnable (#6) |

Root cause of the `--all-features` failure is a one-line copy-paste duplicate. The clippy gate documented in `CLAUDE.md` as the pre-commit verification gate currently fails on default features — work was committed past a broken gate.

---

## Pre-Agent Findings (Phase 1)

| #   | ID                                | Severity     | Category                | Location                                                     | Title                                                                                                                 | Auto-fix?       |
| --- | --------------------------------- | ------------ | ----------------------- | ------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------- | --------------- |
| 1   | engine/build-archivedup           | **blocker**  | correctness/build       | `src/engine/mod.rs:48-52`                                    | Duplicate `mod archive;` breaks `--all-features` compile                                                              | ✅ yes          |
| 2   | engine/build-notifyinsert         | **critical** | correctness/build       | `src/engine/cognitive.rs:224`                                | `notify_insert` not found on `HnswStrategy` under `ann` feature                                                       | ❌ needs design |
| 3   | engine/msrv-violation             | high         | correctness/portability | `src/engine/activity_filter.rs:99`                           | API used is stable-since 1.91.0 but crate MSRV is 1.85.0                                                              | ❌ judgment     |
| 4   | store/explicit-counter-loop       | high         | style (denied)          | `src/store/scopes.rs:116`                                    | `explicit_counter_loop` denied-lint error                                                                             | ✅ yes          |
| 5   | workspace/clippy-gate-broken      | high         | process                 | workspace                                                    | `cargo clippy -- -D warnings` gate fails on default features                                                          | ⚠️ via #3+#4    |
| 6   | workspace/test-allfeatures-broken | high         | process                 | workspace                                                    | `cargo test --all-features` (CLAUDE.md command) broken via #1                                                         | ⚠️ via #1       |
| 7   | store/dead-code                   | medium       | maintainability         | `src/store/activities.rs:101`, `src/store/checkpoints.rs:47` | Never-used methods (`get`, `list_by_session`, `count_by_session`, `list_recent`)                                      | ✅ yes          |
| 8   | workspace/clippy-warnings         | medium       | style                   | workspace (226 warnings)                                     | Clippy warning backlog (doc backticks, missing `# Errors`, `const fn`, significant-drop temporaries, precision casts) | ✅ mostly       |
| 9   | docs/crate-count-drift            | low          | documentation           | `CLAUDE.md`                                                  | Doc says "3 crates"; workspace has 4 (`memory-engine-embed` missing)                                                  | ✅ yes          |

### Finding details

#### 1 — BLOCKER: Duplicate `mod archive;` breaks `--all-features`

`src/engine/mod.rs` lines 48–52:

```rust
#[cfg(feature = "archive")]
mod archive;

#[cfg(feature = "archive")]
mod archive;   // duplicate declaration, identical cfg gate
```

When `archive` is disabled (default), both blocks are excluded → baseline compiles. When `archive` is enabled (`--all-features` or `--features archive`), the module is declared twice → `E0428` ("name `archive` defined multiple times") plus `E0592`/`E0034` on every method in the module (`archive`, `list_archives`, `verify_archives`, `select_archive_candidates`, `build_pak`, `write_pak_to_disk`, `commit_archive`, `verify_single_archive`, `search_archives_fallback`, `archive_dir`).

**Suggested fix:** delete one of the two `#[cfg(feature = "archive")] mod archive;` blocks (lines 51-52).
**References:** rustc E0428, E0592, E0034.

#### 2 — CRITICAL: `notify_insert` missing on `HnswStrategy`

`src/engine/cognitive.rs:224` calls `notify_insert(...)` on `&search::ann::HnswStrategy`; the method does not exist on that type. Only compiled under the `ann` feature (+ cognitive path), so the default build misses it. Indicates interface drift between the ANN strategy and the cognitive pipeline. Separate from #1 — fixing the duplicate `mod` does not resolve this.
**References:** rustc E0599.

#### 3 — HIGH: MSRV violation

`src/engine/activity_filter.rs:99` uses an API stabilized in Rust 1.91.0, but the crate declares MSRV 1.85.0 (clippy `incompatible_msrv`, configured as a denied lint). Compiles on the local toolchain (≥1.91) but is a latent compile break for anyone on the declared minimum. Resolve by either raising the declared MSRV or replacing the API with a 1.85-compatible equivalent.

#### 4 — HIGH: `explicit_counter_loop` denied-lint error

`src/store/scopes.rs:116` — a manual loop counter `depth` flagged by clippy `explicit_counter_loop`, configured as denied → hard error. Suggested fix: `for (depth, segment) in (1_i64..).zip(segments)`.

#### 5 — HIGH (process): clippy verification gate broken

`CLAUDE.md` documents `cargo clippy --workspace --all-targets -- -D warnings` as a pre-commit gate. It currently fails on default features due to #3 and #4 (2 hard errors). Either the gate is not being run, or lint config drifted after last green run.

#### 6 — HIGH (process): `cargo test --all-features` broken

`CLAUDE.md` lists `cargo test --all-features` as a standard command. It cannot run because `--all-features` does not compile (#1). The `--all-features` matrix is effectively untested.

#### 7 — MEDIUM: dead code in store

`src/store/activities.rs:101` (`get`, `list_by_session`, `count_by_session`) and `src/store/checkpoints.rs:47` (`get`, `list_recent`) are never used. Either wire them into the public API / call sites, gate behind a feature, or remove. (Possibly intentional scaffolding for Phase 4b MCP P1/P2 tools — verify against roadmap before deletion.)

#### 8 — MEDIUM: 226 clippy warnings (default features)

Distribution by module (includes test files): engine 84, core-root 40, store 38, bootstrap 22, search 15, pool 9, inspect 7, scope 6, resume 3, consolidation 1, graph 1. Dominant categories: missing-backticks in docs, missing `# Errors`/`# Panics` doc sections, `const fn` opportunities, "temporary with significant Drop can be early dropped", precision-loss casts (`usize as f64`, `usize as i64`, `i64 as usize`), redundant clones in tests. Most are auto-fixable via `cargo clippy --fix`; the precision casts need judgment.

#### 9 — LOW (docs): crate-count drift

`CLAUDE.md` "Status" and "Workspace verification gate" sections say the workspace has 3 crates. It has 4 — `memory-engine-embed` is missing from the docs.

---

## Phase 1 — Discovery Signals (per module)

> ⚠️ **Caveat:** Warning counts and complexity are from a **default-features** build. Code behind the `archive` and `ann` feature gates (parts of `engine`, `search/ann.rs`, archive code) is **lint-blind** here because `--all-features` does not compile (#1). Treat warning_density for `engine`/`search` as a lower bound.

Raw sub-signals (these discriminate; the profile's composite `comp_score` saturates at 1.0 for 9 modules because it divides deep-nesting _line_ count by function count — see footnote):

| Module        | Files |  LOC | Fns | Fns>60 | Nesting-lines | Clippy warns | Churn (6mo) | Test gap | comp_score† |
| ------------- | ----: | ---: | --: | -----: | ------------: | -----------: | ----------: | -------: | ----------: |
| engine        |    21 | 7853 | 293 |     14 |           596 |           84 |        8665 |     0.71 |        1.00 |
| store         |    12 | 7441 | 304 |      5 |           194 |           38 |        8241 |     0.00 |        0.67 |
| mcp           |    10 | 2804 |  99 |      7 |           134 |            — |        3558 |     0.40 |        1.00 |
| search        |     7 | 2503 | 128 |      2 |           151 |           15 |        2725 |     0.14 |        1.00 |
| inspect       |     7 | 2327 |  67 |      2 |            46 |            7 |        3012 |     0.29 |        0.75 |
| bootstrap     |     6 | 2348 |  95 |      4 |           218 |           22 |        2470 |     0.17 |        1.00 |
| core-root     |     5 | 2448 | 134 |      0 |            45 |           40 |        2694 |     0.20 |        0.34 |
| consolidation |     4 |  921 |  28 |      2 |            53 |            1 |         987 |     0.25 |        1.00 |
| archive       |     4 |  387 |  16 |      1 |            26 |            — |         387 |     0.50 |        1.00 |
| cli           |    16 | 1793 |  53 |      6 |           251 |            — |        1871 |     0.94 |        1.00 |
| conflict      |     2 |  492 |  15 |      1 |            32 |            — |         500 |     0.50 |        1.00 |
| graph         |     2 |  399 |  24 |      0 |            22 |            1 |         401 |     0.50 |        0.92 |
| forgetting    |     2 |  517 |  12 |      2 |             0 |            — |         535 |     0.50 |        0.33 |
| scope         |     2 |  370 |  28 |      0 |            48 |            6 |         372 |     0.50 |        1.00 |
| pool          |     2 |  372 |  24 |      0 |             0 |            9 |         388 |     0.50 |        0.00 |
| resume        |     2 |  266 |   9 |      0 |             0 |            3 |         476 |     0.50 |        0.00 |
| embed         |     2 |  314 |  10 |      2 |            30 |            — |         330 |     0.50 |        1.00 |

† `comp_score = clamp((fns_over_60*2 + deep_nesting_lines + unsafe)/total_fns, 0, 1)`. Saturates because `deep_nesting_lines` ≫ `total_fns` for nested modules. Used only as a tiebreaker; ranking leads with raw signals. `unsafe = 0` everywhere (`unsafe_code = "forbid"`).

Scoring weights (profile): `0.35*complexity + 0.25*warning_density + 0.20*churn + 0.20*test_gap`.

---

## Phase 2 — Deep-dive (IN PROGRESS)

**Scope (user choice): ALL 17 modules, exhaustive.** Executed via Workflow engine `wf_43a240d4-be5` — 85 agents (17 modules × 5 lenses: specialist/security/code-reviewer/test-engineer/doc-consistency). Security lens via `octo:personas:security-auditor` (tier-2 fallback; `qa-security` not dispatchable in this harness → all security findings marked `fallback_used: true, fallback_tier: "octo-fallback"`, ran at xhigh not effort:max). Other 4 lenses on Sonnet.

### Supply-chain audit (main-loop, fallback path — cargo-audit/cargo-deny absent)

| ID                    | Severity | Category               | Title                                                                                                                                                                                                                    |
| --------------------- | -------- | ---------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| sc/dup-deps           | info     | supply-chain           | 11 duplicate transitive dep versions: `getrandom`×3 (0.2.17/0.3.4/0.4.2), `hashbrown`×2 (0.15.5/0.16.1), `rand`/`rand_chacha`/`rand_core`×2 (0.8↔0.9 split). Bloat + minor confusion surface.                            |
| sc/no-advisory-gate   | medium   | supply-chain (process) | No RustSec advisory scanning possible: `cargo-audit`, `cargo-deny`, and `deny.toml` all absent. No advisory/license/ban gate in CI for an embeddable agent-memory crate. Fix: add `cargo-deny` + `deny.toml`, run in CI. |
| sc/no-release-profile | info     | build-system           | No `[profile.release]` tuning (`lto`, `codegen-units=1`, `strip`). Fine for the lib; worth setting for the `cli`/`mcp` binaries.                                                                                         |

Clean: 0 git-sourced deps, 0 wildcard version reqs, 0 external path deps, 0 workspace `build.rs`, no `#![deny(warnings)]` brittle pattern. Editions = 2024 (all crates); MSRV declared 1.85 (violated by finding #3). Yanked-crate check inconclusive (offline). Conditional agents `miri-runner`/`cargo-audit-runner`/`cargo-deny-runner` did not run (tools absent; `unsafe` forbidden so miri N/A).

### Agent findings — CONSOLIDATED

85 agents returned **577 raw findings** (0 skipped). After exact + semantic dedup and merging the 12 Phase-1/supply-chain findings: **582 total**.

**Severity recalibration:** agents self-classified **5 blockers**; primary-source verification reduced this to **1 true blocker** (2 were re-reports of #1; the graph/schema ones are real but `high`, not blockers). 18 findings verified against source/compiler.

| Severity  |   Count | Auto-fixable |
| --------- | ------: | -----------: |
| blocker   |       1 |            1 |
| critical  |       2 |            1 |
| high      |      82 |            5 |
| medium    |     223 |           57 |
| low       |     236 |          114 |
| info      |      38 |           18 |
| **Total** | **582** |      **196** |

**Theme:** testing (180) + documentation (101) dominate (48%); only 6 soundness + 46 correctness (core logic is sound). 36 security findings cluster at the MCP untrusted-input boundary (unbounded allocation, path-traversal). All 40 security findings via `octo:personas:security-auditor` tier-2 fallback (xhigh, not effort:max).

**Verified critical-path findings (act-now):**

| Sev      | Location                          | Title                                                                       | Status                                                    |
| -------- | --------------------------------- | --------------------------------------------------------------------------- | --------------------------------------------------------- |
| blocker  | `src/engine/mod.rs:48-52`         | Duplicate `mod archive;` → `--all-features` fails                           | [V] compiler                                              |
| critical | `src/engine/cognitive.rs:224`     | `notify_insert` missing on `HnswStrategy` (ann)                             | [V] compiler                                              |
| critical | `cli/.../batch_ingest.rs:305`     | batch-ingest opens DB read-only then writes → always fails                  | [V] source                                                |
| high     | `src/graph/memory_graph.rs:82-87` | `remove_node` leaves stale `NodeIndex` in `node_map` (petgraph swap-remove) | [V] source; dormant (archive-gated)                       |
| high     | `src/store/schema.rs:687 vs 819`  | `idx_activities_dedup` 5-col (migrate) vs 4-col (fresh) divergence          | [V] source                                                |
| high     | `src/search/ann.rs:333-341`       | `notify_insert` mutates HNSW before `assert_eq!` → corrupt index on panic   | [V] source; ann-gated                                     |
| high     | `src/scope/tree.rs:61-68,147-168` | `ancestors`/`path_for_id` infinite-loop on cyclic `parent_id`               | [V] source; add visited guard                             |
| medium   | `src/pool/connection_pool.rs:206` | infallible `write()` bypasses read-only Rust guard                          | [V] source; `query_only` pragma backstops → not live vuln |

> **Multi-model debate intentionally skipped.** The skill prescribes Gemini+Codex debate for critical/blocker findings, but all top findings were verified against **primary source** (compiler errors + direct code reads) — a stronger oracle than LLM debate. Debating source-confirmed facts would be verification theater. Debate remains available on request for any specific finding where exploitability/severity is a genuine judgment call (e.g., the MCP symlink path-traversal in the high tier).

**Full detail:** [super-qa-phase2-detail-2026-06-01-233426.md](super-qa-phase2-detail-2026-06-01-233426.md) — all 82 highs, category/module buckets, 54-item refactoring backlog.
**Machine-readable:** [super-qa-consolidated-2026-06-01-233426.json](super-qa-consolidated-2026-06-01-233426.json) (582 findings, full fields).
