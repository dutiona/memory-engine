# Hybrid Search

**Status: Implemented**

The search system combines full-text search (BM25 via SQLite FTS5), vector similarity (brute-force cosine), and Reciprocal Rank Fusion (RRF) to rank results. Three search modes are available, with SQL-level and post-filter stages for temporal and scope filtering.

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

Brute-force cosine similarity against all active fact embeddings. Requires `SearchQuery.embedding` to be set. Pure Rust implementation -- no external vector database.

```rust
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    // dot product / (norm_a * norm_b)
}
```

This is O(N) over active facts. Sufficient for embedded use cases; not intended for million-scale corpora.

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

The search overfetches 3x the requested `limit` from each source before merging. This compensates for post-filter attrition -- facts that pass the SQL filters but get removed by the `valid_at` temporal post-filter.

```rust
let overfetch = query.limit.saturating_mul(3).max(query.limit);
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

## Content Hashing

Facts are content-hashed with Blake3 at insertion time. The 32-character hex hash is stored in `content_hash`. This supports fast exact-duplicate detection independent of embedding similarity.

## Usage

```rust
let query = SearchQuery {
    text: Some("memory consolidation".into()),
    embedding: Some(embedder.embed("memory consolidation")?),
    mode: SearchMode::Hybrid,
    limit: 10,
    valid_at: Some(Utc::now()),
    fact_type: Some(FactType::Semantic),
    scope: None,
};

let results = engine.query(&query)?;
for r in &results {
    println!("[{:.4}] {:?} — {}", r.score, r.match_type, r.fact.content);
}
```
