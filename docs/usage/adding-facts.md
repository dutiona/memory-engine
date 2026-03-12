# Adding Facts

Facts are the internalized knowledge in memory-engine. While events are the raw audit trail, facts are what the agent actually knows -- searchable, embeddable, temporally bounded, and organized in a knowledge graph.

## Fact types

Every fact has a `FactType` drawn from the CoALA taxonomy:

```rust
pub enum FactType {
    Episodic,    // What happened: specific events, experiences, observations
    Semantic,    // General knowledge: definitions, relationships, stable truths
    Procedural,  // How-to knowledge: workflows, recipes, tool usage patterns
}
```

**Episodic** facts capture concrete occurrences ("User deployed v2.1 on 2025-03-01"). They tend to decay faster and are good candidates for forgetting.

**Semantic** facts capture stable knowledge ("Rust uses RAII for memory management"). They persist longer and form the backbone of the agent's world model.

**Procedural** facts capture processes ("To deploy, run `make release` then `kubectl apply`"). They are high-value and decay slowly.

## The `add_fact` method

```rust
pub fn add_fact(
    &self,
    content: &str,
    fact_type: FactType,
    source_event_id: Option<i64>,
    embedder: &dyn EmbeddingProvider,
    scope: Option<&str>,
    opts: Option<&AddFactOptions>,
    classifier: Option<&dyn PersistenceClassifier>,
) -> Result<i64>
```

| Parameter         | Description                                                                                      |
| ----------------- | ------------------------------------------------------------------------------------------------ |
| `content`         | The textual content of the fact.                                                                 |
| `fact_type`       | `Episodic`, `Semantic`, or `Procedural`.                                                         |
| `source_event_id` | Optional link back to the originating event for provenance.                                      |
| `embedder`        | Implementation of `EmbeddingProvider` to compute the embedding vector.                           |
| `scope`           | Optional hierarchical scope path (e.g., `"user:michael/project:demo"`). `None` means root scope. |
| `opts`            | Optional overrides for importance, metadata, temporal bounds, and pinning.                       |
| `classifier`      | Optional `PersistenceClassifier` for auto-pinning. Pass `None` to skip.                          |

The method computes the embedding **before** acquiring the write lock. This means slow embedding calls (e.g., network API round-trips) do not block concurrent readers.

Returns the database-assigned fact ID.

## The `EmbeddingProvider` trait

Consumers must implement `EmbeddingProvider` to supply embeddings:

```rust
pub trait EmbeddingProvider {
    fn embed(&self, text: &str) -> Result<Vec<f32>>;
}
```

The returned vector must match the `embed_dim` configured when opening the engine. A dimension mismatch will return an error at insert time.

Here is a minimal example using a zero-vector embedder (useful for testing, not for production):

```rust
use memory_engine::{MemoryEngine, FactType};
use memory_engine::traits::EmbeddingProvider;
use memory_engine::error::Result;

struct ZeroEmbedder {
    dim: usize,
}

impl EmbeddingProvider for ZeroEmbedder {
    fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![0.0; self.dim])
    }
}

let dim = 384;
let engine = MemoryEngine::open_memory(dim)?;
let embedder = ZeroEmbedder { dim };

let fact_id = engine.add_fact(
    "Rust guarantees memory safety without a garbage collector",
    FactType::Semantic,
    None,       // no source event
    &embedder,
    None,       // root scope
    None,       // default options
    None,       // no auto-pin classifier
)?;
```

## `AddFactOptions`

Override defaults by passing `AddFactOptions`:

```rust
pub struct AddFactOptions {
    pub importance: Option<f64>,
    pub metadata: Option<serde_json::Value>,
    pub t_valid: Option<DateTime<Utc>>,
    pub t_invalid: Option<DateTime<Utc>>,
    pub pinned: Option<bool>,
}
```

All fields are `Option` and default to `None`. When `None`:

- `importance` defaults to `0.5`
- `metadata` defaults to `{}`
- `t_valid` and `t_invalid` are unset (the fact is valid for all time)
- `pinned` uses the classifier's decision (or `false` if no classifier is provided). Setting `pinned: Some(true)` overrides the classifier.

Example with overrides:

```rust
use chrono::Utc;
use memory_engine::types::AddFactOptions;

let opts = AddFactOptions {
    importance: Some(0.9),
    metadata: Some(serde_json::json!({
        "source": "deployment-pipeline",
        "confidence": 0.95
    })),
    t_valid: Some(Utc::now()),
    t_invalid: None,  // no expiration
    ..Default::default()
};

let id = engine.add_fact(
    "Production API latency is 42ms p99",
    FactType::Episodic,
    Some(event_id),
    &embedder,
    Some("team:backend/service:api"),
    Some(&opts),
    None,  // no auto-pin classifier
)?;
```

## Bi-temporal timestamps

Facts carry four timestamps. Two are set by the consumer, two are managed by the engine:

| Timestamp   | Set by                          | Meaning                                                                |
| ----------- | ------------------------------- | ---------------------------------------------------------------------- |
| `t_valid`   | Consumer (via `AddFactOptions`) | When the fact became true in the real world.                           |
| `t_invalid` | Consumer (via `AddFactOptions`) | When the fact stopped being true in the real world.                    |
| `t_created` | Engine                          | When the fact was inserted into the database.                          |
| `t_expired` | Engine                          | When the fact was soft-deleted (by forgetting or conflict resolution). |

The `t_valid`/`t_invalid` pair represents _validity time_ -- when the fact holds in reality. The `t_created`/`t_expired` pair represents _transaction time_ -- when the database knew about the fact. This bi-temporal model lets you query "what did the agent know at time T?" separately from "what was true at time T?".

## Hierarchical scoping

The `scope` parameter accepts slash-separated paths like `"user:michael/project:demo"`. The engine resolves these paths into an internal scope tree, creating intermediate nodes as needed.

Scoping enables:

- Isolating facts per user, project, or session
- Querying facts with inheritance (a child scope sees its ancestors' facts)
- Different forgetting policies per scope

When `scope` is `None`, the fact is placed at the root scope (ID 1), which is visible to all scope queries.

```rust
// Facts at different scopes
engine.add_fact("Global preference", FactType::Semantic, None, &embedder,
    None, None, None)?;  // root scope

engine.add_fact("Michael's preference", FactType::Semantic, None, &embedder,
    Some("user:michael"), None, None)?;

engine.add_fact("Project-specific config", FactType::Procedural, None, &embedder,
    Some("user:michael/project:demo"), None, None)?;
```
