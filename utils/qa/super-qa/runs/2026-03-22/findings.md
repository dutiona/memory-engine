# Super QA Findings -- memory-engine

Generated: 2026-03-22
Tool: /super-qa (full 3-phase workflow, all 12 modules)
Model: Claude Opus 4.6 (1M context)

## Executive Summary

The memory-engine codebase is **remarkably clean** for its size (60 files, ~19K LOC Rust):

- 0 clippy warnings (pedantic + nursery enabled)
- `unsafe_code = "forbid"` -- zero unsafe surface
- Edition 2024, rust-version 1.85
- 36/49 source files have inline tests
- No yanked deps, no wildcard versions, no build.rs, no git deps
- No flaky test patterns detected

The findings are concentrated in **design/maintainability** and **testing gaps**, not correctness or security. One actual bug was found (schema version mismatch in restore).

## Summary

| Severity  | Count  | Auto-fixable | Report-only |
| --------- | ------ | ------------ | ----------- |
| Blocker   | 0      | 0            | 0           |
| Critical  | 0      | 0            | 0           |
| High      | 9      | 5            | 4           |
| Medium    | 24     | 13           | 11          |
| Low       | 27     | 14           | 13          |
| Info      | 10     | 2            | 8           |
| **Total** | **70** | **34**       | **36**      |

## Findings by Category

| Category      | Count | High | Med | Low | Info |
| ------------- | ----- | ---- | --- | --- | ---- |
| Correctness   | 7     | 1    | 4   | 2   | 0    |
| Security      | 8     | 0    | 3   | 4   | 1    |
| Design        | 16    | 3    | 7   | 6   | 0    |
| Testing       | 22    | 3    | 8   | 10  | 1    |
| Documentation | 14    | 2    | 5   | 5   | 2    |
| Performance   | 3     | 0    | 0   | 2   | 1    |
| Supply Chain  | 0     | 0    | 0   | 0   | 0    |

---

## High Findings (9)

### H1: restore/correctness-schema-ver [AUTO-FIX]

- **Location:** src/inspect/restore.rs:20
- **Category:** correctness
- **Agents:** specialist, design-reviewer, doc-checker (3/5 agree)
- **Description:** restore.rs duplicates `CURRENT_SCHEMA_VERSION` as 5, but schema.rs defines it as 6. Snapshot validation rejects valid v6 snapshots. **This is a real bug.**
- **Fix:** Import from `crate::store::schema` instead of duplicating. Make constants `pub(crate)`.

### H2: engine/design-god-module [REPORT-ONLY]

- **Location:** src/engine.rs:1-3573
- **Category:** design
- **Description:** engine.rs is 3573 LOC, mixing ingest, query, consolidation, forgetting, conflict, graph, scheduling, pinning, inspection, dump/restore, bootstrap, config, and scope. 6 functions >60 LOC.
- **Fix:** Split into partial impl files: engine/ingest.rs, engine/query.rs, etc.

### H3: engine/design-too-many-params [REPORT-ONLY]

- **Location:** src/engine.rs:328-422
- **Category:** design
- **Description:** add_fact takes 8 parameters with `#[allow(clippy::too_many_arguments)]`.
- **Fix:** Introduce AddFactRequest builder struct.

### H4: lib/design-all-pub [REPORT-ONLY]

- **Location:** src/lib.rs:23-48
- **Category:** design
- **Description:** All internal modules (pool, store, graph, scope, etc.) are `pub`, exposing internals that should be behind the MemoryEngine facade.
- **Fix:** Change to `pub(crate)` for internal modules.

### H5: readme/documentation-broken-example [AUTO-FIX]

- **Location:** README.md:39-58
- **Category:** documentation
- **Description:** README Quick Example won't compile: add_fact missing 7th parameter (classifier), SearchQuery missing rerank_depth field.
- **Fix:** Add `None` as classifier arg, add `rerank_depth: None` field.

### H6: traits/testing-no-tests [AUTO-FIX]

- **Location:** src/traits.rs:1-211
- **Category:** testing
- **Description:** traits.rs has no inline test module. ForgetPolicy::validate() has 5 error paths untested at unit level.
- **Fix:** Add #[cfg(test)] with validate() tests, object safety checks, default values.

### H7: search-query/testing-no-tests [AUTO-FIX]

- **Location:** src/search/query.rs:1-188
- **Category:** testing
- **Description:** MemoryQuery builder with 15 methods and zero unit tests.
- **Fix:** Add tests for new() defaults, effective_limit(), has_search(), has_period(), builder chaining.

### H8: proptest/testing-unused-dep [REPORT-ONLY]

- **Location:** Cargo.toml
- **Category:** testing
- **Description:** proptest declared as dev-dependency but never used. Key candidates: embedding roundtrip, cosine_similarity properties, rrf_merge output completeness.
- **Fix:** Add proptest tests for embedding serde roundtrip, cosine_similarity bounds, rrf_merge completeness.

### H9: insta/testing-unused-dep [AUTO-FIX partial]

- **Location:** Cargo.toml
- **Category:** testing
- **Description:** insta (snapshot testing) declared but never used. Candidates: EngineSnapshot JSON, FactExplanation, EngineStatistics.
- **Fix:** Add insta::assert_json_snapshot! for key output structures.

---

## Medium Findings (24)

### M1: restore/correctness-surfaced-at [AUTO-FIX]

- **Location:** src/inspect/restore.rs:242-276
- **Category:** correctness
- **Description:** Restore INSERT for facts omits `surfaced_at` column. Restored facts lose their surfaced_at timestamp, causing duplicate surface events.

### M2: restore/correctness-event-debug [AUTO-FIX]

- **Location:** src/inspect/restore.rs:222
- **Category:** correctness
- **Description:** Event type serialized via Debug format instead of matching EventStore convention. Fragile if enum variant names diverge.

### M3: hybrid/soundness-rrf-truncation [AUTO-FIX]

- **Location:** src/search/hybrid.rs:64-66
- **Category:** correctness
- **Description:** `rank as u32` silently wraps on >4B candidates. Use `u32::try_from(rank).unwrap_or(u32::MAX)`.

### M4: ann/soundness-assert-panic [AUTO-FIX]

- **Location:** src/search/ann.rs:119-123, 255
- **Category:** correctness
- **Description:** assert_eq! in production code panics if HNSW crate changes ID assignment. Should return error.

### M5: traits/correctness-weight-validation [AUTO-FIX]

- **Location:** src/traits.rs:184-210
- **Category:** correctness
- **Description:** ForgetPolicy::validate() doesn't check weights sum to 1.0. Non-unit sums produce importance scores outside [0,1].

### M6: store-schema/security-vacuum-path [REPORT-ONLY]

- **Location:** src/store/schema.rs:289-291
- **Category:** security
- **Description:** VACUUM INTO uses string interpolation. Null bytes could bypass escaping. Path from consumer API, not end-user input.

### M7: inspect-dump/security-vacuum-path [AUTO-FIX]

- **Location:** src/inspect/dump.rs:137-139
- **Category:** security
- **Description:** Same VACUUM INTO pattern in dump_sqlite(). Add null byte validation.

### M8: inspect-restore/security-unbounded-deser [AUTO-FIX]

- **Location:** src/inspect/restore.rs:66-76
- **Category:** security
- **Description:** serde_json::from_reader with no size limit. Crafted snapshot causes OOM. Add file size check.

### M9: consolidation/security-n2-dedup [AUTO-FIX]

- **Location:** src/consolidation/dedup.rs:30-31
- **Category:** security + performance
- **Description:** O(N^2) dedup loads ALL active facts. 100K facts = 300MB + 10B comparisons. No limit. Add safety cap.

### M10: consolidation/security-n2-cluster [AUTO-FIX]

- **Location:** src/consolidation/cluster.rs:38-41, 91-125
- **Category:** security + performance
- **Description:** O(N^2) greedy clustering. Same concern as M9. Add safety cap.

### M11: engine/design-constructor-proliferation [REPORT-ONLY]

- **Location:** src/engine.rs:94-192
- **Category:** design
- **Description:** 6 constructors instead of a builder pattern. open_memory_with_config is subset of open_memory_with.

### M12: lib/style-glob-reexports [AUTO-FIX]

- **Location:** src/lib.rs:43-48
- **Category:** design
- **Description:** `pub use error::*` and `pub use types::*` dump everything into crate root.

### M13: types/design-stringly-relation [REPORT-ONLY]

- **Location:** src/types.rs:111-121
- **Category:** design
- **Description:** Edge.relation_type is bare String but only uses 3 fixed values.

### M14: error/design-stringly-errors [REPORT-ONLY]

- **Location:** src/error.rs:1-42
- **Category:** design
- **Description:** 6 MemoryError variants are String catch-alls. Callers can't match on specific failures.

### M15: traits/design-embed-dupe [REPORT-ONLY]

- **Location:** src/traits.rs:25-39
- **Category:** design
- **Description:** SummaryGenerator::embed duplicates EmbeddingProvider::embed. Forces implementors to duplicate logic.

### M16: store/refactoring-sql-columns [AUTO-FIX]

- **Location:** src/store/facts.rs (12+ methods)
- **Category:** refactoring
- **Description:** 18-column SELECT list copy-pasted in 12+ methods. Extract const FACT_COLUMNS.

### M17: engine/refactoring-scope-dup [REPORT-ONLY]

- **Location:** src/engine.rs:431-502, 583-647
- **Category:** refactoring
- **Description:** Scope resolution + ANN strategy dispatch duplicated across query() and execute_query().

### M18: engine/design-synthetic-fact-dup [REPORT-ONLY]

- **Location:** src/engine.rs:345-376, bootstrap/mod.rs:207-228
- **Category:** design
- **Description:** Synthetic Fact{id:0} construction for PersistenceClassifier duplicated.

### M19: traits/documentation-stale-phase2 [AUTO-FIX]

- **Location:** src/traits.rs (3 locations)
- **Category:** documentation
- **Description:** Doc comments say "(Phase 2)" on ConsolidationConfig, ConflictArbiter, SummaryGenerator. Phase 2 is complete.

### M20: inspect/documentation-dumpformat [AUTO-FIX]

- **Location:** src/inspect/types.rs:143-144
- **Category:** documentation
- **Description:** DumpFormat::Sqlite doc says "file-backed engines only" but works for in-memory too.

### M21: engine/documentation-unreachable [REPORT-ONLY]

- **Location:** src/engine.rs:574
- **Category:** documentation
- **Description:** unreachable!() in infer_search_mode is a latent panic with no useful error context.

### M22: readme/documentation-missing-traits [AUTO-FIX]

- **Location:** README.md:99-103
- **Category:** documentation
- **Description:** README lists 3 traits but API now has 5 (missing PersistenceClassifier, Reranker).

### M23: test-infra/testing-duplicated-helpers [REPORT-ONLY]

- **Location:** 15+ test modules
- **Category:** testing
- **Description:** make_fact/MockEmbedder duplicated across 8+ test modules. Create shared test_utils.

### M24: doctest/testing-all-ignored [REPORT-ONLY]

- **Location:** async_engine.rs, query.rs
- **Category:** testing
- **Description:** Both doc examples use `rust,ignore`. Zero runnable doc-tests in the crate.

---

## Low Findings (27)

### L1: forgetting/soundness-i64-f64 -- Precision loss in access_count/degree cast (theoretical)

### L2: bootstrap/soundness-total-f64 -- usize to f64 in avg_importance (theoretical)

### L3: engine/correctness-hnsw-unwrap -- unwrap on hnsw_strategy behind guard (fragile, not wrong)

### L4: engine/modern-rust-wildcard-match -- Wildcard arm in SearchMode match hides future variants [AUTO-FIX]

### L5: vector/performance-alloc-in-loop -- Missing Vec::with_capacity in vector_search [AUTO-FIX]

### L6: inspect-dump/security-toctou -- TOCTOU race in dump_sqlite self-protection

### L7: bootstrap-parse/security-unbounded-jsonl -- Unbounded JSONL line parsing [AUTO-FIX]

### L8: forgetting/security-prune-all -- Prune loads all active facts (no cap)

### L9: search-vector/security-brute-all -- Brute-force streams all embeddings (mitigated by ann feature)

### L10: types/security-unbounded-value -- Unbounded serde_json::Value in Event/Fact payloads

### L11: types/design-importance-confusion -- importance vs importance_score naming

### L12: types/refactoring-no-builder -- NewFact has 14 fields with no builder

### L13: traits/design-send-sync -- EmbeddingProvider lacks Send+Sync unlike Reranker

### L14: store/refactoring-schema-1919 -- schema.rs at 1919 LOC (migrations inline)

### L15: consolidation/design-summary-to-fact -- Fake Fact construction for SummaryGenerator

### L16: pool/design-indefinite-block -- Read pool blocks forever with no timeout

### L17: engine/refactoring-bootstrap-scope -- Bootstrap scope resolution duplicated [AUTO-FIX]

### L18: async/style-boilerplate -- AsyncMemoryEngine 592 lines of spawn_blocking boilerplate

### L19: deps/design-bundled-full -- rusqlite bundled-full includes unused features [AUTO-FIX]

### L20: engine/design-pub-crate-pool -- pool field is pub(crate) for one test [AUTO-FIX]

### L21: search/design-individual-loads -- hybrid_search loads facts one-by-one vs batch

### L22: store/style-enum-conversion -- Standalone to_str/from_str duplicates Display/FromStr [AUTO-FIX]

### L23: forgetting/design-conflict-for-validation -- Conflict error used for validation failures

### L24: engine/documentation-undoc-unwrap -- Two unwrap() calls lack expect message [AUTO-FIX]

### L25: search/documentation-bare-unwrap -- vector_search uses .unwrap() vs .expect() [AUTO-FIX]

### L26: explain/documentation-fragile-unwrap -- unwrap after is_none in determine_state [AUTO-FIX]

### L27: strategy/documentation-stale-comment -- "Not wired into the engine yet" is stale [AUTO-FIX]

---

## Info Findings (10)

### I1: types/modern-rust-partialeq -- Fact derives PartialEq not Eq (correct due to f64)

### I2: store/modern-rust-iter-collect -- Manual loops could use rows.collect() [AUTO-FIX]

### I3: store-facts/security-expect-serialize -- .expect() on infallible serde_json::to_string (10 sites) [AUTO-FIX]

### I4: store-schema/security-migration-cast -- Migration index `i as u32` (5 migrations, harmless)

### I5: pool/security-condvar-expect -- expect after Condvar wait (parking_lot has no spurious wakeups)

### I6: store-facts/security-blake3-truncation -- 128-bit truncation (sufficient for content addressing)

### I7: resume/documentation-kb-stubs -- Phase 5 placeholder field in public API

### I8: lib/documentation-brute-force -- Crate doc omits optional HNSW [AUTO-FIX]

### I9: flaky/testing-none-detected -- No flaky test patterns (positive finding)

### I10: scope/correctness-depth-i64 -- ScopeNode.depth is i64 (matches SQLite, correct)

---

## Refactoring Backlog

| ID      | Pattern              | Location                   | Motivation                           | Scope                  | Type   |
| ------- | -------------------- | -------------------------- | ------------------------------------ | ---------------------- | ------ |
| REF-H2  | Split God Module     | engine.rs                  | 3573 LOC, 12+ concerns               | 1 file -> 10 files     | HEAVY  |
| REF-H3  | Builder Pattern      | engine.rs:add_fact         | 8 params, breaking change            | 3 files                | HEAVY  |
| REF-H4  | Restrict Visibility  | lib.rs                     | All pub defeats facade               | 1 file + consumers     | HEAVY  |
| REF-M11 | Builder Pattern      | engine.rs constructors     | 6 variants                           | engine.rs              | HEAVY  |
| REF-M13 | Newtype Enum         | types.rs:Edge              | Stringly-typed relation_type         | types + store + engine | HEAVY  |
| REF-M14 | Split Error Variants | error.rs                   | String catch-alls                    | error.rs + all callers | HEAVY  |
| REF-M15 | Remove Trait Method  | traits.rs:SummaryGenerator | embed() duplicates EmbeddingProvider | traits + consolidation | HEAVY  |
| REF-M17 | Extract Helper       | engine.rs                  | Scope resolution duplication         | engine.rs              | MEDIUM |
| REF-M18 | Extract Constructor  | engine.rs + bootstrap      | Synthetic Fact duplication           | 2 files                | MEDIUM |
| REF-M23 | Extract Test Utils   | 15+ test modules           | Duplicated helpers                   | 1 new + 15 modified    | HEAVY  |
| REF-L12 | Builder Pattern      | types.rs:NewFact           | 14 fields, no builder                | types.rs + tests       | MEDIUM |
| REF-L14 | Extract Migrations   | schema.rs                  | 1919 LOC                             | 1 file -> directory    | MEDIUM |
| REF-L18 | Proc Macro/Remove    | async_engine.rs            | 592 lines boilerplate                | 1 file                 | HEAVY  |

---

## Supply Chain Audit

- **No yanked versions** in Cargo.lock
- **No wildcard versions** in Cargo.toml
- **No build.rs files** in project
- **No git dependencies** -- all from crates.io
- **Duplicate dependency versions** (dev-deps only, not production):
  - getrandom: 0.2, 0.3, 0.4 (via rand 0.8/0.9, tempfile)
  - hashbrown: 0.15, 0.16 (via rusqlite, petgraph)
  - rand: 0.8, 0.9 (direct dev-dep vs proptest)
- **cargo-audit/cargo-deny not installed** -- recommend installing for CI

## Fix Summary

- Auto-fixable findings: 34
- Report-only findings: 36
- Estimated auto-fix commits: ~20 (grouped by category)
