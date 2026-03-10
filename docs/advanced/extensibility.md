# Extensibility

**Status: Implemented**

memory-engine is designed around trait-based extensibility. The core crate has zero LLM or network dependencies. Consumers bring their own models by implementing three traits and configuring one policy struct.

## Design Philosophy

The engine handles storage, retrieval, graph management, temporal semantics, and lifecycle operations. It does not embed text, generate summaries, or decide how to resolve conflicts. These capabilities are injected by the consumer through trait implementations.

This boundary is deliberate:

- **No vendor lock-in.** Consumers choose their embedding model (OpenAI, local ONNX, sentence-transformers, etc.).
- **No network dependency.** The core crate compiles and runs with zero network calls. All external communication happens in consumer-provided trait implementations.
- **Testable.** Mock implementations with fixed vectors are sufficient for testing engine behavior.

The engine is retrieval-only. It does not fine-tune models or perform in-context learning on stored memories (the SparseMemFT boundary).

## Consumer Traits

### EmbeddingProvider

Called during `add_fact()` to compute the embedding vector for new facts. The engine calls this _before_ acquiring the write lock, so slow embedding calls (network API round-trips) don't block readers.

```rust
pub trait EmbeddingProvider {
    fn embed(&self, text: &str) -> Result<Vec<f32>>;
}
```

Implementation example:

```rust
struct OnnxEmbedder {
    session: ort::Session,
    tokenizer: tokenizers::Tokenizer,
    dim: usize,
}

impl EmbeddingProvider for OnnxEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let tokens = self.tokenizer.encode(text, true)
            .map_err(|e| MemoryError::Embedding(e.to_string()))?;
        // Run ONNX inference, extract pooled output
        // ...
        Ok(embedding)
    }
}
```

The returned vector's dimension must match the `embed_dim` configured on the engine. Dimension mismatches are caught at insertion time.

### SummaryGenerator

Called during consolidation (cluster fusion and global integration) to produce textual summaries and their embeddings:

```rust
pub trait SummaryGenerator {
    fn summarize(&self, facts: &[Fact]) -> Result<String>;
    fn embed(&self, text: &str) -> Result<Vec<f32>>;
}
```

The `summarize` method receives a slice of related facts (a cluster or the set of cluster summaries) and returns a textual summary. The `embed` method computes an embedding for that summary text.

Implementation example:

```rust
struct LlmSummarizer {
    client: reqwest::blocking::Client,
    api_key: String,
    embedder: Arc<dyn EmbeddingProvider>,
}

impl SummaryGenerator for LlmSummarizer {
    fn summarize(&self, facts: &[Fact]) -> Result<String> {
        let prompt = format!(
            "Summarize these facts concisely:\n{}",
            facts.iter().map(|f| f.content.as_str()).collect::<Vec<_>>().join("\n")
        );
        // Call LLM API...
        Ok(summary)
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.embedder.embed(text)
    }
}
```

Errors from the `SummaryGenerator` roll back the entire consolidation transaction.

### ConflictArbiter

Called during `resolve_conflict()` to decide how to handle contradicting facts:

```rust
pub trait ConflictArbiter {
    fn arbitrate(&self, old_fact: &Fact, new_fact: &Fact) -> Result<CrudDecision>;
}
```

Returns one of `{ Add, Update, Delete, Noop }`. See [Conflict Resolution](conflict-resolution.md) for the semantics of each decision.

Implementation example:

```rust
struct SemanticArbiter {
    embedder: Arc<dyn EmbeddingProvider>,
    similarity_threshold: f32,
}

impl ConflictArbiter for SemanticArbiter {
    fn arbitrate(&self, old_fact: &Fact, new_fact: &Fact) -> Result<CrudDecision> {
        let sim = cosine_similarity(&old_fact.embedding, &new_fact.embedding);
        if sim > self.similarity_threshold {
            // High similarity: treat as an update (newer replaces older)
            Ok(CrudDecision::Update)
        } else {
            // Low similarity: both facts coexist
            Ok(CrudDecision::Add)
        }
    }
}
```

The arbiter can return an error to abort the resolution with no side effects.

## ForgetPolicy: Configuration, Not a Trait

`ForgetPolicy` is a plain struct with configurable fields, not a trait. The forgetting algorithm (Ebbinghaus decay + multi-signal scoring) is fixed in the engine. Consumers tune it through weights and thresholds rather than replacing the algorithm entirely.

```rust
let policy = ForgetPolicy {
    half_life_days: 90.0,
    min_importance: 0.2,
    recency_weight: 0.4,
    frequency_weight: 0.1,
    graph_degree_weight: 0.3,
    base_importance_weight: 0.2,
    ..ForgetPolicy::default()
};
engine.forget(&policy)?;
```

This is a policy (parameter set), not a strategy (pluggable algorithm). See [Forgetting](forgetting.md) for the full scoring formula.

## Summary of Extension Points

| Extension point     | Type   | When called          | Consumer provides            |
| ------------------- | ------ | -------------------- | ---------------------------- |
| `EmbeddingProvider` | trait  | `add_fact()`         | Text-to-vector model         |
| `SummaryGenerator`  | trait  | `consolidate()`      | Summarization + embedding    |
| `ConflictArbiter`   | trait  | `resolve_conflict()` | Resolution logic             |
| `ForgetPolicy`      | struct | `forget()`           | Decay parameters and weights |
