# ADR-0008: Materialized Importance Score on Facts

**Status:** Accepted
**Date:** 2026-03-10
**Phase:** 3b

## Context

`resume_context()` returns facts sorted by importance for the "high-importance" tier. The importance formula is a weighted composite of 4 signals: recency (Ebbinghaus decay), access frequency, graph connectivity (degree), and base importance.

Computing this on-the-fly during `resume_context()` requires:

1. Loading all active facts
2. For each fact, querying its graph degree (O(1) per fact but N lookups)
3. Sorting by the computed score

This is O(N × degree-lookup) per resume call. At scale (thousands of active facts), this adds latency to what should be a fast boot operation.

## Decision

Store `importance_score` as a materialized column on the `facts` table (`REAL NOT NULL DEFAULT 0.5`). Update it during:

1. **`prune()`** — scores are recomputed for all active facts as part of the forgetting pass
2. **`consolidate()`** — the surviving fact in a dedup pair inherits the max importance of the merged set

`resume_context()` reads the materialized score directly via `ORDER BY importance_score DESC`, making it an O(1) index scan.

## Consequences

**Positive:**

- `resume_context()` sorting is O(1) via the `idx_facts_importance_score` index
- No graph lock needed during resume (scores are pre-computed)
- Scores are available for any future query that needs importance-based ranking

**Negative:**

- Score staleness: between `prune()`/`consolidate()` calls, scores may be stale (new facts get default 0.5, access counts change, graph evolves)
- Acceptable trade-off: consumers who need fresh scores call `forget()` first, which triggers a full recomputation

**Alternatives considered:**

- On-demand computation: rejected due to O(N × degree) cost per resume call
- Background refresh thread: over-engineering for current scale; `prune()` already runs periodically

## Notes

- `importance_score` exists on `Fact` (read from DB) but NOT on `NewFact` (consumer input). The engine computes it; consumers don't set it.
- Default value of 0.5 means newly inserted facts appear in the middle of the ranking until the next prune cycle materializes their true score.
