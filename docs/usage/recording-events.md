# Recording Events

The event log is the append-only, immutable source of truth in memory-engine. Every interaction, tool call, memory operation, and system event is recorded as an `Event`. Facts (internalized knowledge) are derived from events, but the raw event log is never modified or deleted.

## The `NewEvent` struct

To record an event, construct a `NewEvent`:

```rust
pub struct NewEvent {
    pub timestamp: DateTime<Utc>,
    pub event_type: EventType,
    pub payload: serde_json::Value,
    pub source: String,
    pub session_id: Option<String>,
    pub scope_id: i64,
}
```

| Field        | Description                                                     |
| ------------ | --------------------------------------------------------------- |
| `timestamp`  | When the event occurred (UTC).                                  |
| `event_type` | One of the `EventType` variants (see below).                    |
| `payload`    | Arbitrary JSON data associated with the event.                  |
| `source`     | Identifier for the system or component that produced the event. |
| `session_id` | Optional session identifier for grouping related events.        |
| `scope_id`   | Scope this event belongs to. Use `1` for root scope.            |

## Event types

```rust
pub enum EventType {
    Interaction,   // User/agent conversational turns
    ToolCall,      // Tool invocations and their results
    MemoryOp,      // Internal memory operations (consolidation, forgetting)
    SystemEvent,   // Lifecycle events (startup, shutdown, config changes)
}
```

## Ingesting events

Call `engine.ingest(&event)` to append an event. It returns the database-assigned event ID.

```rust
use chrono::Utc;
use memory_engine::{MemoryEngine, NewEvent, EventType};

let engine = MemoryEngine::open_memory(384)?;

let event = NewEvent {
    timestamp: Utc::now(),
    event_type: EventType::Interaction,
    payload: serde_json::json!({
        "role": "user",
        "content": "What is the capital of France?"
    }),
    source: "chat-agent".into(),
    session_id: Some("sess-abc123".into()),
    scope_id: 1,
};

let event_id: i64 = engine.ingest(&event)?;
```

`ingest` acquires the write lock, inserts the event into SQLite, and returns immediately. It is intentionally cheap -- no embedding computation or indexing happens here.

## Events vs facts

Events and facts serve different purposes:

- **Events** are the raw audit trail. They record what happened, exactly as it happened. They are append-only and never modified.
- **Facts** are internalized knowledge derived from events. A fact has an embedding, a type (Episodic/Semantic/Procedural), temporal bounds, and participates in search and the knowledge graph.

A typical workflow: ingest an event first, then extract one or more facts from it using `add_fact`, passing the event ID as `source_event_id` to maintain provenance.

```rust
let event_id = engine.ingest(&event)?;

// Later, extract a fact from this event
engine.add_fact(
    &AddFactRequest {
        content: "The capital of France is Paris".into(),
        fact_type: FactType::Semantic,
        source_event_id: Some(event_id),
        ..Default::default()
    },
    &embedder,
    None,
)?;
```

## Scoping events

The `scope_id` field on `NewEvent` is a raw integer referencing the scope tree. For most cases, use `1` (the root scope). If you need hierarchical scoping, the scope system is covered in more detail in [Adding Facts](adding-facts.md) -- facts are where scope-aware queries become relevant.
