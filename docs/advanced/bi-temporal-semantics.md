# Bi-Temporal Semantics

**Status: Implemented**

memory-engine uses a 4-timestamp bi-temporal model for every fact, inspired by the Graphiti paper (arXiv 2410.13790). This separates _when the engine recorded something_ from _when that something is true in the real world_.

## The 4-Timestamp Model

Every `Fact` carries four temporal fields:

| Field       | Axis        | Meaning                                           |
| ----------- | ----------- | ------------------------------------------------- |
| `t_created` | System time | When the engine inserted this fact                |
| `t_expired` | System time | When the engine retired this fact (soft deletion) |
| `t_valid`   | Valid time  | When this fact becomes true in the real world     |
| `t_invalid` | Valid time  | When this fact stops being true in the real world |

```rust
pub struct Fact {
    // ... other fields ...
    pub t_created: DateTime<Utc>,
    pub t_expired: Option<DateTime<Utc>>,
    pub t_valid: Option<DateTime<Utc>>,
    pub t_invalid: Option<DateTime<Utc>>,
}
```

System time is managed by the engine. Valid time is set by the consumer via `AddFactOptions`.

## System Time: Engine-Managed

`t_created` is set automatically when a fact is inserted. `t_expired` is set by internal operations -- consolidation dedup, forgetting prune, or conflict resolution. The consumer never sets these directly.

Facts are **soft-deleted**: the engine sets `t_expired` rather than removing the row. This preserves the full audit trail. A fact with `t_expired = Some(...)` is invisible to active queries but still exists in the database.

```rust
// The engine expires facts, never hard-deletes them
fact_store.expire(fact_id, now)?;  // sets t_expired = now
```

## Valid Time: Consumer-Set

Valid time represents when a fact holds in the real world. Both fields are optional -- a fact with no valid-time bounds is considered always-valid (within its system-time window).

Set valid time through `AddFactOptions`:

```rust
use memory_engine::types::AddFactOptions;
use chrono::{Utc, Duration};

let now = Utc::now();
let opts = AddFactOptions {
    t_valid: Some(now - Duration::hours(1)),   // became true 1 hour ago
    t_invalid: Some(now + Duration::hours(24)), // stops being true in 24 hours
    ..Default::default()
};

let id = engine.add_fact(
    "deployment freeze until tomorrow",
    FactType::Semantic,
    None,
    &embedder,
    None,
    Some(&opts),
)?;
```

## Temporal Queries

The `SearchQuery` struct has a `valid_at` filter. When set, results are restricted to facts whose valid-time window contains the query timestamp.

```rust
let query = SearchQuery {
    text: Some("deployment".into()),
    embedding: None,
    mode: SearchMode::Fts,
    limit: 10,
    valid_at: Some(Utc::now()), // only facts valid right now
    fact_type: None,
    scope: None,
};
let results = engine.query(&query)?;
```

The temporal filter logic is applied as a post-filter after FTS/vector retrieval:

- If `t_valid` is set on the fact and `t_valid > valid_at`, the fact is excluded (not yet valid).
- If `t_invalid` is set on the fact and `t_invalid <= valid_at`, the fact is excluded (no longer valid).
- Facts with no valid-time bounds pass through unconditionally.
- `t_expired` filtering happens at the SQL level -- expired facts never reach the post-filter.

The 3x overfetch in hybrid search compensates for attrition from this post-filter step.

## Conflict Resolution and Bi-Temporal Expiry

When conflict resolution produces an `Update` or `Delete` decision, the engine sets both `t_expired` and `t_invalid` on the old fact. This marks the fact as both system-retired and real-world-invalidated:

```rust
// Internal: expire_and_invalidate sets both timestamps
"UPDATE facts SET t_expired = ?1, t_invalid = ?1
 WHERE id = ?2 AND t_expired IS NULL"
```

This distinguishes conflict-driven invalidation (both timestamps set) from forgetting-driven expiry (only `t_expired` set).

## Use Cases

**Scheduling (future facts)**. A fact with `t_valid` in the future is invisible to present-time queries but appears when `valid_at` advances past the threshold:

```rust
let opts = AddFactOptions {
    t_valid: Some(Utc::now() + Duration::days(7)),
    ..Default::default()
};
engine.add_fact(
    "new API version goes live next week",
    FactType::Semantic, None, &embedder, None, Some(&opts),
)?;
```

**Historical queries**. Query what the agent knew at a past point in time by setting `valid_at` to a historical timestamp.

**Bounded validity**. Facts with both `t_valid` and `t_invalid` model time-bounded truths -- meeting schedules, temporary policies, seasonal patterns.
