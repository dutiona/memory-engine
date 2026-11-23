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

| Method                | Signature                                                  | Description                                                                                                                    |
| --------------------- | ---------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| `builder`             | `(embed_dim: usize) -> MemoryEngineBuilder<InMemory>`      | Start building an engine: `.build()` opens it in memory; `.path(p)` makes it file-backed, created or opened at `p`, whose recorded dimension must equal `embed_dim`. |
| `declared_identity`   | `(self, identity: EmbeddingFingerprint) -> Self` (file builder) | Open only a store whose recorded embedding identity equals `identity`, checked read-only before any migration (#1065). |
| `machines`            | `(self, names: impl IntoIterator<Item = String>) -> Self` (any builder) | The deployment's machine names: a top-level `machine` equal to one, in an event payload or fact metadata, is stored as written, where `erase --machine` looks (#1136). `build` refuses a name that is not a lowercase DNS label. |
| `read_recorded_embedding` | `(db: &Path) -> Result<RecordedEmbedding>` (free function) | What a store records of its embedding (identity, bare dimension, or nothing), read at any schema version without migrating. |
| `restore_json`        | `(snapshot: &Path, config: &EngineConfig) -> Result<Self>` | Restore from a JSON snapshot into a new file-backed engine. Auto-detects gzip/zstd compression. Config path must not exist.    |
| `restore_json_memory` | `(snapshot: &Path) -> Result<Self>`                        | Restore from a JSON snapshot into a new in-memory engine. Auto-detects compression.                                            |
| `restore_sqlite`      | `(backup: &Path, config: &EngineConfig) -> Result<Self>`   | Restore from a `dump_sqlite()` backup of any schema version into a new file-backed engine, migrated on open. Only accepts clean VACUUM INTO backups (no live WAL DBs). |
| `embed_dim`           | `(&self) -> usize`                                         | Return the configured embedding dimension.                                                                                     |
| `is_file_backed`      | `(&self) -> bool`                                          | Whether the engine uses a file (vs in-memory).                                                                                 |

### Ingest

| Method     | Signature                                                                                        | Description                                                                                                                                                  |
| ---------- | ------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `ingest`   | `(&self, event: &NewEvent) -> Result<i64>`                                                       | Append an event to the log. Returns the event ID.                                                                                                            |
| `add_fact` | `(&self, req: &AddFactRequest, embedder: Arc<dyn EmbeddingProvider>, classifier: Option<Arc<dyn PersistenceClassifier>>) -> Result<i64>` | Compute embedding, blake3 hash, resolve scope, optionally auto-pin via classifier, and insert a fact. Embedding is computed before acquiring the write lock. |

The deferred path (#1021) stores a fact without waiting for an embedder; the [embedding queue](#embedding-queue) gives it its vector later:

| Method               | Signature                                      | Description                                                                                                                                                                                                                                                                           |
| -------------------- | ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `add_fact_deferred`  | `(&self, req: &AddFactRequest) -> Result<i64>` | Insert the fact with no vector and queue it, in one transaction, then wake the drain. Same gates and validation as `add_fact`; no classifier (`opts.pinned`, else unpinned), no identity recorded. Full-text search finds it at once; no vector reader sees it until it is embedded. |
| `embed_work_arrived` | `(&self)` (async)                              | Resolves once a deferred write has committed since the last wake; one stored wake covers every write made while no drain waited.                                                                                                                                                     |

### Embedding queue

The two steps of a deferred-embedding drain (#1021); the drain loop is the consumer's (`memory-engine-mcp`'s `drain::run`).

| Item                      | Signature                                                                                                                             | Description |
| ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- | ----------- |
| `next_embedding`          | `(&self) -> Result<NextEmbedding>`                                                                                                    | The item to embed next: `Due(PendingEmbedding)`, `NotBefore(time)` of the earliest waiting item, or `Idle`. Failed items are never offered. |
| `settle_embedding`        | `(&self, item: &PendingEmbedding, result: Result<Vec<f32>>, fingerprint: &EmbeddingFingerprint, backoff: Backoff) -> Result<Settled>` | Record the embedder's answer. A vector fills the fact (never over another vector) and leaves the queue: `Embedded`; one the store refuses (width, identity) backs the item off: `Retrying`. `Embedder(Unreachable)`: no item changes, `Unreachable`. `Embedder(KeyRejected)`: the queue pauses, `KeyRejected`. `Embedder(InputRejected)`: the item fails, `Rejected`. Any other error backs only the item off by `backoff.delay(attempts + 1)`: `Retrying`. An answered request clears the pause. Refused on a read-only engine. |
| `read_embed_queue_status` | `(db: &Path) -> Result<EmbedQueueStatus>` (free function)                                                                             | The queue's depth, oldest waiting item, failed count, pause and its error, last success and last error, through one read-only SQLite connection: builds no engine, takes no lock, never migrates. A store at another schema version is a `Migration` error. |
| `Backoff`                 | `{ base: Duration, cap: Duration }`; `Backoff::DEFAULT` (1 s, 60 s); `delay(self, n: u32) -> Duration`                                | The per-item retry schedule: `delay(0)` is zero, else `min(cap, base·2^(n−1))`, saturating at `cap`. |
| `Settled`                 | `Embedded`, `Rejected`, `Retrying`, `Unreachable`, `KeyRejected`                                                                      | What `settle_embedding` did; the drain paces itself by it. |

### Facts

| Method              | Signature                                                     | Description                                           |
| ------------------- | ------------------------------------------------------------- | ----------------------------------------------------- |
| `get_fact`          | `(&self, id: i64) -> Result<Fact>`                            | Retrieve a fact by ID. Returns `NotFound` if missing. |
| `list_active_facts` | `(&self, limit: Option<usize>) -> Result<Vec<Fact>>`          | List non-expired facts; `Some(n)` pushes SQL `LIMIT`. |
| `list_summaries`    | `(&self, level: &ConsolidationLevel) -> Result<Vec<Summary>>` | List summaries filtered by consolidation level.       |

### Scopes

| Method               | Signature                                     | Description                                                                                       |
| -------------------- | --------------------------------------------- | ------------------------------------------------------------------------------------------------- |
| `ensure_scope_path`  | `(&self, path: &str) -> Result<i64>`          | Create any missing segment of a scope path and return the leaf's id. Refused on a read-only engine. |
| `resolve_scope_path` | `(&self, path: &str) -> Option<i64>`          | The read-only twin: the path's id from the cached scope tree, or `None`. Never creates a node.      |

### Store lock

| Item                         | Signature                                               | Description                                                                                                                                             |
| ---------------------------- | ------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `StoreLock::acquire`         | `(db: &Path) -> Result<StoreLock, StoreLockError>`      | Take the exclusive advisory lock `<db>.store-lock` without waiting. `Held` when another process holds it, `Io` when it cannot be opened. Released on drop. |
| `StoreLock::path`            | `(&self) -> &Path`                                      | The lock file.                                                                                                                                          |

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
- `include_expired_probe: bool` -- also count expired facts matching the FTS query (one extra SQL count; needs `text`).
- `include_archives: bool` -- also brute-force search the archived `.pak` files, merged and re-ranked by score; a slow
  fallback (`archive` feature).

`SearchResult` fields:

- `fact: Fact` -- the matched fact.
- `score: f64` -- relevance score (search path) or `importance_score` (store path).
- `match_type: MatchType` -- `Fts`, `Vector`, `Both`, or `ImportanceRank` (non-exhaustive).

### Consolidation

| Method        | Signature                                                                                                                                 | Description                                                                                                                                                                                                         |
| ------------- | ----------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `consolidate` | `(&self, generator: &dyn SummaryGenerator, embedder: &dyn EmbeddingProvider, config: &ConsolidationConfig) -> Result<ConsolidationStats>` | Run three-pass consolidation: local dedup, cluster fusion, global integration. The generator produces summary text; the embedder projects it into the fact vector space. Rebuilds graph if duplicates were removed. |

`ConsolidationConfig` fields:

- `dedup_threshold: f32` -- cosine similarity threshold for dedup, in `[0.0, 1.0]` (default 0.90). Facts with similarity **>=** this value are merged; `1.0` merges only exact duplicates.
- `min_cluster_size: usize` -- minimum facts to form a cluster (must be `>= 2`; default 2).

### Pruning

| Method   | Signature                                              | Description                                                               |
| -------- | ------------------------------------------------------ | ------------------------------------------------------------------------- |
| `prune` | `(&self, policy: &PrunePolicy) -> Result<PruneStats>` | Soft-delete facts with computed importance below `policy.min_importance` (the decay prune; the CLI's `prune`). |

### Retraction and erasure

The model's `forget` tool and the maintenance commands `unforget` and `erase` (#1023, ADR-0020 §8).

| Method | Signature | Description |
| ------ | --------- | ----------- |
| `forget_fact` | `(&self, id: i64, homes: &[String], reason: &str, record_at: &str, record: &NewEvent) -> Result<(ForgetOutcome, i64)>` | Retract a fact filed at one of `homes` (exact scope paths), and its active identical copies there, behind tombstones, with the call record in the same transaction. Answers `Forgotten`, `AlreadyForgotten` or `NotFound` and the record's id. |
| `unforget_fact` | `(&self, id: i64) -> Result<UnforgetOutcome>` | Reverse the forget that retracted `id`, restoring every fact it took to its prior state. |
| `erase` | `(&self, target: &EraseTarget) -> Result<EraseReport>` | Hard-delete a fact, or what a machine wrote, with what was derived from it; purge the files after the commit. |

A tombstone blocks its content from every gated insert: `insert_or_reinforce` (`BootstrapReport::facts_blocked`),
`add_facts_batch` (refused whole, `Conflict(Forgotten)`), `add_facts_batch_partial` (that entry only), conflict
resolution, promotion (`promote_atomic` refuses; `apply_cycle_report` skips and counts `ApplyResult::blocked`) and
activity promotion. Only `add_fact`, `add_fact_precomputed` and `add_fact_deferred`, the single explicit assertion, are
never blocked.

`PrunePolicy` fields:

- `half_life_days: f64` -- base Ebbinghaus half-life (default: 69.0).
- `half_life_overrides: HashMap<FactType, f64>` -- per-type overrides.
- `min_importance: f64` -- expiration threshold (default: 0.1).
- `recency_weight`, `frequency_weight`, `graph_degree_weight`, `base_importance_weight` -- signal weights (default: 0.3, 0.2, 0.3, 0.2).

### Lifecycle events

The server's `POST /lifecycle/events` route applies one neutral record per harness event (#1025, ADR-0020 §3, §5, §6;
[MCP server](mcp-server.md#lifecycle-events)). The record types are in `types::lifecycle` (namespaced, never flat).

| Method | Signature | Description |
| ------ | --------- | ----------- |
| `record_lifecycle_event` | `(&self, delivery: &Delivery, body: Result<NeutralRecord, BodyFault>, denylist: &[String]) -> Result<Recorded>` | Apply one delivery under the capture contract, its content redacted with `denylist` too (never an identifier; `&[]` for signatures only, #1121), in one transaction with its `(machine, event ID)` receipt. A held key answers `Duplicate` or `Collision` and writes nothing; an unparseable body (`BodyFault::Parse`) is held as metadata only (`Quarantined`); an unmappable one, a refused identifier or a record off the engine's invariants is `Refused` and leaves no key; otherwise the event, its activity, the memory a main final message or compaction summary becomes (queued for its vector) and its segment step commit together (`Stored`). `ReadOnly` on a read-only engine; `Storage(Unavailable)` when the store cannot take the write now. |
| `end_idle_segments` | `(&self, now: DateTime<Utc>) -> Result<usize>` | End every open segment whose session the server has heard nothing of for 30 minutes before `now`; returns how many ended. |
| `read_quarantined` | `(db: &Path) -> Result<u64>` (free function) | The quarantine records held, through one read-only SQLite connection: builds no engine, takes no lock, never migrates. |

`types::lifecycle` holds `NeutralRecord`, `Content`, `Actor`, `Kind`, `Delivery`, `Recorded`, `BodyFault`, the order key
`OrderKey` and section 5's rule as a pure function, `step`; `LIFECYCLE_BODY_BYTES` (16 MiB) is the one size bound a
captured record meets. The port is the bounded trait `me_storage::Lifecycle`; Postgres does not implement it.

### Backfill import

The server's `POST /backfill/uploads` route (#1026, [MCP server](mcp-server.md#backfill-uploads)) reads one uploaded
file into neutral items and hands them to the engine in one call (`engine::backfill`).

| Method | Signature | Description |
| ------ | --------- | ----------- |
| `import_backfill` | `(&self, upload: &Upload, items: Vec<ImportItem>, denylist: &[String], mode: BatchMode) -> Result<Imported>` | Redact every item with every rule and the author `denylist` (never an identifier), refuse and skip what is off the lifecycle core's invariants, sort the rest by order key, and apply them in one store transaction through `Lifecycle::record_deliveries`: an event under the capture contract (labelled `backfill`, its memory dated by its event), a memory as one vectorless fact recorded by one event. `BatchMode::DryRun` rolls back; `BatchMode::Write { preview }` commits only when no item collided and the preview, a digest of every effect, of the whole body (`Upload::body_sha256`, `Upload::census`) and of the store's outcomes, is `preview`. Nothing is embedded; the scope cache, the drain's wake and the redaction totals move only after a commit. `ReadOnly` on a read-only engine; `Storage(Unavailable)` when the store cannot take the write now. An `Upload` with `copy` set is a plugin store's (#1117): its events are labelled with its kind, yield to live capture from 60 s before a session's first live event, and are `ImportOutcome::Superseded` where another import holds their actor and kind in the session. |

`types::lifecycle` holds the import's types: `Upload` with `Upload::copy` (a plugin store's copy, labelled with its
kind, #1117), `ImportItem` (`Event(ImportEvent)` or `Memory(ImportMemory)`), `MemoryItem`, `BatchMode`, `Imported`
with each item's `ImportOutcome` (including `ImportOutcome::Superseded`) and its `Imported::held_by` (per session,
the other origins holding it), and each memory's `FactPreview` with `FactPreview::importance`; and the store's batch,
`Keyed`, `MemoryWrite`, `Batch` with `Batch::held_by` and `preview_digest`, which the store computes
and the engine relies on. `BACKFILL_BODY_BYTES` (256 MiB) is the route's body limit and the importer's stream bound.
The readers use two parts of ADR-0009's importer, `bootstrap::read_json_lines` (its bounded JSONL loop, one JSON object
per line with its line number) and `bootstrap::memory_item` (a memory file read without redaction or embedding), its
timestamp grammar, `bootstrap::parse_timestamp`, and `bootstrap::records_an_interrupt`, the one rule both transcript
readers judge an interrupted tool call by (#1154).

### Conflict Resolution

| Method             | Signature                                                                                               | Description                                                                                                                                |
| ------------------ | ------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `resolve_conflict` | `(&self, arbiter: &dyn ConflictArbiter, old_id: i64, new_fact: &NewFact) -> Result<ConflictResolution>` | Delegate conflict decision to the arbiter. On `Update`, expires old fact and creates a `superseded_by` edge. Runs in a single transaction. |

### Graph

| Method               | Signature                                                         | Description                                                                                                                                                                                                           |
| -------------------- | ----------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `graph_degree`       | `(&self, fact_id: i64) -> usize`                                  | In + out edge count for a fact.                                                                                                                                                                                       |
| `graph_neighbors`    | `(&self, fact_id: i64) -> Vec<i64>`                               | Outgoing neighbor fact IDs.                                                                                                                                                                                           |
| `graph_component`    | `(&self, fact_id: i64) -> Vec<i64>`                               | All fact IDs in the connected component containing `fact_id`.                                                                                                                                                         |
| `graph_stats`        | `(&self) -> (usize, usize)`                                       | `(node_count, edge_count)`.                                                                                                                                                                                           |
| `graph_has_node`     | `(&self, fact_id: i64) -> bool`                                   | Whether a node exists in the graph.                                                                                                                                                                                   |
| `link_session_facts` | `(&self, session_id: &str, scope: Option<&str>) -> Result<usize>` | Create bidirectional `co_session` edges between all active facts sharing a session. When `scope` is `Some`, only facts within that scope subtree are considered. Idempotent. Returns the number of new edges created. |

### Pinning

| Method       | Signature                        | Description                                                        |
| ------------ | -------------------------------- | ------------------------------------------------------------------ |
| `pin_fact`   | `(&self, id: i64) -> Result<()>` | Pin a fact (exempt it from pruning). Bypasses pruning and dedup.   |
| `unpin_fact` | `(&self, id: i64) -> Result<()>` | Unpin a fact (allow pruning).                                      |

### Inspection

| Method          | Signature                                              | Description                                                                                                   |
| --------------- | ------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------- |
| `statistics`    | `(&self) -> Result<EngineStatistics>`                  | Aggregate counts of facts, edges, summaries, scopes, events, and storage metrics.                             |
| `explain_fact`  | `(&self, id: i64) -> Result<FactExplanation>`          | Why a fact is in its current state: provenance (with source event when available), graph context, scope path. |
| `fact_history`  | `(&self, id: i64) -> Result<FactHistory>`              | Bi-temporal timeline of a fact's lifecycle from its temporal stamps.                                          |
| `replay_events` | `(&self, filter: &ReplayFilter) -> Result<Vec<Event>>` | Replay a filtered segment of the event log. Supports ID range, time window, session, and upcast.              |
| `dump_state`    | `(&self, format: &DumpFormat) -> Result<()>`           | Export full engine state to JSON or SQLite backup.                                                            |

### Scheduling

| Method          | Signature                                                               | Description                                                                                                                                                                                                                                                                                                                       |
| --------------- | ----------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `list_due`      | `(&self, now: DateTime<Utc>, scope: Option<&str>) -> Result<Vec<Fact>>` | List active facts whose `t_valid <= now` (future memory that has surfaced). Stamps `surfaced_at` on first return so callers can distinguish newly-due from previously-surfaced facts — the old heuristic (`access_count > 0 && last_accessed > t_valid`) was unreliable for bootstrap/past-dated facts. `None` scope = root only. |
| `next_due_time` | `(&self, scope: Option<&str>) -> Result<Option<DateTime<Utc>>>`         | Scheduling hint: earliest `t_valid` among active future-dated facts. `None` scope = root only.                                                                                                                                                                                                                                    |

### Resume

| Method           | Signature                                                 | Description                                                                                                     |
| ---------------- | --------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- |
| `resume_context` | `(&self, config: &ResumeConfig) -> Result<ResumeContext>` | 4-tier fact retrieval for session bootstrapping. Returns `NotFound` if the requested scope path does not exist. |

`ResumeConfig` fields:

- `scope_path: Option<String>` -- scope to resume from (ancestors are included).
- `now: Option<DateTime<Utc>>` -- evaluation instant for due facts. `None` (the default) defers to `resume_context`, which resolves `Utc::now()` once at call time; `Some(_)` pins a specific instant.
- `pinned_cap: usize` -- max pinned facts (default: 50).
- `high_importance_cap: usize` -- max high-importance facts (default: 20).
- `high_importance_min: f64` -- minimum `importance_score` for tier 2 (default: 0.7).
- `due_cap: usize` -- max due facts (default: 10).
- `recent_cap: usize` -- max recent facts (default: 10).

`ResumeContext` fields (4 tiers, mutually exclusive):

- `pinned: Vec<Fact>` -- tier 1: facts never pruned, cross-scope, sorted by `importance_score` descending.
- `high_importance: Vec<Fact>` -- tier 2: top facts by materialized `importance_score`.
- `due: Vec<Fact>` -- tier 3: future-memory facts whose `t_valid` has arrived.
- `recent: Vec<Fact>` -- tier 4: most recent facts from scope ancestors.

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
- **Error**: `MemoryError`, `Result`, and the typed causes `ArchiveError`, `ConflictError`, `CycleError`, `MigrationError`, `RerankerError`, `StorageError`
- **Traits**: `ConflictArbiter`, `ConflictResolution`, `ConsolidationConfig`, `ConsolidationStats`, `CrudDecision`, `CycleCtx`, `DeltaProposer`, `DreamCtx`, `DreamCycle`, `EmbeddingProvider`, `InsightStream`, `PersistenceClassifier`, `Reranker`, `SummarizableContent`, `SummaryGenerator`
- **Types**: all public types from the `types` module (`Event`, `Fact`, `Edge`, `Summary`, `NewEvent`, `NewFact`, `NewEdge`, `NewSummary`, `EventType`, `FactType`, `ConsolidationLevel`, `ScopeQuery`, `ScopeNode`, `AddFactOptions`)
- **Embedding queue** (#1021): `Backoff`, `Settled`, `read_embed_queue_status`, and the queue types `PendingEmbedding`, `NextEmbedding`, `EmbedQueueStatus`; `EmbedderError` beside `MemoryError`
- **Lifecycle** (#1025): `read_quarantined` and `LIFECYCLE_BODY_BYTES`; the record types are `memory_engine::types::lifecycle`
- **Backfill** (#1026): `BACKFILL_BODY_BYTES`; the import types are `memory_engine::types::lifecycle`, and
  `read_json_lines`, `memory_item`, `parse_timestamp` and `records_an_interrupt` are in `memory_engine::bootstrap`

## Feature Flags

`crates/memory-engine/Cargo.toml`'s `[features]` table lists the flags; no flag gates the runtime, since `tokio` is a non-optional dependency (#702). `MemoryEngine`'s DB-touching methods are `async fn` and must be `.await`ed from inside a tokio runtime (`#[tokio::main]` or `Runtime::block_on`); there is no `AsyncMemoryEngine` wrapper.

## Error Handling

All fallible methods return `Result<T>`, which is `std::result::Result<T, MemoryError>`.

`MemoryError` is `#[non_exhaustive]`: a `match` on it needs a wildcard arm.

`MemoryError` variants:

| Variant                                                                                                                         | Trigger                                                                                            |
| ------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| `NotFound(String)`                                                                                                              | A requested entity (fact, event, scope, snapshot, …) does not exist.                               |
| `Serialization(serde_json::Error)`                                                                                              | JSON (de)serialization of a payload, snapshot or config failed.                                    |
| `EmbeddingDimension { expected: usize, actual: usize }`                                                                         | An embedding's length differs from the engine's configured dimension.                              |
| `EmbeddingModelMismatch { expected: Box<crate::types::EmbeddingFingerprint>, actual: Box<crate::types::EmbeddingFingerprint> }` | The provider's embedding identity differs from the one the store was created with (ADR 0015).      |
| `EmbeddingReopenRequired { new_dim: usize }`                                                                                    | A reconstruction promoted a new embedding dimension; reopen the engine at `new_dim`.               |
| `Conflict(ConflictError)`                                                                                                       | A precondition or input-validation conflict, or content a forget's tombstone blocks (`Forgotten`, #1023); `ConflictError` names the cause. |
| `Migration(MigrationError)`                                                                                                     | A schema migration or compatibility check failed; `MigrationError` names the cause.                |
| `NotImplemented(String)`                                                                                                        | The operation is recognized but not yet implemented.                                               |
| `Pool(String)`                                                                                                                  | The connection pool could not hand out or manage a connection.                                     |
| `UnsupportedEpoch { db_epoch: u16, supported_epoch: u16 }`                                                                      | The database's storage epoch is too old or too new for this build.                                 |
| `SnapshotTooLarge { cap: u64 }`                                                                                                 | A snapshot's decompressed stream exceeded the size cap (possible decompression bomb).              |
| `Internal(String)`                                                                                                              | An engine invariant was violated: a bug or corrupted state.                                        |
| `IndexInconsistent { fact_id: i64, detail: String }`                                                                            | The write committed, but the in-memory vector index is now inconsistent: rebuild it, do not retry. |
| `Io(std::io::Error)`                                                                                                            | A filesystem I/O operation failed.                                                                 |
| `Reranker(RerankerError)`                                                                                                       | The reranking stage failed; `RerankerError` names the cause.                                       |
| `Archive(ArchiveError)`                                                                                                         | A cold-storage archive operation failed; `ArchiveError` names the cause.                           |
| `ReadOnly`                                                                                                                      | A write was attempted on an engine opened read-only.                                               |
| `Lineage(String)`                                                                                                               | A wisdom-fact lineage or provenance record is missing or inconsistent.                             |
| `Cycle(CycleError)`                                                                                                             | A dream-cycle report failed validation or application; `CycleError` names the cause.               |
| `Storage(StorageError)`                                                                                                         | A failure at the storage seam: an opaque backend error, a missing capability, or `Unavailable` (the store cannot take this write now: busy, locked, full, an I/O failure or a file that cannot be opened; the same write can work later). |
| `Embedder(EmbedderError)`                                                                                                       | The embedding provider got no vector. `EmbedderError` says what a retry can do: `Unreachable` (the request never reached the server), `KeyRejected` (401, 403), `InputRejected` (400, 413, 422: this input will not be embedded as sent), `NoAnswer` (a timeout, a reset, a broken body: no answer came back), `Failed` (any other non-2xx answer). |
