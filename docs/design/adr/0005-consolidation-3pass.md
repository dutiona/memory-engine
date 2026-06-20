# ADR-0005: Three-Pass Consolidation Pipeline

**Status:** Accepted
**Date:** 2026-03-10

## Context

Facts accumulate over time. Without consolidation, the engine stores every incremental observation, including near-duplicates, minor restatements, and thematically related facts that could be merged into higher-level summaries. This degrades retrieval quality (redundant results) and increases storage.

The Memory Survey (2512.13564, section 5.2.1) provides a taxonomy of consolidation strategies organized into three levels:

1. **Local** -- Merge near-duplicate facts (same content, minor wording differences).
2. **Cluster** -- Group thematically related facts into summaries.
3. **Global** -- Update core understanding across the entire knowledge base.

Memento (2508.16153) demonstrated hierarchical summarization for case-based memory. Total Recall (community research, Entry 10) implemented a "dream cycle" with nightly consolidation following a similar pattern.

The key constraint: consolidation must be atomic. A partial consolidation (e.g., dedup succeeds but cluster fusion fails mid-way) would leave the database in an inconsistent state with some facts expired but no summaries created to replace them.

## Decision

Consolidation is a three-pass pipeline, configurable via `ConsolidationConfig`:

**Pass 1: Local Dedup.** Pairwise cosine similarity between active facts. Facts whose similarity is **at or above** `dedup_threshold` (in `[0.0, 1.0]`; `1.0` merges only exact duplicates) are deduplicated: the **lower-importance** fact is soft-expired, with a deterministic tie-break (on equal importance the newer/higher-id fact expires). Edges from expired facts are cascade-expired in both SQLite and the in-memory Petgraph.

**Pass 2: Cluster Fusion.** Remaining active facts are grouped by cosine proximity. Groups meeting `min_cluster_size` are passed to the `SummaryGenerator` trait, which produces a textual summary and its embedding. A `Summary` record at `ConsolidationLevel::Cluster` is created with references to the source fact IDs.

**Pass 3: Global Integration.** All cluster-level summaries are passed to `SummaryGenerator` for a global summary at `ConsolidationLevel::Global`. This captures cross-cluster themes.

All three passes execute within a single `unchecked_transaction`. If any pass fails (including `SummaryGenerator` errors), the entire consolidation rolls back.

After consolidation, the in-memory graph is rebuilt from SQLite to keep degree-based importance scoring consistent for subsequent `forget()` calls.

`ConsolidationStats` is returned with counts: `duplicates_removed`, `clusters_created`, `global_summaries`.

## Consequences

### Positive

- Configurable thresholds let consumers tune aggressiveness. High `dedup_threshold` (e.g., 0.95) is conservative; low (e.g., 0.85) is aggressive.
- The `SummaryGenerator` trait makes the LLM choice a consumer concern. The engine orchestrates the pipeline but does not embed any model.
- Atomic transactions prevent partial consolidation states. Either all three passes succeed or nothing changes.
- The three-level `ConsolidationLevel` enum (Local, Cluster, Global) on `Summary` records makes it clear which pass produced each summary.

### Negative

- Passes 2 and 3 require an LLM (via `SummaryGenerator`). Consolidation cannot run without one.
- Pairwise cosine in Pass 1 is O(N^2) over active facts. This is acceptable at current scale but will not scale to millions of facts.
- The full graph rebuild after consolidation is O(E) where E is the number of active edges. This is a correctness trade-off: rebuilding is simpler and safer than incremental graph mutation.

### Mitigations

- Consumers who want dedup-only can set `min_cluster_size` to a very large value, effectively skipping Passes 2 and 3.
- The O(N^2) dedup can be optimized with locality-sensitive hashing or ANN pre-filtering in a future phase, gated by benchmarks.
- The `last_consolidated_at` config key tracks the most recent consolidation timestamp. Incremental consolidation (only facts since last run) is supported, bounding the working set.
