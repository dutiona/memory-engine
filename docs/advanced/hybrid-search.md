# Hybrid Search

**Status: Implemented**

The search system combines full-text search (BM25 via SQLite FTS5), vector similarity (brute-force or HNSW via runtime dispatch), and Reciprocal Rank Fusion (RRF) to rank results. Three search modes are available, with SQL-level and post-filter stages for temporal and scope filtering.

## Search Modes

```rust
pub enum SearchMode {
    Fts,     // BM25 full-text search only
    Vector,  // Cosine similarity only
    Hybrid,  // Both sources merged via RRF
}
```

### FTS Mode

Uses SQLite FTS5 for BM25-ranked full-text search. Requires `SearchQuery.text` to be set. Expired facts, fact type, and scope are filtered at the SQL level.

### Vector Mode

Cosine similarity against all active fact embeddings. Requires `SearchQuery.embedding` to be set. Pure Rust implementation -- no external vector database.

The engine dispatches between two strategies at query time:

- **Brute-force** (default): O(N) scan with `select_nth_unstable_by` partial sort for top-K. Zero overhead, always correct.
- **HNSW** (with `ann` feature): Approximate nearest neighbor via the `hnsw` crate. Sublinear query time with a widening loop and brute-force fallback for correctness.

Dispatch is controlled by `SearchConfig::ann_threshold` — the minimum active fact count at which HNSW is preferred. When `ann` is not enabled, brute-force is always used.

```rust
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    // dot product / (norm_a * norm_b)
}
```

See [ANN Search](#ann-approximate-nearest-neighbor) below for HNSW details.

### Hybrid Mode

Runs both FTS and vector searches, then merges results using Reciprocal Rank Fusion.

## Reciprocal Rank Fusion (RRF)

RRF merges two ranked lists into a single ranking. For each item, the RRF score is the sum of `1/(k + rank + 1)` across all lists where it appears (rank is 0-based).

```rust
pub fn rrf_merge(fts: &[(i64, f64)], vec: &[(i64, f32)], k: u32) -> Vec<(i64, f64)> {
    let mut scores: HashMap<i64, f64> = HashMap::new();
    for (rank, &(id, _)) in fts.iter().enumerate() {
        *scores.entry(id).or_default() += 1.0 / f64::from(k + rank as u32 + 1);
    }
    for (rank, &(id, _)) in vec.iter().enumerate() {
        *scores.entry(id).or_default() += 1.0 / f64::from(k + rank as u32 + 1);
    }
    // sort descending by score
}
```

The constant `k=60` dampens the impact of rank position. Items appearing in both lists get a boost from both rank contributions.

## Overfetch Strategy

The search computes an effective candidate target from `rerank_depth` (if set) or `limit`, clamped to at least `limit`. The effective target is always multiplied by 3x to compensate for post-filter attrition -- the temporal post-filter runs unconditionally (cutoff defaults to `Utc::now()` when `valid_at` is `None`), and Hybrid mode benefits from extra candidates for RRF fusion quality. On the HNSW path, overfetch is adaptive: the widening loop doubles it on each retry (3x → 6x → 12x) before falling back to brute-force. See [ANN Search](#ann-approximate-nearest-neighbor) for details.

```rust
let effective_target = query.rerank_depth.unwrap_or(query.limit).max(query.limit);
let overfetch = effective_target.saturating_mul(3).max(effective_target);
```

## Filtering

### SQL-Level Filters (Pre-Retrieval)

These filters are pushed into the FTS and vector SQL queries:

- **t_expired IS NULL**: Only active (non-expired) facts.
- **fact_type**: Optional `FactType` filter (Episodic, Semantic, Procedural).
- **scope_ids**: Optional scope restriction (resolved from `ScopeQuery` before search).

### Post-Filter: valid_at

Temporal validity filtering happens after retrieval due to its complex semantics:

```rust
if let Some(valid_at) = query.valid_at {
    if let Some(t_valid) = fact.t_valid {
        if t_valid > valid_at { continue; }       // not yet valid
    }
    if let Some(t_invalid) = fact.t_invalid {
        if t_invalid <= valid_at { continue; }     // no longer valid
    }
}
```

Facts with no valid-time bounds pass unconditionally. See [Bi-Temporal Semantics](bi-temporal-semantics.md) for details.

## SearchQuery

```rust
pub struct SearchQuery {
    pub text: Option<String>,                 // for FTS
    pub embedding: Option<Vec<f32>>,          // for vector
    pub mode: SearchMode,
    pub limit: usize,
    pub rerank_depth: Option<usize>,          // over-fetch for reranker (clamped to >= limit)
    pub valid_at: Option<DateTime<Utc>>,      // temporal post-filter
    pub fact_type: Option<FactType>,          // SQL-level filter
    pub scope: Option<ScopeQuery>,            // SQL-level filter
}
```

## SearchResult

```rust
pub struct SearchResult {
    pub fact: Fact,          // full fact with all fields
    pub score: f64,          // RRF score (hybrid), BM25 score (FTS), or cosine (vector)
    pub match_type: MatchType,
}

pub enum MatchType {
    Fts,     // matched FTS only
    Vector,  // matched vector only
    Both,    // matched both sources
}
```

`MatchType::Both` indicates the result appeared in both FTS and vector result sets, which typically correlates with higher relevance.

## Reranking (Cross-Encoder Refinement)

**Status: Implemented (Phase 4a)**

After RRF merge and result assembly, the engine can optionally pass candidates through a consumer-provided `Reranker` for cross-encoder scoring. This is the standard two-stage retrieval pattern: fast bi-encoder retrieval (FTS + vector) followed by precise cross-encoder reranking on the top-K candidates.

### Why

Bi-encoder similarity (embedding dot-product) scores query and document independently, which is fast but loses cross-attention signal. Cross-encoders process the (query, document) pair jointly, capturing token-level interactions. Research on four-layer cognitive architectures shows cross-encoder reranking on top-20 candidates improves nDCG@10 by 5-15%.

### How It Works

```
FTS/Vector sources → RRF merge → collect top-N facts → Reranker → truncate to limit
```

1. `hybrid_search()` collects up to `effective_target` candidates (from `rerank_depth` or `limit`, whichever is larger).
2. The engine's `query()` method passes these candidates to the `Reranker` trait (if configured) **outside the read lock** -- safe for slow inference or API calls.
3. The reranker returns reordered results with updated scores.
4. Results are unconditionally truncated to `limit`.

### rerank_depth

`rerank_depth` controls how many candidates the reranker sees before the final truncation to `limit`. It is clamped to at least `limit` -- it can only widen the candidate pool, never shrink it.

```rust
let query = SearchQuery {
    text: Some("memory consolidation".into()),
    embedding: Some(embedder.embed("memory consolidation")?),
    mode: SearchMode::Hybrid,
    limit: 10,
    rerank_depth: Some(50),  // reranker sees 50 candidates, output truncated to 10
    valid_at: None,
    fact_type: None,
    scope: None,
};
```

When `rerank_depth` is `None`, falls back to `limit` (no over-fetch beyond temporal 3x).

### Activation Conditions

The reranker fires when **both** conditions are met:

1. A `Reranker` is configured on the engine (via `MemoryEngine::builder(dim).reranker(...)`).
2. The query has `text` set (cross-encoders need query text).

This means reranking applies to FTS, Hybrid, and Vector+text queries. Pure vector queries without text skip the reranker.

### Error Handling

The `Reranker::rerank()` method is failable (`-> Result<Vec<SearchResult>>`). Errors propagate as `MemoryError::Reranker`. This allows consumers to handle inference failures, timeouts, or API errors gracefully.

### Contract Enforcement

After each `rerank()` call, the engine validates that the output is a valid subset of the input candidates — no fabricated fact IDs, no duplicates, and output length <= input length. Violations produce `MemoryError::Reranker` with a diagnostic message identifying the specific breach. See the [Reranker trait documentation](../../src/traits.rs) for the full contract.

See [Extensibility](extensibility.md) for trait definition, implementation example, and engine wiring.

## Content Hashing

Facts are content-hashed with Blake3 at insertion time. The 32-character hex hash is stored in `content_hash`. This supports fast exact-duplicate detection independent of embedding similarity.

## Usage

```rust
let query = SearchQuery {
    text: Some("memory consolidation".into()),
    embedding: Some(embedder.embed("memory consolidation")?),
    mode: SearchMode::Hybrid,
    limit: 10,
    rerank_depth: None,                       // or Some(50) to over-fetch for reranker
    valid_at: Some(Utc::now()),
    fact_type: Some(FactType::Semantic),
    scope: None,
};

let results = engine.query(&query)?;
for r in &results {
    println!("[{:.4}] {:?} — {}", r.score, r.match_type, r.fact.content);
}
```

---

## ANN (Approximate Nearest Neighbor)

**Status: Implemented (behind `ann` feature flag)**

When the `ann` feature is enabled and `SearchConfig` is provided, vector search can dispatch to an HNSW (Hierarchical Navigable Small World) index for sublinear query time.

### Enabling ANN

```toml
[dependencies]
memory-engine = { git = "https://github.com/dutiona/memory-engine", features = ["ann"] }
```

```rust
use memory_engine::MemoryEngine;
use memory_engine::search::SearchConfig;

let engine = MemoryEngine::builder(384)
    .path("memory.db")
    .search_config(SearchConfig {
        ann_threshold: 10_000,  // use HNSW when >= 10K active facts
        ..Default::default()
    })
    .build()?;
```

### Strategy Dispatch

The engine decides per-query whether to use HNSW or brute-force:

```
active_count() >= ann_threshold  →  HNSW
active_count() <  ann_threshold  →  brute-force
```

`active_count()` is O(1) — it reads from the in-memory `fact_to_hnsw` map size, not the database.

Special values:

- `ann_threshold = 0` — always use HNSW (useful for testing)
- `ann_threshold = usize::MAX` — never use HNSW (disables ANN without removing the feature flag); the HNSW index is not built at all in this case

### HNSW Architecture

```
┌─────────────────────────────────────────────┐
│                HnswStrategy                  │
│  ┌────────────────────────────────────────┐  │
│  │           RwLock<HnswInner>            │  │
│  │  ┌──────────────────────────────────┐  │  │
│  │  │  Hnsw<CosineMetric, Vec<f32>>    │  │  │
│  │  │  (M=16, M0=32, ef_construction=200)│ │  │
│  │  ├──────────────────────────────────┤  │  │
│  │  │  index_to_fact: Vec<i64>         │  │  │
│  │  │  (hnsw_index → fact_id)          │  │  │
│  │  ├──────────────────────────────────┤  │  │
│  │  │  fact_to_hnsw: HashMap<i64,usize>│  │  │
│  │  │  (fact_id → current hnsw_index)  │  │  │
│  │  ├──────────────────────────────────┤  │  │
│  │  │  tombstones: HashSet<usize>      │  │  │
│  │  │  (expired hnsw indices)          │  │  │
│  │  └──────────────────────────────────┘  │  │
│  └────────────────────────────────────────┘  │
└─────────────────────────────────────────────┘
```

**CosineMetric** implements `space::Metric<Vec<f32>>` with `Unit = u32`. Cosine distance `(1 - similarity)` is converted to `u32` via `f32::to_bits()`. For non-negative f32 values, the bit representation preserves total order, satisfying the `Ord` requirement. NaN inputs are guarded (treated as orthogonal, distance = 1.0).

### Two-Phase Search

HNSW search uses a two-phase approach to minimize lock contention:

1. **Phase 1 (read lock held):** Query the HNSW graph for candidate fact IDs. Tombstoned indices are excluded. The read lock is released immediately after.

2. **Phase 2 (no lock):** For each candidate, verify it is still active in the database (`t_expired IS NULL`), apply fact_type and scope filters, load the stored embedding, and compute exact cosine similarity for final scoring.

This design ensures that the HNSW read lock is held only for the graph traversal (microseconds), not for database I/O.

### Widening Loop

If aggressive filters (scope, fact_type) eliminate too many HNSW candidates, the search widens:

```
Attempt 1: ef=100,  overfetch=limit×3
Attempt 2: ef=200,  overfetch=limit×6
Attempt 3: ef=400,  overfetch=limit×12
Fallback:  brute-force (guaranteed correct)
```

Both `ef_search` (search pool accuracy) and `overfetch` (candidate count) are doubled each retry. This prevents the failure mode where increasing only `ef` finds the same small candidate set more accurately without discovering new candidates.

### Lifecycle Hooks

The HNSW index stays synchronized with the database through lifecycle hooks on `VectorSearchStrategy`:

| Engine method      | Hook called                                 | Timing          |
| ------------------ | ------------------------------------------- | --------------- |
| `add_fact()`       | `notify_insert(fact_id, &embedding)`        | After DB commit |
| `forget()`         | `notify_expire(fact_id)` for each pruned    | After DB commit |
| `resolve_conflict` | `notify_expire` + `notify_insert` as needed | After DB commit |
| `consolidate()`    | `notify_expire(fact_id)` for each deduped   | After DB commit |

Hooks always fire **after** the database transaction commits. This ensures the HNSW index only reflects committed data. The lock ordering is always DB → HNSW (never reversed).

### Tombstone Semantics

Tombstones track **HNSW internal indices** (not fact IDs). This is critical for correctness when a fact is replaced:

1. `notify_expire(42)` → looks up `fact_to_hnsw[42]` → tombstones HNSW index N
2. `notify_insert(42, new_emb)` → tombstones old HNSW index (if any) → inserts new entry at index M → updates `fact_to_hnsw[42] = M`

If tombstones tracked fact IDs instead, step 2 would un-tombstone the old entry (index N), causing stale results near the old embedding.

### Benchmarks

HNSW benchmarks are included behind the `ann` feature:

```bash
cargo bench --features ann -- hnsw_search    # HNSW at 1K/10K/50K/100K
cargo bench -- vector_search                  # brute-force baseline
```

### Recall Guarantee

An integration test (`tests/ann_recall_test.rs`) verifies HNSW recall against brute-force ground truth:

- 5,000 facts, 5 diverse queries
- Per-query recall@10 ≥ 0.7
- Average recall@10 ≥ 0.9
