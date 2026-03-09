# API Reference

Full generated API documentation: <https://dutiona.github.io/memory-engine/>

This page summarizes the public API surface of the `memory-engine` crate.

## Entry Points

`MemoryEngine`
: Facade over all memory primitives. All public methods take `&self`. Thread-safe via internal connection pool and `RwLock`-protected caches. Share across threads with `Arc<MemoryEngine>`.

`EngineConfig`
: Configuration for file-backed engines. Fields: `path: PathBuf`, `embed_dim: usize`, `read_pool_size: usize` (default: 4).

## Public Methods by Category

### Lifecycle

| Method           | Signature                                 | Description                                                                                          |
| ---------------- | ----------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| `open`           | `(config: &EngineConfig) -> Result<Self>` | Open or create a file-backed engine. Validates `embed_dim` against stored value on subsequent opens. |
| `open_memory`    | `(embed_dim: usize) -> Result<Self>`      | Open an in-memory engine (testing).                                                                  |
| `embed_dim`      | `(&self) -> usize`                        | Return the configured embedding dimension.                                                           |
| `is_file_backed` | `(&self) -> bool`                         | Whether the engine uses a file (vs in-memory).                                                       |

### Ingest

| Method     | Signature                                                                            | Description                                                                                                              |
| ---------- | ------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------ |
| `ingest`   | `(&self, event: &NewEvent) -> Result<i64>`                                           | Append an event to the log. Returns the event ID.                                                                        |
| `add_fact` | `(&self, content, fact_type, source_event_id, embedder, scope, opts) -> Result<i64>` | Compute embedding, blake3 hash, resolve scope, and insert a fact. Embedding is computed before acquiring the write lock. |

### Facts

| Method              | Signature                                                     | Description                                           |
| ------------------- | ------------------------------------------------------------- | ----------------------------------------------------- |
| `get_fact`          | `(&self, id: i64) -> Result<Fact>`                            | Retrieve a fact by ID. Returns `NotFound` if missing. |
| `list_active_facts` | `(&self) -> Result<Vec<Fact>>`                                | List all non-expired facts.                           |
| `list_summaries`    | `(&self, level: &ConsolidationLevel) -> Result<Vec<Summary>>` | List summaries filtered by consolidation level.       |

### Query

| Method  | Signature                                                   | Description                                                                                                                             |
| ------- | ----------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `query` | `(&self, query: &SearchQuery) -> Result<Vec<SearchResult>>` | Hybrid search combining FTS5, vector similarity, and RRF merge. Respects scope filtering. Returns empty results for nonexistent scopes. |

`SearchQuery` fields:

- `text: Option<String>` -- FTS query string.
- `embedding: Option<Vec<f32>>` -- query vector for semantic search.
- `mode: SearchMode` -- `Fts`, `Vector`, or `Hybrid`.
- `limit: usize` -- max results.
- `valid_at: Option<DateTime<Utc>>` -- temporal filter on valid time.
- `fact_type: Option<FactType>` -- filter by fact type.
- `scope: Option<ScopeQuery>` -- scope resolution strategy.

`SearchResult` fields:

- `fact: Fact` -- the matched fact.
- `score: f64` -- relevance score.
- `match_type: MatchType` -- `Fts`, `Vector`, or `Both`.

### Consolidation

| Method        | Signature                                                                                               | Description                                                                                                               |
| ------------- | ------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| `consolidate` | `(&self, generator: &dyn SummaryGenerator, config: &ConsolidationConfig) -> Result<ConsolidationStats>` | Run three-pass consolidation: local dedup, cluster fusion, global integration. Rebuilds graph if duplicates were removed. |

`ConsolidationConfig` fields:

- `dedup_threshold: f32` -- cosine similarity threshold for dedup (e.g., 0.92).
- `min_cluster_size: usize` -- minimum facts to form a cluster.

### Forgetting

| Method   | Signature                                              | Description                                                               |
| -------- | ------------------------------------------------------ | ------------------------------------------------------------------------- |
| `forget` | `(&self, policy: &ForgetPolicy) -> Result<PruneStats>` | Soft-delete facts with computed importance below `policy.min_importance`. |

`ForgetPolicy` fields:

- `half_life_days: f64` -- base Ebbinghaus half-life (default: 69.0).
- `half_life_overrides: HashMap<FactType, f64>` -- per-type overrides.
- `min_importance: f64` -- expiration threshold (default: 0.1).
- `recency_weight`, `frequency_weight`, `graph_degree_weight`, `base_importance_weight` -- signal weights (default: 0.3, 0.2, 0.3, 0.2).

### Conflict Resolution

| Method             | Signature                                                                                               | Description                                                                                                                                |
| ------------------ | ------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `resolve_conflict` | `(&self, arbiter: &dyn ConflictArbiter, old_id: i64, new_fact: &NewFact) -> Result<ConflictResolution>` | Delegate conflict decision to the arbiter. On `Update`, expires old fact and creates a `superseded_by` edge. Runs in a single transaction. |

### Graph

| Method            | Signature                           | Description                                                   |
| ----------------- | ----------------------------------- | ------------------------------------------------------------- |
| `graph_degree`    | `(&self, fact_id: i64) -> usize`    | In + out edge count for a fact.                               |
| `graph_neighbors` | `(&self, fact_id: i64) -> Vec<i64>` | Outgoing neighbor fact IDs.                                   |
| `graph_component` | `(&self, fact_id: i64) -> Vec<i64>` | All fact IDs in the connected component containing `fact_id`. |
| `graph_stats`     | `(&self) -> (usize, usize)`         | `(node_count, edge_count)`.                                   |
| `graph_has_node`  | `(&self, fact_id: i64) -> bool`     | Whether a node exists in the graph.                           |

### Resume

| Method           | Signature                                                 | Description                                                                                                     |
| ---------------- | --------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- |
| `resume_context` | `(&self, config: &ResumeConfig) -> Result<ResumeContext>` | 3-tier fact retrieval for session bootstrapping. Returns `NotFound` if the requested scope path does not exist. |

`ResumeConfig` fields:

- `scope_path: Option<String>` -- scope to resume from (ancestors are included).
- `core_min_importance: f64` -- importance threshold for the core tier.
- `identity_limit`, `core_limit`, `recent_limit` -- max facts per tier.

`ResumeContext` fields:

- `identity: Vec<Fact>` -- root-scope, highest-importance facts.
- `core: Vec<Fact>` -- above-threshold facts from ancestors.
- `recent: Vec<Fact>` -- most recent facts from ancestors.

### Config

| Method       | Signature                                       | Description                    |
| ------------ | ----------------------------------------------- | ------------------------------ |
| `get_config` | `(&self, key: &str) -> Result<Option<String>>`  | Read a config value by key.    |
| `set_config` | `(&self, key: &str, value: &str) -> Result<()>` | Write a config value (upsert). |

## Re-exported Types

The crate root (`lib.rs`) re-exports these items for convenience:

- **Engine**: `MemoryEngine`, `EngineConfig`
- **Error**: `MemoryError`, `Result`
- **Traits**: `EmbeddingProvider`
- **Types**: all public types from the `types` module (`Event`, `Fact`, `Edge`, `Summary`, `NewEvent`, `NewFact`, `NewEdge`, `NewSummary`, `EventType`, `FactType`, `ConsolidationLevel`, `ScopeQuery`, `ScopeNode`, `AddFactOptions`)
- **Store utilities**: `serialize_embedding`, `deserialize_embedding`

## Feature Flags

`async`
: Enables the `async_engine` module, which provides `AsyncMemoryEngine`. This is a thin wrapper that delegates every method to the synchronous `MemoryEngine` via `tokio::task::spawn_blocking`. All method signatures mirror the sync API but return `impl Future`.

## Error Handling

All fallible methods return `Result<T>`, which is `std::result::Result<T, MemoryError>`.

`MemoryError` variants:

| Variant                                   | Trigger                                                         |
| ----------------------------------------- | --------------------------------------------------------------- |
| `NotFound(String)`                        | Requested fact or scope does not exist.                         |
| `Database(rusqlite::Error)`               | SQLite operation failed.                                        |
| `Serialization(serde_json::Error)`        | JSON serialization/deserialization failed.                      |
| `EmbeddingDimension { expected, actual }` | Embedding vector has wrong number of dimensions.                |
| `Conflict(String)`                        | Conflict resolution logic error or invalid policy parameters.   |
| `Migration(String)`                       | Schema migration failed (e.g., `embed_dim` mismatch on reopen). |
| `NotImplemented(String)`                  | Feature not yet implemented.                                    |
| `Pool(String)`                            | Connection pool error.                                          |
