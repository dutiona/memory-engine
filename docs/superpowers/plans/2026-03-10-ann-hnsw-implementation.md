# ANN HNSW Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement HNSW-based approximate nearest neighbor search behind the existing `VectorSearchStrategy` trait, with automatic dispatch based on fact count.

**Architecture:** The `hnsw` crate (rust-cv, v0.11.0) provides a pure-Rust HNSW graph that owns its data (no lifetime issues, no unsafe needed). A new `HnswStrategy` wraps the HNSW index with ID mappings and a tombstone set for lazy deletion. The engine dispatches between `BruteForce` and `HnswStrategy` based on `SearchConfig.ann_threshold` vs active fact count at query time. The `ann` feature flag gates the dependency.

**Tech Stack:** `hnsw` 0.11.0, `space` 0.17, `rand` 0.8 (SmallRng, feature `small_rng`), `parking_lot` (RwLock for concurrent search)

**Prerequisite:** PR #27 (benchmark baseline + strategy trait) must be merged to `main` before starting this work. The branch for this plan stacks on top of PR #27's changes (`VectorSearchStrategy` trait, `BruteForce`, `SearchConfig` in `src/search/strategy.rs`).

**Closes:** #3, #26, #30

**References:**

- Issue #3: Original ANN concern
- Issue #26: Benchmark baseline + strategy trait plan
- Issue #30: HNSW implementation tracking
- Issue #12: LanceDB decision document (stays open)
- Baseline PR: https://github.com/dutiona/memory-engine/pull/27
- Benchmark data: PR #27 description

## Review Changelog

### Round 3 (Codex GPT-5.4 R2 findings)

Addressed 3 high findings + 1 open question from Codex R2:

- **R3-1**: Removed `if results.len() >= limit { break; }` early-exit in Phase 2. All overfetched candidates are now scored, sorted by exact cosine similarity, then truncated to `limit`. This ensures the true best-by-exact-score results are returned.
- **R3-2**: Restructured widening loop to check post-filter result count (not pre-filter candidate count). The entire HNSW fetch → post-filter → score cycle now repeats with doubled `ef_search`. Added actual brute-force fallback: if HNSW can't satisfy after `MAX_WIDEN_ATTEMPTS`, falls back to `vector_search()`.
- **R3-3**: Replaced `load_embedding(conn, fact_id, embed_dim)` with `load_embedding(conn, fact_id, self.embed_dim)` — uses the validated field, not the caller-supplied parameter.
- **R3-4**: Clarified `resolve_conflict` Replace path: replacement insertion goes through `add_fact()` which already calls `notify_insert()`. Only `notify_expire(old_id)` needs adding in Task 8.

### Round 2 (Codex GPT-5.4 + Gemini)

Addressed 18 findings (3 blockers, 6 high, 7 medium, 2 low):

- **B1**: Added explicit prerequisite — PR #27 must merge first. Plan branches from merged main.
- **B2**: Fixed dispatch — Task 7 now implements query-time dispatch: count active facts, compare against `ann_threshold`, delegate to HNSW or fall back to brute-force.
- **B3**: Fixed transaction safety — mutation hooks now fire AFTER successful DB commit, not inside the transaction. Rollback cannot desync.
- **H4**: Added widening loop — if post-filter yields < limit results, re-query with 2x ef_search, up to 3 attempts. Final fallback to brute-force if HNSW can't satisfy.
- **H5**: Results now sorted by descending score after exact re-scoring.
- **H6**: Replaced `debug_assert` with runtime assertion + explicit `HashMap<usize, i64>` for ID mapping. No release-only bugs.
- **H7**: Fixed deps — `parking_lot` already in Cargo.toml; `rand` needs `features = ["small_rng"]`; `blake3` and `tempfile` already in dev-deps.
- **H8**: `embed_dim` now validated against `self.embed_dim` in search. Field is used.
- **H9**: Added `#[allow(clippy::ptr_arg)]` on Metric impl — `space::Metric` trait requires `&P` where `P=Vec<f32>`, cannot use `&[f32]`.
- **M10**: Production index now uses `SmallRng::seed_from_u64(42)` for deterministic graph topology. Seed is a tunable constant.
- **M11**: Recall test now uses content-based matching (content strings) instead of SQLite row IDs across databases.
- **M12**: Recall test expanded — 5 diverse queries, average recall >= 0.9, min recall >= 0.7 per query.
- **M13**: CosineMetric now clamps to [0, 2], returns 1.0 for zero-norm vectors (max distance for degenerate input).
- **M14**: Search releases read lock before DB I/O — collect HNSW candidates first, drop lock, then batch filter.
- **M15**: Added tombstone compaction note — rebuild threshold when `tombstones.len() > index_to_fact.len() / 4`.
- **M16**: Added implementation sketch for returning expired IDs from `prune()` and `local_dedup()`.
- **L17**: `unwrap_or_default()` replaced with `?` propagation on `serde_json::to_string`.
- **L18**: Added doc comment on `build_from_db` noting memory proportional to fact count.

---

## File Structure

| File                       | Action | Responsibility                                                                       |
| -------------------------- | ------ | ------------------------------------------------------------------------------------ |
| `Cargo.toml`               | Modify | Add `ann` feature flag + deps (`hnsw`, `space`, `rand`)                              |
| `src/search/ann.rs`        | Create | `CosineMetric`, `HnswIndex`, `HnswStrategy`                                          |
| `src/search/strategy.rs`   | Modify | Add lifecycle hooks to trait (`notify_insert`, `notify_expire`)                      |
| `src/search/mod.rs`        | Modify | Add `pub mod ann;` (cfg-gated) + re-exports                                          |
| `src/engine.rs`            | Modify | Wire `SearchConfig` into `EngineConfig`, build index on open, call hooks on mutation |
| `benches/search_bench.rs`  | Modify | Add HNSW benchmark group + recall comparison                                         |
| `tests/ann_recall_test.rs` | Create | Integration test: HNSW recall vs brute-force oracle                                  |

---

## Chunk 1: Library Integration + Cosine Metric

### Task 1: Add dependencies behind feature flag

**Files:**

- Modify: `Cargo.toml`

- [ ] **Step 1: Add `ann` feature and dependencies**

```toml
# In [dependencies]
hnsw = { version = "0.11", optional = true }
space = { version = "0.17", optional = true }
rand = { version = "0.8", features = ["small_rng"], optional = true }

# In [features]
ann = ["dep:hnsw", "dep:space", "dep:rand"]

# In [dev-dependencies]  (blake3 and tempfile already present)
rand = { version = "0.8", features = ["small_rng"] }
```

Note: `parking_lot` is already a dependency. `blake3` and `tempfile` are already in dev-dependencies. `rand` needs `small_rng` feature for `SmallRng`.

- [ ] **Step 2: Verify it compiles with and without the feature**

Run: `cargo check && cargo check --features ann`
Expected: Both succeed.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build: add ann feature flag with hnsw, space, rand deps"
```

### Task 2: Implement CosineMetric

**Files:**

- Create: `src/search/ann.rs`
- Modify: `src/search/mod.rs`

The `space::Metric` trait (v0.17) requires:

```rust
trait Metric<P> {
    type Unit: Unsigned + Ord + Copy;
    fn distance(&self, a: &P, b: &P) -> Self::Unit;
}
```

Cosine distance = 1 - cosine_similarity, range [0, 2]. For non-negative f32, `to_bits()` preserves total order, yielding a u32 suitable for `Unit`.

- [ ] **Step 1: Write the failing test**

In `src/search/ann.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_metric_identical_vectors() {
        let m = CosineMetric;
        let a = vec![1.0_f32, 0.0, 0.0];
        let b = vec![1.0_f32, 0.0, 0.0];
        assert_eq!(m.distance(&a, &b), 0.0_f32.to_bits());
    }

    #[test]
    fn cosine_metric_orthogonal_vectors() {
        let m = CosineMetric;
        let a = vec![1.0_f32, 0.0, 0.0];
        let b = vec![0.0_f32, 1.0, 0.0];
        assert_eq!(m.distance(&a, &b), 1.0_f32.to_bits());
    }

    #[test]
    fn cosine_metric_preserves_distance_ordering() {
        let m = CosineMetric;
        let query = vec![1.0_f32, 0.0, 0.0];
        let close = vec![0.9_f32, 0.1, 0.0];
        let far = vec![0.0_f32, 1.0, 0.0];
        assert!(m.distance(&query, &close) < m.distance(&query, &far));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --features ann cosine_metric`
Expected: FAIL — `CosineMetric` not defined.

- [ ] **Step 3: Implement CosineMetric**

In `src/search/ann.rs`:

```rust
//! HNSW-based approximate nearest neighbor search.
//!
//! Gated behind the `ann` feature flag.
//! Uses the `hnsw` crate (rust-cv) which owns inserted vectors,
//! avoiding lifetime issues with the HNSW graph.

use space::Metric;

use crate::search::cosine_similarity;

/// Cosine distance metric for the `hnsw` crate.
///
/// Converts cosine distance (1 - similarity) to `u32` via `f32::to_bits()`.
/// For non-negative f32 values, bit representation preserves total order,
/// satisfying `space::Metric`'s `Unit: Ord` requirement.
///
/// Edge cases:
/// - Zero-norm vectors return distance 1.0 (maximum cosine distance for
///   degenerate input, placing them far from all real vectors).
/// - Result clamped to [0, 2] to avoid NaN/negative from floating-point noise.
#[derive(Copy, Clone)]
pub struct CosineMetric;

#[allow(clippy::ptr_arg)] // space::Metric trait requires &P where P=Vec<f32>
impl Metric<Vec<f32>> for CosineMetric {
    type Unit = u32;

    fn distance(&self, a: &Vec<f32>, b: &Vec<f32>) -> u32 {
        let sim = cosine_similarity(a, b);
        // cosine_similarity returns 0.0 for zero-norm vectors.
        // Clamp to [0, 2] to handle floating-point edge cases and NaN.
        let dist = (1.0 - sim).clamp(0.0, 2.0);
        dist.to_bits()
    }
}
```

Add module declaration in `src/search/mod.rs`:

```rust
#[cfg(feature = "ann")]
pub mod ann;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --features ann cosine_metric`
Expected: All 3 pass.

- [ ] **Step 5: Commit**

```bash
git add src/search/ann.rs src/search/mod.rs
git commit -m "feat(search): add CosineMetric for hnsw space::Metric trait"
```

### Task 3: Validate HNSW basics (spike)

This task validates that the `hnsw` crate works as expected: build index, insert, search, verify results are reasonable. This is throwaway test code that stays as a permanent validation.

**Files:**

- Modify: `src/search/ann.rs` (add to tests module)

- [ ] **Step 1: Write the spike test**

```rust
#[test]
fn hnsw_spike_basic_search() {
    use hnsw::{Hnsw, Searcher};
    use rand::rngs::SmallRng;
    use rand::SeedableRng;

    // Build a small index: 5 vectors, dim=4, M=8, M0=16
    // Use seeded RNG for deterministic test results.
    let mut index: Hnsw<CosineMetric, Vec<f32>, SmallRng, 8, 16> =
        Hnsw::new_params_and_prng(
            CosineMetric,
            hnsw::Params::new().ef_construction(100),
            SmallRng::seed_from_u64(42),
        );
    let mut searcher = Searcher::default();

    let vectors = vec![
        vec![1.0, 0.0, 0.0, 0.0],  // id 0: "north"
        vec![0.9, 0.1, 0.0, 0.0],  // id 1: close to north
        vec![0.0, 1.0, 0.0, 0.0],  // id 2: "east"
        vec![0.0, 0.0, 1.0, 0.0],  // id 3: "up"
        vec![-1.0, 0.0, 0.0, 0.0], // id 4: "south" (opposite)
    ];

    for v in &vectors {
        index.insert(v.clone(), &mut searcher);
    }

    // Search for "north" — should find id 0 first, id 1 second
    let query = vec![1.0_f32, 0.0, 0.0, 0.0];
    let mut dest = vec![hnsw::Neighbor::invalid(); 3];
    let results = index.nearest(&query, 24, &mut searcher, &mut dest);

    assert!(results.len() >= 2, "should find at least 2 neighbors");
    // Closest should be id 0 (exact match, distance 0)
    assert_eq!(results[0].index, 0);
    assert_eq!(results[0].distance, 0);
    // Second closest should be id 1
    assert_eq!(results[1].index, 1);
}

#[test]
fn hnsw_is_send_sync() {
    use hnsw::Hnsw;
    use rand::rngs::SmallRng;

    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Hnsw<CosineMetric, Vec<f32>, SmallRng, 16, 32>>();
}
```

- [ ] **Step 2: Run spike tests**

Run: `cargo test --features ann hnsw_spike && cargo test --features ann hnsw_is_send`
Expected: Both pass. If `hnsw_is_send_sync` fails, we need a `Mutex` wrapper (note for Task 5).

- [ ] **Step 3: Commit**

```bash
git add src/search/ann.rs
git commit -m "test(search): spike validating hnsw crate integration"
```

**Note:** If the spike reveals API differences (e.g., `new_params` doesn't exist, or `Neighbor` fields differ), adjust the plan accordingly. The `hnsw` crate API documentation at 42% coverage means some discovery is expected.

---

## Chunk 2: HnswStrategy Implementation

### Task 4: Add lifecycle hooks to VectorSearchStrategy trait

**Files:**

- Modify: `src/search/strategy.rs`

The trait needs hooks so the engine can notify strategies of mutations. Default implementations are no-ops (BruteForce stays unchanged).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn brute_force_lifecycle_hooks_are_noop() {
    let bf = BruteForce;
    // These should compile and do nothing.
    bf.notify_insert(1, &[1.0, 0.0, 0.0]);
    bf.notify_expire(1);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test brute_force_lifecycle`
Expected: FAIL — methods don't exist.

- [ ] **Step 3: Add hooks with default implementations**

In `src/search/strategy.rs`, add to the trait:

```rust
/// Called after a fact is inserted. Strategies that maintain an in-memory
/// index should add the vector. Default: no-op.
fn notify_insert(&self, _fact_id: i64, _embedding: &[f32]) {}

/// Called after a fact is expired (soft-deleted). Strategies that maintain
/// an in-memory index should mark it for exclusion. Default: no-op.
fn notify_expire(&self, _fact_id: i64) {}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test brute_force_lifecycle && cargo test`
Expected: All tests pass (161+).

- [ ] **Step 5: Commit**

```bash
git add src/search/strategy.rs
git commit -m "feat(search): add notify_insert/notify_expire hooks to VectorSearchStrategy"
```

### Task 5: Implement HnswStrategy

**Files:**

- Modify: `src/search/ann.rs`

This is the core implementation. `HnswStrategy` wraps an HNSW index with:

- ID mappings (HNSW index → fact_id and reverse)
- A tombstone set for lazily handling expired facts
- `RwLock` for concurrent read access (search) with exclusive write (insert/expire)

HNSW parameters: M=16, M0=32 (standard values). ef_construction=200 (high quality build). ef_search=100 (tunable later via SearchConfig).

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod hnsw_strategy_tests {
    use super::*;
    use crate::search::strategy::VectorSearchStrategy;
    use crate::store::facts::FactStore;
    use crate::store::schema::{init_schema, open_memory};
    use crate::types::{FactType, NewFact};
    use chrono::Utc;

    const DIM: usize = 4;

    fn setup_with_facts() -> (rusqlite::Connection, Vec<i64>) {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        let store = FactStore::new(&conn, DIM);
        let mut ids = Vec::new();
        let embeddings = vec![
            vec![1.0, 0.0, 0.0, 0.0],
            vec![0.9, 0.1, 0.0, 0.0],
            vec![0.0, 1.0, 0.0, 0.0],
        ];
        for (i, emb) in embeddings.into_iter().enumerate() {
            let fact = NewFact {
                content: format!("fact {i}"),
                content_hash: String::new(),
                embedding: emb.clone(),
                fact_type: FactType::Semantic,
                t_created: Utc::now(),
                t_expired: None,
                t_valid: None,
                t_invalid: None,
                source_event_id: None,
                scope_id: 1,
                importance: 0.5,
                access_count: 0,
                last_accessed: Utc::now(),
                metadata: serde_json::json!({}),
            };
            let id = store.insert(&fact).unwrap();
            ids.push(id);
        }
        (conn, ids)
    }

    #[test]
    fn hnsw_strategy_finds_nearest() {
        let (conn, ids) = setup_with_facts();
        let strategy = HnswStrategy::build_from_db(&conn, DIM).unwrap();

        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let results = strategy.search(&conn, &query, DIM, 2, None, None).unwrap();

        assert_eq!(results.len(), 2);
        // Closest should be fact 0 (exact match)
        assert_eq!(results[0].fact_id, ids[0]);
        // Second closest should be fact 1
        assert_eq!(results[1].fact_id, ids[1]);
    }

    #[test]
    fn hnsw_strategy_notify_insert_updates_index() {
        let (conn, ids) = setup_with_facts();
        let strategy = HnswStrategy::build_from_db(&conn, DIM).unwrap();

        // Insert a new fact into BOTH the DB and the HNSW index.
        // The DB insert is needed because search post-filters via DB lookup.
        let new_emb = vec![0.99, 0.01, 0.0, 0.0];
        let store = FactStore::new(&conn, DIM);
        let new_fact = NewFact {
            content: "new close fact".into(),
            content_hash: String::new(),
            embedding: new_emb.clone(),
            fact_type: FactType::Semantic,
            t_created: Utc::now(),
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            scope_id: 1,
            importance: 0.5,
            access_count: 0,
            last_accessed: Utc::now(),
            metadata: serde_json::json!({}),
        };
        let new_id = store.insert(&new_fact).unwrap();
        strategy.notify_insert(new_id, &new_emb);

        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let results = strategy.search(&conn, &query, DIM, 3, None, None).unwrap();

        let found_ids: Vec<i64> = results.iter().map(|r| r.fact_id).collect();
        assert!(found_ids.contains(&new_id), "newly inserted fact should appear in results");
        assert!(found_ids.contains(&ids[0]), "original closest fact should still appear");
    }

    #[test]
    fn hnsw_strategy_notify_expire_excludes_from_results() {
        let (conn, ids) = setup_with_facts();
        let strategy = HnswStrategy::build_from_db(&conn, DIM).unwrap();

        // Expire fact 0
        strategy.notify_expire(ids[0]);

        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let results = strategy.search(&conn, &query, DIM, 2, None, None).unwrap();

        let found_ids: Vec<i64> = results.iter().map(|r| r.fact_id).collect();
        assert!(!found_ids.contains(&ids[0]), "expired fact should be excluded");
    }
}
```

- [ ] **Step 2: Run to verify tests fail**

Run: `cargo test --features ann hnsw_strategy`
Expected: FAIL — `HnswStrategy` not defined.

- [ ] **Step 3: Implement HnswStrategy**

In `src/search/ann.rs`, add:

```rust
use std::collections::HashSet;

use hnsw::{Hnsw, Searcher};
use parking_lot::RwLock;
use rand::rngs::SmallRng;
use rusqlite::Connection;

use crate::error::Result;
use crate::search::strategy::VectorSearchStrategy;
use crate::search::vector::VectorResult;
use crate::store::deserialize_embedding;
use crate::types::FactType;

/// HNSW approximate nearest neighbor strategy.
///
/// Maintains an in-memory HNSW graph built from the fact store.
/// Provides O(log N) approximate search vs brute-force O(N).
///
/// # Concurrency
///
/// Uses `RwLock<HnswInner>`: multiple concurrent searches (read lock),
/// exclusive access for insert/expire (write lock).
pub struct HnswStrategy {
    inner: RwLock<HnswInner>,
    embed_dim: usize,
}

struct HnswInner {
    /// The HNSW graph. M=16, M0=32 are standard HNSW parameters.
    /// Uses seeded SmallRng for deterministic graph topology.
    index: Hnsw<CosineMetric, Vec<f32>, SmallRng, 16, 32>,
    /// Maps HNSW item index (usize) → fact_id (i64).
    /// Uses Vec for O(1) lookup; the `hnsw` crate assigns sequential indices.
    /// Validated at insert time with a runtime assertion (not debug-only).
    index_to_fact: Vec<i64>,
    /// Tombstone set: expired fact_ids excluded from search results.
    /// When `tombstones.len() > index_to_fact.len() / 4`, consider full
    /// rebuild to reclaim graph quality (deferred — tracked in issue #31).
    tombstones: HashSet<i64>,
}

/// Over-fetch factor for HNSW search to account for post-filtering.
const OVERFETCH_FACTOR: usize = 3;

/// Default ef_search parameter (candidate pool size during search).
/// Higher = better recall, slower. 100 is a good balance.
const DEFAULT_EF_SEARCH: usize = 100;

/// Maximum widening attempts when post-filtering leaves too few results.
const MAX_WIDEN_ATTEMPTS: usize = 3;

impl HnswStrategy {
    /// Build an HNSW index from all active facts in the database.
    ///
    /// Loads all non-expired fact embeddings, inserts them into the HNSW graph.
    /// Called during engine initialization.
    ///
    /// **Memory:** Allocates proportional to total active facts × embedding dimension.
    /// At 100K facts × 128-dim × 4 bytes = ~50 MB for vectors alone, plus graph overhead.
    pub fn build_from_db(conn: &Connection, embed_dim: usize) -> Result<Self> {
        use rand::SeedableRng;

        // Seeded RNG for deterministic graph topology (reproducible benchmarks/tests).
        const HNSW_SEED: u64 = 42;
        let mut index: Hnsw<CosineMetric, Vec<f32>, SmallRng, 16, 32> =
            Hnsw::new_params_and_prng(
                CosineMetric,
                hnsw::Params::new().ef_construction(200),
                SmallRng::seed_from_u64(HNSW_SEED),
            );
        let mut searcher = Searcher::default();
        let mut index_to_fact = Vec::new();

        // Load all active fact embeddings
        let mut stmt = conn.prepare(
            "SELECT id, embedding FROM facts WHERE t_expired IS NULL ORDER BY id"
        )?;
        let rows = stmt.query_map([], |row| {
            let id: i64 = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            Ok((id, blob))
        })?;

        for row in rows {
            let (fact_id, blob) = row?;
            let embedding = deserialize_embedding(&blob, embed_dim)?;
            let hnsw_id = index.insert(embedding, &mut searcher);
            // Runtime assertion — not debug-only. If the hnsw crate ever
            // changes its ID assignment, this catches it immediately.
            assert_eq!(hnsw_id, index_to_fact.len(),
                "HNSW index must assign sequential IDs (got {hnsw_id}, expected {})",
                index_to_fact.len());
            index_to_fact.push(fact_id);
        }

        Ok(Self {
            inner: RwLock::new(HnswInner {
                index,
                index_to_fact,
                tombstones: HashSet::new(),
            }),
            embed_dim,
        })
    }
}

impl VectorSearchStrategy for HnswStrategy {
    fn search(
        &self,
        conn: &Connection,
        query_embedding: &[f32],
        embed_dim: usize,
        limit: usize,
        fact_type: Option<&FactType>,
        scope_ids: Option<&[i64]>,
    ) -> Result<Vec<VectorResult>> {
        if query_embedding.len() != self.embed_dim {
            return Err(crate::error::MemoryError::EmbeddingDimension {
                expected: self.embed_dim,
                actual: query_embedding.len(),
            });
        }

        // Widening loop: HNSW fetch → post-filter → check sufficiency → widen or fallback.
        // The loop widens ef_search when post-filtered results are insufficient,
        // and falls back to brute-force if HNSW cannot satisfy the query after all attempts.
        let mut ef = DEFAULT_EF_SEARCH;
        let mut results = Vec::new();

        for attempt in 0..MAX_WIDEN_ATTEMPTS {
            // Phase 1: Collect HNSW candidates under read lock, then release.
            // This minimizes lock contention — DB I/O happens outside the lock.
            let candidates = {
                let inner = self.inner.read();
                let query_vec = query_embedding.to_vec();
                let mut searcher = Searcher::default();

                let overfetch = limit * OVERFETCH_FACTOR;
                let mut dest = vec![hnsw::Neighbor::invalid(); overfetch];
                let neighbors = inner.index.nearest(&query_vec, ef, &mut searcher, &mut dest);

                let mut cands = Vec::new();
                for neighbor in neighbors {
                    let fact_id = inner.index_to_fact[neighbor.index];
                    if !inner.tombstones.contains(&fact_id) {
                        cands.push(fact_id);
                    }
                }
                cands
            }; // Read lock released here

            // Phase 2: Post-filter and exact-score ALL candidates via DB (no HNSW lock held).
            // We score every surviving candidate so the final sort gives the true
            // best-by-exact-score top `limit`, not just the first `limit` that passed filters.
            results.clear();
            results.reserve(candidates.len());
            for fact_id in candidates {
                // Post-filter: check fact_type and scope via DB if filters are active
                if fact_type.is_some() || scope_ids.is_some() {
                    let passes = check_fact_filters(conn, fact_id, fact_type, scope_ids)?;
                    if !passes {
                        continue;
                    }
                }

                // Compute exact cosine similarity for the score
                // (HNSW u32 distance is for ordering only, not a meaningful score)
                let stored_emb = load_embedding(conn, fact_id, self.embed_dim)?;
                let score = crate::search::cosine_similarity(query_embedding, &stored_emb);
                results.push(VectorResult { fact_id, score });
            }

            // Check sufficiency AFTER post-filtering (not before)
            if results.len() >= limit {
                break;
            }
            ef *= 2; // Widen search for next attempt
        }

        // If HNSW widening couldn't satisfy, fall back to brute-force.
        if results.len() < limit {
            return crate::search::vector_search(
                conn, query_embedding, self.embed_dim, limit, fact_type, scope_ids,
            );
        }

        // Sort ALL scored results by descending exact cosine similarity, then keep top `limit`.
        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);

        Ok(results)
    }

    fn name(&self) -> &str {
        "hnsw"
    }

    fn notify_insert(&self, fact_id: i64, embedding: &[f32]) {
        let mut inner = self.inner.write();
        let vec = embedding.to_vec();
        let mut searcher = Searcher::default();
        let hnsw_id = inner.index.insert(vec, &mut searcher);
        assert_eq!(hnsw_id, inner.index_to_fact.len(),
            "HNSW sequential ID invariant violated on insert");
        inner.index_to_fact.push(fact_id);
        inner.tombstones.remove(&fact_id);
    }

    fn notify_expire(&self, fact_id: i64) {
        let mut inner = self.inner.write();
        inner.tombstones.insert(fact_id);
    }
}

/// Check if a fact passes type and scope filters.
///
/// Uses the same `json_each` pattern as `vector_search` for scope filtering.
fn check_fact_filters(
    conn: &Connection,
    fact_id: i64,
    fact_type: Option<&FactType>,
    scope_ids: Option<&[i64]>,
) -> Result<bool> {
    use crate::store::facts::fact_type_to_str;

    let exists: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM facts
            WHERE id = ?1
            AND t_expired IS NULL
            AND (?2 IS NULL OR fact_type = ?2)
            AND (?3 IS NULL OR scope_id IN (SELECT value FROM json_each(?3)))
        )",
        rusqlite::params![
            fact_id,
            fact_type.map(fact_type_to_str),
            scope_ids.map(|ids| serde_json::to_string(ids).expect("scope_ids serialization")),
        ],
        |row| row.get(0),
    )?;

    Ok(exists)
}

/// Load a single fact's embedding from the database.
fn load_embedding(conn: &Connection, fact_id: i64, embed_dim: usize) -> Result<Vec<f32>> {
    let blob: Vec<u8> = conn.query_row(
        "SELECT embedding FROM facts WHERE id = ?1",
        [fact_id],
        |row| row.get(0),
    )?;
    deserialize_embedding(&blob, embed_dim)
}
```

**Important:** The `check_fact_filters` and `load_embedding` functions hit SQLite per candidate. This is acceptable because:

- HNSW returns O(k) candidates (not O(N))
- Each DB hit is an indexed primary key lookup (~1 µs)
- The alternative (caching all metadata in memory) adds complexity for minimal gain

- [ ] **Step 4: Run tests**

Run: `cargo test --features ann hnsw_strategy`
Expected: All 3 tests pass.

- [ ] **Step 5: Run full test suite**

Run: `cargo test --features ann`
Expected: All tests pass (161+ existing + new).

- [ ] **Step 6: Commit**

```bash
git add src/search/ann.rs
git commit -m "feat(search): implement HnswStrategy with build_from_db, insert, expire"
```

---

## Chunk 3: Dispatch Wiring

### Task 6: Wire SearchConfig into EngineConfig

**Files:**

- Modify: `src/engine.rs`
- Modify: `src/search/strategy.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn engine_config_default_has_no_search_config() {
    let config = EngineConfig::new("test.db".into(), 128);
    assert!(config.search_config.is_none());
}

#[test]
fn engine_config_with_search_config() {
    let mut config = EngineConfig::new("test.db".into(), 128);
    config.search_config = Some(SearchConfig::default());
    assert_eq!(config.search_config.unwrap().ann_threshold, 50_000);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test engine_config_default_has_no_search`
Expected: FAIL — `search_config` field doesn't exist.

- [ ] **Step 3: Add search_config to EngineConfig**

In `src/engine.rs`, add to `EngineConfig`:

```rust
/// Search dispatch configuration. When `Some` and the `ann` feature is
/// enabled, the engine builds an HNSW index and dispatches based on
/// `ann_threshold`. When `None`, always uses brute-force.
pub search_config: Option<crate::search::SearchConfig>,
```

Update `EngineConfig::new` to set `search_config: None`.

- [ ] **Step 4: Run to verify tests pass**

Run: `cargo test engine_config`
Expected: Pass.

- [ ] **Step 5: Commit**

```bash
git add src/engine.rs
git commit -m "feat(engine): add optional SearchConfig to EngineConfig"
```

### Task 7: Auto-select strategy on engine open

**Files:**

- Modify: `src/engine.rs`

**Dispatch architecture:**

- **Init time:** If `search_config` is `Some` and `ann` feature is enabled, always build the HNSW index alongside brute-force. Both strategies are available.
- **Query time:** Count active facts. If count >= `ann_threshold`, use HNSW. Otherwise, fall back to brute-force. This handles the dynamic nature of fact count over the engine's lifetime.
- The engine holds both strategies when ANN is configured: `vector_strategy` (BruteForce, always available) and `hnsw_strategy: Option<HnswStrategy>`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(feature = "ann")]
#[test]
fn engine_with_ann_uses_hnsw_above_threshold() {
    use memory_engine::search::SearchConfig;

    let mut config = EngineConfig::new_memory(4);
    config.search_config = Some(SearchConfig { ann_threshold: 2, ..Default::default() });
    let engine = MemoryEngine::open(&config).unwrap();

    // With 0 facts: below threshold → brute-force
    assert_eq!(engine.active_strategy_name(), "brute_force");

    // Add 3 facts (above threshold of 2)
    let embedder = /* ... */;
    for i in 0..3 {
        engine.add_fact(&format!("fact {i}"), FactType::Semantic, None, &embedder, None, None).unwrap();
    }

    // Now above threshold → hnsw
    assert_eq!(engine.active_strategy_name(), "hnsw");
}

#[test]
fn engine_without_config_always_brute_force() {
    let engine = MemoryEngine::open_memory(4).unwrap();
    assert_eq!(engine.active_strategy_name(), "brute_force");
}
```

Add `active_strategy_name()` that checks threshold at call time:

```rust
/// Name of the strategy that would be used for a query right now.
pub fn active_strategy_name(&self) -> &str {
    if self.should_use_hnsw() { "hnsw" } else { "brute_force" }
}

fn should_use_hnsw(&self) -> bool {
    if let Some(ref hnsw) = self.hnsw_strategy {
        let conn = self.pool.read();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM facts WHERE t_expired IS NULL",
            [], |row| row.get(0),
        ).unwrap_or(0);
        count as usize >= self.search_config.map_or(usize::MAX, |c| c.ann_threshold)
    } else {
        false
    }
}
```

- [ ] **Step 2: Implement strategy selection in init_from_pool**

In `src/engine.rs`, modify `init_from_pool` to build HNSW alongside BruteForce:

```rust
fn init_from_pool(
    pool: ConnectionPool,
    embed_dim: usize,
    search_config: Option<SearchConfig>,
) -> Result<Self> {
    let (graph, scope_tree, hnsw_strategy) = {
        let conn = pool.write();
        Self::validate_or_set_embed_dim(&conn, embed_dim)?;
        let graph = MemoryGraph::load_from_db(&conn)?;
        let scope_tree = ScopeTree::load(&conn)?;

        let hnsw = match &search_config {
            #[cfg(feature = "ann")]
            Some(_config) => {
                Some(crate::search::ann::HnswStrategy::build_from_db(&conn, embed_dim)?)
            }
            #[cfg(not(feature = "ann"))]
            Some(_config) => {
                tracing::warn!("search_config provided but `ann` feature not enabled");
                None
            }
            None => None,
        };

        (graph, scope_tree, hnsw)
    };
    Ok(Self {
        pool,
        embed_dim,
        graph: RwLock::new(graph),
        scope_tree: RwLock::new(scope_tree),
        vector_strategy: Box::new(BruteForce),
        hnsw_strategy,
        search_config,
    })
}
```

In `query()`, dispatch based on `should_use_hnsw()`:

```rust
let strategy: &dyn VectorSearchStrategy = if self.should_use_hnsw() {
    self.hnsw_strategy.as_ref().unwrap()
} else {
    &*self.vector_strategy
};
```

Update `open()` and `open_memory()` to pass config through. `open_memory` stays with `None` for backward compatibility. Add a new `open_memory_with_config` for testing.

- [ ] **Step 3: Run full test suite**

Run: `cargo test && cargo test --features ann`
Expected: All pass.

- [ ] **Step 4: Commit**

```bash
git add src/engine.rs
git commit -m "feat(engine): auto-select HNSW strategy when SearchConfig is provided"
```

### Task 8: Call lifecycle hooks from engine mutations

**Files:**

- Modify: `src/engine.rs`

**CRITICAL: Transaction safety.** Mutation hooks MUST fire AFTER the DB transaction commits successfully. If a hook fires inside a transaction that later rolls back, the HNSW index will be desynced from SQLite until restart. Pattern:

```rust
// CORRECT: hook after commit
let fact_id = {
    let conn = self.pool.write();
    let id = FactStore::new(&conn, self.embed_dim).insert(&new_fact)?;
    // ... other DB work within same transaction ...
    id
}; // DB lock released, transaction committed
// NOW safe to notify
if let Some(ref hnsw) = self.hnsw_strategy {
    hnsw.notify_insert(fact_id, &new_fact.embedding);
}
```

- [ ] **Step 1: Add notify_insert call to add_fact**

After the DB transaction commits (connection lock released), call `notify_insert`:

```rust
let fact_id = {
    let conn = self.pool.write();
    FactStore::new(&conn, self.embed_dim).insert(&new_fact)?
}; // DB lock released = transaction committed
if let Some(ref hnsw) = self.hnsw_strategy {
    hnsw.notify_insert(fact_id, &new_fact.embedding);
}
Ok(fact_id)
```

- [ ] **Step 2: Add notify_expire calls to all mutation paths that expire facts**

There are three engine methods that expire facts. All must follow the same pattern: DB commit first, then notify.

1. **`forget(&self, policy)`** (line ~298) — delegates to `forgetting::prune()` which calls `FactStore::expire()` internally. The `PruneStats` returned contains `pruned_count` but not the expired IDs.
   - Modify `prune()` to also return `expired_ids: Vec<i64>` — the list of fact IDs that were expired.
   - After `prune()` returns (DB committed), call `notify_expire` for each.

   ```rust
   // In prune() — collect IDs during expiration:
   let mut expired_ids = Vec::new();
   for fact in facts_to_prune {
       store.expire(fact.id)?;
       expired_ids.push(fact.id);
   }
   // Return both stats and IDs
   Ok((PruneStats { pruned_count: expired_ids.len() }, expired_ids))
   ```

2. **`resolve_conflict(&self, arbiter, old_id, new_fact)`** (line ~315) — delegates to `conflict::resolve_conflict()` which may expire the old fact. When the resolution is `Replace`, the old fact is expired and the replacement fact is inserted.
   - After `resolve_conflict()` returns with `Replace`, call `notify_expire(old_id)`.
   - **Important:** The replacement fact insertion goes through `add_fact()`, which already calls `notify_insert()` for the new fact. Therefore only `notify_expire(old_id)` needs to be added here — the new fact's index entry is handled by the existing `add_fact` → `notify_insert` path. No additional `notify_insert` call is needed in `resolve_conflict`.

3. **`consolidate(&self, ...)`** (line ~272) — delegates to dedup which calls `FactStore::expire()` on duplicates.
   - Similarly modify `local_dedup()` to return the list of expired duplicate IDs.
   - After `local_dedup()` returns, call `notify_expire` for each.

**Lock ordering note:** All three methods acquire `self.pool.write()` first. The `notify_expire` call acquires `inner.write()` on the HNSW `RwLock`. This establishes lock ordering: DB Mutex → HNSW RwLock. This is consistent with `add_fact` which follows the same order. **Never acquire these locks in reverse order.**

- [ ] **Step 3: Run full test suite**

Run: `cargo test && cargo test --features ann`
Expected: All pass.

- [ ] **Step 4: Commit**

```bash
git add src/engine.rs
git commit -m "feat(engine): call strategy lifecycle hooks on fact insert/expire"
```

---

## Chunk 4: Benchmarks + Recall Testing

### Task 9: Add HNSW benchmark group

**Files:**

- Modify: `benches/search_bench.rs`

- [ ] **Step 1: Add cfg-gated HNSW benchmark**

```rust
#[cfg(feature = "ann")]
fn bench_hnsw_search(c: &mut Criterion) {
    use memory_engine::search::SearchConfig;

    let mut group = c.benchmark_group("hnsw_search");

    for &size in &[1_000, 10_000, 50_000, 100_000] {
        let samples = if size >= 50_000 { 10 } else { 20 };
        group.sample_size(samples);

        // Build engine with HNSW
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("bench_hnsw.db");
        let mut config = EngineConfig::new(db_path, DIM);
        config.search_config = Some(SearchConfig::default());
        // ... setup similar to existing bench but with HNSW engine
        // Populate, then benchmark query
    }
    group.finish();
}
```

- [ ] **Step 2: Add group to criterion_group and replace criterion_main**

**Important:** The existing `criterion_main!(benches);` must be REPLACED, not duplicated.
Delete the existing `criterion_main!` call and replace with:

```rust
#[cfg(feature = "ann")]
criterion_group!(
    ann_benches,
    bench_hnsw_search,
);

// Replace the existing criterion_main!(benches) with this cfg-gated pair:
#[cfg(feature = "ann")]
criterion_main!(benches, ann_benches);

#[cfg(not(feature = "ann"))]
criterion_main!(benches);
```

- [ ] **Step 3: Run HNSW benchmarks**

Run: `cargo bench --features ann -- hnsw_search/1000`
Expected: Completes successfully, shows timing.

- [ ] **Step 4: Commit**

```bash
git add benches/search_bench.rs
git commit -m "bench: add HNSW search benchmark group behind ann feature"
```

### Task 10: Add recall integration test

**Files:**

- Create: `tests/ann_recall_test.rs`

Recall test: for a dataset of N facts, compare HNSW top-k against brute-force top-k across multiple queries. Recall@k = |intersection| / k. We require average recall >= 0.9 and minimum recall >= 0.7 per query.

Uses content strings (not SQLite row IDs) for set comparison — the two engines have separate databases with potentially different ID sequences.

- [ ] **Step 1: Write the recall test**

```rust
//! Integration test: HNSW recall vs brute-force oracle.
//!
//! Verifies that HnswStrategy returns results with >= 90% average overlap
//! with the brute-force ground truth across multiple diverse queries.

#![cfg(feature = "ann")]

use std::collections::HashSet;

use memory_engine::engine::{EngineConfig, MemoryEngine};
use memory_engine::search::hybrid::{SearchMode, SearchQuery};
use memory_engine::search::SearchConfig;
use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::FactType;

const DIM: usize = 32;
const N: usize = 5_000;
const K: usize = 10;

struct Blake3Embedder;

impl EmbeddingProvider for Blake3Embedder {
    fn embed(&self, text: &str) -> memory_engine::error::Result<Vec<f32>> {
        let hash = blake3::hash(text.as_bytes());
        let bytes = hash.as_bytes();
        Ok((0..DIM)
            .map(|i| {
                let byte = bytes[i % 32];
                (f32::from(byte) / 255.0).mul_add(2.0, -1.0)
            })
            .collect())
    }
}

#[test]
fn hnsw_recall_at_k_exceeds_threshold() {
    // Build brute-force engine
    let dir_bf = tempfile::tempdir().unwrap();
    let config_bf = EngineConfig::new(dir_bf.path().join("bf.db"), DIM);
    let engine_bf = MemoryEngine::open(&config_bf).unwrap();

    // Build HNSW engine (threshold=0 → always use HNSW)
    let dir_ann = tempfile::tempdir().unwrap();
    let mut config_ann = EngineConfig::new(dir_ann.path().join("ann.db"), DIM);
    config_ann.search_config = Some(SearchConfig { ann_threshold: 0, ..Default::default() });
    let engine_ann = MemoryEngine::open(&config_ann).unwrap();

    let embedder = Blake3Embedder;
    for i in 0..N {
        let content = format!("fact number {i} about topic {}", i % 50);
        engine_bf
            .add_fact(&content, FactType::Semantic, None, &embedder, None, None)
            .unwrap();
        engine_ann
            .add_fact(&content, FactType::Semantic, None, &embedder, None, None)
            .unwrap();
    }

    // Multiple diverse queries for robust recall measurement
    let queries = [
        "fact about topic 7",
        "fact about topic 42",
        "fact number 100 about topic 0",
        "completely unrelated query string",
        "fact about topic 25",
    ];

    let mut recalls = Vec::new();
    for query_text in &queries {
        let query_emb = embedder.embed(query_text).unwrap();
        let query = SearchQuery {
            text: None,
            embedding: Some(query_emb),
            mode: SearchMode::Vector,
            limit: K,
            valid_at: None,
            fact_type: None,
            scope: None,
        };

        let bf_results = engine_bf.query(&query).unwrap();
        let ann_results = engine_ann.query(&query).unwrap();

        // Compare by content strings, not row IDs (separate DBs)
        let bf_contents: HashSet<&str> =
            bf_results.iter().map(|r| r.fact.content.as_str()).collect();
        let ann_contents: HashSet<&str> =
            ann_results.iter().map(|r| r.fact.content.as_str()).collect();

        let overlap = bf_contents.intersection(&ann_contents).count();
        let recall = overlap as f64 / K as f64;
        recalls.push(recall);

        assert!(
            recall >= 0.7,
            "HNSW recall@{K} = {recall:.2} for query '{query_text}' (min 0.7). \
             BF: {bf_contents:?}, ANN: {ann_contents:?}"
        );
    }

    let avg_recall = recalls.iter().sum::<f64>() / recalls.len() as f64;
    assert!(
        avg_recall >= 0.9,
        "Average HNSW recall@{K} = {avg_recall:.2} (expected >= 0.9). \
         Per-query: {recalls:?}"
    );
}
```

- [ ] **Step 2: Run recall test**

Run: `cargo test --features ann --test ann_recall_test`
Expected: Pass with recall >= 0.9.

- [ ] **Step 3: Commit**

```bash
git add tests/ann_recall_test.rs
git commit -m "test: add HNSW recall integration test (recall@10 >= 0.9)"
```

---

## Chunk 5: Final Verification + Cleanup

### Task 11: Full verification

- [ ] **Step 1: Run all checks (all feature combinations)**

```bash
# Default (no ann) — must still compile and pass
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test

# With ann feature
cargo check --all-targets --features ann
cargo clippy --all-targets --features ann -- -D warnings
cargo test --features ann

# Compile-only benchmark check
cargo bench --features ann -- --test
```

All must pass clean. The `--no-default-features` check is unnecessary since `default = []`.

- [ ] **Step 2: Run benchmarks and compare**

```bash
cargo bench -- vector_search/10000     # brute-force baseline
cargo bench --features ann -- hnsw_search/10000  # HNSW
```

Record the comparison. HNSW should be significantly faster at N >= 50K.

- [ ] **Step 3: Commit any cleanup**

```bash
git add -A
git commit -m "chore: final cleanup for ANN implementation"
```

---

## Risks and Mitigations

| Risk                                                | Mitigation                                                                       |
| --------------------------------------------------- | -------------------------------------------------------------------------------- |
| `hnsw` crate API differs from docs (42% documented) | Task 3 is an explicit spike; adjust plan if API surprises                        |
| `Hnsw` struct not `Send + Sync`                     | Task 3 includes compile-time check; wrap in `Mutex` if needed                    |
| HNSW recall < 90% at small N                        | Tune ef_construction (200→400) and ef_search (100→200)                           |
| Post-filter DB hits slow down HNSW                  | Read lock released before DB I/O; O(k) PK lookups ≈ k µs                         |
| `space` 0.17 `Metric` API doesn't match docs        | Spike validates; fallback: implement adapter                                     |
| Index rebuild on engine open slow for large DBs     | Acceptable for embedded use; cold-start optimization tracked in #31              |
| Transaction rollback desyncs HNSW                   | Hooks fire AFTER commit only; rollback cannot affect in-memory index             |
| Tombstone accumulation degrades recall              | Rebuild threshold at 25% tombstones; periodic rebuild deferred to future work    |
| Overfetch insufficient under heavy filtering        | Widening loop (3 attempts, doubling ef_search); final fallback to brute-force    |
| `clippy::ptr_arg` on `&Vec<f32>` in Metric impl     | `#[allow]` with comment — `space::Metric` trait requires `&P` where `P=Vec<f32>` |
| Memory proportional to fact count on startup        | Documented on `build_from_db`; operational limit for embedded use                |

## HNSW Parameter Reference

| Parameter                | Value | Rationale                                                 |
| ------------------------ | ----- | --------------------------------------------------------- |
| M (max connections)      | 16    | Standard. Higher = better recall, more memory             |
| M0 (layer-0 connections) | 32    | Typically 2\*M                                            |
| ef_construction          | 200   | High-quality graph build. One-time cost                   |
| ef_search                | 100   | Good recall/speed balance. Tunable via SearchConfig later |
| Overfetch factor         | 3x    | Compensates for post-filter exclusions                    |
| RNG seed                 | 42    | Deterministic graph topology for reproducible benchmarks  |
| Max widen attempts       | 3     | Retry with 2x ef_search when post-filtering exhausts pool |
