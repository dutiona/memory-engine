# Crate Layout

Module map for the `memory-engine` crate. Each module corresponds to a file or directory under `src/`.

```text
memory_engine (lib.rs)
  +-- engine        Facade + MemoryEngine struct + EngineConfig
  +-- types         Core data types and enums
  +-- error         Error enum and Result alias
  +-- traits        Consumer-implemented traits and policy types
  +-- storage/      Persistence PORT (StorageBackend trait family) — infra abstraction
  |     +-- sqlite/  SqliteBackend: the SQLite impl of the port (cfg async)
  +-- search/       Hybrid retrieval pipeline
  +-- store/        SQLite persistence layer (the SQL SqliteBackend delegates to)
  +-- graph         In-memory knowledge graph
  +-- consolidation Three-pass consolidation pipeline
  +-- forgetting    Ebbinghaus decay and importance scoring
  +-- pool          Connection pool (N readers + 1 writer)
  +-- scope         Hierarchical scope tree cache
  +-- bootstrap     Session log bootstrap pipeline
  +-- resume        Session bootstrapping (4-tier retrieval)
  +-- inspect       Debugging and observability APIs
```

## Module Descriptions

`engine`
: Facade over all memory primitives. Defines `MemoryEngine` (the main entry point) and `EngineConfig`. The engine is async-native: its DB-touching methods are `async fn` that `.await` an `Arc<dyn StorageBackend>` port, so thread safety and blocking-IO offload live in the backend (`spawn_blocking`); the in-memory caches the engine still owns stay `RwLock`-protected. `engine::conflict` holds the bi-temporal `MemoryEngine::resolve_conflict` (delegated to the consumer `ConflictArbiter`).

`engine::cycle`
: Phase-5a dream-cycle subsystem (#49). `report` — the delta-based `CycleReport` vocabulary (`CycleDelta`, `IdentityOutput`, `CycleMetadata`, `ApplyResult`, `IMPORTANCE_STEP`). `context` — `CycleContext`, the retrieve-before-reflect wrapper around `DreamContext`. `apply` — `MemoryEngine::apply_cycle_report`, the validate-all-then-apply-all transactional delta applier. `dbscan` — a pure deterministic DBSCAN clustering core. `default_impl` — `DefaultDreamCycle`, the shipped pure-Rust producer. See `docs/advanced/dream-cycle.md` and ADR 0014.

`types`
: Core data types returned by and passed into the engine. Includes full structs (`Event`, `Fact`, `Edge`, `Summary`, `ScopeNode`), insertion structs (`NewEvent`, `NewFact`, `NewEdge`, `NewSummary`), enums (`EventType`, `FactType`, `ConsolidationLevel`, `ScopeQuery`), and option structs (`AddFactOptions`).

`error`
: `MemoryError` enum with `thiserror` derivation. Variants: `NotFound`, `Database` (from `rusqlite`), `Serialization` (from `serde_json`), `EmbeddingDimension`, `Conflict`, `Migration`, `NotImplemented`, `Pool`, `UnsupportedEpoch`, `Internal`, `Io`, `Bootstrap`, `Reranker`, `ReadOnly`. Also exports `Result<T>` as an alias for `std::result::Result<T, MemoryError>`.

`traits`
: Consumer-implemented traits that the engine delegates to for domain-specific behavior:

- `EmbeddingProvider` -- compute text embeddings (local model or API).
- `SummaryGenerator` -- generate summary text from fact clusters (embedding is done by the `EmbeddingProvider` injected into `consolidate()`).
- `ConflictArbiter` -- decide how to resolve contradicting facts (returns `CrudDecision`).
- `PersistenceClassifier` -- decide whether a new fact should be pinned (unforgettable). Optional, default returns `false`.
  Also defines `ForgetPolicy` (Ebbinghaus parameters and signal weights), `ConsolidationConfig`, `ConsolidationStats`, `PruneStats`, `ConflictResolution`, and `CrudDecision`.

`storage`
: The persistence **port** (infrastructure abstraction) — deliberately distinct from `traits` (consumer capability injection). Defines the `StorageBackend` umbrella supertrait over six bounded-context traits — `FactGraph`, `EventLog`, `SearchIndex`, `ConsolidationStore`, `SessionStore`, `SchemaManager` — plus the feature-gated `ColdStorage` (held separately, not a supertrait bound). Cross-cutting types: the closed `FactFilter` (with `TemporalFilter` / `MetadataPredicate`), the dialect-free `BackendCapabilities` / `LexicalRanker` tier signal, and `StorageError` (driver-opaque, in `error`). All traits are `async_trait`/`Send + Sync`; `SearchIndex` returns ranked `(id, f64)` pairs (RRF fuses by rank engine-side). The concrete implementation lives in **`storage/sqlite/`** (`SqliteBackend`): one file per bounded trait, each a thin delegation over two private primitives — `block_read` / `block_write` (sync `rusqlite` wrapped in `spawn_blocking`) — plus a `for_each_streamed` bridge (cap-1 `tokio::sync::mpsc`) for the streaming methods. It **delegates** to the `store/` + `search/` SQL verbatim (it does not absorb it — `#634`'s `PgBackend` reuses none of those bodies). The seam confines `rusqlite` below it: a driver `Database` error is remapped to driver-opaque `StorageError::Backend`. The async-native engine holds this port as `Arc<dyn StorageBackend>` and `.await`s it for all DB-touching work (#631); a Postgres `PgBackend` behind the same trait family is future work (#633/#634).

`search`
: Hybrid search pipeline combining three retrieval modes:

- `search::fts` -- FTS5 full-text search with BM25 ranking.
- `search::vector` -- brute-force cosine similarity over stored embeddings.
- `search::hybrid` -- orchestrator that dispatches to FTS, vector, or both, then merges via Reciprocal Rank Fusion (RRF). Defines `SearchQuery`, `SearchResult`, `SearchMode`, and `MatchType`.

`store`
: SQLite persistence layer. Sub-modules handle distinct tables:

- `store::schema` -- DDL, migrations, `get_config`/`set_config`.
- `store::embedding_meta` -- single-active **facade** over `store::embedding_spaces`: typed persistence of the canonical `EmbeddingFingerprint` identity tuple (`load`/`store`/`record_if_absent`), recorded on the first embedding write (ADR 0015). The degenerate single-space case of the Knowledge layer's multi-space registry.
- `store::embedding_spaces` -- the `embedding_spaces` registry table owner (#622): `SpaceStatus`/`EmbeddingSpace` domain types, the `status` enum (`active`/`populating`/`deprecated`), the partial-unique single-active invariant, and row CRUD (`find_active`/`find_by_name`/`list_spaces`/`insert_active`/`upsert_active_fingerprint`). The #623 reconstruction seams live here too: `insert_populating`/`begin_populating` (idempotent open, crash-resume), `activate`, `deprecate`. HNSW reconfig + operator UX stay deferred to #624/#689.
- `store::fact_vectors` -- per-`(fact, space)` vector rows for the **non-active** embedding spaces (#623): the `populating` space mid-backfill, and a `deprecated` space retained for rollback. The **active** space's vectors stay in `facts.embedding` (no read-path change). Owns the cursorless anti-join backfill window, the idempotent `ON CONFLICT` batch write, `count_unbackfilled`, the atomic copy-swap `promote_space`, and the dump-streaming `for_each`.
- `store::events` -- `EventStore` (insert, get).
- `store::facts` -- `FactStore` (insert, get, list_active, expire, update importance).
- `store::summaries` -- `SummaryStore` (insert, list by level).
- `store::scopes` -- `ScopeStore` (ensure_path, get, list).
  Also exports `serialize_embedding` and `deserialize_embedding` for `Vec<f32>` <-> BLOB conversion.

`graph`
: In-memory `petgraph`-backed knowledge graph (`MemoryGraph`). Loaded from the `edges` table on engine open. Provides degree, neighbors, connected component, node/edge counts, and add/remove operations. Protected by `RwLock` inside `MemoryEngine`.

`consolidation`
: Three-pass consolidation pipeline:

1. **Local dedup** -- expire near-duplicate facts (cosine similarity above threshold).
2. **Cluster fusion** -- group related facts and generate cluster-level summaries.
3. **Global integration** -- produce cross-cluster summaries.
   Accepts a `SummaryGenerator` trait object, an `EmbeddingProvider` trait object (to embed the generated summaries), and `ConsolidationConfig`.

`forgetting`
: Ebbinghaus-based decay with multi-signal importance scoring. Computes a weighted combination of recency (exponential decay), access frequency, graph connectivity, and base importance. Facts scoring below `ForgetPolicy::min_importance` are soft-deleted (their `t_expired` is set). Returns `PruneStats`.

`engine::conflict`
: Bi-temporal conflict resolution (`MemoryEngine::resolve_conflict`). Given an existing fact and a candidate, delegates to a `ConflictArbiter` for the decision (`Add`, `Update`, `Delete`, `Noop`). On `Update`, the old fact is expired and a `superseded_by` edge is created in the graph. All mutations run in a single transaction.

`engine::reconstruct`
: Background reconstruction orchestration (`MemoryEngine::reconstruct`, #623). Re-embeds stored fact content under a new **same-dimension** identity with no downtime: open (or resume) a `populating` space → backfill it off the write lock (embedding under `spawn_blocking`) → catch-up pass → atomic copy-swap promote. The embedder stays engine-side; the backend does pure DB ops. Returns a `PromoteOutcome`. Different-dim is the #742 follow-up; the live HNSW rebuild is #624. See `docs/advanced/reconstruction.md`.

`pool`
: `ConnectionPool` wrapping SQLite connections. Uses `parking_lot::Mutex` for the single write connection and a bounded pool of read connections. Supports both file-backed and in-memory modes. Configurable read pool size (default: 4). Read-only mode (`open_read_only`) validates schema version without init/migrate and guards all writes with `MemoryError::ReadOnly`. Since #631 the pool is owned by `SqliteBackend` (the `storage` port), not the engine: the engine awaits the backend, which runs pool access on `tokio::task::spawn_blocking`. The engine touches a raw connection only at construction time (open-time validation).

`scope`
: `ScopeTree` -- in-memory cache of the hierarchical scope tree. Loaded from the `scopes` table on engine open. Resolves `ScopeQuery` variants (`Exact`, `Subtree`, `Ancestors`, `Inherited`) to sets of scope IDs without hitting the database.

`bootstrap`
: Session log bootstrap pipeline. Parses Claude Code JSONL session logs and imports noteworthy episodes (bug fixes, decisions, conventions, learnings) as historical facts. Sub-modules handle each pipeline stage: `parse` (JSONL deserialization), `filter` (turn reconstruction and keyword pre-filter), `outcome` (heuristic session outcome classification), `extract` (fact extraction via the `SessionExtractor` trait), and `metrics` (configuration, reporting, and prewarm quality metrics). Uses savepoint transactions for crash safety and event-based idempotency to prevent duplicate imports.

`resume`
: Session bootstrapping via `ResumeConfig` and `ResumeContext`. Implements 4-tier retrieval:

1. **Pinned** -- unforgettable facts (`is_pinned = true`), cross-scope, sorted by `importance_score` descending.
2. **High-importance** -- facts with materialized `importance_score` above a configurable threshold.
3. **Due** -- future-memory facts whose `t_valid` has arrived (`t_valid <= now`).
4. **Recent** -- most recent facts from scope ancestors.
   Tiers are mutually exclusive (a fact appears in at most one tier).

`inspect`
: Inspection APIs for debugging and observability. Sub-modules handle distinct concerns:

- `inspect::types` -- all inspection-specific types (`FactExplanation`, `FactState`, `EngineStatistics`, `ReplayFilter`, `DumpFormat`, etc.).
- `inspect::explain` -- `explain_fact()` state analysis and `fact_history()` temporal reconstruction.
- `inspect::replay` -- `ReplayFilter` to `EventFilter` conversion for event replay.
- `inspect::dump` -- JSON snapshot serialization and SQLite `VACUUM INTO` backup.
- `inspect::statistics` -- SQL count queries for aggregate statistics.

## Re-exports from `lib.rs`

The crate root re-exports the most commonly used items so consumers can `use memory_engine::*`:

- `MemoryEngine`, `EngineConfig` (from `engine`)
- `MemoryError`, `Result` (from `error`)
- `EmbeddingProvider` (from `traits`)
- `SessionExtractor`, `KeywordExtractor`, `BootstrapConfig`, `BootstrapReport` (from `bootstrap`)
- All public types (from `types`)
- `serialize_embedding`, `deserialize_embedding` (from `store`)
- `inspect_types` (from `inspect::types` — inspection-specific types)
