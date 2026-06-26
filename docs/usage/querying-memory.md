# Querying Memory

memory-engine supports three search modes: full-text search (FTS5 with BM25 ranking), vector similarity (cosine distance), and hybrid search (Reciprocal Rank Fusion merge of both). All queries go through `engine.query(&search_query)`.

## The `SearchQuery` struct

```rust
pub struct SearchQuery {
    pub text: Option<String>,
    pub embedding: Option<Vec<f32>>,
    pub mode: SearchMode,
    pub limit: usize,
    pub valid_at: Option<DateTime<Utc>>,
    pub fact_type: Option<FactType>,
    pub scope: Option<ScopeQuery>,
}
```

| Field       | Description                                                                       |
| ----------- | --------------------------------------------------------------------------------- |
| `text`      | Query string for FTS. Required when `mode` is `Fts` or `Hybrid`.                  |
| `embedding` | Query vector for similarity search. Required when `mode` is `Vector` or `Hybrid`. |
| `mode`      | `Fts`, `Vector`, or `Hybrid`.                                                     |
| `limit`     | Maximum number of results to return.                                              |
| `valid_at`  | Optional temporal filter -- only return facts valid at this point in time.        |
| `fact_type` | Optional filter by `FactType` (Episodic, Semantic, Procedural).                   |
| `scope`     | Optional scope-aware filter (see below).                                          |

## Search modes

### `SearchMode::Fts`

Keyword search using SQLite FTS5 with BM25 ranking. Requires `text` to be set. Good for exact term matches.

### `SearchMode::Vector`

Cosine similarity search against stored embeddings. Requires `embedding` to be set. Good for semantic similarity even when exact terms differ.

### `SearchMode::Hybrid`

Runs both FTS and vector search, then merges results using Reciprocal Rank Fusion (RRF) with k=60. Over-fetches 3x the requested limit from each source before merging to improve result quality. This is the recommended mode for most use cases -- it combines the precision of keyword matching with the recall of semantic similarity.

## The `SearchResult` struct

```rust
pub struct SearchResult {
    pub fact: Fact,
    pub score: f64,
    pub match_type: MatchType,
}
```

`match_type` indicates which source(s) contributed to the result: `Fts`, `Vector`, or `Both`.

## Basic example

```rust
use memory_engine::search::{SearchQuery, SearchMode};

let results = engine.query(&SearchQuery {
    text: Some("memory safety".into()),
    embedding: Some(embedder.embed("memory safety")?),
    mode: SearchMode::Hybrid,
    limit: 10,
    valid_at: None,
    fact_type: None,
    scope: None,
})?;

for result in &results {
    println!("[{:.4}] {:?} — {}",
        result.score, result.match_type, result.fact.content);
}
```

## Temporal filtering

Set `valid_at` to filter facts by their real-world validity window (`t_valid` / `t_invalid`). A fact is included only if:

- `t_valid` is unset or `<= valid_at`, AND
- `t_invalid` is unset or `> valid_at`

This is a post-filter applied after FTS/vector scoring. The engine over-fetches 3x to compensate for post-filter attrition.

```rust
use chrono::Utc;

let results = engine.query(&SearchQuery {
    text: Some("API latency".into()),
    embedding: None,
    mode: SearchMode::Fts,
    limit: 5,
    valid_at: Some(Utc::now()),  // only facts valid right now
    fact_type: None,
    scope: None,
})?;
```

Expired facts (`t_expired IS NOT NULL`) are always excluded at the SQL level -- you never see soft-deleted facts in results.

## Fact type filtering

Restrict results to a specific `FactType`:

```rust
let results = engine.query(&SearchQuery {
    text: Some("deploy".into()),
    embedding: None,
    mode: SearchMode::Fts,
    limit: 10,
    valid_at: None,
    fact_type: Some(FactType::Procedural),  // only how-to knowledge
    scope: None,
})?;
```

The fact type filter is pushed into the SQL query (not a post-filter), so it does not reduce result count below `limit` unexpectedly.

## Scope-aware queries

Use `ScopeQuery` to restrict results by hierarchical scope. Scope paths are consumer-facing strings like `"user:michael/project:demo"`.

```rust
pub enum ScopeQuery {
    Exact(String),     // Facts at exactly this scope path
    Subtree(String),   // Facts at this scope and all descendants
    Ancestors(String), // Facts at this scope and all ancestors up to root
    Inherited(String), // Ancestors + subtree (full inherited context)
}
```

| Variant     | Use case                                                                                    |
| ----------- | ------------------------------------------------------------------------------------------- |
| `Exact`     | Facts scoped to exactly one node.                                                           |
| `Subtree`   | Everything under a project or user, including nested scopes.                                |
| `Ancestors` | Facts this scope inherits from parent scopes (e.g., user preferences visible to a project). |
| `Inherited` | Combines `Ancestors` and `Subtree` -- the full context visible to a scope.                  |

```rust
use memory_engine::types::ScopeQuery;

// Only facts at this exact scope
let results = engine.query(&SearchQuery {
    text: Some("config".into()),
    embedding: None,
    mode: SearchMode::Fts,
    limit: 10,
    valid_at: None,
    fact_type: None,
    scope: Some(ScopeQuery::Inherited("user:michael/project:demo".into())),
})?;
```

Scope resolution happens against an in-memory scope tree (loaded from SQLite on engine open). If the scope path does not exist, the query returns an empty result set rather than falling through to an unscoped search.
