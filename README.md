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

Part of a [four-layer cognitive architecture](https://github.com/dutiona/autonomous-agent-project) research project. Companion to [knowledge-base](https://github.com/dutiona/knowledge-base) (Python MCP server, Knowledge layer).

## Quick Example

```rust
use memory_engine::{EngineConfig, MemoryEngine, FactType, AddFactRequest, EmbeddingProvider, MemoryError};

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
        &AddFactRequest {
            content: "Rust's ownership model prevents data races at compile time".into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        },
        &embedder,
        None, // no persistence classifier
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
- **ANN search** — HNSW index behind `ann` feature flag, with `VectorSearchStrategy` trait for pluggable backends
- **Bi-temporal facts** — 4 timestamps per fact: system time (created/expired) + real-world validity
- **Event sourcing** — append-only log enables replay, audit, and storage migration
- **Trait-based extensibility** — `EmbeddingProvider`, `SummaryGenerator`, `ConflictArbiter`, `PersistenceClassifier`, `Reranker`
- **Hierarchical scoping** — isolate facts by context (e.g., `"user:alice/project:demo"`)
- **Pinned facts** — exempt critical facts from decay and deduplication
- **5-tier resume** — `resume_context()` bootstraps agent sessions with graduated detail levels
- **Introspection** — `explain_fact()`, `replay_events()`, `statistics()`, import/export (JSON + SQLite, gzip/zstd compression)
- **MCP server** — `memory-engine-mcp` crate exposes engine operations as MCP tools
- **CLI inspector** — `memory-engine-cli` for terminal-based inspection and debugging
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
│  explain_fact · statistics           │
├──────────────────────────────────────┤
│  Search                              │
│  ├─ FTS5 (BM25)                      │
│  ├─ Vector (cosine, brute-force)     │
│  ├─ HNSW ANN (feature: ann)         │
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
├──────────────────────────────────────┤
│  Tooling                             │
│  ├─ memory-engine-mcp (MCP server)   │
│  └─ memory-engine-cli (inspector)    │
└──────────────────────────────────────┘
```

</details>

## Documentation

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

For HNSW approximate nearest neighbor search:

```toml
[dependencies]
memory-engine = { git = "https://github.com/dutiona/memory-engine", features = ["ann"] }
```

**Requirements:** Rust 1.85+ (edition 2024). No external services needed.

## Workspace Crates

| Crate               | Purpose                                          |
| ------------------- | ------------------------------------------------ |
| `memory-engine`     | Core library (5 primitives, traits, storage)     |
| `memory-engine-mcp` | MCP server adapter (rmcp-based, P0+P1 tools)     |
| `memory-engine-cli` | Terminal inspector (query, explain, dump, stats) |

## Current Status

**Phase 4b complete** (486 tests, ~25K lines Rust). Through 7 phases of development:

| Phase  | Focus                                      | Status                                  |
| ------ | ------------------------------------------ | --------------------------------------- |
| **1**  | Ingest → Query loop                        | Done                                    |
| **2**  | Graph, consolidation, forgetting, conflict | Done                                    |
| **3**  | Hardening & concurrency                    | Done                                    |
| **3a** | ANN search (HNSW)                          | Done                                    |
| **3b** | Temporal memory & agent lifecycle          | Done                                    |
| **4a** | Introspection & data (import/export)       | Done                                    |
| **4b** | MCP server & CLI inspector                 | Done                                    |
| **5**  | Cognitive pipelines (DreamCycle)           | Design complete, implementation pending |

## Research Foundation

Built on analysis of 15 papers (9 core + 6 context adaptation) including CoALA, Graphiti, Mem0, A-Mem, ACE, AWM, Reflexion, GEPA, and APC.
See [research basis](docs/design/research-basis.md) and [ADRs](docs/design/adr/) for the full rationale.

## License

Licensed under either of:

- [MIT license](LICENSE-MIT)
- [Apache License, Version 2.0](LICENSE-APACHE)

at your option.
