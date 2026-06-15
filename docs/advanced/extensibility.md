# Extensibility

**Status: Implemented**

memory-engine is designed around trait-based extensibility. The core crate has zero LLM or network dependencies. Consumers bring their own models by implementing five traits and configuring one policy struct.

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

Called during consolidation (cluster fusion and global integration) to produce textual summaries:

```rust
pub trait SummaryGenerator {
    fn summarize(&self, facts: &[Fact]) -> Result<String>;
}
```

The `summarize` method receives a slice of related facts (a cluster or the set of cluster summaries) and returns a textual summary. Embedding of that summary is **not** the generator's job: the `EmbeddingProvider` passed alongside the generator into `consolidate()` embeds it, so summaries share the fact vector space. (A redundant `SummaryGenerator::embed` was removed; it merely duplicated `EmbeddingProvider::embed`.)

Implementation example:

```rust
struct LlmSummarizer {
    client: reqwest::blocking::Client,
    api_key: String,
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
}
```

Errors from the `SummaryGenerator` (or the `EmbeddingProvider`) roll back the entire consolidation transaction.

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

### PersistenceClassifier

Called during `add_fact()` to decide whether a newly inserted fact should be pinned (unforgettable). This is an optional parameter — passing `None` for the classifier skips auto-pinning.

```rust
pub trait PersistenceClassifier {
    /// Decide if a fact should be pinned (never forgotten).
    fn should_pin(&self, fact: &Fact) -> bool {
        let _ = fact;
        false
    }
}
```

The default implementation returns `false` — opt-in, zero behavior change for existing consumers.

**Classifier input caveat:** The `Fact` passed to `should_pin()` during `add_fact()` is a pre-insert synthetic with `id=0`, `scope_id=0`, and `importance_score` seeded from the base `importance`. Classifiers should only rely on `content`, `fact_type`, `importance` (caller hint), and `metadata` — not on `id`, `scope_id`, `importance_score`, or `access_count`.

Implementation example:

```rust
use memory_engine::traits::PersistenceClassifier;
use memory_engine::types::Fact;

struct KeywordPinner {
    keywords: Vec<String>,
}

impl PersistenceClassifier for KeywordPinner {
    fn should_pin(&self, fact: &Fact) -> bool {
        // Pin facts containing critical keywords
        self.keywords.iter().any(|kw| fact.content.contains(kw))
    }
}
```

When both the classifier returns `true` and `AddFactOptions::pinned` is explicitly set, the explicit `pinned` field takes precedence.

### Reranker

Called during `query()` to refine the top-K search results using a cross-encoder or similar precise scoring model. This is the second stage in a two-stage retrieval pipeline: fast bi-encoder retrieval (FTS + vector + RRF) followed by precise cross-encoder reranking.

```rust
pub trait Reranker: Send + Sync {
    fn rerank(&self, query: &str, candidates: &[SearchResult]) -> Result<Vec<(usize, f64)>>;
    fn name(&self) -> &str;
}
```

The `rerank` method receives the query text and a borrowed slice of candidate results from `hybrid_search()`. It returns `(index, score)` pairs referencing positions in the input slice. The engine reconstructs the final result set from these indices, preserving the original `Fact` and `MatchType` values unchanged — this structurally prevents the reranker from mutating fact content, embeddings, or match types.

The method is failable -- errors propagate as `MemoryError::Reranker`. All returned scores must be finite (not NaN or Inf).

`Send + Sync` bounds are required because `MemoryEngine` is shared across threads via `Arc`.

Implementation example:

```rust
use memory_engine::traits::Reranker;
use memory_engine::search::hybrid::SearchResult;

struct CrossEncoderReranker {
    client: reqwest::blocking::Client,
    model_url: String,
}

impl Reranker for CrossEncoderReranker {
    fn rerank(&self, query: &str, candidates: &[SearchResult]) -> Result<Vec<(usize, f64)>> {
        // Score each (query, document) pair with the cross-encoder
        let mut scored: Vec<(usize, f64)> = candidates
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let score = self.cross_encode(query, &r.fact.content)?;
                Ok((i, score))
            })
            .collect::<Result<Vec<_>>>()?;

        // Sort by cross-encoder score descending
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        Ok(scored)
    }

    fn name(&self) -> &str {
        "cross_encoder"
    }
}
```

The reranker is optional. When no reranker is configured (or the query has no text), results are returned directly from `hybrid_search()`.

**Engine wiring:**

```rust
// File-backed engine with reranker
let engine = MemoryEngine::open_with_reranker(&config, Some(Box::new(my_reranker)))?;

// In-memory engine with reranker (for testing)
let engine = MemoryEngine::open_memory_with(dim, None, Some(Box::new(my_reranker)))?;

// Check active reranker
assert_eq!(engine.reranker_name(), Some("cross_encoder"));
```

**Contract enforcement:** The engine validates reranker output at runtime (defense-in-depth). After each `rerank()` call, `query()` checks that: (1) every output fact ID was present in the input candidates, (2) no duplicate IDs appear, and (3) output length does not exceed input length. Violations produce `MemoryError::Reranker` with a diagnostic message. This guards against buggy reranker implementations while preserving the trusted-consumer model for well-behaved code.

**Lock semantics:** Reranking runs _outside_ the database read lock. This is critical because cross-encoder inference (local model or API call) can take 10-100ms per candidate. The read lock is held only for `hybrid_search()`, then released before reranking begins.

**Research rationale:** Four-layer cognitive architecture research shows cross-encoder reranking on top-20 candidates improves nDCG@10 by 5-15%. The engine is positioned at layer 2 (retrieval + reranking), while layers 3-4 (semantic extraction, context adaptation) are consumer responsibilities.

### SessionExtractor

Called during `bootstrap_session()` and `bootstrap_directory()` to extract facts from candidate episodes identified by the keyword pre-filter. Lives in the `bootstrap` module (not `traits.rs`) because it is domain-specific to the bootstrap pipeline.

```rust
pub trait SessionExtractor {
    fn extract(
        &self,
        episode: &CandidateEpisode,
        outcome: &SessionOutcome,
    ) -> Result<Vec<ExtractedFact>>;
}
```

The method receives a `CandidateEpisode` (pre-filtered conversation turns with matched keywords and a category) and the session's heuristic outcome. It returns zero or more `ExtractedFact` values, each with content, fact type, importance, category, and metadata.

The default implementation (`KeywordExtractor`) maps `(category, outcome)` pairs to `(FactType, importance)` without requiring an LLM:

```rust
use memory_engine::bootstrap::KeywordExtractor;

let extractor = KeywordExtractor;
// Bug + Success → Procedural fact at importance 0.7
// Convention + any → Procedural fact at importance 0.8
// Decision + any → Semantic fact at importance 0.6
```

For higher-quality extraction, implement `SessionExtractor` with an LLM to produce parameterized procedural patterns or multi-fact outputs per episode.

## VectorSearchStrategy: Internal Dispatch Trait

`VectorSearchStrategy` is an internal trait that enables runtime dispatch between brute-force and HNSW vector search. Unlike the consumer traits above, this is **not** implemented by consumers — it is an engine-internal abstraction.

```rust
pub trait VectorSearchStrategy: Send + Sync {
    fn search(&self, conn: &Connection, query_embedding: &[f32],
              embed_dim: usize, limit: usize,
              fact_type: Option<&FactType>, scope_ids: Option<&[i64]>,
    ) -> Result<Vec<VectorResult>>;

    fn notify_insert(&self, _fact_id: i64, _embedding: &[f32]) {}
    fn notify_expire(&self, _fact_id: i64) {}

    fn name(&self) -> &str;
}
```

Two implementations exist:

| Strategy       | Feature flag | When used                       |
| -------------- | ------------ | ------------------------------- |
| `BruteForce`   | (always)     | `active_count < ann_threshold`  |
| `HnswStrategy` | `ann`        | `active_count >= ann_threshold` |

The lifecycle hooks (`notify_insert`, `notify_expire`) are no-ops on `BruteForce` and maintain the in-memory HNSW index on `HnswStrategy`.

## SearchConfig: ANN Dispatch Threshold

```rust
pub struct SearchConfig {
    pub ann_threshold: usize,  // default: 50_000
}
```

Controls when the engine switches from brute-force to HNSW. Passed via `EngineConfig::search_config`. Without `SearchConfig` (or without the `ann` feature), brute-force is always used.

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

| Extension point         | Type           | When called           | Provided by                         |
| ----------------------- | -------------- | --------------------- | ----------------------------------- |
| `EmbeddingProvider`     | consumer trait | `add_fact()`          | Text-to-vector model                |
| `SummaryGenerator`      | consumer trait | `consolidate()`       | Summarization (embedding via `EmbeddingProvider`) |
| `ConflictArbiter`       | consumer trait | `resolve_conflict()`  | Resolution logic                    |
| `PersistenceClassifier` | consumer trait | `add_fact()`          | Auto-pinning logic                  |
| `Reranker`              | consumer trait | `query()`             | Cross-encoder reranking (Phase 4a)  |
| `SessionExtractor`      | consumer trait | `bootstrap_session()` | Episode-to-fact extraction          |
| `ForgetPolicy`          | struct         | `forget()`            | Decay parameters                    |
| `VectorSearchStrategy`  | internal trait | `query()`             | Engine (BruteForce or HnswStrategy) |
| `SearchConfig`          | struct         | `open()`              | ANN dispatch threshold              |
