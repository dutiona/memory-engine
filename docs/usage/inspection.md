# Inspecting Engine State

The inspection APIs expose the internal state of the memory engine for debugging,
monitoring, and tooling integration (e.g., CLI dashboards, MCP servers).

## Why

A black-box engine is hard to debug. When an autonomous agent's memories behave
unexpectedly — facts disappear, importance scores drift, conflict resolution
makes the wrong call — you need introspection. The five inspection methods answer
the questions developers and operators ask most:

| Question                                | Method            |
| --------------------------------------- | ----------------- |
| How many facts/edges/events do I have?  | `statistics()`    |
| Why is this fact active/expired/pinned? | `explain_fact()`  |
| How did this fact evolve over time?     | `fact_history()`  |
| What happened in the event log?         | `replay_events()` |
| Can I snapshot/backup the whole engine? | `dump_state()`    |

### Research context

The design draws on **context adaptation** research (ACE, AWM, Reflexion — see
`docs/design/plans/2026-03-09-future-phases-design.md`) which emphasizes that
memory systems for agents must be _observable_ to support self-correction loops
and human-in-the-loop debugging.

## Statistics

Aggregate counts across all tables, plus storage metrics.

```rust
let stats = engine.statistics()?;
println!("Active facts: {}", stats.facts.active);
println!("Pinned facts: {}", stats.facts.pinned);
println!("Due facts:    {}", stats.facts.due);
println!("Edges:        {}/{} active/total", stats.edges.active, stats.edges.total);
println!("DB size:      {} bytes", stats.storage.main_db_bytes);
```

`EngineStatistics` contains:

- `facts: FactStats` — total, active, expired, pinned, due (bi-temporal query at `Utc::now()`)
- `edges: EdgeStats` — total, active, expired
- `summaries: SummaryStats` — total, breakdown by `ConsolidationLevel`
- `scopes: ScopeStats` — total, max depth
- `events: EventStats` — total count
- `storage: StorageStats` — page count, page size, `main_db_bytes` (excludes WAL/SHM sidecars), file path

## Explain Fact

Answers "why is this fact in its current state?" Returns provenance, graph context,
and the resolved scope path. For due facts, the `surfaced_at` timestamp records
when `list_due()` or `resume_context()` first returned the fact — `None` means it
has not yet been surfaced to any consumer.

```rust
let explanation = engine.explain_fact(fact_id)?;
match &explanation.state {
    FactState::Active => println!("Alive and well"),
    FactState::Expired { reason } => println!("Expired: {reason:?}"),
    FactState::Pinned => println!("Pinned (unforgettable)"),
    FactState::Due { t_valid, surfaced_at } => {
        println!("Due since {t_valid}, surfaced_at={surfaced_at:?}")
    }
    FactState::Invalidated { t_invalid } => {
        println!("Invalidated at {t_invalid}")
    }
}
println!("Scope: {}", explanation.scope_path);
println!("Graph degree: {}", explanation.graph_context.degree);
```

### State priority

The state is determined by the **first matching** rule:

1. `t_expired.is_some()` → `Expired` (overrides everything)
2. `t_invalid <= now` → `Invalidated` (no longer valid but not garbage-collected)
3. `is_pinned` → `Pinned`
4. `t_valid <= now` and not invalidated → `Due`
5. Otherwise → `Active`

### Limitations

- **`ExpiredReason`** is best-effort. Most expired facts return `Unknown` because
  forgetting, conflict resolution, and dedup do not yet log `MemoryOp` events.
  A future version will add event-based audit trails.
- **`GraphContext`** reflects the current (active-only) graph. Expired edges are
  not retained in `MemoryGraph`. Historical connectivity requires replaying events.

## Fact History

Reconstructs a fact's temporal lifecycle from its bi-temporal timestamps.

```rust
let history = engine.fact_history(fact_id)?;
for entry in &history.timeline {
    println!("{}: {:?}", entry.timestamp, entry.kind);
}
// Output (sorted by timestamp):
// 2026-03-01T10:00:00Z: BecameValid
// 2026-03-01T12:00:00Z: Created
// 2026-03-02T00:00:00Z: BecameInvalid
```

Timeline entries are computed from the fact's four temporal fields:

| Timestamp   | Entry Kind      | Meaning                                  |
| ----------- | --------------- | ---------------------------------------- |
| `t_created` | `Created`       | Fact was inserted into the engine        |
| `t_valid`   | `BecameValid`   | Fact's real-world validity began         |
| `t_invalid` | `BecameInvalid` | Fact's real-world validity ended         |
| `t_expired` | `Expired`       | Fact was garbage-collected (soft-delete) |

Entries are sorted chronologically. A fact with no temporal bounds has a single
`Created` entry.

## Replay Events

Replay a segment of the append-only event log for debugging consolidation,
forgetting, or conflict resolution.

```rust
use memory_engine::inspect::types::{ReplayFilter, ReplayOrder};

// Replay events 10–50 with raw payloads (no upcasting)
let filter = ReplayFilter {
    id_range: Some((10, 50)),
    ..Default::default()
};
let events = engine.replay_events(&filter)?;

// Replay by time window with upcasted payloads
let filter = ReplayFilter {
    since: Some(start),
    until: Some(end),
    upcast: true,
    order: ReplayOrder::TimestampOrder,
    ..Default::default()
};
```

### Filter options

| Field        | Type                    | Default          | Description                                  |
| ------------ | ----------------------- | ---------------- | -------------------------------------------- |
| `since`      | `Option<DateTime<Utc>>` | `None`           | Earliest timestamp (inclusive)               |
| `until`      | `Option<DateTime<Utc>>` | `None`           | Latest timestamp (inclusive)                 |
| `id_range`   | `Option<(i64, i64)>`    | `None`           | Event ID range (min, max), both inclusive    |
| `session_id` | `Option<String>`        | `None`           | Filter by session                            |
| `event_type` | `Option<EventType>`     | `None`           | Filter by type                               |
| `limit`      | `Option<usize>`         | `None`           | Max events to return                         |
| `upcast`     | `bool`                  | `false`          | Apply upcaster chain to payloads             |
| `order`      | `ReplayOrder`           | `InsertionOrder` | `InsertionOrder` (by ID) or `TimestampOrder` |

The default `InsertionOrder` (`ORDER BY id ASC`) is deterministic — events with
backdated timestamps still appear in append order.

## Dump State

Export the full engine state for backup, migration, or offline analysis.

```rust
use memory_engine::inspect::types::DumpFormat;

// JSON snapshot (works for both file-backed and in-memory)
engine.dump_state(&DumpFormat::Json("snapshot.json".into()))?;

// SQLite backup via VACUUM INTO (file-backed only)
engine.dump_state(&DumpFormat::Sqlite("backup.db".into()))?;
```

### Format comparison

| Format | Portability     | Speed  | In-memory engines  | Includes embeddings |
| ------ | --------------- | ------ | ------------------ | ------------------- |
| JSON   | High (any lang) | Slower | Yes                | Yes (large output)  |
| SQLite | Native (SQLite) | Fast   | No (returns error) | Yes (binary BLOB)   |

The JSON dump produces an `EngineSnapshot` containing all facts, edges, summaries,
scopes, events, and config. Raw events are stored (not upcasted) for snapshot fidelity.

The SQLite dump uses `VACUUM INTO`, producing an atomic, defragmented copy of the
database without WAL sidecars.

## Async API

All five methods are mirrored in `AsyncMemoryEngine` (requires the `async` feature):

```rust
let stats = async_engine.statistics().await?;
let explanation = async_engine.explain_fact(id).await?;
```
