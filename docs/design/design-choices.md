# Design Choices

Each decision below was informed by the research foundation (9 papers, community synthesis, multi-AI debate) and validated through implementation. Status markers indicate whether the decision is realized in code.

---

## No Layers -- One Store, Multiple Projections **{Implemented}**

Early design explored a 5-layer hierarchy (L0-L4) inspired by CoALA and community patterns. This collapsed during implementation. Instead, `fact_type` is a tag (`Episodic`, `Semantic`, `Procedural`), not a partition. All facts live in one table, one FTS5 index, one vector space.

**Rationale:** A-Mem demonstrated that self-organizing memory without predefined schemas outperforms rigid layering. The tag approach gives consumers full flexibility to query across types or filter by type, without maintaining separate storage backends or synchronization logic.

**Trade-off:** No physical isolation between memory types. If a consumer needs hard boundaries, they use scopes instead.

---

## Traits for LLM Operations **{Implemented}**

The engine defines three traits (`EmbeddingProvider`, `SummaryGenerator`, `ConflictArbiter`) and has zero network or LLM dependencies. All intelligence is injected by the consumer.

**Rationale:** AgeMem validated this pattern -- memory operations as tool-based actions where the agent decides what/when to store. Keeping LLM calls out of the engine means: (1) the core is deterministic and testable with mocks, (2) consumers can use any embedding model (local, API, quantized), (3) the engine works in constrained environments (embedded, WASM).

**Trade-off:** Consumers must implement these traits, which adds integration work. But the alternative -- baking in a specific model -- would make the engine opinionated and fragile.

---

## Event-Sourced -- Append-Only Log as Source of Truth **{Implemented}**

Every interaction enters the system through an append-only event log (`EventStore`). Facts are explicitly derived from events by the consumer calling `add_fact`, not auto-projected.

**Rationale:** Multi-AI debate consensus: event-sourcing provides an audit trail, enables replay into alternative storage backends (the migration safety net), and separates raw observations from derived knowledge. Graphiti's "non-lossy dynamic updates" pattern is the academic equivalent.

**Trade-off:** The consumer bears the burden of deciding what to extract from events. This is intentional -- the research consistently shows that active curation produces better memory than passive logging.

---

## Soft Deletion **{Implemented}**

Facts are expired by setting `t_expired`, never hard-deleted. This applies to forgetting (Ebbinghaus decay), conflict resolution (superseded facts), and deduplication (consolidation).

**Rationale:** Graphiti's bi-temporal model requires full history for temporal reasoning. Hard deletion destroys audit trails and makes it impossible to answer "what did the agent believe at time T?" Soft deletion also makes undo trivial (clear `t_expired`).

**Trade-off:** Storage grows monotonically. Mitigated by planned archival compression (Phase 4) where cold non-pinned facts are moved to `.pak` files.

---

## Vector Search Strategy: Brute-Force + HNSW Dispatch **{Implemented}**

Vector search uses runtime strategy dispatch between brute-force (O(N) cosine scan) and HNSW approximate nearest neighbor, gated by the `ann` feature flag and a configurable fact-count threshold.

### The path to this decision

1. **Phase 1-3a: Brute-force only.** O(N) cosine similarity scan with `select_nth_unstable_by` partial sort for top-K. Zero complexity overhead. Sufficient for sub-50ms at thousands of facts.

2. **Benchmark baseline (PR #27).** Established Criterion benchmarks at 1K–100K facts across dimensions 128/384/768. Confirmed brute-force exceeds acceptable latency around 50K facts. Introduced `VectorSearchStrategy` trait and `SearchConfig` struct to prepare the dispatch plumbing without committing to an ANN backend.

3. **ANN backend selection.** Considered LanceDB (deferred — heavy dependency, external server model), FAISS (C++ FFI, `unsafe`), and the `hnsw` crate (rust-cv, 0.11). Chose `hnsw` because: pure Rust (respects `#![forbid(unsafe_code)]`), small dependency surface, in-process (no external server), and the `space::Metric` trait maps cleanly to our cosine distance.

4. **HNSW implementation (PR #33).** Feature-gated behind `ann`. `CosineMetric` wraps cosine distance as `u32` via `f32::to_bits()` for the `space::Metric<Unit=u32>` requirement. `HnswStrategy` maintains an in-memory index alongside a `fact_to_hnsw: HashMap<i64, usize>` mapping and a tombstone set of HNSW indices (not fact IDs — a correction from code review that prevents stale entries when facts are replaced).

5. **Dispatch logic.** `should_use_hnsw()` compares `HnswStrategy::active_count()` (O(1) in-memory) against `SearchConfig::ann_threshold`. The engine eagerly builds the HNSW index on `open()` when the threshold is reachable, and skips it entirely when `ann_threshold == usize::MAX`.

6. **Widening loop with brute-force fallback.** HNSW search uses a 3-attempt widening loop that doubles both `ef_search` and `overfetch` each retry (scaling overfetch alongside ef was a review-driven correction — without it, aggressive filters exhaust the same small candidate set). If widening fails to produce enough results, the engine falls back to brute-force for correctness.

7. **Concurrency model.** `RwLock<HnswInner>` with two-phase search: Phase 1 collects HNSW candidates under a read lock, Phase 2 post-filters and exact-scores via DB queries without holding the lock. Lifecycle hooks (`notify_insert`/`notify_expire`) fire after DB commits to maintain transaction safety.

### Design decisions locked in by review

- **Tombstones track HNSW indices, not fact IDs.** When a fact is expired and re-inserted (e.g., via `resolve_conflict`), the old HNSW entry must be tombstoned by its internal index, not by fact ID. Otherwise, removing a fact ID from the tombstone set un-tombstones all historical entries for that fact.

- **NaN guard in CosineMetric.** `cosine_similarity` returns 0.0 for zero-norm vectors, but NaN can propagate from degenerate inputs. The metric guards with `is_nan()` before `clamp()` to prevent incorrect distance ordering.

- **No eager build when threshold is unreachable.** If `ann_threshold == usize::MAX`, the HNSW index is never built, avoiding wasted memory and startup time.

**Trade-off:** The `ann` feature adds ~3 crate dependencies (`hnsw`, `space`, `rand`). When disabled, the engine has zero ANN overhead — the entire module is compiled out.

**Future refinement:** The threshold is currently total-active-fact-count. Candidate-set size after scope/type filtering, embedding dimension, and filter selectivity are future refinements for smarter dispatch.

---

## Send+Sync via ConnectionPool **{Implemented}**

`MemoryEngine` is `Send + Sync`. Thread safety comes from `ConnectionPool` (N readers + 1 writer via `parking_lot::Mutex`) and `RwLock` for the in-memory graph and scope tree.

**History:** Phases 1-2 used a single-writer `!Send` design where the engine owned one `Connection`. This was adequate for single-threaded testing but blocked integration with async runtimes (tokio) and multi-threaded consumers. Phase 3 replaced it with the current pool-based design.

**Trade-off:** More complex internal locking. Mitigated by keeping lock scopes narrow -- embedding computation happens outside the write lock, scope resolution uses short-lived read locks, and graph queries never hold the pool.

---

## Hierarchical Scoping **{Implemented}**

Scopes form a tree (not a flat namespace). Scope paths like `"user:michael/project:demo"` resolve to integer IDs. `ScopeQuery` supports Exact, Subtree, Ancestors, and Inherited resolution modes.

**Rationale:** Mem0's hierarchical user/session/agent levels proved that flat scoping is insufficient. An agent working on multiple projects for multiple users needs isolation without running separate databases. The tree structure with path-based resolution gives consumers a natural namespace model.

**Trade-off:** Scope resolution adds a lookup step to every query. Mitigated by the in-memory `ScopeTree` cache -- resolution is a tree walk, not a database query.

---

## Unforgettable Facts **{Planned}**

An `is_pinned: bool` flag on facts, with a `PersistenceClassifier` trait hook that lets the consumer decide which facts should never be forgotten.

**Rationale:** Identity facts ("the user's name is Michael", "this agent runs on Mac Mini M4") must survive all forgetting cycles. Without pinning, the Ebbinghaus decay curve would eventually expire them if they are not accessed frequently enough.

**Design:** The `PersistenceClassifier::should_pin(fact) -> bool` trait lets the consumer use LLM-based classification, rule-based logic, or a combination.

---

## Future Memory **{Planned}**

Facts can have `t_valid` set to a future timestamp. They exist in storage but do not surface in queries until their date arrives. `resume_context(now)` will use the `now` parameter to filter.

**Rationale:** Agents need to schedule reminders ("meeting on Friday") and defer knowledge ("this API deprecation takes effect in March"). Graphiti's bi-temporal model already provides the `t_valid` field; this feature activates it for scheduling.

**API:** `drain_due(now)` returns facts whose `t_valid` has arrived. `next_due_time()` returns the earliest pending `t_valid` for timer-based polling.

---

## MCP Server **{Planned}**

A `memory-engine-mcp` workspace member that exposes the engine's API as MCP tools. Separate from the core crate.

**Rationale:** MCP is the standard protocol for agent-to-tool communication. The engine should be usable as an MCP server so any MCP-compatible agent can use it without Rust integration. Keeping it as a separate workspace member prevents the core crate from depending on MCP/async/networking.

---

## Knowledge Base Integration **{Planned}**

A `KnowledgeBaseConnector` trait that lets the engine reference external knowledge bases. Facts can carry a `KnowledgeRef` URI field pointing to raw content.

**Rationale:** Memory and knowledge are distinct (see [Research Basis](research-basis.md)). The engine stores what the agent has internalized; knowledge bases store raw content. But facts should be able to cite their sources. The connector trait keeps the boundary clean: the engine never fetches content directly, but it can tell the consumer where to look.

**Degradation:** When the KB is unreachable, the engine returns a "memory lapse" marker. The consumer retries later.

---

## Read-Only Open Path **{Implemented}**

`MemoryEngine::builder(dim).path(p).read_only(true).build()` opens a database without write capability — no `init_schema()`, no `migrate()`, no writable connection pool.

**What:** A `read_only` flag (set via the file-backed builder's `.read_only(true)` setter, or `EngineConfig::new(p, dim).with_read_only(true)`) that, when set:

1. Validates schema version and epoch compatibility without writing
2. Opens all connections with `PRAGMA query_only = ON` (including the internal slot used for cache loading)
3. Guards all write methods with `MemoryError::ReadOnly` at the Rust level

**Why:** Identified during PR #99 (CLI inspector) code review. The standard `open()` path always creates a writable connection pool and may run schema migrations. For operator tools (CLI inspect, MCP read-only queries, monitoring dashboards) that only need to read facts and statistics, this is undesirable:

- An accidental schema upgrade on a live database could corrupt data if the migration has bugs, or conflict with a concurrent agent writing to the same DB
- Migration creates WAL-safe backups, which consumes disk space and I/O on production databases that the operator only intended to inspect
- The Principle of Least Privilege: inspection tools should not require write access to the database they are inspecting

This mirrors a common pattern in database tooling: `psql` vs `pg_dump --read-only`, SQLite's `PRAGMA query_only`, and Redis's `--readonly` flag for replicas. The motivation is both operational safety and defense against accidental mutation.

**How — defense in depth:**

1. **File existence guard:** `open_read_only` rejects nonexistent paths before SQLite can create an empty file (which would be a write side effect)
2. **Schema validation without mutation:** `validate_schema_version()` checks epoch and version compatibility using only SELECT queries — no DDL, no config writes
3. **SQLite-level enforcement:** All connections have `PRAGMA query_only = ON`, so even if a code path bypasses the Rust guard, SQLite rejects the write
4. **Rust-level enforcement:** `pool.try_write()` checks the `read_only` flag and returns `MemoryError::ReadOnly` before acquiring the mutex

**Trade-off:** `list_due()` and `resume_context()` have write side effects (stamping `surfaced_at`). In read-only mode, these return `ReadOnly` if unsurfaced facts exist. CLI tools should use `list_active_facts()` for inspection. A future follow-up (#93) may add a read-only variant that skips stamping.
