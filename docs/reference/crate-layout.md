# Crate Layout

Module map for the `memory-engine` crate. Each module corresponds to a file or directory under `src/`.

```text
memory_engine (lib.rs)
  +-- engine        Facade + MemoryEngine struct + EngineConfig
  +-- types         Core data types and enums
  +-- error         Error enum and Result alias
  +-- traits        Consumer-implemented traits and policy types
  +-- search/       Hybrid retrieval pipeline
  +-- store/        SQLite persistence layer
  +-- graph         In-memory knowledge graph
  +-- consolidation Three-pass consolidation pipeline
  +-- forgetting    Ebbinghaus decay and importance scoring
  +-- conflict      Bi-temporal conflict resolution
  +-- pool          Connection pool (N readers + 1 writer)
  +-- scope         Hierarchical scope tree cache
  +-- resume        Session bootstrapping (5-tier retrieval)
  +-- async_engine  Async wrapper (feature-gated)
```

## Module Descriptions

`engine`
: Facade over all memory primitives. Defines `MemoryEngine` (the main entry point) and `EngineConfig`. All public methods take `&self`; thread safety is handled internally via `ConnectionPool` and `RwLock`-protected caches.

`types`
: Core data types returned by and passed into the engine. Includes full structs (`Event`, `Fact`, `Edge`, `Summary`, `ScopeNode`), insertion structs (`NewEvent`, `NewFact`, `NewEdge`, `NewSummary`), enums (`EventType`, `FactType`, `ConsolidationLevel`, `ScopeQuery`), and option structs (`AddFactOptions`).

`error`
: `MemoryError` enum with `thiserror` derivation. Variants: `NotFound`, `Database` (from `rusqlite`), `Serialization` (from `serde_json`), `EmbeddingDimension`, `Conflict`, `Migration`, `NotImplemented`, `Pool`. Also exports `Result<T>` as an alias for `std::result::Result<T, MemoryError>`.

`traits`
: Consumer-implemented traits that the engine delegates to for domain-specific behavior:

- `EmbeddingProvider` -- compute text embeddings (local model or API).
- `SummaryGenerator` -- generate summaries from fact clusters, plus embed the result.
- `ConflictArbiter` -- decide how to resolve contradicting facts (returns `CrudDecision`).
- `PersistenceClassifier` -- decide whether a new fact should be pinned (unforgettable). Optional, default returns `false`.
  Also defines `ForgetPolicy` (Ebbinghaus parameters and signal weights), `ConsolidationConfig`, `ConsolidationStats`, `PruneStats`, `ConflictResolution`, and `CrudDecision`.

`search`
: Hybrid search pipeline combining three retrieval modes:

- `search::fts` -- FTS5 full-text search with BM25 ranking.
- `search::vector` -- brute-force cosine similarity over stored embeddings.
- `search::hybrid` -- orchestrator that dispatches to FTS, vector, or both, then merges via Reciprocal Rank Fusion (RRF). Defines `SearchQuery`, `SearchResult`, `SearchMode`, and `MatchType`.

`store`
: SQLite persistence layer. Sub-modules handle distinct tables:

- `store::schema` -- DDL, migrations, `get_config`/`set_config`.
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
   Accepts a `SummaryGenerator` trait object and `ConsolidationConfig`.

`forgetting`
: Ebbinghaus-based decay with multi-signal importance scoring. Computes a weighted combination of recency (exponential decay), access frequency, graph connectivity, and base importance. Facts scoring below `ForgetPolicy::min_importance` are soft-deleted (their `t_expired` is set). Returns `PruneStats`.

`conflict`
: Bi-temporal conflict resolution. Given an existing fact and a candidate, delegates to a `ConflictArbiter` for the decision (`Add`, `Update`, `Delete`, `Noop`). On `Update`, the old fact is expired and a `superseded_by` edge is created in the graph. All mutations run in a single transaction.

`pool`
: `ConnectionPool` wrapping SQLite connections. Uses `parking_lot::Mutex` for the single write connection and a bounded pool of read connections. Supports both file-backed and in-memory modes. Configurable read pool size (default: 4).

`scope`
: `ScopeTree` -- in-memory cache of the hierarchical scope tree. Loaded from the `scopes` table on engine open. Resolves `ScopeQuery` variants (`Exact`, `Subtree`, `Ancestors`, `Inherited`) to sets of scope IDs without hitting the database.

`resume`
: Session bootstrapping via `ResumeConfig` and `ResumeContext`. Implements 5-tier retrieval:

1. **Pinned** -- unforgettable facts (`is_pinned = true`), cross-scope, sorted by `importance_score` descending.
2. **High-importance** -- facts with materialized `importance_score` above a configurable threshold.
3. **Due** -- future-memory facts whose `t_valid` has arrived (`t_valid <= now`).
4. **Recent** -- most recent facts from scope ancestors.
5. **KB stubs** -- placeholder for Phase 5 knowledge-base references.
   Tiers are mutually exclusive (a fact appears in at most one tier).

`async_engine`
: `AsyncMemoryEngine` -- thin async wrapper around `MemoryEngine` using `tokio::task::spawn_blocking`. Feature-gated under the `"async"` Cargo feature. Mirrors the synchronous API surface.

## Re-exports from `lib.rs`

The crate root re-exports the most commonly used items so consumers can `use memory_engine::*`:

- `MemoryEngine`, `EngineConfig` (from `engine`)
- `MemoryError`, `Result` (from `error`)
- `EmbeddingProvider` (from `traits`)
- All public types (from `types`)
- `serialize_embedding`, `deserialize_embedding` (from `store`)
