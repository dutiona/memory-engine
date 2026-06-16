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
the archived Future Phases Design plan, [memory-engine#605](https://github.com/dutiona/memory-engine/issues/605)) which emphasizes that
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

### Provenance and source event

`FactProvenance` includes the originating event when the fact has a `source_event_id`:

```rust
let explanation = engine.explain_fact(fact_id)?;
if let Some(event) = &explanation.provenance.source_event {
    println!("Created from event {}: {:?}", event.id, event.event_type);
    println!("Payload: {}", event.payload);
}
```

- **What:** `FactProvenance.source_event` contains the full `Event` that produced this
  fact, fetched via upcasted read so the payload is always at the latest schema revision.
- **Why:** Enables full provenance tracing — from fact to originating event — without
  a separate event lookup. Useful for debugging, auditing, and tooling integration.
- **How:** Automatically populated when the fact has a `source_event_id` (set during
  `add_fact()` when extracting from an ingested event). `None` for facts created
  without a source event.

Note: the full `Event` envelope is exposed (payload, source, session_id, origin_node_id,
sequence_id). This is intentional for inspection/debugging use cases.

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

// Replay events 10–50 with raw payloads (no upcasting).
// `ReplayFilter` is `#[non_exhaustive]`, so construct via `default()` + fields.
let mut filter = ReplayFilter::default();
filter.id_range = Some((10, 50));
let events = engine.replay_events(&filter)?;

// Replay by time window with upcasted payloads
let mut filter = ReplayFilter::default();
filter.since = Some(start);
filter.until = Some(end);
filter.upcast = true;
filter.order = ReplayOrder::TimestampOrder;
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

## Import/Export

### Why

Long-running agents need backup/restore for disaster recovery, migration between
machines, and offline analysis. The event-sourced architecture (ADR-0001) makes
full-state export/import natural — the `EngineSnapshot` captures the complete
materialized view. Compressed exports reduce storage costs for archival
(see the archived Future Phases Design plan [memory-engine#605](https://github.com/dutiona/memory-engine/issues/605) § Phase 4 for the
"10 agents × 10 years" cold storage scenario).

Research context: context adaptation research (ACE, AWM — see research basis)
emphasizes that memory systems must be _portable_ and _inspectable_ to support
multi-device agents and human oversight.

### Export (dump)

Export the full engine state for backup, migration, or offline analysis.

```rust
use memory_engine::inspect_types::DumpFormat;

// JSON snapshot (works for both file-backed and in-memory)
engine.dump_state(&DumpFormat::Json("snapshot.json".into()))?;

// Gzip-compressed JSON (requires `compress-gzip` feature)
engine.dump_state(&DumpFormat::JsonGzip("snapshot.json.gz".into()))?;

// Zstd-compressed JSON (requires `compress-zstd` feature)
engine.dump_state(&DumpFormat::JsonZstd("snapshot.json.zst".into()))?;

// SQLite backup via VACUUM INTO (file-backed and in-memory)
engine.dump_state(&DumpFormat::Sqlite("backup.db".into()))?;
```

### Format comparison

| Format    | Portability     | Speed  | In-memory engines  | Includes embeddings | Feature flag    |
| --------- | --------------- | ------ | ------------------ | ------------------- | --------------- |
| JSON      | High (any lang) | Slower | Yes                | Yes (large output)  | —               |
| JSON+gzip | High            | Medium | Yes                | Yes (compressed)    | `compress-gzip` |
| JSON+zstd | High            | Medium | Yes                | Yes (compressed)    | `compress-zstd` |
| SQLite    | Native (SQLite) | Fast   | Yes (SQLite 3.27+) | Yes (binary BLOB)   | —               |

The JSON dump produces an `EngineSnapshot` containing all facts, edges, summaries,
scopes, events, and config. Raw events are stored (not upcasted) for snapshot fidelity.

The SQLite dump uses `VACUUM INTO`, producing an atomic, defragmented copy of the
database without WAL sidecars.

### Import (restore)

Restore from a backup into a **new** engine. Import always targets a fresh database —
no merge/additive semantics. This is a deliberate constraint: merging state is a
sync problem deferred to future CRDT-based work.

```rust
use memory_engine::engine::{EngineConfig, MemoryEngine};
use std::path::Path;

// Restore JSON snapshot into a new file-backed engine.
// config.embed_dim must match the snapshot's embed_dim.
let config = EngineConfig::new("restored.db".into(), 768);
let engine = MemoryEngine::restore_json(Path::new("snapshot.json"), &config)?;

// Restore into a new in-memory engine (auto-detects compression).
let engine = MemoryEngine::restore_json_memory(Path::new("snapshot.json.gz"))?;

// Restore from a SQLite backup (only accepts dump_sqlite() output).
let engine = MemoryEngine::restore_sqlite(Path::new("backup.db"), &config)?;
```

**Constraints:**

- Target path (`config.path`) must not already exist.
- `config.embed_dim` is validated against the snapshot — mismatches are rejected.
- `restore_sqlite` only accepts clean `VACUUM INTO` backups produced by `dump_state`.
  Copying a live WAL-mode database is unsafe.
- `restore_json` and `restore_sqlite` accept the full `EngineConfig`, preserving
  pool size, search config, upcaster registry, and backup directory settings.
- Compression is auto-detected from magic bytes (gzip: `1f 8b`, zstd: `28 b5 2f fd`).
- On failure, orphan database files are cleaned up automatically.

## Async API

All inspection and import/export methods are mirrored in `AsyncMemoryEngine`
(requires the `async` feature):

```rust
let stats = async_engine.statistics().await?;
let explanation = async_engine.explain_fact(id).await?;

// Async restore
let engine = AsyncMemoryEngine::restore_json_memory("snapshot.json".into()).await?;
```
