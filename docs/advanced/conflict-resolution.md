# Conflict Resolution

**Status: Implemented**

Conflict resolution handles contradicting facts through a consumer-defined arbiter. The engine provides the transaction mechanics and graph updates; the consumer decides what to do. Inspired by the Mem0 CRUD pattern.

## The ConflictArbiter Trait

Consumers implement `ConflictArbiter` to define resolution logic:

```rust
pub trait ConflictArbiter {
    fn arbitrate(&self, old_fact: &Fact, new_fact: &Fact) -> Result<CrudDecision>;
}
```

The arbiter receives both the existing fact and the candidate new fact (with `id=0` as a placeholder since it hasn't been inserted yet). It returns one of four decisions.

## CrudDecision

```rust
pub enum CrudDecision {
    Add,
    Update,
    Delete,
    Noop,
}
```

### Add

Both facts coexist. The new fact is inserted and a `"supplements"` edge is created from the new fact to the old fact. Neither fact is expired.

```
[new fact] --supplements--> [old fact]
```

### Update

The old fact is expired and invalidated (`t_expired` and `t_invalid` both set). The new fact is inserted. A `"contradicts"` edge is created from the new fact to the old fact. All existing edges involving the old fact are cascade-expired.

```
[new fact] --contradicts--> [old fact (expired)]
```

### Delete

The old fact is expired and invalidated. All its edges are cascade-expired. The new fact is NOT inserted. No new edges are created.

### Noop

No changes. Neither fact is modified or inserted. No edges are created.

## ConflictResolution Return Type

```rust
pub struct ConflictResolution {
    pub decision: CrudDecision,
    pub old_fact_id: i64,
    pub new_fact_id: Option<i64>, // Some for Add/Update, None for Delete/Noop
}
```

## Usage

```rust
use memory_engine::traits::{ConflictArbiter, CrudDecision};
use memory_engine::types::Fact;
use memory_engine::error::Result;

struct RecencyArbiter;

impl ConflictArbiter for RecencyArbiter {
    fn arbitrate(&self, old_fact: &Fact, new_fact: &Fact) -> Result<CrudDecision> {
        // Newer fact wins -- update to replace the old one
        if new_fact.t_created > old_fact.t_created {
            Ok(CrudDecision::Update)
        } else {
            Ok(CrudDecision::Noop)
        }
    }
}

// Resolve: arbiter decides, engine executes
let result = engine.resolve_conflict(&RecencyArbiter, old_fact_id, &new_fact)?;
match result.decision {
    CrudDecision::Update => println!("replaced fact {}", result.old_fact_id),
    CrudDecision::Noop => println!("kept existing fact"),
    _ => {}
}
```

## Transaction Semantics

All mutations for a single conflict resolution happen in one SQLite transaction:

1. Load the old fact.
2. Call `arbiter.arbitrate(old, new)`.
3. Execute the decision (expire, insert, create edges) within a transaction.
4. Commit.
5. Update the in-memory graph only after successful commit.

If the arbiter returns an error, no mutations occur. If a database operation fails mid-transaction, the transaction rolls back.

## Graph Updates

The in-memory graph is updated after the transaction commits:

- **Add**: New edge added (`new_id -> old_id`, type `"supplements"`).
- **Update**: All edges involving `old_id` are removed, then the new `"contradicts"` edge is added.
- **Delete**: All edges involving `old_id` are removed.
- **Noop**: No graph changes.

## Error Handling

- `MemoryError::NotFound` if `old_id` doesn't exist in the database.
- Errors from the arbiter's `arbitrate()` call propagate directly.
- `MemoryError::Database` on SQL failures.

The arbiter can return errors to signal that it cannot make a decision (e.g., insufficient context). This aborts the resolution without any side effects.
