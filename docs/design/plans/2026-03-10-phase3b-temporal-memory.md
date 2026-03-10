# Phase 3b: Temporal Memory & Agent Lifecycle — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the memory engine time-aware and give agents a proper cognitive boot sequence — unforgettable facts, future memory surfacing, scheduling API, reworked `resume_context()`, materialized importance scores, and event envelope forward-compatibility.

**Architecture:** Six features layered bottom-up: schema migration first (adds columns), then type/trait changes, then store-level queries, then engine facade wiring, then async wrapper. Each feature builds on the previous. The existing migration framework (v1→v2) extends naturally to v2→v3.

**Tech Stack:** Rust, SQLite (rusqlite), chrono, serde, parking_lot, petgraph, tokio (async feature)

**Branch:** `feat/memory-engine-phase3` (existing remote branch with all Phase 3 work)

**Design Doc:** `docs/design/plans/2026-03-09-future-phases-design.md` (approved, all decisions final)

---

## Context

Phase 3 delivered hardening (thread safety, async, connection pool, scoping, benchmarks, `AddFactOptions`, migration framework). Phase 3b builds on that foundation to add temporal intelligence: facts that never decay, facts that surface at future dates, a scheduling API, and a reworked `resume_context()` that composes all these signals into a proper cognitive boot sequence.

The conceptual framing matters: Memory ≠ Knowledge ≠ Wisdom. This engine stores _what the agent has internalized_ — the Memory layer. Unforgettable facts are the agent's identity and core beliefs. Future memory is the agent's ability to defer action. The scheduling API lets the consumer drive the engine without polling. The importance score materializes the multi-signal formula so `resume_context()` can sort without recomputing.

---

## File Structure

### Files to modify

| File                       | Changes                                                                                                                                                                                                                                                                              |
| -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `src/store/schema.rs`      | Add `CURRENT_SCHEMA_VERSION = 3`, `migrate_v2_to_v3()`, update `TABLES_DDL` for fresh installs, add `TABLES_V2_DDL` test helper                                                                                                                                                      |
| `src/types.rs`             | Add `is_pinned: bool` + `importance_score: f64` to `Fact` (read-only output). Add `is_pinned: bool` to `NewFact` (no `importance_score` — engine-computed). Add `pinned: Option<bool>` to `AddFactOptions`. Add `origin_node_id`, `sequence_id`, `created_at` to `Event`, `NewEvent` |
| `src/traits.rs`            | Add `PersistenceClassifier` trait                                                                                                                                                                                                                                                    |
| `src/store/facts.rs`       | Add `list_pinned()`, `list_due()`, `next_due_time()`, `set_pinned()`, `update_importance_score()`. Modify `insert()` to handle `is_pinned` + `importance_score`. Modify `list_active()` read paths for new columns                                                                   |
| `src/store/events.rs`      | Modify `insert()` and `row_to_event()` for new envelope fields                                                                                                                                                                                                                       |
| `src/forgetting/policy.rs` | Skip pinned facts in `prune()`. Update `importance_score` column after computing importance                                                                                                                                                                                          |
| `src/resume/context.rs`    | Rework to 5-tier pipeline: `ResumeConfig` gets `now`, `due_cap`, `pinned_cap`. `ResumeContext` gets `pinned`, `due` tiers + `kb_stubs` placeholder                                                                                                                                   |
| `src/engine.rs`            | Wire `PersistenceClassifier` into `add_fact()`. Add `list_due()`, `next_due_time()`, `pin_fact()`, `unpin_fact()` methods. Update `forget()` to pass through pinned skip. Rework `resume_context()` to accept `now`                                                                  |
| `src/async_engine.rs`      | Mirror new sync methods: `list_due()`, `next_due_time()`, `pin_fact()`, `unpin_fact()`, updated `resume_context()`                                                                                                                                                                   |
| `src/consolidation/mod.rs` | Update `importance_score` on surviving facts after consolidation                                                                                                                                                                                                                     |

### Files to create

None. All changes extend existing modules.

### Documentation to update

| File                                     | Changes                                                    |
| ---------------------------------------- | ---------------------------------------------------------- |
| `docs/ROADMAP.md`                        | Update Phase 3b status to ✅, add implementation learnings |
| `docs/reference/api.md`                  | Document new public methods and traits                     |
| `docs/advanced/forgetting.md`            | Document pinned fact bypass                                |
| `docs/advanced/bi-temporal-semantics.md` | Document future memory and `t_valid` surfacing             |
| `docs/design/adr/`                       | ADR-0008: Materialized importance score rationale          |
| `docs/getting-started/core-concepts.md`  | Add pinned facts and future memory concepts                |

---

## Dependency Order

```
Task 1: Schema migration v2→v3
  ↓
Task 2: Type changes (Fact, NewFact, Event, NewEvent, AddFactOptions)
  ↓
Task 3: Store-level reads/writes (FactStore, EventStore)
  ↓
Task 4: PersistenceClassifier trait
  ↓
Task 5: Forgetting — pinned bypass + importance_score materialization
  ↓
Task 6: Consolidation — importance_score update
  ↓
Task 7: resume_context() rework (5-tier pipeline)
  ↓
Task 8: Scheduling API (list_due, next_due_time)
  ↓
Task 9: Engine facade wiring
  ↓
Task 10: AsyncMemoryEngine mirror
  ↓
Task 11: Documentation updates
  ↓
Task 12: Integration tests
```

---

## Chunk 1: Schema & Types

### Task 1: Schema Migration v2→v3

**Files:**

- Modify: `src/store/schema.rs`

This migration adds 5 columns across 2 tables:

- `facts.is_pinned` — `INTEGER NOT NULL DEFAULT 0` (SQLite bool)
- `facts.importance_score` — `REAL NOT NULL DEFAULT 0.5`
- `events.origin_node_id` — `TEXT NOT NULL DEFAULT 'local'`
- `events.sequence_id` — `INTEGER NOT NULL DEFAULT 0`
- `events.created_at` — `TEXT` (nullable, advisory)

Plus 4 indexes: partial on `is_pinned`, on `importance_score`, partial on `t_valid` (for scheduling queries), and composite on `origin_node_id + sequence_id`.

- [ ] **Step 1: Write migration test**

Add `TABLES_V2_DDL` and `INDEXES_V2_DDL` test helpers (copies of current `TABLES_DDL`/`INDEXES_DDL` without new columns) and an `init_schema_v2()` helper, following the existing `init_schema_v1()` pattern.

**Critical:** Do NOT use `init_schema()` for migration tests — once `CURRENT_SCHEMA_VERSION` is bumped to 3, `init_schema()` creates v3. Use a dedicated v2 fixture, same pattern as the existing `init_schema_v1()`.

```rust
/// Test helper: creates v2 schema (Phase 3 tables with scopes, no pinned/envelope).
fn init_schema_v2(conn: &Connection) -> Result<()> {
    conn.execute_batch(TABLES_V2_DDL)?;
    conn.execute_batch(SCOPES_DDL)?;
    conn.execute_batch(FTS5_DDL)?;
    conn.execute_batch(TRIGGERS_DDL)?;
    conn.execute_batch(INDEXES_V2_DDL)?;
    set_config(conn, "schema_version", "2")?;
    Ok(())
}

#[test]
fn migrate_v2_to_v3_adds_pinned_and_envelope() {
    let conn = open_memory().unwrap();
    init_schema_v2(&conn).unwrap();

    // Insert a fact before migration
    conn.execute(
        "INSERT INTO facts (content, content_hash, embedding, fact_type, t_created, importance, access_count, last_accessed, metadata, scope_id)
         VALUES ('test', 'hash', X'00', 'episodic', datetime('now'), 0.5, 0, datetime('now'), '{}', 1)",
        [],
    ).unwrap();

    // Insert an event before migration
    conn.execute(
        "INSERT INTO events (timestamp, event_type, payload, source, scope_id)
         VALUES (datetime('now'), 'Interaction', '{}', 'test', 1)",
        [],
    ).unwrap();

    migrate(&conn).unwrap();

    // Verify new columns with defaults
    let (is_pinned, importance_score): (i64, f64) = conn
        .query_row(
            "SELECT is_pinned, importance_score FROM facts WHERE content = 'test'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(is_pinned, 0);
    assert!((importance_score - 0.5).abs() < f64::EPSILON);

    // Verify event envelope fields
    let (origin, seq_id): (String, i64) = conn
        .query_row(
            "SELECT origin_node_id, sequence_id FROM events WHERE source = 'test'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(origin, "local");
    assert_eq!(seq_id, 0);

    assert_eq!(
        get_config(&conn, "schema_version").unwrap(),
        Some("3".to_string())
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib store::schema::tests::migrate_v2_to_v3 -- --nocapture`
Expected: FAIL — `migrate_v2_to_v3` function doesn't exist yet

- [ ] **Step 3: Implement migration**

In `schema.rs`:

1. Bump `CURRENT_SCHEMA_VERSION` to `3`.
2. Add `migrate_v2_to_v3` function.
3. Append to `MIGRATIONS` array.
4. Update `TABLES_DDL` to include new columns for fresh installs.
5. Add new indexes to `INDEXES_DDL`.
6. Add `TABLES_V2_DDL` test helper (copy of current `TABLES_DDL` without new columns).

```rust
const CURRENT_SCHEMA_VERSION: u32 = 3;

const MIGRATIONS: &[MigrationFn] = &[migrate_v1_to_v2, migrate_v2_to_v3];

fn migrate_v2_to_v3(conn: &Connection) -> Result<()> {
    // facts: pinned flag + materialized importance score
    conn.execute_batch(
        "ALTER TABLE facts ADD COLUMN is_pinned INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE facts ADD COLUMN importance_score REAL NOT NULL DEFAULT 0.5;",
    )?;
    // events: forward-compat envelope for future sync
    conn.execute_batch(
        "ALTER TABLE events ADD COLUMN origin_node_id TEXT NOT NULL DEFAULT 'local';
         ALTER TABLE events ADD COLUMN sequence_id INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE events ADD COLUMN created_at TEXT;",
    )?;
    // indexes
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_facts_pinned ON facts(is_pinned) WHERE is_pinned = 1;
         CREATE INDEX IF NOT EXISTS idx_facts_importance_score ON facts(importance_score);
         CREATE INDEX IF NOT EXISTS idx_facts_t_valid_due ON facts(t_valid) WHERE t_valid IS NOT NULL AND t_expired IS NULL;
         CREATE INDEX IF NOT EXISTS idx_events_origin_seq ON events(origin_node_id, sequence_id);",
    )?;
    Ok(())
}
```

Update `TABLES_DDL` — add to the `facts` CREATE TABLE:

```sql
    is_pinned INTEGER NOT NULL DEFAULT 0,
    importance_score REAL NOT NULL DEFAULT 0.5,
```

Add to the `events` CREATE TABLE:

```sql
    origin_node_id TEXT NOT NULL DEFAULT 'local',
    sequence_id INTEGER NOT NULL DEFAULT 0,
    created_at TEXT
```

Update `INDEXES_DDL` — append:

```sql
CREATE INDEX IF NOT EXISTS idx_facts_pinned ON facts(is_pinned) WHERE is_pinned = 1;
CREATE INDEX IF NOT EXISTS idx_facts_importance_score ON facts(importance_score);
CREATE INDEX IF NOT EXISTS idx_facts_t_valid_due ON facts(t_valid) WHERE t_valid IS NOT NULL AND t_expired IS NULL;
CREATE INDEX IF NOT EXISTS idx_events_origin_seq ON events(origin_node_id, sequence_id);
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib store::schema -- --nocapture`
Expected: ALL schema tests pass (including existing v1→v2 migration tests)

- [ ] **Step 5: Write fresh-install test for v3 schema**

```rust
#[test]
fn fresh_db_creates_v3_schema() {
    let conn = open_memory().unwrap();
    init_schema(&conn).unwrap();
    assert_eq!(
        get_config(&conn, "schema_version").unwrap(),
        Some("3".to_string())
    );
    // Verify is_pinned column exists
    conn.execute(
        "INSERT INTO facts (content, content_hash, embedding, fact_type, t_created, last_accessed, is_pinned, importance_score)
         VALUES ('test', 'h', X'00', 'episodic', datetime('now'), datetime('now'), 1, 0.9)",
        [],
    ).unwrap();
    let pinned: i64 = conn
        .query_row("SELECT is_pinned FROM facts WHERE content = 'test'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(pinned, 1);
}
```

- [ ] **Step 6: Run all schema tests**

Run: `cargo test --lib store::schema -- --nocapture`
Expected: ALL pass

- [ ] **Step 7: Update index count test**

The `all_nine_indexes_created` test counts indexes. Update the expected count: currently 15, adding 4 new (pinned, importance_score, t_valid_due, origin_seq) = 19.

- [ ] **Step 8: Commit**

```
feat(schema): add migration v2→v3 for pinned facts and event envelope

Adds is_pinned, importance_score to facts table.
Adds origin_node_id, sequence_id, created_at to events table.
Partial indexes on is_pinned (filtered) and importance_score.
```

---

### Task 2: Type Changes

**Files:**

- Modify: `src/types.rs`

Add new fields to `Fact`, `NewFact`, `Event`, `NewEvent`, and `AddFactOptions`.

- [ ] **Step 1: Update `Fact` struct**

Add after `scope_id`:

```rust
pub is_pinned: bool,
pub importance_score: f64,
```

- [ ] **Step 2: Update `NewFact` struct**

Add after `scope_id`:

```rust
pub is_pinned: bool,
```

**Do NOT add `importance_score` to `NewFact`.** It is a materialized/derived value computed by the engine during `prune()` and `consolidate()`. Exposing it on the input type would let callers inject arbitrary scores and break the invariant. The `INSERT` in `FactStore::insert()` should use `DEFAULT 0.5` for `importance_score` (the column default), and the engine updates it later via `update_importance_score()`.

- [ ] **Step 3: Update `Event` struct**

Add after `scope_id`:

```rust
/// Node that originated this event (for future multi-node sync).
pub origin_node_id: String,
/// Monotonic sequence within the origin node (for ordering/dedup in sync).
pub sequence_id: i64,
/// When the event was ingested into this node's store (ingest-time).
/// Distinct from `timestamp` which is the event's logical time (event-time).
/// `timestamp` = "when did this happen?" / `created_at` = "when did we record it?"
pub created_at: Option<DateTime<Utc>>,
```

- [ ] **Step 4: Update `NewEvent` struct**

Add after `scope_id`:

```rust
pub origin_node_id: String,
pub sequence_id: i64,
pub created_at: Option<DateTime<Utc>>,
```

- [ ] **Step 5: Update `AddFactOptions`**

Add:

```rust
/// Pin this fact (unforgettable). Overrides auto-classification.
pub pinned: Option<bool>,
```

- [ ] **Step 6: Fix all compiler errors**

Every place that constructs `Fact`, `NewFact`, `Event`, or `NewEvent` needs the new fields. This is intentional — the compiler finds every callsite. Fix them all:

- `src/store/facts.rs` — `row_to_fact()` deserialize, `insert()` serialize
- `src/store/events.rs` — `row_to_event()` deserialize, `insert()` serialize
- `src/engine.rs` — `add_fact()` constructs `NewFact`
- `src/async_engine.rs` — passes through to engine
- `src/resume/context.rs` — test helpers
- `src/forgetting/policy.rs` — test `Fact` construction
- `src/conflict/temporal.rs` — test `Fact` construction
- `src/consolidation/*.rs` — may construct facts
- All test modules that construct these types

For `NewFact` in `engine.rs::add_fact()`:

```rust
is_pinned: opts.pinned.unwrap_or(false),
// importance_score is NOT set here — it's engine-computed via prune()/consolidate()
// The DB column default (0.5) applies on INSERT
```

For `NewEvent` default construction, use:

```rust
origin_node_id: "local".into(),
sequence_id: 0,
created_at: None,
```

- [ ] **Step 7: Run full test suite**

Run: `cargo test`
Expected: ALL pass (type changes are purely additive, defaults preserve behavior)

- [ ] **Step 8: Commit**

```
feat(types): add is_pinned, importance_score to Fact and event envelope fields

Extends Fact with is_pinned (bool) and importance_score (f64).
Extends NewFact with is_pinned (bool) only — importance_score is engine-computed.
Extends Event/NewEvent with origin_node_id, sequence_id, created_at.
Adds pinned option to AddFactOptions.
All existing tests updated with default values.
```

---

## Chunk 2: Store Layer & Trait

### Task 3: FactStore and EventStore Updates

**Files:**

- Modify: `src/store/facts.rs`
- Modify: `src/store/events.rs`

#### FactStore new methods

- [ ] **Step 1: Write test for `list_pinned()`**

```rust
#[test]
fn list_pinned_returns_only_pinned_active_facts() {
    let conn = setup();
    let fs = FactStore::new(&conn, DIM);
    let mut pinned = make_fact("pinned fact", 0.5);
    pinned.is_pinned = true;
    fs.insert(&pinned).unwrap();
    fs.insert(&make_fact("normal fact", 0.5)).unwrap();

    let result = fs.list_pinned(&[]).unwrap(); // empty = all scopes
    assert_eq!(result.len(), 1);
    assert!(result[0].is_pinned);
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test --lib store::facts::tests::list_pinned -- --nocapture`

- [ ] **Step 3: Implement `list_pinned()`**

```rust
/// List active pinned (unforgettable) facts, optionally filtered by scope.
///
/// Scope handling: pinned facts are cross-scope by default (identity facts
/// matter regardless of scope). Pass `scope_ids` to restrict if needed,
/// or pass an empty slice to get all pinned facts across all scopes.
pub fn list_pinned(&self, scope_ids: &[i64]) -> Result<Vec<Fact>> {
    if scope_ids.is_empty() {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM facts WHERE t_expired IS NULL AND is_pinned = 1
             ORDER BY importance_score DESC"
        )?;
        let rows = stmt.query_map([], |row| Ok(self.row_to_fact(row)))?;
        rows.map(|r| r.map_err(Into::into).and_then(|f| f)).collect()
    } else {
        let placeholders: String = scope_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT * FROM facts WHERE t_expired IS NULL AND is_pinned = 1
             AND scope_id IN ({placeholders})
             ORDER BY importance_score DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<Box<dyn rusqlite::types::ToSql>> =
            scope_ids.iter().map(|id| Box::new(*id) as _).collect();
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
            Ok(self.row_to_fact(row))
        })?;
        rows.map(|r| r.map_err(Into::into).and_then(|f| f)).collect()
    }
}
```

- [ ] **Step 4: Write test for `list_due()`**

```rust
#[test]
fn list_due_surfaces_facts_with_past_t_valid() {
    let conn = setup();
    let fs = FactStore::new(&conn, DIM);
    let now = Utc::now();

    // Fact with t_valid in the past (should surface)
    let mut past = make_fact("past reminder", 0.5);
    past.t_valid = Some(now - Duration::hours(1));
    fs.insert(&past).unwrap();

    // Fact with t_valid in the future (should NOT surface)
    let mut future = make_fact("future reminder", 0.5);
    future.t_valid = Some(now + Duration::hours(1));
    fs.insert(&future).unwrap();

    // Fact with no t_valid (should NOT surface — not a scheduled fact)
    fs.insert(&make_fact("regular fact", 0.5)).unwrap();

    let result = fs.list_due(now, &[]).unwrap(); // empty = all scopes
    assert_eq!(result.len(), 1);
    assert!(result[0].content.contains("past"));
}
```

- [ ] **Step 5: Implement `list_due()`**

```rust
/// List active, valid facts where `t_valid <= now` and `t_valid IS NOT NULL`.
/// These are "future memory" facts whose scheduled time has arrived.
/// Excludes facts where `t_invalid <= now` (bi-temporally invalidated).
/// Pass `scope_ids` to filter by scope, or empty slice for all scopes.
pub fn list_due(&self, now: DateTime<Utc>, scope_ids: &[i64]) -> Result<Vec<Fact>> {
    let now_str = now.to_rfc3339();
    let base = "SELECT * FROM facts
         WHERE t_expired IS NULL AND t_valid IS NOT NULL AND t_valid <= ?1
         AND (t_invalid IS NULL OR t_invalid > ?1)";
    let sql = if scope_ids.is_empty() {
        format!("{base} ORDER BY t_valid ASC")
    } else {
        let placeholders: String = scope_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        format!("{base} AND scope_id IN ({placeholders}) ORDER BY t_valid ASC")
    };
    let mut stmt = self.conn.prepare(&sql)?;
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> =
        vec![Box::new(now_str)];
    for id in scope_ids {
        params.push(Box::new(*id));
    }
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
        Ok(self.row_to_fact(row))
    })?;
    rows.map(|r| r.map_err(Into::into).and_then(|f| f)).collect()
}
```

- [ ] **Step 6: Write test for `next_due_time()`**

```rust
#[test]
fn next_due_time_returns_earliest_future_t_valid() {
    let conn = setup();
    let fs = FactStore::new(&conn, DIM);
    let now = Utc::now();

    // No future facts → None
    assert!(fs.next_due_time(now, &[]).unwrap().is_none());

    // Add future fact
    let mut future = make_fact("reminder", 0.5);
    future.t_valid = Some(now + Duration::hours(2));
    fs.insert(&future).unwrap();

    let mut sooner = make_fact("sooner reminder", 0.5);
    sooner.t_valid = Some(now + Duration::hours(1));
    fs.insert(&sooner).unwrap();

    let next = fs.next_due_time(now, &[]).unwrap().unwrap();
    // Should be the sooner one
    assert!(next < now + Duration::hours(2));
}
```

- [ ] **Step 7: Implement `next_due_time()`**

```rust
/// Earliest future `t_valid` among active facts with `t_valid > now`.
/// Returns `None` if no future-dated facts exist.
/// This answers: "when should I next call `list_due()`?"
pub fn next_due_time(&self, now: DateTime<Utc>, scope_ids: &[i64]) -> Result<Option<DateTime<Utc>>> {
    let now_str = now.to_rfc3339();
    let base = "SELECT MIN(t_valid) FROM facts
         WHERE t_expired IS NULL AND t_valid IS NOT NULL AND t_valid > ?1
         AND (t_invalid IS NULL OR t_invalid > ?1)";
    let sql = if scope_ids.is_empty() {
        base.to_string()
    } else {
        let placeholders: String = scope_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        format!("{base} AND scope_id IN ({placeholders})")
    };
    let mut stmt = self.conn.prepare(&sql)?;
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> =
        vec![Box::new(now_str)];
    for id in scope_ids {
        params.push(Box::new(*id));
    }
    let result: Option<String> = stmt.query_row(
        rusqlite::params_from_iter(params), |r| r.get(0)
    )?;
    match result {
        Some(s) => {
            let dt = DateTime::parse_from_rfc3339(&s)
                .map_err(|e| crate::error::MemoryError::Migration(format!("bad t_valid: {e}")))?
                .with_timezone(&Utc);
            Ok(Some(dt))
        }
        None => Ok(None),
    }
}
```

- [ ] **Step 8: Write test for `set_pinned()`**

```rust
#[test]
fn set_pinned_toggles_flag() {
    let conn = setup();
    let fs = FactStore::new(&conn, DIM);
    let id = fs.insert(&make_fact("toggleable", 0.5)).unwrap();

    let fact = fs.get(id).unwrap();
    assert!(!fact.is_pinned);

    fs.set_pinned(id, true).unwrap();
    let fact = fs.get(id).unwrap();
    assert!(fact.is_pinned);

    fs.set_pinned(id, false).unwrap();
    let fact = fs.get(id).unwrap();
    assert!(!fact.is_pinned);
}
```

- [ ] **Step 9: Implement `set_pinned()` and `update_importance_score()`**

```rust
/// Set or clear the pinned (unforgettable) flag on a fact.
pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<()> {
    let rows = self.conn.execute(
        "UPDATE facts SET is_pinned = ?1 WHERE id = ?2",
        rusqlite::params![pinned as i64, id],
    )?;
    if rows == 0 {
        return Err(crate::error::MemoryError::NotFound(format!("fact {id}")));
    }
    Ok(())
}

/// Update the materialized importance score for a fact.
pub fn update_importance_score(&self, id: i64, score: f64) -> Result<()> {
    self.conn.execute(
        "UPDATE facts SET importance_score = ?1 WHERE id = ?2",
        rusqlite::params![score, id],
    )?;
    Ok(())
}
```

- [ ] **Step 10: Update `row_to_fact()` and `insert()` for new columns**

In `row_to_fact()`, add reads for `is_pinned` (as i64, convert to bool) and `importance_score`.
In `insert()`, add the new columns to the INSERT statement.

- [ ] **Step 11: Update EventStore for envelope fields**

In `insert()`, add `origin_node_id`, `sequence_id`, `created_at` to the INSERT.
In `row_to_event()`, read the new columns.

- [ ] **Step 12: Run full test suite**

Run: `cargo test`
Expected: ALL pass

- [ ] **Step 13: Commit**

```
feat(store): add pinned queries, due-time queries, and event envelope fields

FactStore: list_pinned(), list_due(now), next_due_time(now),
set_pinned(), update_importance_score().
EventStore: origin_node_id, sequence_id, created_at in insert/read.
```

---

### Task 4: PersistenceClassifier Trait

**Files:**

- Modify: `src/traits.rs`

- [ ] **Step 1: Add the trait**

```rust
/// Trait for classifying whether a fact should be pinned (unforgettable).
///
/// Consumers implement this to apply domain-specific rules:
/// LLM-based classification, regex matching, importance thresholds, etc.
///
/// Default implementation returns `false` — opt-in, zero behavior change.
pub trait PersistenceClassifier {
    /// Decide if a fact should be pinned (never forgotten).
    fn should_pin(&self, fact: &Fact) -> bool {
        let _ = fact; // suppress unused warning in default impl
        false
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test`
Expected: ALL pass (trait is additive, default impl means no breakage)

- [ ] **Step 3: Commit**

```
feat(traits): add PersistenceClassifier trait for unforgettable fact classification
```

---

## Chunk 3: Forgetting & Consolidation

### Task 5: Forgetting — Pinned Bypass + Importance Materialization

**Files:**

- Modify: `src/forgetting/policy.rs`

Two changes:

1. `prune()` skips facts where `is_pinned = true`
2. After computing importance for all active facts, update `importance_score` column

- [ ] **Step 1: Write test for pinned bypass**

```rust
#[test]
fn prune_skips_pinned_facts() {
    let conn = open_memory().unwrap();
    init_schema(&conn).unwrap();

    let now = Utc::now();
    let old_time = now - Duration::days(200);
    let embed_dim = 4;
    let fact_store = FactStore::new(&conn, embed_dim);

    // Pinned fact with low importance and old age — would normally be pruned
    fact_store.insert(&NewFact {
        content: "pinned identity".into(),
        content_hash: "hp".into(),
        embedding: vec![0.1; embed_dim],
        fact_type: FactType::Semantic,
        t_created: old_time,
        t_expired: None,
        t_valid: None,
        t_invalid: None,
        source_event_id: None,
        scope_id: 1,
        importance: 0.01,
        access_count: 0,
        last_accessed: old_time,
        metadata: serde_json::json!({}),
        is_pinned: true,
    }).unwrap();

    // Unpinned fact with same characteristics — should be pruned
    fact_store.insert(&NewFact {
        content: "forgettable".into(),
        content_hash: "hf".into(),
        embedding: vec![0.2; embed_dim],
        fact_type: FactType::Episodic,
        t_created: old_time,
        t_expired: None,
        t_valid: None,
        t_invalid: None,
        source_event_id: None,
        scope_id: 1,
        importance: 0.01,
        access_count: 0,
        last_accessed: old_time,
        metadata: serde_json::json!({}),
        is_pinned: false,
    }).unwrap();

    let mut graph = MemoryGraph::new();
    let policy = ForgetPolicy { min_importance: 0.3, ..ForgetPolicy::default() };
    let stats = prune(&conn, &mut graph, &policy, embed_dim, now).unwrap();

    assert_eq!(stats.facts_expired, 1); // only unpinned
    assert_eq!(stats.facts_evaluated, 2);

    // Pinned fact still active
    let active = fact_store.list_active().unwrap();
    assert_eq!(active.len(), 1);
    assert!(active[0].is_pinned);
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test --lib forgetting::policy::tests::prune_skips_pinned -- --nocapture`

- [ ] **Step 3: Implement pinned bypass in `prune()`**

Modify the filter in `prune()`:

```rust
let to_expire: Vec<i64> = active_facts
    .iter()
    .filter(|fact| {
        if fact.is_pinned {
            return false; // unforgettable — never decay
        }
        let degree = graph.degree(fact.id);
        compute_importance(fact, degree, now, policy) < policy.min_importance
    })
    .map(|f| f.id)
    .collect();
```

- [ ] **Step 4: Write test for importance score materialization**

```rust
#[test]
fn prune_materializes_importance_scores() {
    let conn = open_memory().unwrap();
    init_schema(&conn).unwrap();

    let now = Utc::now();
    let embed_dim = 4;
    let fact_store = FactStore::new(&conn, embed_dim);

    fact_store.insert(&NewFact {
        content: "scored fact".into(),
        content_hash: "hs".into(),
        embedding: vec![0.1; embed_dim],
        fact_type: FactType::Semantic,
        t_created: now,
        t_expired: None,
        t_valid: None,
        t_invalid: None,
        source_event_id: None,
        scope_id: 1,
        importance: 0.8,
        access_count: 10,
        last_accessed: now,
        metadata: serde_json::json!({}),
        is_pinned: false,
    }).unwrap();

    let mut graph = MemoryGraph::new();
    let policy = ForgetPolicy::default();
    prune(&conn, &mut graph, &policy, embed_dim, now).unwrap();

    // After prune, importance_score should be updated from default
    let fact = fact_store.get(1).unwrap();
    assert!(fact.importance_score != 0.5, "importance_score should have been updated");
}
```

- [ ] **Step 5: Implement importance materialization in `prune()`**

After the scoring loop, before the expire transaction, update all active facts' scores:

```rust
// Materialize importance scores for all active facts
let tx = conn.unchecked_transaction()?;
let fact_store = FactStore::new(&tx, embed_dim);

for fact in &active_facts {
    let degree = graph.degree(fact.id);
    let score = compute_importance(fact, degree, now, policy);
    fact_store.update_importance_score(fact.id, score)?;
}

// Then expire low-importance unpinned facts
for &fact_id in &to_expire {
    fact_store.expire(fact_id, now)?;
    EdgeStore::new(&tx).expire_by_fact(fact_id, now)?;
}

tx.commit()?;
```

Note: This replaces the current separate `tx` block. Materialize + expire in one transaction.

- [ ] **Step 6: Run all forgetting tests**

Run: `cargo test --lib forgetting -- --nocapture`
Expected: ALL pass

- [ ] **Step 7: Commit**

```
feat(forgetting): skip pinned facts and materialize importance scores

Pinned facts (is_pinned=true) bypass the forgetting pipeline entirely.
All active facts get their importance_score updated in the DB during prune().
```

---

### Task 6: Consolidation — Pinned Protection + Importance Score Update

**Files:**

- Modify: `src/consolidation/mod.rs` (or `dedup.rs` depending on where surviving facts are handled)

Two changes:

1. **Pinned facts are excluded from dedup candidates.** "Unforgettable" means no semantic mutation path can alter or merge them. They can still be explicitly updated via `pin_fact()`/`unpin_fact()`, but automated consolidation skips them.
2. After dedup merges unpinned facts, the surviving fact's `importance_score` is refreshed.

- [ ] **Step 1: Write test for pinned dedup bypass**

```rust
#[test]
fn consolidation_skips_pinned_facts() {
    // Two near-duplicate facts, one pinned. Dedup should NOT merge them.
    // The pinned fact must survive unchanged.
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };

    let opts_pin = AddFactOptions { pinned: Some(true), ..Default::default() };
    let pin_id = engine.add_fact("the sky is blue", FactType::Semantic, None, &embedder, None, Some(&opts_pin), None).unwrap();
    let dup_id = engine.add_fact("the sky is blue", FactType::Semantic, None, &embedder, None, None, None).unwrap();

    let generator = MockGenerator;
    engine.consolidate(&generator, &ConsolidationConfig::default()).unwrap();

    // Both facts should still exist — pinned fact prevented merge
    let pinned = engine.get_fact(pin_id).unwrap();
    assert!(pinned.is_pinned);
    assert!(pinned.t_expired.is_none());
}
```

- [ ] **Step 2: Implement pinned bypass in dedup**

In the dedup pass, filter out pinned facts from candidate sets:

```rust
// Pinned facts are never dedup candidates — "unforgettable" means no automated mutation
let candidates: Vec<&Fact> = cluster.iter()
    .filter(|f| !f.is_pinned)
    .collect();
if candidates.len() < 2 {
    continue; // nothing to dedup (pinned facts excluded, or only 1 unpinned left)
}
```

- [ ] **Step 3: Write test for importance_score update on survivor**

Test that after consolidation deduplicates unpinned facts, the surviving fact's `importance_score` is updated (it inherits the max of the merged set).

- [ ] **Step 4: Implement importance_score update**

In the dedup pass, after selecting the survivor and expiring duplicates:

1. Update the survivor's base `importance` to `max(merged_facts.importance)` (inherits best hint).
2. Recompute `importance_score` using the composite formula (matching `prune()`), NOT just the raw hint.

```rust
// Inherit max base importance from merged set
let max_importance = merged_facts.iter()
    .map(|f| f.importance)
    .fold(0.0_f64, f64::max);
fact_store.update_importance(survivor_id, max_importance)?;

// Recompute composite importance_score (same formula as prune())
let survivor = fact_store.get(survivor_id)?;
let degree = graph.degree(survivor_id);
let score = compute_importance(&survivor, degree, now, policy);
fact_store.update_importance_score(survivor_id, score)?;
```

**Critical:** `importance_score` is always a composite value. Setting it to `max(importance)` would break the invariant defined in ADR-0008.

- [ ] **Step 5: Run tests**

Run: `cargo test --lib consolidation -- --nocapture`
Expected: ALL pass

- [ ] **Step 6: Commit**

```
feat(consolidation): skip pinned facts in dedup, update importance_score on survivor

Pinned facts are excluded from dedup candidates — "unforgettable" means
no automated semantic mutation. Surviving unpinned facts get their
importance_score updated to the max of the merged set.
```

---

## Chunk 4: Resume Context Rework

### Task 7: Rework `resume_context()` to 5-Tier Pipeline

**Files:**

- Modify: `src/resume/context.rs`

The current 3-tier (identity/core/recent) becomes 5-tier:

1. **Pinned** — all pinned facts (always present, no cap by default but configurable)
2. **High-importance** — top-N by `importance_score` (materialized, no recomputation)
3. **Due** — active facts with `t_valid <= now` (non-destructive; repeated calls return the same facts until they expire or `t_valid` changes)
4. **Scope-filtered recent** — newest by `t_created` from active scope
5. **KB stubs** — placeholder `Vec<String>` for Phase 5 `KnowledgeRef` URIs

- [ ] **Step 1: Redesign `ResumeConfig`**

```rust
#[derive(Debug, Clone)]
pub struct ResumeConfig {
    /// Scope path to resume from. None = root only.
    pub scope_path: Option<String>,
    /// Current time for due-fact evaluation.
    pub now: DateTime<Utc>,
    /// Max pinned facts. Default: 50 (generous — pinned facts are identity).
    pub pinned_cap: usize,
    /// Max high-importance facts (by materialized score). Default: 20.
    pub high_importance_cap: usize,
    /// Minimum importance_score for high-importance tier. Default: 0.7.
    pub high_importance_min: f64,
    /// Max due facts (future memory now surfacing). Default: 10.
    pub due_cap: usize,
    /// Max recent facts (scope-filtered). Default: 10.
    pub recent_cap: usize,
}

impl Default for ResumeConfig {
    fn default() -> Self {
        Self {
            scope_path: None,
            now: Utc::now(),
            pinned_cap: 50,
            high_importance_cap: 20,
            high_importance_min: 0.7,
            due_cap: 10,
            recent_cap: 10,
        }
    }
}
```

- [ ] **Step 2: Redesign `ResumeContext`**

```rust
#[derive(Debug, Clone)]
pub struct ResumeContext {
    /// Pinned (unforgettable) facts — agent identity, core beliefs.
    pub pinned: Vec<Fact>,
    /// High-importance facts by materialized score.
    pub high_importance: Vec<Fact>,
    /// Future-memory facts whose t_valid has arrived.
    pub due: Vec<Fact>,
    /// Most recent facts from active scope.
    pub recent: Vec<Fact>,
    /// Placeholder: KB reference URIs for Phase 5.
    pub kb_stubs: Vec<String>,
}
```

- [ ] **Step 3: Implement 5-tier pipeline**

```rust
/// Internal function — the engine resolves `ResumeConfig.scope_path` → `(root_id, scope_ids)`
/// before calling this. `root_id` is the root scope ID, `scope_ids` includes root + descendant scopes.
pub fn resume_context(
    conn: &Connection,
    root_id: i64,
    scope_ids: &[i64],
    embed_dim: usize,
    config: &ResumeConfig,
) -> Result<ResumeContext> {
    let fact_store = FactStore::new(conn, embed_dim);
    let mut seen: HashSet<i64> = HashSet::new();

    // Tier 1: Pinned facts (always present, cross-scope — identity matters everywhere)
    let pinned_all = fact_store.list_pinned(&[])?; // empty = all scopes
    let pinned: Vec<Fact> = pinned_all.into_iter()
        .take(config.pinned_cap)
        .collect();
    seen.extend(pinned.iter().map(|f| f.id));

    // Tier 2: High-importance by materialized score
    let high_importance = fact_store.list_by_importance_score(
        scope_ids,
        config.high_importance_min,
        config.high_importance_cap,
        &seen,
    )?;
    seen.extend(high_importance.iter().map(|f| f.id));

    // Tier 3: Due facts (future memory now surfacing, scope-filtered)
    let due_all = fact_store.list_due(config.now, scope_ids)?;
    let due: Vec<Fact> = due_all.into_iter()
        .filter(|f| !seen.contains(&f.id))
        .take(config.due_cap)
        .collect();
    seen.extend(due.iter().map(|f| f.id));

    // Tier 4: Scope-filtered recent
    let recent = fact_store.list_by_scopes_recent(
        scope_ids,
        config.recent_cap,
        &seen,
    )?;

    // Tier 5: KB stubs (Phase 5 placeholder)
    let kb_stubs = Vec::new();

    Ok(ResumeContext {
        pinned,
        high_importance,
        due,
        recent,
        kb_stubs,
    })
}
```

- [ ] **Step 4: Add `list_by_importance_score()` to FactStore**

```rust
/// List active facts ordered by materialized importance_score, excluding IDs in `exclude`.
/// Pass empty `scope_ids` to query across all scopes.
pub fn list_by_importance_score(
    &self,
    scope_ids: &[i64],
    min_score: f64,
    limit: usize,
    exclude: &HashSet<i64>,
) -> Result<Vec<Fact>> {
    let base = "SELECT * FROM facts
         WHERE t_expired IS NULL AND importance_score >= ?1";
    let sql = if scope_ids.is_empty() {
        format!("{base} ORDER BY importance_score DESC LIMIT ?2")
    } else {
        let placeholders: String = scope_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        format!("{base} AND scope_id IN ({placeholders})
                 ORDER BY importance_score DESC LIMIT ?")
    };
    // Bind min_score, scope_ids (if any), limit. Filter exclude set post-query.
    // ... (follows same pattern as list_due/list_pinned)
}
```

- [ ] **Step 5: Rewrite all resume_context tests**

Update tests to use new `ResumeConfig` fields (`now`, `pinned_cap`, etc.) and new `ResumeContext` tiers (`pinned`, `high_importance`, `due`, `recent`).

Key tests:

- `resume_empty_db` — all tiers empty
- `resume_pinned_always_present` — pinned facts appear regardless of scope/importance
- `resume_due_surfaces_at_time` — future fact surfaces when `now >= t_valid`
- `resume_tiers_mutually_exclusive` — no fact in multiple tiers
- `resume_high_importance_by_score` — uses `importance_score`, not `importance`
- `resume_scope_filtering` — recent tier respects scope

- [ ] **Step 6: Run tests**

Run: `cargo test --lib resume -- --nocapture`
Expected: ALL pass

- [ ] **Step 7: Commit**

```
feat(resume): rework resume_context() to 5-tier cognitive boot sequence

Tiers: pinned → high_importance → due → recent → kb_stubs.
Pinned facts are always present (agent identity).
Due facts surface when t_valid <= now (future memory).
High-importance uses materialized importance_score (no recomputation).
KB stubs placeholder for Phase 5 knowledge integration.

BREAKING: ResumeConfig and ResumeContext fields changed.
```

---

## Chunk 5: Scheduling API & Engine Wiring

### Task 8: Engine Facade — New Public Methods

**Files:**

- Modify: `src/engine.rs`

- [ ] **Step 1: Add `PersistenceClassifier` to `add_fact()` signature**

The `add_fact` method gains an optional `classifier` parameter:

```rust
pub fn add_fact(
    &self,
    content: &str,
    fact_type: FactType,
    source_event_id: Option<i64>,
    embedder: &dyn EmbeddingProvider,
    scope: Option<&str>,
    opts: Option<&AddFactOptions>,
    classifier: Option<&dyn PersistenceClassifier>,
) -> Result<i64> {
```

After constructing `new_fact`, before insert:

```rust
// Precedence rule: explicit opts.pinned ALWAYS wins (even Some(false)).
// Classifier only runs when opts.pinned is None (unspecified).
let is_pinned = match opts.and_then(|o| o.pinned) {
    Some(explicit) => explicit, // caller said yes or no — honor it
    None => classifier.map_or(false, |c| {
        // Build a temporary Fact for classification (id=0, no DB yet)
        let temp = Fact { id: 0, /* ... fields from new_fact ... */ };
        c.should_pin(&temp)
    }),
};
```

**Precedence is explicit and tested:** `Some(true)` → pinned. `Some(false)` → not pinned, even if classifier would say yes. `None` → classifier decides (default: not pinned).

**Classifier input caveat:** The `Fact` passed to `should_pin()` is a pre-insert synthetic with `id=0`, `importance_score=0.5` (default), and no graph connectivity. Classifiers should only rely on `content`, `fact_type`, `importance` (caller hint), and `metadata` — not on `id`, `importance_score`, or `access_count`. Document this constraint in the `PersistenceClassifier` trait rustdoc.

**Important:** This is a breaking API change. All callers of `add_fact` need updating. Given the existing pattern of optional params, `Option<&dyn PersistenceClassifier>` is consistent.

- [ ] **Step 2: Add `list_due()` method**

```rust
/// List facts whose scheduled time has arrived.
/// Returns active facts where `t_valid <= now` and `t_valid IS NOT NULL`.
///
/// **Non-destructive**: repeated calls with the same `now` return the same results.
/// This is the incremental counterpart to `resume_context()` — use it in agent
/// loops to check for newly-surfaced future memory without a full boot sequence.
pub fn list_due(&self, now: DateTime<Utc>, scope: Option<&str>) -> Result<Vec<Fact>> {
    let scope_ids = self.resolve_scope_ids(scope)?;
    self.with_read(|conn| {
        FactStore::new(conn, self.embed_dim).list_due(now, &scope_ids)
    })
}
```

- [ ] **Step 3: Add `next_due_time()` method**

```rust
/// Scheduling hint: when will the next future fact become due?
/// Returns the earliest `t_valid > now` among active, valid future-dated facts.
///
/// This does NOT report already-due facts — use `list_due(now)` for those.
/// Typical agent loop: call `list_due(now)` to process current items,
/// then `next_due_time()` to set a timer for the next check.
pub fn next_due_time(&self, scope: Option<&str>) -> Result<Option<DateTime<Utc>>> {
    let scope_ids = self.resolve_scope_ids(scope)?;
    self.with_read(|conn| {
        FactStore::new(conn, self.embed_dim).next_due_time(Utc::now(), &scope_ids)
    })
}
```

- [ ] **Step 4: Add `pin_fact()` and `unpin_fact()`**

```rust
/// Pin a fact (make it unforgettable).
pub fn pin_fact(&self, id: i64) -> Result<()> {
    let conn = self.write_conn();
    FactStore::new(&conn, self.embed_dim).set_pinned(id, true)
}

/// Unpin a fact (allow forgetting).
pub fn unpin_fact(&self, id: i64) -> Result<()> {
    let conn = self.write_conn();
    FactStore::new(&conn, self.embed_dim).set_pinned(id, false)
}
```

- [ ] **Step 5: Update `resume_context()` to pass `now`**

The engine method should pass `config.now` through (already in the new `ResumeConfig`).

- [ ] **Step 6: Write engine-level tests**

```rust
#[test]
fn list_due_returns_scheduled_facts() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    let past = Utc::now() - chrono::Duration::hours(1);
    let future = Utc::now() + chrono::Duration::hours(1);

    // Past-due fact
    engine.add_fact("check release", FactType::Semantic, None, &embedder, None,
        Some(&AddFactOptions { t_valid: Some(past), ..Default::default() }),
        None,
    ).unwrap();

    // Future fact
    engine.add_fact("future check", FactType::Semantic, None, &embedder, None,
        Some(&AddFactOptions { t_valid: Some(future), ..Default::default() }),
        None,
    ).unwrap();

    let due = engine.list_due(Utc::now(), None).unwrap();
    assert_eq!(due.len(), 1);
    assert!(due[0].content.contains("check release"));

    let next = engine.next_due_time(None).unwrap();
    assert!(next.is_some()); // the future fact
}

#[test]
fn pin_unpin_fact() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    let id = engine.add_fact("pinnable", FactType::Semantic, None, &embedder, None, None, None).unwrap();

    assert!(!engine.get_fact(id).unwrap().is_pinned);
    engine.pin_fact(id).unwrap();
    assert!(engine.get_fact(id).unwrap().is_pinned);
    engine.unpin_fact(id).unwrap();
    assert!(!engine.get_fact(id).unwrap().is_pinned);
}

#[test]
fn add_fact_with_explicit_pin() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    let opts = AddFactOptions { pinned: Some(true), ..Default::default() };
    let id = engine.add_fact("identity", FactType::Semantic, None, &embedder, None, Some(&opts), None).unwrap();
    assert!(engine.get_fact(id).unwrap().is_pinned);
}

#[test]
fn add_fact_with_classifier() {
    struct PinSemantic;
    impl PersistenceClassifier for PinSemantic {
        fn should_pin(&self, fact: &Fact) -> bool {
            fact.fact_type == FactType::Semantic
        }
    }

    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    let classifier = PinSemantic;

    let id = engine.add_fact("auto-pinned", FactType::Semantic, None, &embedder, None, None, Some(&classifier)).unwrap();
    assert!(engine.get_fact(id).unwrap().is_pinned);

    let id2 = engine.add_fact("not pinned", FactType::Episodic, None, &embedder, None, None, Some(&classifier)).unwrap();
    assert!(!engine.get_fact(id2).unwrap().is_pinned);
}

#[test]
fn explicit_pinned_false_overrides_classifier() {
    struct AlwaysPin;
    impl PersistenceClassifier for AlwaysPin {
        fn should_pin(&self, _fact: &Fact) -> bool { true }
    }

    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    let classifier = AlwaysPin;

    // Explicit Some(false) must win over classifier that returns true
    let opts = AddFactOptions { pinned: Some(false), ..Default::default() };
    let id = engine.add_fact("not pinned despite classifier", FactType::Semantic, None, &embedder, None, Some(&opts), Some(&classifier)).unwrap();
    assert!(!engine.get_fact(id).unwrap().is_pinned);
}
```

- [ ] **Step 7: Run all engine tests**

Run: `cargo test --lib engine -- --nocapture`
Expected: ALL pass

- [ ] **Step 8: Commit**

```
feat(engine): wire scheduling API, pinning, and PersistenceClassifier

New public methods: list_due(now), next_due_time(), pin_fact(), unpin_fact().
add_fact() accepts optional PersistenceClassifier for auto-pinning.
resume_context() uses reworked 5-tier pipeline.

BREAKING: add_fact() signature gains classifier parameter.
```

---

### Task 9: AsyncMemoryEngine Mirror

**Files:**

- Modify: `src/async_engine.rs`

- [ ] **Step 1: Add async wrappers for new methods**

```rust
pub async fn list_due(&self, now: DateTime<Utc>, scope: Option<String>) -> Result<Vec<Fact>> {
    let engine = self.inner.clone();
    tokio::task::spawn_blocking(move || engine.list_due(now, scope.as_deref()))
        .await
        .map_err(join_err)?
}

pub async fn next_due_time(&self, scope: Option<String>) -> Result<Option<DateTime<Utc>>> {
    let engine = self.inner.clone();
    tokio::task::spawn_blocking(move || engine.next_due_time(scope.as_deref()))
        .await
        .map_err(join_err)?
}

pub async fn pin_fact(&self, id: i64) -> Result<()> {
    let engine = self.inner.clone();
    tokio::task::spawn_blocking(move || engine.pin_fact(id))
        .await
        .map_err(join_err)?
}

pub async fn unpin_fact(&self, id: i64) -> Result<()> {
    let engine = self.inner.clone();
    tokio::task::spawn_blocking(move || engine.unpin_fact(id))
        .await
        .map_err(join_err)?
}
```

- [ ] **Step 2: Update `add_fact` signature to include classifier**

```rust
pub async fn add_fact(
    &self,
    content: String,
    fact_type: FactType,
    source_event_id: Option<i64>,
    embedder: Arc<dyn EmbeddingProvider + Send + Sync>,
    scope: Option<String>,
    opts: Option<AddFactOptions>,
    classifier: Option<Arc<dyn PersistenceClassifier + Send + Sync>>,
) -> Result<i64> {
```

- [ ] **Step 3: Write async test for list_due**

```rust
#[tokio::test]
async fn async_list_due() {
    let engine = AsyncMemoryEngine::open_memory(DIM).await.unwrap();
    let embedder: Arc<dyn EmbeddingProvider + Send + Sync> = Arc::new(MockEmbedder { dim: DIM });
    let past = Utc::now() - chrono::Duration::hours(1);
    let opts = AddFactOptions { t_valid: Some(past), ..Default::default() };
    engine.add_fact("reminder".into(), FactType::Semantic, None, embedder, None, Some(opts), None).await.unwrap();

    let due = engine.list_due(Utc::now(), None).await.unwrap();
    assert_eq!(due.len(), 1);
}
```

- [ ] **Step 4: Run async tests**

Run: `cargo test --lib async_engine -- --nocapture`
Expected: ALL pass

- [ ] **Step 5: Commit**

```
feat(async): mirror list_due, next_due_time, pin/unpin in AsyncMemoryEngine
```

---

## Chunk 6: Documentation & Integration Tests

### Task 10: Documentation Updates

**Files:**

- Modify: `docs/ROADMAP.md`
- Modify: `docs/reference/api.md`
- Modify: `docs/advanced/forgetting.md`
- Modify: `docs/advanced/bi-temporal-semantics.md`
- Modify: `docs/getting-started/core-concepts.md`
- Create: `docs/design/adr/0008-materialized-importance.md`

- [ ] **Step 1: Update ROADMAP.md**

Mark Phase 3b features as ✅. Add implementation learnings section.

- [ ] **Step 2: Update API reference**

Document: `list_due()`, `next_due_time()`, `pin_fact()`, `unpin_fact()`, `PersistenceClassifier`, updated `resume_context()`, updated `AddFactOptions`.

- [ ] **Step 3: Update forgetting docs**

Document the pinned bypass: "Facts with `is_pinned = true` are never expired by the forgetting pipeline."

- [ ] **Step 4: Update bi-temporal docs**

Document future memory: "Facts with `t_valid` in the future remain invisible until their date arrives. Use `list_due(now)` for incremental checks or `resume_context()` for full boot."

- [ ] **Step 5: Write ADR-0008**

Decision: Materialize importance score on facts.
Context: `resume_context()` needs fast sorting by importance. Computing on-the-fly requires loading graph degree for every fact.
Decision: Store `importance_score` on facts table, update during `prune()` and `consolidate()`.

Consequences:

- **Performance**: O(1) sort vs O(N × degree) recomputation. `resume_context()` becomes a simple `ORDER BY importance_score DESC` query.
- **Staleness trade-off**: Between `prune()`/`consolidate()` calls, scores are stale. This means `resume_context()` may omit recently-important facts or include recently-decayed ones. This is acceptable for pre-1.0: consumers who need fresh ranking call `forget()` first. Document this as "eventual consistency of importance" in the ADR.
- **Correctness maintenance**: If importance signals change (access_count bumps, graph edge additions) without a prune cycle, the materialized score lags. Future phases may add lightweight refresh triggers (e.g., bump score on access_count threshold), but Phase 3b does not.

**Clarify `importance` vs `importance_score`**: `importance` is the **caller-provided hint** (input, set at creation via `AddFactOptions`). `importance_score` is the **engine-computed materialized value** (output, derived from `importance` + recency + access_count + graph degree via the Ebbinghaus formula). Both fields remain — `importance` is the base signal, `importance_score` is the composite.

- [ ] **Step 6: Update core concepts**

Add pinned facts and future memory to the conceptual overview.

- [ ] **Step 7: Commit**

```
docs: update documentation for Phase 3b features
```

---

### Task 11: Integration Tests

**Files:**

- Modify: existing test modules in `src/engine.rs`

End-to-end scenarios testing feature interactions:

- [ ] **Step 1: Full lifecycle test**

```rust
#[test]
fn full_lifecycle_pinned_and_future_memory() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    let now = Utc::now();

    // Add pinned identity fact
    let opts_pin = AddFactOptions { pinned: Some(true), importance: Some(0.95), ..Default::default() };
    let pin_id = engine.add_fact("I am an AI assistant", FactType::Semantic, None, &embedder, None, Some(&opts_pin), None).unwrap();

    // Add future reminder
    let future = now + chrono::Duration::hours(24);
    let opts_future = AddFactOptions { t_valid: Some(future), ..Default::default() };
    engine.add_fact("Check release notes tomorrow", FactType::Episodic, None, &embedder, None, Some(&opts_future), None).unwrap();

    // Add normal fact (forgettable)
    engine.add_fact("Had coffee today", FactType::Episodic, None, &embedder, None, None, None).unwrap();

    // resume_context at current time — future fact should NOT appear in due tier
    let ctx = engine.resume_context(&ResumeConfig { now, ..Default::default() }).unwrap();
    assert!(!ctx.pinned.is_empty());
    assert!(ctx.due.is_empty());

    // list_due at current time — nothing due yet
    assert!(engine.list_due(now, None).unwrap().is_empty());

    // list_due at future time — reminder surfaces
    let later = now + chrono::Duration::hours(25);
    let due = engine.list_due(later, None).unwrap();
    assert_eq!(due.len(), 1);

    // next_due_time should return the future fact's t_valid
    let next = engine.next_due_time(None).unwrap();
    assert!(next.is_some());

    // Forget with aggressive policy — pinned fact survives
    let policy = ForgetPolicy { min_importance: 0.99, ..ForgetPolicy::default() };
    engine.forget(&policy).unwrap();
    let fact = engine.get_fact(pin_id).unwrap();
    assert!(fact.t_expired.is_none()); // still alive
    assert!(fact.is_pinned);
}
```

- [ ] **Step 2: Run all tests**

Run: `cargo test`
Expected: ALL pass

- [ ] **Step 3: Commit**

```
test: add full lifecycle integration test for Phase 3b features
```

---

## Operational Steps

### Task 12: Worktree & Branch

- [ ] **Step 1: Fetch and checkout the phase3 branch**

```bash
git fetch origin feat/memory-engine-phase3
git worktree add ../memory-engine-phase3b origin/feat/memory-engine-phase3
cd ../memory-engine-phase3b
```

Work proceeds in this worktree. All changes build on top of the Phase 3 commits.

### Task 13: Plan Issue

- [ ] **Step 1: Publish this plan as a GitHub issue**

```bash
gh issue create --title "Phase 3b: Temporal Memory & Agent Lifecycle" \
  --body-file docs/superpowers/plans/2026-03-10-phase3b-temporal-memory.md \
  --label "enhancement,phase-3b"
```

### Task 14: PR & Review

- [ ] **Step 1: After implementation, commit and push**
- [ ] **Step 2: Create PR referencing the plan issue**
- [ ] **Step 3: Run `/super-review` for multi-model review**
- [ ] **Step 4: Address review feedback**
- [ ] **Step 5: Squash merge into main when review converges**

---

## Verification

After implementation, verify:

1. **All tests pass:** `cargo test` — 0 failures
2. **Clippy clean:** `cargo clippy --all-targets -- -D warnings`
3. **Migration roundtrip:** Create v2 DB → run `migrate()` → verify v3 columns exist with correct defaults
4. **Fresh install:** Open new DB → verify v3 schema directly
5. **Benchmarks still run:** `cargo bench` (Phase 3 benchmarks should be unaffected)
6. **No regressions:** Phase 1/2/3 functionality unchanged for unpinned, non-scheduled facts

---

## Risk Notes

1. **`add_fact()` signature change is breaking.** All consumers must add the `classifier` parameter. Mitigation: `Option<&dyn PersistenceClassifier>` with `None` preserving current behavior.
2. **`ResumeConfig`/`ResumeContext` field changes are breaking.** Old field names (`identity`, `core`) replaced. Mitigation: This is Phase 3 (pre-1.0), breaking changes are expected.
3. **Importance score staleness.** Scores update only during `prune()` and `consolidate()`. Between calls, scores may be stale. On insert, `importance_score` gets the DB default (0.5), which is deliberately conservative — newly inserted facts won't rank artificially high. The initial value is corrected on the next `prune()` cycle. Note: calling `forget()` refreshes scores but may also expire facts — it is a maintenance operation, not a "refresh scores" API. A dedicated `refresh_scores()` method is deferred to a future phase if needed. See ADR-0008 for detailed trade-off analysis.
4. **Event envelope fields are metadata-only.** No behavioral change. `origin_node_id` defaults to `'local'`, `sequence_id` to `0`. Future sync work will use these. Note: these fields are unauthenticated metadata — if future sync trusts `origin_node_id`/`sequence_id`, authentication must be added at that time.
5. **Pinned facts and trust boundaries.** Pinning makes content persist indefinitely and surface reliably at boot via `resume_context()`. This increases the persistence of any prompt-injected or privacy-sensitive memory. Mitigation: pinning is intended for trusted inputs (identity, core beliefs). Explicit `unpin_fact()` + fact deletion still work on pinned facts. Document that callers should validate content before pinning.
6. **`importance_score` is NOT on `NewFact`.** Callers cannot inject arbitrary scores. The field only appears on `Fact` (read path). The engine computes it during `prune()` and `consolidate()`. The DB column default (0.5) applies on insert.
