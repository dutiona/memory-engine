# memory-engine

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE-MIT)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org)

Embedded memory engine for autonomous AI agents.

Provides 5 core primitives for agent long-term memory:

| Primitive       | Description                                              |
| --------------- | -------------------------------------------------------- |
| **Ingest**      | Append events to an immutable log (source of truth)      |
| **Query**       | Hybrid retrieval (FTS5 + vector + graph) with filters    |
| **Consolidate** | Merge, cluster, and integrate memories (dream cycle)     |
| **Forget**      | Decay, prune, and archive stale facts                    |
| **Resolve**     | Bi-temporal conflict arbitration for contradicting facts |

Built on SQLite (WAL mode) with zero network or LLM dependencies in the core crate.
Consumers bring their own embedding model, summarizer, and conflict resolver via traits.

## Quick Example

```rust
use memory_engine::{EngineConfig, MemoryEngine, FactType, EmbeddingProvider, MemoryError};

// Consumers implement EmbeddingProvider with their model of choice
struct DummyEmbedder;
impl EmbeddingProvider for DummyEmbedder {
    fn embed(&self, _text: &str) -> Result<Vec<f32>, MemoryError> {
        Ok(vec![0.0; 384]) // replace with real embeddings
    }
}

fn main() -> Result<(), MemoryError> {
    let engine = MemoryEngine::open_memory(384)?;
    let embedder = DummyEmbedder;

    // Add a fact (embedding computed automatically)
    let id = engine.add_fact(
        "Rust's ownership model prevents data races at compile time",
        FactType::Semantic,
        None,       // no source event
        &embedder,
        None,       // root scope
        None,       // default options
        None,       // no persistence classifier
    )?;

    // Query with hybrid search
    use memory_engine::search::hybrid::{SearchQuery, SearchMode};
    let results = engine.query(&SearchQuery {
        text: Some("ownership data races".into()),
        embedding: Some(embedder.embed("ownership data races")?),
        mode: SearchMode::Hybrid,
        limit: 5,
        rerank_depth: None,
        valid_at: None,
        fact_type: None,
        scope: None,
    })?;

    for r in &results {
        println!("[{:.3}] {}", r.score, r.fact.content);
    }
    Ok(())
}
```

## Key Features

- **Hybrid search** — FTS5 (BM25) + vector (cosine) merged via Reciprocal Rank Fusion
- **Bi-temporal facts** — 4 timestamps per fact: system time (created/expired) + real-world validity
- **Event sourcing** — append-only log enables replay, audit, and storage migration
- **Trait-based extensibility** — `EmbeddingProvider`, `SummaryGenerator`, `ConflictArbiter`, `PersistenceClassifier`, `Reranker`
- **Hierarchical scoping** — isolate facts by context (e.g., `"user:alice/project:demo"`)
- **Thread-safe** — `Send + Sync` via connection pool and `RwLock` caches
- **Zero external services** — SQLite bundled, pure Rust vector search

<details>
<summary>Architecture</summary>

```
Consumer (AI agent, CLI tool, MCP server)
    │
    ▼
┌──────────────────────────────────────┐
│           MemoryEngine               │
│  ingest · add_fact · query           │
│  consolidate · forget · resolve      │
│  resume_context · scoped queries     │
├──────────────────────────────────────┤
│  Search                              │
│  ├─ FTS5 (BM25)                      │
│  ├─ Vector (cosine, brute-force)     │
│  └─ Hybrid (RRF, k=60)              │
├──────────────────────────────────────┤
│  Storage                             │
│  ├─ SQLite WAL (events, facts, FTS)  │
│  └─ Petgraph (in-memory graph)       │
├──────────────────────────────────────┤
│  Traits (consumer-provided)          │
│  ├─ EmbeddingProvider                │
│  ├─ SummaryGenerator                 │
│  ├─ ConflictArbiter                  │
│  ├─ PersistenceClassifier            │
│  └─ Reranker                         │
└──────────────────────────────────────┘
```

</details>

## Documentation

- [Narrative docs](https://memory-engine.readthedocs.io/) (Sphinx)
- [API reference](https://dutiona.github.io/memory-engine/) (cargo doc)
- [Design & research basis](docs/design/research-basis.md)
- [Architecture Decision Records](docs/design/adr/)

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
memory-engine = { git = "https://github.com/dutiona/memory-engine" }
```

For async support:

```toml
[dependencies]
memory-engine = { git = "https://github.com/dutiona/memory-engine", features = ["async"] }
```

**Requirements:** Rust 1.85+ (edition 2024). No external services needed.

## Research Foundation

Built on analysis of 15 papers including CoALA, Graphiti, Mem0, A-Mem, and the Memory Survey.
See [research basis](docs/design/research-basis.md) and [ADRs](docs/design/adr/) for the full rationale.

## License

Licensed under either of:

- [MIT license](LICENSE-MIT)
- [Apache License, Version 2.0](LICENSE-APACHE)

at your option.
