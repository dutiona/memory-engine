# ADR-0002: SQLite + Petgraph over SurrealDB

**Status:** Accepted
**Date:** 2026-03-10

## Context

The memory engine requires embedded storage with no external services. The target deployment is a single-process Rust binary on a Mac Mini M4 (32GB), where ~22GB is consumed by the backbone LLM (Qwen 3.5 35B at Q4_K_M). Memory overhead for the storage layer must be minimal.

Four storage stacks were evaluated against the papers' choices:

- **Neo4j** -- Used by Graphiti/Zep for temporal knowledge graphs. Discarded: external JVM process, violates the embedded constraint.
- **Qdrant** -- Used by Mem0 for vector search. Discarded: external service.
- **SurrealDB 3.0** -- Rust-native, multi-model (vector + graph + KV + temporal) in a single binary. $44M raised. Evaluated as "best if stable" but too young when assessed (Feb 2026). The 3.0 release had insufficient production track record.
- **LanceDB** -- Embedded Rust vector DB, columnar with versioning. Strong candidate for vector search but deferred (Issue #3) because brute-force cosine completes in <50ms at expected scale.

The research (ROADMAP OQ1, Research Journal Entry 11) concluded: start with the simplest battle-tested stack, rely on event sourcing for future migration.

## Decision

SQLite (via `rusqlite` with `bundled-full` feature) as the persistence layer, with Petgraph (`DiGraph`) for in-memory graph operations.

SQLite configuration:

- WAL mode for concurrent reads during queries.
- FTS5 virtual table with BM25 ranking for keyword search.
- Embedding vectors stored as BLOBs (f32 arrays).
- Brute-force cosine similarity with `select_nth_unstable_by` partial sort for top-K.

Petgraph configuration:

- In-memory `DiGraph` loaded from SQLite `edges` table at startup.
- Rebuilt after consolidation (dedup removes facts, graph must stay consistent).
- Used for degree-based importance scoring in the forgetting module.

## Consequences

### Positive

- Battle-tested. SQLite has decades of production use. Zero deployment complexity.
- Zero external dependencies. No JVM, no separate process, no network calls.
- FTS5 provides competitive keyword search. Research (claude-memory, Research Journal Entry 5) showed FTS5+BM25 achieves 74% on LoCoMo benchmark when the retriever is an LLM constructing targeted queries.
- Petgraph provides O(degree) edge operations via `EdgeRef` + `edges_directed()`, sufficient for the graph operations needed (connectivity scoring, edge cascade on fact expiry).

### Negative

- Single-process only. No concurrent writers. `MemoryEngine` is `!Send` in Phases 1-2 (Phase 3 adds `ConnectionPool` with N readers + 1 writer via `parking_lot`).
- Vector search is O(N) brute-force. No ANN index.
- Graph is fully in-memory. Memory usage scales with edge count.
- No native temporal query operators (bi-temporal logic is application-level SQL).

### Mitigations

- Event sourcing (ADR-0001) guarantees migration to SurrealDB or any future backend by replaying the log. No lock-in.
- ANN migration to LanceDB is tracked as Issue #3, triggered when benchmarks show >50ms latency at scale.
- Phase 3 `ConnectionPool` addresses the concurrency limitation with `Send + Sync` via `RwLock`.
- Graph memory is bounded by the number of active (non-expired) edges, which consolidation and forgetting keep in check.
