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

Compares the facts added since the last consolidation (those above its watermark, see [Incremental Operation](#incremental-operation)) against all active facts. **Pinned facts are excluded from dedup** — they are never candidates for expiration, and pinned candidates are skipped during comparison. This ensures pinned facts, which are never pruned, are preserved even if near-duplicates exist.

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

Groups active facts using greedy single-linkage clustering at `cluster_threshold` (default 0.85, lower than the dedup threshold). For each cluster meeting `min_cluster_size`, the `SummaryGenerator` produces a textual summary, which the `EmbeddingProvider` then embeds into the fact vector space. These are stored as `Cluster`-level summaries.

Prior cluster summaries are deleted before new ones are created, making this pass idempotent.

Scope assignment for cluster summaries uses majority vote across source facts, with the lowest scope_id as tie-breaker.

### Pass 3: Global Integration

Summarizes all cluster-level summaries into a single global summary. The `SummaryGenerator` receives the cluster summaries (wrapped as pseudo-`Fact` structs). The global summary is always root-scoped (`scope_id=1`) since it aggregates across all clusters.

Prior global summaries are deleted before the new one is created (idempotent).

Returns 1 if a global summary was created, 0 if no clusters exist.

## Configuration

```rust
#[non_exhaustive]
pub struct ConsolidationConfig {
    /// Minimum cosine similarity for two facts to be duplicates (merged at ≥ this value).
    pub dedup_threshold: f32,
    /// Similarity threshold for single-linkage clustering; looser than `dedup_threshold`.
    pub cluster_threshold: f32,
    /// Minimum number of facts a group needs to be summarized into a cluster (≥ 2).
    pub min_cluster_size: usize,
}
```

Defaults: `dedup_threshold = 0.90`, `cluster_threshold = 0.85`, `min_cluster_size = 2`. Construct it via `ConsolidationConfig::builder()` or `ConsolidationConfig::default()` — the struct is `#[non_exhaustive]`, so a struct literal is not usable outside the crate.

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
    fn summarize(&self, items: &[SummarizableContent<'_>]) -> Result<String>;
}
```

A typical implementation wraps an LLM call for `summarize()`. The engine has no LLM dependency -- this is entirely consumer-provided.

Summary **embedding** is performed by the `EmbeddingProvider` passed alongside the generator into `consolidate()`, so summaries share the same vector space as the facts they summarize. The generator no longer embeds: a dedicated `SummaryGenerator::embed` once duplicated `EmbeddingProvider::embed` and was removed in favor of injecting the embedder directly into the consolidation call chain.

## Usage

```rust
let config = ConsolidationConfig::builder()
    .dedup_threshold(0.92)
    .build();
// Both providers are shared: `consolidate` takes an `Arc<dyn SummaryGenerator>` and an `Arc<dyn EmbeddingProvider>`.
let stats = engine
    .consolidate(Arc::clone(&generator), Arc::clone(&embedder), &config)
    .await?;
println!(
    "deduped={}, clusters={}, global={}",
    stats.duplicates_removed, stats.clusters_created, stats.global_summaries
);
```

## Graph Rebuild

After dedup, if any duplicates were removed, the engine rebuilds the in-memory graph from the database. This keeps degree-based scoring (used by the decay prune) consistent with the current state of active edges.

```rust
// me_consolidate::consolidate, after apply_plan returns the ids it actually expired
if !expired_ids.is_empty() {
    let edges = ctx.storage.list_active_edges().await?;
    *graph.write() = MemoryGraph::from_active_edges(&edges);
}
```

## Incremental Operation

The engine tracks `last_consolidated_fact_id` in the config table. On subsequent runs, only the active facts whose id is above it drive dedup; each is compared against every active fact. This avoids quadratic re-comparison of the entire fact store.

The watermark is a fact id, not a timestamp (#1080): ids are assigned in commit order, so a fact committed after a run sits above that run's watermark whatever its `t_created`. A backdated fact (`t_created` in the past, as bootstrap and backfill write) and a fact committed or embedded while a run was in flight are compared at the next run.

After a run, the watermark advances to the highest fact id the run saw, but only when dedup ran (not over its cap), no fact was waiting for its vector, and no survivor disappeared before the merge was written. Otherwise it holds, and the next run compares those facts again.

Three writes make facts below the watermark eligible again, so they lower or clear it: `unforget` lowers it below the lowest restored fact, unpinning a fact lowers it below that fact (dedup skips pinned facts), and a reconstruction clears it (every vector is new). A run whose store changed this way between its read and its write discards its plan, and the next run recomputes. A restore of an export with id marks (`sequences`, #1131) keeps the watermark and the generation, since every new id starts above the marks (#1144). A snapshot without them resets each id sequence to the highest surviving id, so an erased id can be reused: its restore resets both, and the first run after it compares every fact. So does the first run after the upgrade that replaced `last_consolidated_at`, once.
