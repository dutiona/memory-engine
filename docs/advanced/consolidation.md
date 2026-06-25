# Consolidation

**Status: Implemented**

Consolidation compresses the fact store by removing duplicates and generating hierarchical summaries via a three-pass pipeline: dedup, cluster fusion, global integration. This taxonomy follows the Memory Survey's consolidation framework.

Each run is structured **read → compute → write** so the engine's single write lock is _not_ held across the consumer `SummaryGenerator`/`EmbeddingProvider` calls, which are unbounded network IO (#409): a brief locked read snapshots the active set, the three passes (including all consumer IO) then run lock-free against that snapshot to produce a plan, and a final brief locked transaction applies the plan. This keeps other writers — and, for an in-memory pool, readers — from being starved for the IO duration, while preserving atomicity (below).

## Three-Pass Pipeline

```
Pass 1: Local Dedup       Pass 2: Cluster Fusion       Pass 3: Global Integration
  cosine >= threshold -->   group by similarity     -->   summarize all clusters
  expire duplicates         generate cluster summaries     into one global summary
```

Atomicity is preserved end-to-end. The compute phase produces a plan without touching the store, so a failure there (including errors from the `SummaryGenerator`/`EmbeddingProvider`) aborts the run before any write. The write phase applies the whole plan — expirations, cluster + global summaries, embedding identity, watermark — in a **single SQLite transaction**, so a failure there rolls everything back. Either way the store is never left half-consolidated.

### Pass 1: Local Dedup

Compares facts created since the last consolidation against all active facts. **Pinned facts are excluded from dedup** — they are never candidates for expiration, and pinned candidates are skipped during comparison. This ensures unforgettable facts are preserved even if near-duplicates exist.

Pairs with cosine similarity **at or above** `dedup_threshold` are resolved by expiring the lower-importance fact. Tie-break on equal importance: the newer fact (higher id) is expired. `dedup_threshold` lives in `[0.0, 1.0]`; a value of `1.0` merges only exact duplicates (identical embeddings).

```rust
// dedup.rs — core logic (simplified)
if new_fact.is_pinned { continue; }  // pinned facts skip dedup entirely

let similarity = cosine_similarity(&new_fact.embedding, &candidate.embedding);
if candidate.is_pinned { continue; }  // pinned candidates are also protected

if similarity >= threshold {  // >= so threshold 1.0 merges exact duplicates
    let expire_id = if new_fact.base_importance < candidate.base_importance {
        new_fact.id
    } else if new_fact.base_importance > candidate.base_importance {
        candidate.id
    } else {
        new_fact.id.max(candidate.id) // equal importance: newer expires
    };
    fact_store.expire(expire_id, now)?;
    edge_store.expire_by_fact(expire_id, now)?;
}
```

When a duplicate pair is resolved, the surviving fact **inherits the loser's `importance_score`** if it is higher. This prevents loss of accumulated importance during dedup.

Expired facts have their edges cascade-expired as well, keeping the graph consistent.

### Pass 2: Cluster Fusion

Groups active facts using greedy single-linkage clustering at a similarity threshold of 0.85 (hardcoded, lower than the dedup threshold). For each cluster meeting `min_cluster_size`, the `SummaryGenerator` produces a textual summary, which the `EmbeddingProvider` then embeds into the fact vector space. These are stored as `Cluster`-level summaries.

Prior cluster summaries are deleted before new ones are created, making this pass idempotent.

Scope assignment for cluster summaries uses majority vote across source facts, with the lowest scope_id as tie-breaker.

### Pass 3: Global Integration

Summarizes all cluster-level summaries into a single global summary. The `SummaryGenerator` receives the cluster summaries (wrapped as pseudo-`Fact` structs). The global summary is always root-scoped (`scope_id=1`) since it aggregates across all clusters.

Prior global summaries are deleted before the new one is created (idempotent).

Returns 1 if a global summary was created, 0 if no clusters exist.

## Configuration

```rust
pub struct ConsolidationConfig {
    /// Cosine similarity threshold for dedup (e.g., 0.92).
    pub dedup_threshold: f32,
    /// Minimum cluster size for fusion (e.g., 3).
    pub min_cluster_size: usize,
}
```

## Statistics

```rust
pub struct ConsolidationStats {
    pub duplicates_removed: usize,
    pub clusters_created: usize,
    pub global_summaries: usize, // 0 or 1
}
```

## The SummaryGenerator Trait

Consumers implement `SummaryGenerator` to provide summarization logic. The engine calls this during cluster fusion and global integration:

```rust
pub trait SummaryGenerator {
    fn summarize(&self, facts: &[Fact]) -> Result<String>;
}
```

A typical implementation wraps an LLM call for `summarize()`. The engine has no LLM dependency -- this is entirely consumer-provided.

Summary **embedding** is performed by the `EmbeddingProvider` passed alongside the generator into `consolidate()`, so summaries share the same vector space as the facts they summarize. The generator no longer embeds: a dedicated `SummaryGenerator::embed` once duplicated `EmbeddingProvider::embed` and was removed in favor of injecting the embedder directly into the consolidation call chain.

## Usage

```rust
let config = ConsolidationConfig {
    dedup_threshold: 0.92,
    min_cluster_size: 3,
};
let stats = engine.consolidate(&my_summary_generator, &my_embedding_provider, &config)?;
println!(
    "deduped={}, clusters={}, global={}",
    stats.duplicates_removed, stats.clusters_created, stats.global_summaries
);
```

## Graph Rebuild

After dedup, if any duplicates were removed, the engine rebuilds the in-memory graph from the database. This keeps degree-based scoring (used by the forgetting system) consistent with the current state of active edges.

```rust
// engine.rs — post-consolidation
if stats.duplicates_removed > 0 {
    *self.graph.write() = MemoryGraph::load_from_db(&conn)?;
}
```

## Incremental Operation

The engine tracks `last_consolidated_at` in the config table. On subsequent runs, only facts created after this timestamp are compared during dedup. This avoids quadratic re-comparison of the entire fact store.

After successful completion, `last_consolidated_at` is updated to the current time.
