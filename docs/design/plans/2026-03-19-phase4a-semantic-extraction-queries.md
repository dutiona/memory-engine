# Phase 4a: Semantic Extraction Queries — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a fluent `MemoryQuery` builder that composes existing search primitives into high-level extraction queries, exposing two genuinely new capabilities (temporal period filtering and importance/pinned pre-filtering) that are currently unreachable through the `engine.query()` API.

**Architecture:** A builder struct lives in a new `src/search/query.rs` module. It constructs and dispatches to existing `SearchQuery`, `FactStore` methods, and scope resolution — composing rather than duplicating. Each filter dimension is optional; filters combine with AND semantics. The builder borrows `&MemoryEngine` at execution time, keeping it decoupled from engine internals.

**Tech Stack:** Rust, chrono, existing MemoryEngine/SearchQuery/FactStore/ScopeTree

**Branch:** `feat/phase4a-semantic-extraction` (new branch from main)

**Issue:** #41

**Design Doc:** This document.

---

## Context

Phase 3b delivered temporal memory, pinned facts, importance scoring, and scheduling. These capabilities exist at the store level (`FactStore::list_pinned`, `list_by_importance_score`, `list_due`) and partially at the engine facade (`engine.list_due`, `engine.pin_fact`), but there is no unified query API that composes them with the hybrid search pipeline.

Today, consumers who want "all semantic facts in scope X with importance > 0.7 valid during period [A, B)" must manually:

1. Resolve scope IDs via `ScopeQuery`
2. Call `engine.query()` for text/vector search
3. Post-filter by importance score
4. Post-filter by temporal range (the existing `valid_at` is a point-in-time cutoff, not a period)

`MemoryQuery` eliminates this manual composition with a fluent builder that handles scope resolution, temporal ranges, importance thresholds, pinned filtering, and hybrid search in a single call.

### What's genuinely new (not just sugar)

1. **Temporal period filtering** — `SearchQuery.valid_at` is a point-in-time cutoff (facts valid at instant T). `MemoryQuery.period(start, end)` is a range query: facts whose validity window `[t_valid, t_invalid)` **overlaps** with `[start, end)`. This requires a new SQL predicate, not just setting `valid_at`.

2. **Importance/pinned pre-filtering through query()** — Currently `list_by_importance_score()` and `list_pinned()` live on `FactStore` and are unreachable via the engine's `query()` path. `MemoryQuery` composes these as pre-filters intersected with search results.

### What's out of scope

- LLM-in-the-loop semantic extraction (speculative — defer)
- Temporal intent bias (Phase 5b per debate synthesis)
- Reranker trait (separate issue #42)
- Graph traversal queries (future extension)

---

## Design Decisions

### D1: Builder struct vs trait-based query

**Decision:** Concrete `MemoryQuery` struct with fluent builder methods.

**Rationale:** Follows the same pattern as `AddFactOptions` — a plain struct with optional fields. No need for trait indirection; there's only one implementation and no consumer-pluggable behavior. A trait would be premature abstraction.

### D2: Execution model — `execute(&engine)` vs `engine.execute_query(&q)`

**Decision:** `engine.execute_query(&MemoryQuery)` method on `MemoryEngine`.

**Rationale:** The builder needs access to engine internals (scope tree, connection pool, vector strategy). Rather than exposing these as public API for `execute(&engine)`, we add a method on `MemoryEngine` that accepts the builder. This keeps engine internals private. The builder is just data — the engine interprets it.

### D3: Period overlap semantics

**Decision:** A fact with validity `[t_valid, t_invalid)` overlaps period `[start, end)` when:

```
(t_valid IS NULL OR t_valid < end) AND (t_invalid IS NULL OR t_invalid > start)
```

NULL `t_valid` means "valid since creation" (unbounded start). NULL `t_invalid` means "still valid" (unbounded end). This is standard interval overlap with half-open intervals.

### D4: Composition strategy for importance/pinned + search

**Decision:** Two-phase approach:

1. Run hybrid search (FTS + vector) with scope/fact_type filters → get `Vec<SearchResult>`
2. Post-filter results by importance threshold and pinned flag

**Rationale:** The alternative (pre-filter by importance, then search within that set) would require passing an ID whitelist into the search SQL, which doesn't compose well with FTS5 and vector search. Post-filtering is simpler and correct — the search over-fetches (existing 3x overfetch), then we filter. For importance-only queries (no text/embedding), we bypass search entirely and go directly to `FactStore::list_by_importance_score`.

### D5: Empty query behavior + temporal safety

**Decision:** `MemoryQuery::new()` with no filters set returns all active (non-expired) facts **that are temporally valid at `Utc::now()`**, sorted by `importance_score DESC`, up to the default limit (50).

**Rationale:** The scheduling model guarantees that future-dated facts (`t_valid > now`) are invisible to regular queries — they only surface via `list_due()`/`resume_context()`. This invariant is tested by `engine::tests::list_due_returns_scheduled_facts`. All `execute_query()` code paths MUST apply the same temporal cutoff: `(t_valid IS NULL OR t_valid <= now) AND (t_invalid IS NULL OR t_invalid > now)` unless a period or explicit `valid_at` overrides it. This applies to non-search branches too (`list_by_importance_score`, `list_pinned`, `list_active`).

### D7: Search mode inference

**Decision:** When `search_mode` is `None`, infer it deterministically from provided inputs:

- `text` + `embedding` → `SearchMode::Hybrid`
- `text` only → `SearchMode::Fts`
- `embedding` only → `SearchMode::Vector`
- Neither → no search (importance/store path)

If `search_mode` is explicitly set but conflicts with available inputs (e.g., `SearchMode::Hybrid` with only text), return `MemoryError::Conflict`.

**Rationale:** Avoids invalid states in the builder. Consumers who know what they want can set the mode explicitly; most will just set text/embedding and let the builder infer.

### D8: `#[non_exhaustive]` on `MatchType`

**Decision:** Add `#[non_exhaustive]` to the `MatchType` enum before adding `ImportanceRank`.

**Rationale:** `MatchType` is a public enum re-exported at the crate root. Adding a variant without `#[non_exhaustive]` is a semver-breaking change (downstream exhaustive matches break). Adding `#[non_exhaustive]` first is itself technically breaking, but it's the right forward-compatible move and can be batched into the same minor version bump.

### D9: Rename `min_importance` to `min_importance_score`

**Decision:** The builder field is `min_importance_score: Option<f64>` (not `min_importance`).

**Rationale:** `Fact` has two importance fields: `importance` (caller hint, 0-1) and `importance_score` (materialized runtime score from the forget policy). The builder filters on `importance_score` via `FactStore::list_by_importance_score()`. The name must be unambiguous.

### D6: Result type

**Decision:** Reuse `SearchResult` when search is involved; return `Vec<Fact>` wrapped in `QueryResult` with metadata when no search is used.

Revised: Use a unified `QueryResult` enum to avoid forcing consumers to match on the query type:

```rust
pub struct QueryResult {
    pub facts: Vec<Fact>,
    pub scores: Option<Vec<(i64, f64)>>,  // fact_id → relevance score (only when search used)
    pub match_types: Option<Vec<(i64, MatchType)>>,  // only when search used
    pub total_before_filter: usize,  // count before importance/pinned filtering
}
```

Revised again: Keep it simple — always return `Vec<SearchResult>`. When no text/embedding search is involved, synthesize `SearchResult` with `score = importance_score` and `match_type = MatchType::Fts` (as a sentinel — we can add `MatchType::None` later if needed). This avoids a new result type entirely.

**Final decision:** Return `Vec<SearchResult>`. For non-search queries (importance-only, pinned-only), wrap `Fact` into `SearchResult { fact, score: fact.importance_score, match_type: MatchType::Fts }`. Add `MatchType::ImportanceRank` variant to distinguish. This is 3 lines of change to the existing enum.

---

## File Structure

### Files to modify

| File                   | Changes                                                                                    |
| ---------------------- | ------------------------------------------------------------------------------------------ |
| `src/search/hybrid.rs` | Add `MatchType::ImportanceRank` variant to the enum                                        |
| `src/search/mod.rs`    | Re-export `query` submodule and `MemoryQuery`                                              |
| `src/engine.rs`        | Add `pub fn execute_query(&self, query: &MemoryQuery) -> Result<Vec<SearchResult>>` method |
| `src/async_engine.rs`  | Add `pub async fn execute_query(&self, query: MemoryQuery) -> Result<Vec<SearchResult>>`   |
| `src/lib.rs`           | Re-export `MemoryQuery` from top-level                                                     |

### Files to create

| File                  | Purpose                                        |
| --------------------- | ---------------------------------------------- |
| `src/search/query.rs` | `MemoryQuery` builder struct + builder methods |

### Documentation to update

| File                             | Changes                                                                        |
| -------------------------------- | ------------------------------------------------------------------------------ |
| `docs/ROADMAP.md`                | Update Phase 4a status                                                         |
| `docs/reference/api.md`          | Document `MemoryQuery`, `execute_query`, `MatchType::ImportanceRank`           |
| `docs/usage/querying-memory.md`  | Add `MemoryQuery` usage alongside `SearchQuery`                                |
| `docs/advanced/hybrid-search.md` | Document new `MatchType` variant, score semantics for non-search results       |
| `CHANGELOG.md`                   | `#[non_exhaustive]` on `MatchType`, new `ImportanceRank` variant (semver note) |

---

## API Surface

```rust
/// Fluent query builder for composing memory extraction queries.
///
/// All filters are optional and combine with AND semantics.
/// An empty query returns all temporally-valid active facts sorted by importance_score.
/// Future-dated facts (t_valid > now) are excluded by default (scheduling model invariant).
#[derive(Debug, Clone, Default)]
pub struct MemoryQuery {
    pub scope: Option<ScopeQuery>,
    pub period_start: Option<DateTime<Utc>>,
    pub period_end: Option<DateTime<Utc>>,
    pub text: Option<String>,
    pub embedding: Option<Vec<f32>>,
    pub search_mode: Option<SearchMode>,  // None → inferred from text/embedding (see D7)
    pub fact_type: Option<FactType>,
    pub min_importance_score: Option<f64>,  // filters on Fact.importance_score (see D9)
    pub pinned_only: bool,
    pub limit: Option<usize>,  // default: 50
    pub valid_at: Option<DateTime<Utc>>,  // point-in-time (mutually exclusive with period)
}

impl MemoryQuery {
    pub fn new() -> Self { Self::default() }

    // --- Scope ---
    pub fn scope_exact(mut self, path: impl Into<String>) -> Self;
    pub fn scope_subtree(mut self, path: impl Into<String>) -> Self;
    pub fn scope_ancestors(mut self, path: impl Into<String>) -> Self;
    pub fn scope_inherited(mut self, path: impl Into<String>) -> Self;

    // --- Temporal ---
    /// Point-in-time filter (existing SearchQuery semantics). Mutually exclusive with period().
    pub fn valid_at(mut self, at: DateTime<Utc>) -> Self;
    /// Period overlap filter (NEW: facts whose [t_valid, t_invalid) overlaps [start, end)).
    /// Mutually exclusive with valid_at().
    pub fn period(mut self, start: DateTime<Utc>, end: DateTime<Utc>) -> Self;

    // --- Semantic search ---
    pub fn text(mut self, query: impl Into<String>) -> Self;
    pub fn embedding(mut self, emb: Vec<f32>) -> Self;
    /// Override inferred search mode. Returns Conflict if incompatible with text/embedding.
    pub fn search_mode(mut self, mode: SearchMode) -> Self;

    // --- Fact filters ---
    pub fn fact_type(mut self, ft: FactType) -> Self;
    /// Filter by materialized importance_score (not the base importance hint).
    pub fn min_importance_score(mut self, threshold: f64) -> Self;
    pub fn pinned_only(mut self) -> Self;

    // --- Pagination ---
    pub fn limit(mut self, n: usize) -> Self;
}
```

Usage:

```rust
// Semantic search within a scope and time period
let results = engine.execute_query(
    &MemoryQuery::new()
        .scope_subtree("user:michael/project:demo")
        .period(week_ago, now)
        .text("deployment issue")
        .min_importance_score(0.3)
        .limit(20)
)?;

// Importance-ranked extraction (no search)
let top_facts = engine.execute_query(
    &MemoryQuery::new()
        .scope_inherited("user:michael")
        .min_importance_score(0.7)
        .limit(10)
)?;

// All pinned facts
let pinned = engine.execute_query(
    &MemoryQuery::new()
        .pinned_only()
)?;
```

---

## Dependency Order

```
Task 1: Add MatchType::ImportanceRank variant
  ↓
Task 2: Create MemoryQuery builder struct + methods (src/search/query.rs)
  ↓
Task 3: Implement period overlap SQL in FactStore (new method)
  ↓
Task 4: Implement execute_query() on MemoryEngine
  ↓
Task 5: Async mirror (AsyncMemoryEngine)
  ↓
Task 6: Integration tests
  ↓
Task 7: Documentation updates
```

---

## Tasks

### Task 1: Add `MatchType::ImportanceRank` variant + `#[non_exhaustive]`

- [ ] Add `#[non_exhaustive]` attribute to `MatchType` enum in `src/search/hybrid.rs` (D8)
- [ ] Add `ImportanceRank` variant to `MatchType`
- [ ] Add `Serialize, Deserialize` derives to `MatchType` (currently missing — needed for API consistency)
- [ ] Audit all `match` arms on `MatchType` in `src/` and `tests/` — add wildcard or explicit `ImportanceRank` handling
- [ ] Update `docs/reference/api.md`, `docs/advanced/hybrid-search.md`, `docs/usage/querying-memory.md` to document the new variant and the semantic change to `SearchResult.score` (which now may represent `importance_score` for non-search results)

**Breaking change note:** Adding `#[non_exhaustive]` + new variant is technically breaking for downstream exhaustive matches. Document in CHANGELOG as a minor-version breaking change.

**Estimated scope:** ~15 lines changed across 4-5 files.

---

### Task 2: Create `MemoryQuery` builder

- [ ] Create `src/search/query.rs` with `MemoryQuery` struct and all builder methods
- [ ] Add `pub mod query;` to `src/search/mod.rs`
- [ ] Re-export `MemoryQuery` from `src/search/mod.rs`
- [ ] Add `pub use search::query::MemoryQuery;` to `src/lib.rs` (alongside existing re-exports)

**Key decisions:**

- All fields `pub` (consistent with `SearchQuery`, `AddFactOptions` — no hidden state)
- `Default` derives naturally (all `None`/`false`)
- Builder methods take `self` by value (move chain pattern), return `Self`

**Estimated scope:** ~80 lines new code, 3 lines changed in existing files.

---

### Task 3: Period overlap SQL in `FactStore`

- [ ] Add `list_active_in_period(start, end, scope_ids, fact_type)` to `FactStore`
- [ ] SQL predicate: `t_expired IS NULL AND (t_valid IS NULL OR t_valid < ?end) AND (t_invalid IS NULL OR t_invalid > ?start)`
- [ ] Optional scope filtering via `json_each` (consistent with existing `list_pinned`, `list_by_importance_score`)
- [ ] Optional `fact_type` filtering (pushed into SQL, not post-filter)
- [ ] Returns `Vec<Fact>` ordered by `importance_score DESC` (most important within period first)
- [ ] Unit tests in `src/store/facts.rs`:
  - Period fully containing fact validity → match
  - Period partially overlapping → match
  - Period completely before/after fact → no match
  - NULL t_valid (unbounded start) → matches any period
  - NULL t_invalid (unbounded end) → matches any period
  - Scope filtering with period
  - fact_type filtering with period

**Key insight:** This is the genuinely new SQL — everything else in the builder composes existing queries. The overlap predicate handles all edge cases: NULL t_valid (unbounded start), NULL t_invalid (unbounded end), and bounded intervals. Timestamps use RFC 3339 format consistent with all other `FactStore` methods.

**Estimated scope:** ~60 lines new code + ~50 lines tests.

---

### Task 4: Implement `execute_query()` on `MemoryEngine`

- [ ] Add `pub fn execute_query(&self, query: &MemoryQuery) -> Result<Vec<SearchResult>>` to `MemoryEngine`
- [ ] **Validation phase** (before any query):
  - `period_start` and `period_end` must both be set or both unset → `MemoryError::Conflict`
  - `valid_at` and `period` are mutually exclusive → `MemoryError::Conflict`
  - `search_mode` vs text/embedding compatibility (D7) → `MemoryError::Conflict`
- [ ] **Temporal safety invariant** (D5 fix — addresses BLOCKER): ALL code paths must apply the default temporal cutoff `now = Utc::now()` to exclude future-dated facts, unless overridden by `valid_at` or `period`. This preserves the scheduling model invariant tested by `engine::tests::list_due_returns_scheduled_facts`.
- [ ] **Execution logic** (pseudocode):
  ```
  0. Validate inputs (see above)
  1. Resolve scope_ids from query.scope (via scope_tree.resolve_query(), same as existing query())
     - If scope set but path not found → return Ok(vec![]) (same as query())
  2. Compute effective_cutoff:
     - If valid_at set → use it
     - If period set → None (period handles its own temporal semantics)
     - Else → Some(Utc::now())  // DEFAULT: hide future-dated facts
  3. Infer search_mode from text/embedding if not explicit (D7)
  4. If text or embedding present → SEARCH PATH:
     a. Build SearchQuery { text, embedding, mode, limit: query.limit * 3, valid_at: effective_cutoff, fact_type, scope }
     b. Call hybrid_search() — do NOT inflate limit again (hybrid_search already 3x overfetches internally)
     c. Post-filter results by: period overlap (if set), min_importance_score, pinned_only
     d. Truncate to query.limit
  5. Else → STORE PATH (no text/embedding):
     a. Fetch candidates via the most selective store method available:
        - If pinned_only → FactStore::list_pinned(scope_ids)
        - Else if min_importance_score set → FactStore::list_by_importance_score(scope_ids, threshold, limit, exclude={})
        - Else if period set → FactStore::list_active_in_period(start, end, scope_ids)
        - Else → FactStore::list_by_importance_score(scope_ids, 0.0, limit, exclude={})
          (NOT list_active() — this applies scope filtering + ordering)
     b. Post-filter ALL candidates by:
        - Temporal cutoff (effective_cutoff): skip facts where t_valid > cutoff or t_invalid <= cutoff
        - Period overlap (if set and not already the primary query)
        - Scope (if set and not already the primary filter)
        - fact_type (if set)
        - min_importance_score (if set and not already the primary filter)
        - pinned_only (if set and not already the primary filter)
     c. Wrap into SearchResult with MatchType::ImportanceRank, score = fact.importance_score
     d. Sort by importance_score DESC, truncate to query.limit
  ```
- [ ] Helper: `fn fact_to_search_result(fact: Fact) -> SearchResult` (wraps with ImportanceRank)
- [ ] Helper: `fn passes_temporal_cutoff(fact: &Fact, cutoff: Option<DateTime<Utc>>) -> bool`

**Key fixes from review:**

- **BLOCKER fix (scope-only):** Store path no longer falls through to `list_active()`. Uses `list_by_importance_score(scope_ids, 0.0, ...)` which respects scope + ordering.
- **BLOCKER fix (future-dated leakage):** All paths apply temporal cutoff via `passes_temporal_cutoff()`.
- **HIGH fix (overfetch):** Search path delegates limit directly — no double inflation. `hybrid_search` handles its own 3x internally.

**Estimated scope:** ~150 lines new code.

---

### Task 5: Async mirror

- [ ] Add `pub async fn execute_query(&self, query: MemoryQuery) -> Result<Vec<SearchResult>>` to `AsyncMemoryEngine`
- [ ] Pattern: `spawn_blocking` with owned clone (same as all other async methods)
- [ ] `MemoryQuery` is `Clone + Send + 'static` (all fields are owned types)

**Estimated scope:** ~15 lines.

---

### Task 6: Integration tests

- [ ] Create test module in `src/search/query.rs` or a new `tests/query_builder.rs`
- [ ] Test cases:
  1. Empty query → returns all active temporally-valid facts (future-dated excluded)
  2. Text search only → same results as `engine.query()`
  3. Scope filter only → only facts in scope (BLOCKER regression test)
  4. Fact type filter only → only facts of that type
  5. Period filter only → facts overlapping the period
  6. Importance threshold → only high-importance_score facts
  7. Pinned only → only pinned facts
  8. Composed: scope + period + text + importance → intersection
  9. Period + point-in-time mutual exclusion → error
  10. Period edge cases: unbounded t_valid, unbounded t_invalid
  11. Empty results (no matches) → empty vec, no error
  12. **Future-dated facts invisible** (BLOCKER regression test): insert fact with t_valid in future, verify empty query and scope-only query both exclude it
  13. Search mode inference: text-only → Fts, embedding-only → Vector, both → Hybrid
  14. Search mode conflict: explicit Hybrid with only text → error
  15. Default limit (50) applied when limit not set
- [ ] Async integration test (feature-gated)

**Estimated scope:** ~250 lines of test code.

---

### Task 7: Documentation updates

- [ ] Update `docs/ROADMAP.md` — Phase 4a status
- [ ] Update `docs/reference/api.md` — `MemoryQuery` builder, `execute_query`, `MatchType::ImportanceRank`
- [ ] Update `docs/usage/querying-memory.md` — add `MemoryQuery` usage examples alongside `SearchQuery`
- [ ] Update `docs/advanced/hybrid-search.md` — document `MatchType::ImportanceRank` and that `SearchResult.score` may now represent `importance_score` for non-search results
- [ ] Ensure doc comments on all public items (builder methods, struct, execute_query)
- [ ] CHANGELOG entry: `#[non_exhaustive]` on `MatchType` + new `ImportanceRank` variant (semver note)

---

## Operational Steps

- [ ] **Worktree** — Create `feat/phase4a-semantic-extraction` worktree branch
- [ ] **Plan issue** — Publish plan as GitHub issue comment on #41
- [ ] **PR** — Commit, push, open PR referencing #41
- [ ] **Review** — Invoke `/super-review` for multi-model review
- [ ] **Merge** — Squash merge into main after review converges
