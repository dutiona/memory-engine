# Glossary

Key terms used throughout the memory-engine crate and its documentation.

Event
: Immutable record in the append-only event log. Each event has a type (`Interaction`, `ToolCall`, `MemoryOp`, `SystemEvent`), a JSON payload, a source identifier, and a scope. Events are the source of truth from which facts are derived.

Fact
: Derived knowledge unit with bi-temporal timestamps and a dense embedding vector. Facts have a type (`Episodic`, `Semantic`, `Procedural`), an importance score, and a content hash for deduplication. Facts are never mutated in place -- they are expired and replaced.

Edge
: Typed, weighted relationship between two facts in the knowledge graph. Edges carry a `relation_type` string (e.g., `superseded_by`), a numeric weight, and their own temporal bounds. Created during conflict resolution and consolidation.

Summary
: Consolidated text generated from a cluster of related facts. Tagged with a `ConsolidationLevel` (`Local`, `Cluster`, `Global`) indicating which pass produced it. Includes its own embedding for retrieval.

Bi-temporal
: Tracking two independent time axes for each fact. **System time** (`t_created`, `t_expired`) records when the engine learned and retired the fact. **Valid time** (`t_valid`, `t_invalid`) records when the fact is true in the real world. Enables point-in-time queries on either axis.

RRF (Reciprocal Rank Fusion)
: Score merging algorithm that combines ranked lists from FTS and vector search. For each result, computes `1 / (k + rank)` per list and sums. Produces a single ranked output without requiring score normalization across heterogeneous retrieval methods.

FTS5
: SQLite full-text search extension. Uses BM25 ranking to score text matches. The engine maintains an FTS5 virtual table over fact content, queried during the text retrieval pass.

Consolidation
: Three-pass process that compresses the fact store. Pass 1 (local dedup) expires near-duplicate facts by cosine similarity. Pass 2 (cluster fusion) groups related facts and generates cluster summaries via a `SummaryGenerator`. Pass 3 (global integration) produces cross-cluster summaries.

Forgetting
: Ebbinghaus-based decay that soft-deletes low-importance facts. Importance is computed as a weighted sum of four signals: recency (exponential decay with configurable half-life), access frequency, graph connectivity (degree), and base importance. Facts below the threshold have `t_expired` set rather than being physically deleted.

Scope
: Hierarchical isolation context expressed as a path string (e.g., `user:michael/project:demo`). Each path segment becomes a node in the scope tree. Facts, edges, and summaries are tagged with a scope ID. Scopes enable multi-tenant and multi-project isolation within a single database.

ScopeQuery
: Enum controlling how scopes are resolved during search. `Exact` matches a single scope. `Subtree` matches a scope and all descendants. `Ancestors` matches a scope and all parents up to root. `Inherited` combines ancestors and subtree for full inherited context.

Content hash
: blake3 hex digest of fact content, truncated to 32 characters. Used during deduplication to detect exact content matches before falling back to embedding similarity.

Embedding
: Dense vector representation of text, computed by a consumer-provided `EmbeddingProvider`. Stored as a BLOB in SQLite. Used for cosine-similarity vector search and for dedup/clustering during consolidation. Dimension is fixed per engine instance and validated on open.

Importance
: Computed score in the range [0, 1] that determines a fact's resistance to forgetting. Combines four weighted signals: recency decay, access frequency (`log(access_count + 1)`), graph degree (`log(edges + 1)`), and the fact's base importance value.

Pinned fact
: A fact marked as unforgettable (`is_pinned = true`). Pinned facts are never expired by forgetting and never deduplicated during consolidation. They appear in tier 1 of `resume_context()`. Facts can be pinned explicitly via `pin_fact()`, through `AddFactOptions { pinned: Some(true) }`, or automatically by a `PersistenceClassifier`.

Future memory
: A fact with `t_valid` set in the future. Invisible to present-time queries until `t_valid` arrives. Retrieved via `list_due(now)` when the time comes — the first call stamps `surfaced_at` so callers can distinguish newly-due facts from previously-surfaced ones. Enables reminders, deferred knowledge, and scheduled agent behavior.

Importance score (materialized)
: A composite score in [0, 1] stored on each fact as `importance_score`. Computed during `forget()` as a weighted sum of recency, frequency, graph connectivity, and base importance. Used by `resume_context()` tier 2 (high-importance) to select facts without recomputation.

PersistenceClassifier
: Consumer-provided trait that decides whether a newly inserted fact should be pinned. Called during `add_fact()` with a pre-insert synthetic `Fact`. Default implementation returns `false` (opt-in). Classifiers should rely on `content`, `fact_type`, `importance`, and `metadata` only.

Scheduling API
: The `list_due(now, scope)` and `next_due_time(scope)` methods. `list_due` returns active facts whose `t_valid` has arrived and stamps `surfaced_at` on first return. `next_due_time` returns the earliest future `t_valid` for poll scheduling.

Global summary invariant
: Global-level summaries (consolidation pass 3) are always placed at `scope_id=1` (root scope), regardless of the scopes of the underlying facts. Cluster-level summaries use majority-vote scope assignment.

Resume context
: Five-tier fact retrieval for bootstrapping a new agent session. Tier 1 (pinned): unforgettable facts, cross-scope. Tier 2 (high-importance): facts above a materialized `importance_score` threshold. Tier 3 (due): future-memory facts whose `t_valid` has arrived. Tier 4 (recent): most recent facts from scope ancestors. Tier 5 (kb_stubs): placeholder for Phase 5 knowledge-base references. Tiers are mutually exclusive.

WAL (Write-Ahead Logging)
: SQLite journaling mode used by the connection pool. Enables concurrent readers while a single writer commits. The engine opens all connections in WAL mode to maximize read throughput without blocking on writes.

Connection pool
: Internal `ConnectionPool` providing N read connections and 1 write connection, protected by `parking_lot` synchronization primitives. The write connection uses a `Mutex`; read connections are distributed from a bounded pool. Default read pool size is 4.

CrudDecision
: Enum returned by a `ConflictArbiter` during conflict resolution. Variants: `Add` (keep both facts), `Update` (expire old, insert new with `superseded_by` edge), `Delete` (expire old, discard new), `Noop` (no changes).
