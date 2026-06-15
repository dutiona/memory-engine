# Forgetting

**Status: Implemented**

The forgetting system prunes stale facts using Ebbinghaus decay combined with multi-signal importance scoring. Facts scoring below a configurable threshold are soft-deleted. This keeps the active fact store bounded without losing history.

## Importance Scoring

Each fact's importance is computed as a weighted sum of 4 normalized signals:

```
importance = w_recency * recency
           + w_frequency * frequency
           + w_degree * connectivity
           + w_base * base_importance
```

### Signal 1: Recency (Ebbinghaus Decay)

Models memory retention as an exponential decay curve:

```
retention = 2^(-age_days / half_life)
```

Returns 1.0 at age=0, 0.5 at age=half_life, 0.25 at age=2\*half_life. Age is measured from `last_accessed`, not `t_created`.

```rust
fn ebbinghaus_decay(age_days: f64, half_life: f64) -> f64 {
    f64::exp2(-age_days / half_life)
}
```

### Signal 2: Frequency (Access Count)

Logarithmic normalization of access count, capped at 1.0:

```
frequency = ln(access_count + 1) / ln(101)
```

100 accesses yields the maximum score. The `ln_1p` function is used for numerical accuracy near zero.

### Signal 3: Graph Connectivity (Degree)

Logarithmic normalization of the fact's graph degree (in + out edges):

```
connectivity = ln(degree + 1) / ln(51)
```

50 connections yields the maximum score. This rewards facts that participate in many relationships.

### Signal 4: Base Importance

The `fact.importance` field, set at insertion time (default 0.5, configurable via `AddFactOptions`). Already in [0, 1].

## ForgetPolicy

All parameters are configurable through the `ForgetPolicy` struct:

```rust
pub struct ForgetPolicy {
    pub half_life_days: f64,           // default: 69.0
    pub half_life_overrides: HashMap<FactType, f64>,
    pub min_importance: f64,           // default: 0.1
    pub recency_weight: f64,           // default: 0.3
    pub frequency_weight: f64,         // default: 0.2
    pub graph_degree_weight: f64,      // default: 0.3
    pub base_importance_weight: f64,   // default: 0.2
}
```

`ForgetPolicy` is a struct, not a trait. It represents a parameter set (policy), not a pluggable strategy.

### Per-FactType Half-Life Overrides

Different fact types can have different decay rates. Episodic memories (conversations, events) decay faster than procedural knowledge (how-to instructions):

```rust
use std::collections::HashMap;
use memory_engine::types::FactType;
use memory_engine::traits::ForgetPolicy;

let mut overrides = HashMap::new();
overrides.insert(FactType::Episodic, 30.0);     // 30-day half-life
overrides.insert(FactType::Procedural, 365.0);  // 1-year half-life

let policy = ForgetPolicy {
    half_life_overrides: overrides,
    ..ForgetPolicy::default()
};
```

Facts with a type not in the override map use `half_life_days` (default 69 days).

### Validation

`ForgetPolicy::validate()` checks all parameters before pruning begins:

- `half_life_days` must be > 0
- All `half_life_overrides` values must be > 0
- `min_importance` must be in [0, 1]
- All weights must be >= 0

Returns `MemoryError::Conflict` on invalid parameters. The `forget()` method calls `validate()` automatically.

## Pruning Process

```rust
let policy = ForgetPolicy {
    min_importance: 0.3,
    ..ForgetPolicy::default()
};
let stats = engine.forget(&policy)?;
```

The pruning process:

1. Validate policy parameters.
2. Load all active facts.
3. **Skip pinned facts** — facts with `is_pinned = true` are never candidates for pruning, regardless of their score.
4. Score each remaining fact using the 4-signal formula with current graph degrees.
5. **Materialize `importance_score`** — each scored fact has its `importance_score` field updated in the database via `update_importance_score()`. This makes the composite score available to `resume_context()` and other queries without recomputation.
6. Collect facts scoring below `min_importance`.
7. In a single transaction: soft-delete each fact (`t_expired = now`) and cascade-expire its edges.
8. After commit: remove expired edges from the in-memory graph.

Scoring happens before any mutations, so degree values are consistent across the batch.

## PruneStats

```rust
pub struct PruneStats {
    pub facts_expired: usize,
    pub facts_evaluated: usize,
}
```

## Edge Cascade

When a fact is expired by the forgetting system, all edges involving that fact (both as source and target) are also expired in SQLite and removed from the in-memory graph:

```rust
for &fact_id in &to_expire {
    fact_store.expire(fact_id, now)?;
    edge_store.expire_by_fact(fact_id, now)?;
}
// After commit:
for &fact_id in &to_expire {
    graph.remove_edges_by_fact(fact_id);
}
```

This prevents dangling edges from inflating degree scores for surviving facts.
