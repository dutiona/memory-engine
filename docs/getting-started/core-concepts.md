# Core Concepts

## Events vs Facts

The engine separates raw observations from derived knowledge:

**Events** are the raw, immutable audit log. Every interaction, tool call, or system event
is appended to the event log. Events are never modified or deleted.

**Facts** are knowledge the agent has internalized. They are derived from events (or
created directly) and carry metadata: embeddings, importance scores, temporal bounds.
Facts can be expired (soft-deleted) but never hard-deleted.

```
Event (raw)                    Fact (derived)
─────────────                  ──────────────
"User said X"        →        "X prefers Y"
"Tool returned Z"    →        "Z is true as of today"
"Error occurred"     →        "Service W is unreliable"
```

## Fact Types

Facts are tagged with a type from the CoALA taxonomy:

Episodic
: What happened. Specific experiences and interactions.
_"User asked about Rust on March 10"_

Semantic
: General knowledge. Timeless truths and definitions.
_"Rust prevents data races at compile time"_

Procedural
: How to do things. Processes, recipes, and workflows.
_"To deploy, run `cargo build --release` then copy to /opt/bin"_

All three types live in the same store. `fact_type` is a tag, not a partition.

## Bi-Temporal Model

Every fact carries 4 timestamps:

| Timestamp   | Meaning                           | Managed by |
| ----------- | --------------------------------- | ---------- |
| `t_created` | When the engine recorded it       | Engine     |
| `t_expired` | When the engine soft-deleted it   | Engine     |
| `t_valid`   | When it becomes true in the world | Consumer   |
| `t_invalid` | When it stops being true          | Consumer   |

System time (`t_created`/`t_expired`) tracks the engine's knowledge state.
Valid time (`t_valid`/`t_invalid`) tracks real-world truth.

This enables:

- **Historical queries**: "What did the agent know as of last Tuesday?"
- **Future scheduling**: Set `t_valid` in the future; the fact surfaces when its time arrives
- **Soft deletion**: Expired facts are never hard-deleted, preserving the full audit trail

## Trait System

The engine has zero network or LLM dependencies. Consumers provide implementations
for operations that require external models:

`EmbeddingProvider`
: Compute dense vector embeddings from text. Called during `add_fact()`.

`SummaryGenerator`
: Generate text summaries from fact clusters. Called during `consolidate()`.

`ConflictArbiter`
: Decide how to resolve contradicting facts (Add/Update/Delete/Noop). Called during `resolve_conflict()`.

`ForgetPolicy`
: Configuration struct (not a trait) controlling Ebbinghaus decay parameters.

`PersistenceClassifier`
: Decide whether a new fact should be pinned (unforgettable). Called during `add_fact()`. Optional.

This design means the core crate compiles without any ML framework, HTTP client, or API key.

## Pinned Facts

Facts can be marked as **pinned** (unforgettable). Pinned facts:

- Are never expired by the forgetting system (`forget()` skips them)
- Are never deduplicated during consolidation
- Appear in the top tier of `resume_context()` (tier 1: pinned)
- Can be pinned explicitly via `pin_fact(id)` / `unpin_fact(id)`, through `AddFactOptions { pinned: Some(true), .. }`, or automatically via a `PersistenceClassifier`

Pinned facts model agent identity, core beliefs, and critical knowledge that should persist indefinitely.

## Future Memory & Scheduling

Facts with `t_valid` set in the future are invisible to present-time queries but surface when their time arrives. The scheduling API supports this:

- `list_due(now, scope)` — returns facts whose `t_valid <= now` (they have become valid). Stamps `surfaced_at` on first return.
- `next_due_time(scope)` — returns the earliest future `t_valid`, so the consumer knows when to poll again

The `surfaced_at` timestamp lets callers distinguish newly-due facts from previously-surfaced ones. This enables reminders, deferred knowledge, and time-triggered agent behavior.

## Scoping

Facts are organized in a hierarchical scope tree using slash-separated paths:

```
root (scope_id=1)
├── user:alice
│   ├── project:demo
│   └── project:prod
└── user:bob
    └── project:research
```

Scope queries control visibility:

- **Exact** — facts at exactly this scope
- **Subtree** — this scope and all descendants
- **Ancestors** — this scope and all parents up to root
- **Inherited** — ancestors + subtree (full inherited context)

**Global summary invariant:** Global-level summaries (pass 3 of consolidation) are always placed at `scope_id=1` (root scope), regardless of the scopes of the underlying facts. Cluster-level summaries (pass 2) use majority-vote scope assignment from their source facts.

## Seven Primitives

| Primitive       | Method                          | Purpose                                             |
| --------------- | ------------------------------- | --------------------------------------------------- |
| **Ingest**      | `ingest()`                      | Append event to the audit log                       |
| **Query**       | `query()`                       | Hybrid search with temporal and scope filters       |
| **Consolidate** | `consolidate()`                 | Merge duplicates, cluster facts, summarize          |
| **Forget**      | `forget()`                      | Decay and prune low-importance facts (skips pinned) |
| **Resolve**     | `resolve_conflict()`            | Arbitrate contradicting facts                       |
| **Schedule**    | `list_due()`, `next_due_time()` | Surface future-memory facts and poll timing         |
| **Resume**      | `resume_context()`              | 5-tier session bootstrapping                        |

## Threading

`MemoryEngine` is `Send + Sync`. Thread safety is provided by:

- **ConnectionPool** — N read connections + 1 write connection (via `parking_lot::Mutex`)
- **RwLock** caches — concurrent reads on the in-memory graph and scope tree

All public methods take `&self`. Share via `Arc<MemoryEngine>`.
