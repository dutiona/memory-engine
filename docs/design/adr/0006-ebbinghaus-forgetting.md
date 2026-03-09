# ADR-0006: Multi-Signal Importance Scoring for Forgetting

**Status:** Accepted
**Date:** 2026-03-10

## Context

Facts accumulate without bound. Consolidation (ADR-0005) merges duplicates and creates summaries, but does not remove low-value facts. A separate forgetting mechanism is needed to prune facts that are no longer worth retrieving.

The Memory Survey (2512.13564) identifies three forgetting signals:

1. **Time expiration** -- Facts decay with age.
2. **Access frequency** -- Rarely accessed facts are less valuable.
3. **Informational value** -- Facts with low intrinsic importance can be pruned.

Community research (Research Journal Entry 10) added a fourth signal from Ori Mnemos: **graph connectivity** -- well-connected facts (high degree in the knowledge graph) are structurally important and should resist forgetting.

The Ebbinghaus forgetting curve (exponential decay over time) provides the temporal foundation. Memori (community project) used a 69-day half-life with decay scoring, validating this approach at the implementation level.

The challenge: no single signal is sufficient. A recent but trivial fact should still be forgotten. An old but highly-connected fact should persist. The scoring must combine all signals with tunable weights.

## Decision

Forgetting uses a `ForgetPolicy` struct with a weighted importance score computed from 4 signals:

| Signal             | Weight (default) | Computation                                             |
| ------------------ | ---------------- | ------------------------------------------------------- |
| Recency            | 0.3              | Ebbinghaus decay: `0.5^(days_since_access / half_life)` |
| Frequency          | 0.2              | `ln(access_count + 1) / ln(101.0)` (capped)             |
| Graph connectivity | 0.3              | `ln(edge_count + 1) / ln(51.0)` (capped)                |
| Base importance    | 0.2              | `fact.importance` (set at creation, range [0, 1])       |

Final score: `recency_weight * decay + frequency_weight * freq_norm + graph_degree_weight * degree_norm + base_importance_weight * importance`

Facts with a final score below `min_importance` (default: 0.1) are soft-expired (`t_expired` set). Expired facts' edges are cascade-expired in both SQLite and the in-memory graph.

Configuration:

- `half_life_days`: Default 69.0 days. Per-`FactType` overrides via `half_life_overrides` HashMap (e.g., Episodic=30, Procedural=365).
- All weights must be >= 0. They do not need to sum to 1 (the score is not normalized).
- `ForgetPolicy::validate()` checks all invariants before execution.

Normalization uses `ln_1p` for numerical accuracy near zero, with named ceiling constants (101.0 for frequency, 51.0 for connectivity) to bound the normalized values to [0, 1].

`PruneStats` is returned with `facts_expired` and `facts_evaluated` counts.

## Consequences

### Positive

- Graph-aware: well-connected facts (hubs in the knowledge graph) naturally resist forgetting. This preserves structural integrity.
- Tunable: consumers can shift the balance between signals. An agent that values recency can increase `recency_weight`; one that values structure can increase `graph_degree_weight`.
- Per-type half-lives allow episodic facts (transient observations) to decay faster than procedural facts (learned skills).
- Soft deletion preserves the audit trail (consistent with ADR-0001 and ADR-0003).

### Negative

- Requires periodic `forget()` calls. The engine does not auto-forget -- the consumer must schedule it (e.g., in a nightly "dream cycle").
- Graph degree is computed from the in-memory Petgraph, so the graph must be loaded and current. Stale graph data leads to inaccurate connectivity scores.
- The ceiling constants (101.0, 51.0) are heuristic. Facts with >100 accesses or >50 edges all score 1.0 on their respective signals.

### Mitigations

- Phase 3b plans `is_pinned` flag and `PersistenceClassifier` trait to mark unforgettable facts, providing a hard override for the scoring system.
- Phase 4 plans materialized importance scores on facts for fast retrieval without recomputation.
- Graph rebuild is performed after consolidation (ADR-0005), keeping the graph consistent before forgetting runs.
