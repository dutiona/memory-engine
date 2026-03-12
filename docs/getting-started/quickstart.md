# Quickstart

A minimal end-to-end example: open an engine, ingest an event, add a fact, and query it.

## 1. Implement EmbeddingProvider

The engine has no built-in embedding model. You provide one via the `EmbeddingProvider` trait:

```rust
use memory_engine::{EmbeddingProvider, MemoryError};

struct DummyEmbedder;

impl EmbeddingProvider for DummyEmbedder {
    fn embed(&self, _text: &str) -> Result<Vec<f32>, MemoryError> {
        // Replace with your embedding model (e.g., ONNX Runtime, API call)
        Ok(vec![0.0; 384])
    }
}
```

The dimension (384 here) must match what you pass to `open_memory()`.

## 2. Open an Engine

```rust
use memory_engine::MemoryEngine;

// In-memory (for testing)
let engine = MemoryEngine::open_memory(384)?;

// File-backed (for production)
use memory_engine::EngineConfig;
use std::path::PathBuf;

let config = EngineConfig::new(PathBuf::from("my_agent.db"), 384);
let engine = MemoryEngine::open(&config)?;
```

`MemoryEngine` is `Send + Sync` — wrap in `Arc` to share across threads.

## 3. Ingest an Event

Events form the append-only audit log:

```rust
use memory_engine::types::{NewEvent, EventType};
use chrono::Utc;

let event_id = engine.ingest(&NewEvent {
    timestamp: Utc::now(),
    event_type: EventType::Interaction,
    payload: serde_json::json!({"user": "alice", "message": "What is Rust?"}),
    source: "chat".into(),
    session_id: Some("session-1".into()),
    scope_id: 1, // root scope
    origin_node_id: "local".into(),
    sequence_id: 0,
    created_at: None,
})?;
```

## 4. Add a Fact

Facts are derived knowledge the agent has internalized:

```rust
use memory_engine::FactType;

let embedder = DummyEmbedder;
let fact_id = engine.add_fact(
    "Rust is a systems programming language focused on safety and performance",
    FactType::Semantic,
    Some(event_id),  // link to source event
    &embedder,
    None,            // root scope
    None,            // default options (importance=0.5)
    None,            // no auto-pin classifier
)?;
```

## 5. Query

```rust
use memory_engine::search::hybrid::{SearchQuery, SearchMode};

let results = engine.query(&SearchQuery {
    text: Some("systems programming safety".into()),
    embedding: Some(embedder.embed("systems programming safety")?),
    mode: SearchMode::Hybrid,
    limit: 5,
    valid_at: None,
    fact_type: None,
    scope: None,
})?;

for r in &results {
    println!("[{:.3}] {}", r.score, r.fact.content);
}
```

Hybrid mode combines FTS5 (keyword) and vector (semantic) results via Reciprocal Rank Fusion.

## Next Steps

- [Core Concepts](core-concepts.md) — events vs facts, bi-temporal model, trait system
- [Adding Facts](../usage/adding-facts.md) — scopes, options, temporal bounds
- [Querying Memory](../usage/querying-memory.md) — search modes, filters, scoped queries
