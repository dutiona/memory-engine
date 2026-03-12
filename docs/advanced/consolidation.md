# Consolidation

**Status: Implemented**

Consolidation compresses the fact store by removing duplicates and generating hierarchical summaries. It runs as a three-pass pipeline within a single SQLite transaction: dedup, cluster fusion, global integration. This taxonomy follows the Memory Survey's consolidation framework.

## Three-Pass Pipeline

```
Pass 1: Local Dedup       Pass 2: Cluster Fusion       Pass 3: Global Integration
  cosine > threshold  -->   group by similarity     -->   summarize all clusters
  expire duplicates         generate cluster summaries     into one global summary
```

All three passes execute atomically. If any pass fails (including errors from the `SummaryGenerator`), the entire consolidation rolls back.

### Pass 1: Local Dedup

Compares facts created since the last consolidation against all active facts. **Pinned facts are excluded from dedup** — they are never candidates for expiration, and pinned candidates are skipped during comparison. This ensures unforgettable facts are preserved even if near-duplicates exist.

Pairs with cosine similarity above `dedup_threshold` are resolved by expiring the lower-importance fact. Tie-break on equal importance: the newer fact (higher id) is expired.

```rust
// dedup.rs — core logic (simplified)
if new_fact.is_pinned { continue; }  // pinned facts skip dedup entirely

let similarity = cosine_similarity(&new_fact.embedding, &candidate.embedding);
if candidate.is_pinned { continue; }  // pinned candidates are also protected

if similarity > threshold {
    let expire_id = if new_fact.importance < candidate.importance {
        new_fact.id
    } else if new_fact.importance > candidate.importance {
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

Groups active facts using greedy single-linkage clustering at a similarity threshold of 0.85 (hardcoded, lower than the dedup threshold). For each cluster meeting `min_cluster_size`, the `SummaryGenerator` produces a textual summary and embedding. These are stored as `Cluster`-level summaries.

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

Consumers implement `SummaryGenerator` to provide summarization and embedding logic. The engine calls this during cluster fusion and global integration:

```rust
pub trait SummaryGenerator {
    fn summarize(&self, facts: &[Fact]) -> Result<String>;
    fn embed(&self, text: &str) -> Result<Vec<f32>>;
}
```

A typical implementation wraps an LLM call for `summarize()` and an embedding model call for `embed()`. The engine has no LLM dependency -- this is entirely consumer-provided.

## Usage

```rust
let config = ConsolidationConfig {
    dedup_threshold: 0.92,
    min_cluster_size: 3,
};
let stats = engine.consolidate(&my_summary_generator, &config)?;
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
