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

| Method     | Signature                                                                                        | Description                                                                                                                                                  |
| ---------- | ------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `ingest`   | `(&self, event: &NewEvent) -> Result<i64>`                                                       | Append an event to the log. Returns the event ID.                                                                                                            |
| `add_fact` | `(&self, content, fact_type, source_event_id, embedder, scope, opts, classifier) -> Result<i64>` | Compute embedding, blake3 hash, resolve scope, optionally auto-pin via classifier, and insert a fact. Embedding is computed before acquiring the write lock. |

### Bootstrap

| Method                | Signature                                                                                 | Description                                                                                                             |
| --------------------- | ----------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| `bootstrap_session`   | `(&self, reader, embedder, extractor, config, classifier) -> Result<BootstrapReport>`     | Bootstrap a single JSONL session log. Savepoint-wrapped for crash safety. Uses marker event for idempotency.            |
| `bootstrap_directory` | `(&self, dir: &Path, embedder, extractor, config, classifier) -> Result<BootstrapReport>` | Bootstrap all top-level `*.jsonl` files in a directory. Aggregates reports. Individual failures are logged and skipped. |

### Facts

| Method              | Signature                                                     | Description                                           |
| ------------------- | ------------------------------------------------------------- | ----------------------------------------------------- |
| `get_fact`          | `(&self, id: i64) -> Result<Fact>`                            | Retrieve a fact by ID. Returns `NotFound` if missing. |
| `list_active_facts` | `(&self) -> Result<Vec<Fact>>`                                | List all non-expired facts.                           |
| `list_summaries`    | `(&self, level: &ConsolidationLevel) -> Result<Vec<Summary>>` | List summaries filtered by consolidation level.       |

### Query

| Method          | Signature                                                   | Description                                                                                                                             |
| --------------- | ----------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `query`         | `(&self, query: &SearchQuery) -> Result<Vec<SearchResult>>` | Hybrid search combining FTS5, vector similarity, and RRF merge. Respects scope filtering. Returns empty results for nonexistent scopes. |
| `execute_query` | `(&self, query: &MemoryQuery) -> Result<Vec<SearchResult>>` | Fluent query builder API composing scope, temporal, search, and filter dimensions. See `MemoryQuery` below.                             |

`SearchQuery` fields:

- `text: Option<String>` -- FTS query string.
- `embedding: Option<Vec<f32>>` -- query vector for semantic search.
- `mode: SearchMode` -- `Fts`, `Vector`, or `Hybrid`.
- `limit: usize` -- max results.
- `valid_at: Option<DateTime<Utc>>` -- temporal filter on valid time.
- `fact_type: Option<FactType>` -- filter by fact type.
- `scope: Option<ScopeQuery>` -- scope resolution strategy.

`MemoryQuery` fields (all optional, AND semantics):

- `scope: Option<ScopeQuery>` -- scope filter.
- `period_start/period_end: Option<DateTime<Utc>>` -- temporal period overlap `[start, end)`.
- `text: Option<String>` -- FTS query (triggers search path).
- `embedding: Option<Vec<f32>>` -- vector query (triggers search path).
- `search_mode: Option<SearchMode>` -- override inferred mode (default: infer from text/embedding).
- `fact_type: Option<FactType>` -- filter by fact type.
- `min_importance_score: Option<f64>` -- minimum materialized importance score.
- `pinned_only: bool` -- return only pinned facts.
- `limit: Option<usize>` -- max results (default: 50).
- `valid_at: Option<DateTime<Utc>>` -- point-in-time filter (mutually exclusive with period).

`SearchResult` fields:

- `fact: Fact` -- the matched fact.
- `score: f64` -- relevance score (search path) or `importance_score` (store path).
- `match_type: MatchType` -- `Fts`, `Vector`, `Both`, or `ImportanceRank` (non-exhaustive).

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

| Method               | Signature                                    | Description                                                                                                                              |
| -------------------- | -------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| `graph_degree`       | `(&self, fact_id: i64) -> usize`             | In + out edge count for a fact.                                                                                                          |
| `graph_neighbors`    | `(&self, fact_id: i64) -> Vec<i64>`          | Outgoing neighbor fact IDs.                                                                                                              |
| `graph_component`    | `(&self, fact_id: i64) -> Vec<i64>`          | All fact IDs in the connected component containing `fact_id`.                                                                            |
| `graph_stats`        | `(&self) -> (usize, usize)`                  | `(node_count, edge_count)`.                                                                                                              |
| `graph_has_node`     | `(&self, fact_id: i64) -> bool`              | Whether a node exists in the graph.                                                                                                      |
| `link_session_facts` | `(&self, session_id: &str) -> Result<usize>` | Create bidirectional `co_session` edges between all active facts sharing a session. Idempotent. Returns the number of new edges created. |

### Pinning

| Method       | Signature                        | Description                                                        |
| ------------ | -------------------------------- | ------------------------------------------------------------------ |
| `pin_fact`   | `(&self, id: i64) -> Result<()>` | Pin a fact (make it unforgettable). Bypasses forgetting and dedup. |
| `unpin_fact` | `(&self, id: i64) -> Result<()>` | Unpin a fact (allow forgetting).                                   |

### Inspection

| Method          | Signature                                              | Description                                                                                      |
| --------------- | ------------------------------------------------------ | ------------------------------------------------------------------------------------------------ |
| `statistics`    | `(&self) -> Result<EngineStatistics>`                  | Aggregate counts of facts, edges, summaries, scopes, events, and storage metrics.                |
| `explain_fact`  | `(&self, id: i64) -> Result<FactExplanation>`          | Why a fact is in its current state: provenance, graph context, scope path.                       |
| `fact_history`  | `(&self, id: i64) -> Result<FactHistory>`              | Bi-temporal timeline of a fact's lifecycle from its temporal stamps.                             |
| `replay_events` | `(&self, filter: &ReplayFilter) -> Result<Vec<Event>>` | Replay a filtered segment of the event log. Supports ID range, time window, session, and upcast. |
| `dump_state`    | `(&self, format: &DumpFormat) -> Result<()>`           | Export full engine state to JSON or SQLite backup.                                               |

### Scheduling

| Method          | Signature                                                               | Description                                                                                                                                                                                                 |
| --------------- | ----------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `list_due`      | `(&self, now: DateTime<Utc>, scope: Option<&str>) -> Result<Vec<Fact>>` | List active facts whose `t_valid <= now` (future memory that has surfaced). Stamps `surfaced_at` on first return so callers can distinguish newly-due from previously-surfaced facts — the old heuristic (`access_count > 0 && last_accessed > t_valid`) was unreliable for bootstrap/past-dated facts. `None` scope = root only. |
| `next_due_time` | `(&self, scope: Option<&str>) -> Result<Option<DateTime<Utc>>>`         | Scheduling hint: earliest `t_valid` among active future-dated facts. `None` scope = root only.                                                                                                              |

### Resume

| Method           | Signature                                                 | Description                                                                                                     |
| ---------------- | --------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- |
| `resume_context` | `(&self, config: &ResumeConfig) -> Result<ResumeContext>` | 5-tier fact retrieval for session bootstrapping. Returns `NotFound` if the requested scope path does not exist. |

`ResumeConfig` fields:

- `scope_path: Option<String>` -- scope to resume from (ancestors are included).
- `now: DateTime<Utc>` -- current time for due-fact evaluation.
- `pinned_cap: usize` -- max pinned facts (default: 50).
- `high_importance_cap: usize` -- max high-importance facts (default: 20).
- `high_importance_min: f64` -- minimum `importance_score` for tier 2 (default: 0.7).
- `due_cap: usize` -- max due facts (default: 10).
- `recent_cap: usize` -- max recent facts (default: 10).

`ResumeContext` fields (5 tiers, mutually exclusive):

- `pinned: Vec<Fact>` -- tier 1: unforgettable facts, cross-scope, sorted by `importance_score` descending.
- `high_importance: Vec<Fact>` -- tier 2: top facts by materialized `importance_score`.
- `due: Vec<Fact>` -- tier 3: future-memory facts whose `t_valid` has arrived.
- `recent: Vec<Fact>` -- tier 4: most recent facts from scope ancestors.
- `kb_stubs: Vec<String>` -- tier 5: placeholder for Phase 5 knowledge-base references.

### Bootstrap

| Method                | Signature                                                                                 | Description                                                                                                                                                    |
| --------------------- | ----------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `bootstrap_session`   | `(&self, reader, embedder, extractor, config, classifier) -> Result<BootstrapReport>`     | Parse a single JSONL session log and import noteworthy episodes as historical facts. Uses savepoint transactions for crash safety and event-based idempotency. |
| `bootstrap_directory` | `(&self, dir: &Path, embedder, extractor, config, classifier) -> Result<BootstrapReport>` | Discover and bootstrap all top-level `*.jsonl` files in a directory. Individual session failures are logged and skipped. Returns an aggregated report.         |

`BootstrapConfig` fields:

- `scope: Option<String>` -- scope path for ingested facts (e.g., `"project:my-app"`). `None` = root scope.
- `max_turns: usize` -- maximum turns to process per session. `0` = no limit.
- `skip_existing: bool` -- skip sessions already bootstrapped (default: `true`).

`BootstrapReport` fields:

- `sessions_processed`, `sessions_skipped`, `entries_parsed`, `entries_malformed` -- pipeline counters.
- `turns_reconstructed`, `candidates_found`, `facts_created`, `events_ingested` -- per-stage metrics.
- `outcome_counts: OutcomeCounts` -- breakdown by session outcome (success/failure/indeterminate).
- `category_counts: CategoryCounts` -- breakdown by episode category (bug/decision/convention/learning).
- `prewarm_metrics: PrewarmMetrics` -- fact-type distribution and average importance for cold-start analysis.

### Config

| Method       | Signature                                       | Description                    |
| ------------ | ----------------------------------------------- | ------------------------------ |
| `get_config` | `(&self, key: &str) -> Result<Option<String>>`  | Read a config value by key.    |
| `set_config` | `(&self, key: &str, value: &str) -> Result<()>` | Write a config value (upsert). |

## Re-exported Types

The crate root (`lib.rs`) re-exports these items for convenience:

- **Engine**: `MemoryEngine`, `EngineConfig`
- **Error**: `MemoryError`, `Result`
- **Traits**: `EmbeddingProvider` (note: `PersistenceClassifier` is available via `memory_engine::traits::PersistenceClassifier`, not re-exported at root)
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
| `Internal(String)`                        | I/O failure or other internal error (e.g., dump write failure). |
