# ADR-0004: Zero LLM/Network Dependencies in Core

**Status:** Accepted
**Date:** 2026-03-10

## Context

The memory engine must work with any embedding model (local or API), any LLM (for summarization and conflict arbitration), and any forgetting strategy. The target backbone is Qwen 3.5 35B running locally via MLX, but the engine must not assume this.

The research showed diverse backend choices across papers:

- Graphiti/Zep uses Neo4j + OpenAI embeddings.
- Mem0 uses Qdrant + configurable LLM.
- A-Mem uses custom storage + LLM-driven self-organization.
- AgeMem (2601.01885) validated a trait-based approach where memory operations are exposed as tools, with the agent choosing when to invoke them.

The engine is explicitly retrieval-only. Parameter update approaches (SparseMemFT, Doc-to-LoRA) are out of scope -- the engine stores and retrieves, the consumer decides what to do with results (ROADMAP, Conceptual Boundaries).

## Decision

The core crate defines three consumer-implemented traits and one configuration struct:

```rust
trait EmbeddingProvider {
    fn embed(&self, text: &str) -> Result<Vec<f32>>;
}

trait SummaryGenerator {
    fn summarize(&self, facts: &[Fact]) -> Result<String>;
    fn embed(&self, text: &str) -> Result<Vec<f32>>;
}

trait ConflictArbiter {
    fn arbitrate(&self, old_fact: &Fact, new_fact: &Fact) -> Result<CrudDecision>;
}

struct ForgetPolicy { /* configurable weights and thresholds */ }
```

`EmbeddingProvider` is required for `add_fact()` and `query()`. `SummaryGenerator` is required for `consolidate()`. `ConflictArbiter` is required for `resolve_conflict()`. `ForgetPolicy` is a plain struct with `Default` impl.

The `CrudDecision` enum (Add, Update, Delete, Noop) follows Mem0's conflict resolution pattern (2504.19413).

Traits added since original decision: `PersistenceClassifier` (Phase 3b) for unforgettable facts, `Reranker` (Phase 4a) for cross-encoder reranking after RRF. Future traits planned: `KnowledgeBaseConnector` (Phase 5) for external knowledge integration.

## Consequences

### Positive

- The core crate has zero network dependencies and zero LLM dependencies. It compiles fast and has a minimal dependency tree.
- Any embedding model works: local (nomic-embed-text on Jetson), API (OpenAI, Cohere), or custom. The engine does not care.
- Testing is straightforward. Tests use trivial trait implementations (fixed embeddings, no-op summarizers) without mocking network calls.
- The boundary between engine and consumer is explicit. The engine never interprets content -- it stores, indexes, and retrieves.

### Negative

- Consumers must implement 1-3 traits depending on which features they use. This is more integration code than an all-in-one solution.
- `SummaryGenerator` requires both `summarize()` and `embed()` methods, meaning the consumer must wire up both an LLM and an embedding model for consolidation.
- No default implementations are provided for any trait. The engine ships no models.

### Mitigations

- Trait implementations are typically thin wrappers (5-20 lines) around existing LLM client libraries.
- The `ForgetPolicy` struct has sensible defaults via `Default::default()`, so consumers who want forgetting only need to call `forget()` with no configuration.
- Future companion crates (e.g., `memory-engine-ollama`) could provide ready-made trait implementations for common providers.
