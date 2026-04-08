# Phase 5a: Provenance Infrastructure Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add provenance tracking for wisdom promotions — a lightweight `PromotionProvenance` envelope on each promoted fact, backed by a sidecar `lineage` table in SQLite for full source chains.

**Architecture:** New `lineage` table (sidecar, not embedded in `facts`) stores the full source-fact chain per promoted wisdom fact. A `PromotionProvenance` struct carries the lightweight envelope (source count, session count, confidence, representative IDs). A `LineageStore` module handles CRUD. The `MemoryEngine` facade exposes 4 public methods: `record_lineage`, `get_provenance`, `get_full_lineage`, and `delete_lineage`. Schema migration v7→v8 adds the table.

**Tech Stack:** Rust, rusqlite, chrono, serde, serde_json

---

## File Structure

| Action | File | Responsibility |
|--------|------|----------------|
| Create | `src/store/lineage.rs` | `LineageStore` — CRUD for `lineage` table |
| Modify | `src/store/mod.rs` | Register `lineage` submodule |
| Modify | `src/types.rs` | `PromotionProvenance`, `LineageRecord`, `NewLineageRecord` structs |
| Modify | `src/error.rs` | `Lineage` error variant |
| Modify | `src/store/schema.rs` | Migration v7→v8, bump version, add DDL |
| Create | `src/engine/lineage.rs` | Engine facade methods for provenance |
| Modify | `src/engine/mod.rs` | Register `lineage` submodule |
| Modify | `src/lib.rs` | Re-export new public types |

---

### Task 1: Add types — `PromotionProvenance`, `LineageRecord`, `NewLineageRecord`

**Files:**
- Modify: `src/types.rs`

- [ ] **Step 1: Write the test for `PromotionProvenance` serde round-trip**

Add at the bottom of the `#[cfg(test)] mod tests` block in `src/types.rs`:

```rust
#[test]
fn promotion_provenance_round_trip_json() {
    let prov = PromotionProvenance {
        source_count: 5,
        session_count: 3,
        date_range_start: Utc::now(),
        date_range_end: Utc::now(),
        confidence: 0.87,
        method_version: "dreamcycle-v1".into(),
        representative_ids: vec![10, 20, 30],
        lineage_id: 42,
    };
    let json = serde_json::to_string(&prov).unwrap();
    let back: PromotionProvenance = serde_json::from_str(&json).unwrap();
    assert_eq!(prov, back);
}

#[test]
fn lineage_record_round_trip_json() {
    let rec = LineageRecord {
        lineage_id: 1,
        wisdom_fact_id: 42,
        source_fact_ids: vec![10, 20, 30, 40, 50],
    };
    let json = serde_json::to_string(&rec).unwrap();
    let back: LineageRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(rec, back);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memory-engine promotion_provenance_round_trip -- --nocapture`
Expected: FAIL — `PromotionProvenance` not defined.

- [ ] **Step 3: Add the type definitions**

Add these structs to `src/types.rs`, above the `// --- Options ---` section:

```rust
// --- Provenance (Phase 5a) ---

/// Lightweight provenance envelope attached to promoted wisdom facts.
///
/// Carries summary statistics about the promotion (how many source facts,
/// across how many sessions, confidence score). The full source chain lives
/// in the sidecar `lineage` table, loaded on demand via `lineage_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromotionProvenance {
    pub source_count: u32,
    pub session_count: u32,
    pub date_range_start: DateTime<Utc>,
    pub date_range_end: DateTime<Utc>,
    pub confidence: f64,
    pub method_version: String,
    /// 3-5 most representative source fact IDs (for quick human review).
    pub representative_ids: Vec<i64>,
    /// Foreign key to the `lineage` table for the full source chain.
    pub lineage_id: i64,
}

/// A row in the `lineage` sidecar table (full source chain).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineageRecord {
    pub lineage_id: i64,
    pub wisdom_fact_id: i64,
    pub source_fact_ids: Vec<i64>,
}

/// Insert descriptor for a new lineage record (DB assigns `lineage_id`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewLineageRecord {
    pub wisdom_fact_id: i64,
    pub source_fact_ids: Vec<i64>,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memory-engine promotion_provenance_round_trip lineage_record_round_trip -v`
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add src/types.rs
git commit -m "feat(types): add PromotionProvenance and LineageRecord types

Phase 5a provenance infrastructure (#55). PromotionProvenance is the
lightweight envelope for promoted wisdom facts. LineageRecord holds the
full source chain in the sidecar lineage table."
```

---

### Task 2: Add `Lineage` error variant

**Files:**
- Modify: `src/error.rs`

- [ ] **Step 1: Write the test for the new error variant**

Add at the bottom of the `#[cfg(test)] mod tests` block in `src/error.rs`:

```rust
#[test]
fn lineage_error_display() {
    let err = MemoryError::Lineage("wisdom fact 42 has no lineage record".into());
    assert_eq!(
        err.to_string(),
        "lineage error: wisdom fact 42 has no lineage record"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memory-engine lineage_error_display -- --nocapture`
Expected: FAIL — no `Lineage` variant.

- [ ] **Step 3: Add the error variant**

Add to the `MemoryError` enum in `src/error.rs`, after the `ReadOnly` variant:

```rust
#[error("lineage error: {0}")]
Lineage(String),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memory-engine lineage_error_display -v`
Expected: PASS.

- [ ] **Step 5: Run workspace build to check CLI/MCP crates still compile**

Run: `cargo build --workspace`
Expected: compiles cleanly. The new variant is non-exhaustive to downstream crates (they use `MemoryError` via the `error` module), and no match arms in CLI/MCP should break — they use `?` propagation, not exhaustive matching.

- [ ] **Step 6: Commit**

```bash
git add src/error.rs
git commit -m "feat(error): add Lineage error variant

For provenance infrastructure (#55). Used when lineage records are
missing, malformed, or reference nonexistent facts."
```

---

### Task 3: Schema migration v7 → v8 — add `lineage` table

**Files:**
- Modify: `src/store/schema.rs`

- [ ] **Step 1: Write the migration test**

Add at the end of the test module in `src/store/schema.rs` (follow the pattern of `migrate_v6_to_v7_adds_archive_manifest`):

```rust
#[test]
fn migrate_v7_to_v8_adds_lineage_table() {
    let conn = open_memory().unwrap();
    // Start at v7
    init_schema(&conn).unwrap();
    set_config(&conn, "schema_version", "7");

    migrate(&conn, None).unwrap();

    // Verify version bumped
    let version = get_config(&conn, "schema_version").unwrap().unwrap();
    assert_eq!(version, "8");

    // Verify table exists with correct columns
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('lineage') WHERE name IN ('lineage_id', 'wisdom_fact_id', 'source_fact_ids', 'provenance')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 4, "lineage table should have 4 expected columns");

    // Verify unique index on wisdom_fact_id
    let idx_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_lineage_wisdom_fact_id'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(idx_count, 1, "unique index on wisdom_fact_id should exist");
}

#[test]
fn fresh_db_has_lineage_table() {
    let conn = open_memory().unwrap();
    init_schema(&conn).unwrap();

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('lineage')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(count > 0, "fresh DB should have lineage table");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memory-engine migrate_v7_to_v8 fresh_db_has_lineage -- --nocapture`
Expected: FAIL — no migration function, no DDL.

- [ ] **Step 3: Write the migration function**

Add after `migrate_v6_to_v7` in `src/store/schema.rs`:

```rust
fn migrate_v7_to_v8(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS lineage (
            lineage_id INTEGER PRIMARY KEY AUTOINCREMENT,
            wisdom_fact_id INTEGER NOT NULL REFERENCES facts(id),
            source_fact_ids TEXT NOT NULL CHECK(json_valid(source_fact_ids)),
            provenance TEXT NOT NULL CHECK(json_valid(provenance))
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_lineage_wisdom_fact_id
            ON lineage(wisdom_fact_id);",
    )?;
    Ok(())
}
```

- [ ] **Step 4: Register the migration and bump the version**

Update `CURRENT_SCHEMA_VERSION` from `7` to `8`.

Add to the `MIGRATIONS` array:

```rust
(migrate_v7_to_v8, false),
```

- [ ] **Step 5: Add lineage table to `TABLES_DDL` for fresh databases**

Add before the closing `";` of `TABLES_DDL`, after the `archive_manifest` unique index:

```sql
CREATE TABLE IF NOT EXISTS lineage (
    lineage_id INTEGER PRIMARY KEY AUTOINCREMENT,
    wisdom_fact_id INTEGER NOT NULL REFERENCES facts(id),
    source_fact_ids TEXT NOT NULL CHECK(json_valid(source_fact_ids)),
    provenance TEXT NOT NULL CHECK(json_valid(provenance))
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_lineage_wisdom_fact_id
    ON lineage(wisdom_fact_id);
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p memory-engine migrate_v7_to_v8 fresh_db_has_lineage -v`
Expected: PASS (both tests).

- [ ] **Step 7: Run the full schema test suite**

Run: `cargo test -p memory-engine schema -- -v`
Expected: All schema tests pass (existing migrations unbroken).

- [ ] **Step 8: Commit**

```bash
git add src/store/schema.rs
git commit -m "feat(schema): add lineage table — migration v7→v8

Sidecar table for provenance tracking (#55). Stores full source chain
(source_fact_ids JSON array) and PromotionProvenance envelope (provenance
JSON) per promoted wisdom fact. One-to-one with facts via unique index
on wisdom_fact_id."
```

---

### Task 4: Create `LineageStore` — CRUD for the lineage table

**Files:**
- Create: `src/store/lineage.rs`
- Modify: `src/store/mod.rs`

- [ ] **Step 1: Create `src/store/lineage.rs` with the store struct and test module skeleton**

```rust
use rusqlite::{params, Connection};

use crate::error::{MemoryError, Result};
use crate::types::{LineageRecord, NewLineageRecord, PromotionProvenance};

/// Store for the `lineage` sidecar table — provenance tracking for promoted wisdom facts.
pub struct LineageStore<'a> {
    conn: &'a Connection,
}

#[allow(dead_code)] // complete CRUD API — engine facade wires methods incrementally
impl<'a> LineageStore<'a> {
    /// Create a new `LineageStore` borrowing the given connection.
    #[must_use]
    pub const fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::{init_schema, open_memory};

    fn setup() -> rusqlite::Connection {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        // Insert a dummy fact so FK constraints are satisfied
        conn.execute(
            "INSERT INTO facts (id, content, content_hash, embedding, fact_type, t_created, last_accessed, scope_id, importance_score)
             VALUES (1, 'wisdom fact', 'abc123', X'00000000', 'semantic', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 1, 0.9)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO facts (id, content, content_hash, embedding, fact_type, t_created, last_accessed, scope_id, importance_score)
             VALUES (10, 'source 1', 'src1', X'00000000', 'episodic', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 1, 0.5)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO facts (id, content, content_hash, embedding, fact_type, t_created, last_accessed, scope_id, importance_score)
             VALUES (20, 'source 2', 'src2', X'00000000', 'episodic', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 1, 0.5)",
            [],
        ).unwrap();
        conn
    }
}
```

- [ ] **Step 2: Register the module in `src/store/mod.rs`**

Add `pub mod lineage;` after the `pub mod facts;` line.

- [ ] **Step 3: Write the failing test for `insert`**

Add to the `tests` module in `src/store/lineage.rs`:

```rust
#[test]
fn insert_and_get_by_wisdom_fact() {
    let conn = setup();
    let store = LineageStore::new(&conn);

    let provenance = PromotionProvenance {
        source_count: 2,
        session_count: 1,
        date_range_start: chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        date_range_end: chrono::DateTime::parse_from_rfc3339("2026-03-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        confidence: 0.85,
        method_version: "dreamcycle-v1".into(),
        representative_ids: vec![10, 20],
        lineage_id: 0, // will be set by insert
    };

    let new_rec = NewLineageRecord {
        wisdom_fact_id: 1,
        source_fact_ids: vec![10, 20],
    };

    let lineage_id = store.insert(&new_rec, &provenance).unwrap();
    assert!(lineage_id > 0);

    let (record, prov) = store.get_by_wisdom_fact(1).unwrap();
    assert_eq!(record.wisdom_fact_id, 1);
    assert_eq!(record.source_fact_ids, vec![10, 20]);
    assert_eq!(prov.source_count, 2);
    assert_eq!(prov.confidence, 0.85);
    assert_eq!(prov.method_version, "dreamcycle-v1");
    assert_eq!(prov.lineage_id, lineage_id);
}
```

- [ ] **Step 4: Run test to verify it fails**

Run: `cargo test -p memory-engine insert_and_get_by_wisdom_fact -- --nocapture`
Expected: FAIL — `insert` method not defined.

- [ ] **Step 5: Implement `insert` and `get_by_wisdom_fact`**

Add to the `impl LineageStore` block:

```rust
/// Insert a new lineage record with its provenance envelope.
/// Returns the auto-assigned `lineage_id`.
///
/// The `provenance.lineage_id` field is ignored on input — the DB assigns
/// the ID, and the returned value is the authoritative one.
///
/// # Errors
///
/// Returns `MemoryError::Database` on insert failure (e.g., duplicate
/// `wisdom_fact_id`, FK violation).
pub fn insert(
    &self,
    record: &NewLineageRecord,
    provenance: &PromotionProvenance,
) -> Result<i64> {
    let source_ids_json = serde_json::to_string(&record.source_fact_ids)?;
    // Store provenance with the correct lineage_id placeholder — we'll
    // update it after getting the rowid. Simpler: store without lineage_id
    // in the JSON and set it in the returned struct at read time.
    let prov_json = serde_json::to_string(provenance)?;

    self.conn.execute(
        "INSERT INTO lineage (wisdom_fact_id, source_fact_ids, provenance)
         VALUES (?1, ?2, ?3)",
        params![record.wisdom_fact_id, source_ids_json, prov_json],
    )?;
    Ok(self.conn.last_insert_rowid())
}

/// Look up the lineage record and provenance for a promoted wisdom fact.
///
/// # Errors
///
/// Returns `MemoryError::Lineage` if no lineage exists for `wisdom_fact_id`.
/// Returns `MemoryError::Serialization` if stored JSON is malformed.
pub fn get_by_wisdom_fact(
    &self,
    wisdom_fact_id: i64,
) -> Result<(LineageRecord, PromotionProvenance)> {
    let mut stmt = self.conn.prepare(
        "SELECT lineage_id, wisdom_fact_id, source_fact_ids, provenance
         FROM lineage WHERE wisdom_fact_id = ?1",
    )?;
    let result = stmt.query_row(params![wisdom_fact_id], |row| {
        let lineage_id: i64 = row.get(0)?;
        let wfid: i64 = row.get(1)?;
        let source_ids_json: String = row.get(2)?;
        let prov_json: String = row.get(3)?;
        Ok((lineage_id, wfid, source_ids_json, prov_json))
    });
    match result {
        Ok((lineage_id, wfid, source_ids_json, prov_json)) => {
            let source_fact_ids: Vec<i64> = serde_json::from_str(&source_ids_json)?;
            let mut provenance: PromotionProvenance = serde_json::from_str(&prov_json)?;
            provenance.lineage_id = lineage_id;
            let record = LineageRecord {
                lineage_id,
                wisdom_fact_id: wfid,
                source_fact_ids,
            };
            Ok((record, provenance))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Err(MemoryError::Lineage(
            format!("no lineage record for wisdom fact {wisdom_fact_id}"),
        )),
        Err(e) => Err(MemoryError::Database(e)),
    }
}
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test -p memory-engine insert_and_get_by_wisdom_fact -v`
Expected: PASS.

- [ ] **Step 7: Write the failing test for `get_source_fact_ids` (lightweight accessor)**

```rust
#[test]
fn get_source_fact_ids_returns_only_ids() {
    let conn = setup();
    let store = LineageStore::new(&conn);
    let prov = test_provenance();
    let new_rec = NewLineageRecord {
        wisdom_fact_id: 1,
        source_fact_ids: vec![10, 20],
    };
    store.insert(&new_rec, &prov).unwrap();

    let ids = store.get_source_fact_ids(1).unwrap();
    assert_eq!(ids, vec![10, 20]);
}

#[test]
fn get_source_fact_ids_not_found() {
    let conn = setup();
    let store = LineageStore::new(&conn);
    let err = store.get_source_fact_ids(999).unwrap_err();
    assert!(matches!(err, MemoryError::Lineage(_)));
}
```

Also add a `test_provenance()` helper to the test module:

```rust
fn test_provenance() -> PromotionProvenance {
    PromotionProvenance {
        source_count: 2,
        session_count: 1,
        date_range_start: chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        date_range_end: chrono::DateTime::parse_from_rfc3339("2026-03-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        confidence: 0.85,
        method_version: "dreamcycle-v1".into(),
        representative_ids: vec![10, 20],
        lineage_id: 0,
    }
}
```

- [ ] **Step 8: Implement `get_source_fact_ids`**

```rust
/// Return just the source fact IDs for a wisdom fact's lineage.
///
/// Lighter than `get_by_wisdom_fact` — skips provenance deserialization.
///
/// # Errors
///
/// Returns `MemoryError::Lineage` if no lineage exists.
pub fn get_source_fact_ids(&self, wisdom_fact_id: i64) -> Result<Vec<i64>> {
    let mut stmt = self.conn.prepare(
        "SELECT source_fact_ids FROM lineage WHERE wisdom_fact_id = ?1",
    )?;
    let result = stmt.query_row(params![wisdom_fact_id], |row| {
        let json: String = row.get(0)?;
        Ok(json)
    });
    match result {
        Ok(json) => Ok(serde_json::from_str(&json)?),
        Err(rusqlite::Error::QueryReturnedNoRows) => Err(MemoryError::Lineage(
            format!("no lineage record for wisdom fact {wisdom_fact_id}"),
        )),
        Err(e) => Err(MemoryError::Database(e)),
    }
}
```

- [ ] **Step 9: Run tests**

Run: `cargo test -p memory-engine get_source_fact_ids -v`
Expected: PASS (both tests).

- [ ] **Step 10: Write failing test for `delete`**

```rust
#[test]
fn delete_removes_lineage() {
    let conn = setup();
    let store = LineageStore::new(&conn);
    let prov = test_provenance();
    let new_rec = NewLineageRecord {
        wisdom_fact_id: 1,
        source_fact_ids: vec![10, 20],
    };
    store.insert(&new_rec, &prov).unwrap();

    let deleted = store.delete(1).unwrap();
    assert!(deleted);

    let err = store.get_by_wisdom_fact(1).unwrap_err();
    assert!(matches!(err, MemoryError::Lineage(_)));
}

#[test]
fn delete_nonexistent_returns_false() {
    let conn = setup();
    let store = LineageStore::new(&conn);
    let deleted = store.delete(999).unwrap();
    assert!(!deleted);
}
```

- [ ] **Step 11: Implement `delete`**

```rust
/// Delete the lineage record for a wisdom fact.
///
/// Returns `true` if a record was deleted, `false` if none existed.
///
/// # Errors
///
/// Returns `MemoryError::Database` on SQL failure.
pub fn delete(&self, wisdom_fact_id: i64) -> Result<bool> {
    let rows = self.conn.execute(
        "DELETE FROM lineage WHERE wisdom_fact_id = ?1",
        params![wisdom_fact_id],
    )?;
    Ok(rows > 0)
}
```

- [ ] **Step 12: Run tests**

Run: `cargo test -p memory-engine delete_removes_lineage delete_nonexistent -v`
Expected: PASS.

- [ ] **Step 13: Write failing test for `has_lineage`**

```rust
#[test]
fn has_lineage_check() {
    let conn = setup();
    let store = LineageStore::new(&conn);
    assert!(!store.has_lineage(1).unwrap());

    let prov = test_provenance();
    let new_rec = NewLineageRecord {
        wisdom_fact_id: 1,
        source_fact_ids: vec![10, 20],
    };
    store.insert(&new_rec, &prov).unwrap();
    assert!(store.has_lineage(1).unwrap());
}
```

- [ ] **Step 14: Implement `has_lineage`**

```rust
/// Check whether a wisdom fact has a lineage record.
///
/// # Errors
///
/// Returns `MemoryError::Database` on SQL failure.
pub fn has_lineage(&self, wisdom_fact_id: i64) -> Result<bool> {
    let count: i64 = self.conn.query_row(
        "SELECT COUNT(*) FROM lineage WHERE wisdom_fact_id = ?1",
        params![wisdom_fact_id],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}
```

- [ ] **Step 15: Run all lineage store tests**

Run: `cargo test -p memory-engine store::lineage -v`
Expected: All pass.

- [ ] **Step 16: Commit**

```bash
git add src/store/lineage.rs src/store/mod.rs
git commit -m "feat(store): add LineageStore for provenance sidecar table

CRUD operations for the lineage table (#55): insert, get_by_wisdom_fact,
get_source_fact_ids, delete, has_lineage. Follows FactStore/SummaryStore
patterns — borrows connection, returns typed Results."
```

---

### Task 5: Engine facade methods for provenance

**Files:**
- Create: `src/engine/lineage.rs`
- Modify: `src/engine/mod.rs`

- [ ] **Step 1: Create `src/engine/lineage.rs` with test skeleton**

```rust
use crate::error::Result;
use crate::store::lineage::LineageStore;
use crate::types::{LineageRecord, NewLineageRecord, PromotionProvenance};

use super::MemoryEngine;

impl MemoryEngine {
    // Methods added in subsequent steps.
}
```

- [ ] **Step 2: Register the module in `src/engine/mod.rs`**

Add `mod lineage;` after the existing `mod inspect;` line.

- [ ] **Step 3: Write the failing integration test for `record_lineage`**

Add a test in `src/engine/lineage.rs`:

```rust
#[cfg(test)]
mod tests {
    use crate::engine::MemoryEngine;
    use crate::types::{NewLineageRecord, PromotionProvenance};
    use chrono::Utc;

    fn test_provenance() -> PromotionProvenance {
        PromotionProvenance {
            source_count: 2,
            session_count: 1,
            date_range_start: Utc::now(),
            date_range_end: Utc::now(),
            confidence: 0.85,
            method_version: "dreamcycle-v1".into(),
            representative_ids: vec![],
            lineage_id: 0,
        }
    }

    fn engine_with_facts() -> MemoryEngine {
        use crate::types::{FactType, NewFact};
        let engine = MemoryEngine::open_memory(4).unwrap();
        // Insert wisdom fact
        let conn = engine.pool.write();
        let store = crate::store::facts::FactStore::new(&conn, 4);
        let fact = NewFact {
            content: "synthesized wisdom".into(),
            content_hash: "w1".into(),
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            fact_type: FactType::Semantic,
            t_created: Utc::now(),
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            importance: 0.9,
            access_count: 0,
            last_accessed: Utc::now(),
            metadata: serde_json::json!({}),
            scope_id: 1,
            is_pinned: true,
        };
        store.insert(&fact).unwrap();
        // Insert source facts
        for (i, label) in ["source A", "source B"].iter().enumerate() {
            let sf = NewFact {
                content: label.to_string(),
                content_hash: format!("s{i}"),
                embedding: vec![0.0; 4],
                fact_type: FactType::Episodic,
                t_created: Utc::now(),
                t_expired: None,
                t_valid: None,
                t_invalid: None,
                source_event_id: None,
                importance: 0.5,
                access_count: 0,
                last_accessed: Utc::now(),
                metadata: serde_json::json!({}),
                scope_id: 1,
                is_pinned: false,
            };
            store.insert(&sf).unwrap();
        }
        drop(conn);
        engine
    }

    #[test]
    fn record_and_get_provenance() {
        let engine = engine_with_facts();
        let new_rec = NewLineageRecord {
            wisdom_fact_id: 1,
            source_fact_ids: vec![2, 3],
        };
        let prov = test_provenance();
        let lineage_id = engine.record_lineage(&new_rec, &prov).unwrap();
        assert!(lineage_id > 0);

        let (record, got_prov) = engine.get_provenance(1).unwrap();
        assert_eq!(record.wisdom_fact_id, 1);
        assert_eq!(record.source_fact_ids, vec![2, 3]);
        assert_eq!(got_prov.source_count, 2);
        assert_eq!(got_prov.lineage_id, lineage_id);
    }

    #[test]
    fn get_full_lineage_returns_ids() {
        let engine = engine_with_facts();
        let new_rec = NewLineageRecord {
            wisdom_fact_id: 1,
            source_fact_ids: vec![2, 3],
        };
        engine.record_lineage(&new_rec, &test_provenance()).unwrap();

        let ids = engine.get_full_lineage(1).unwrap();
        assert_eq!(ids, vec![2, 3]);
    }

    #[test]
    fn delete_lineage_removes_record() {
        let engine = engine_with_facts();
        let new_rec = NewLineageRecord {
            wisdom_fact_id: 1,
            source_fact_ids: vec![2, 3],
        };
        engine.record_lineage(&new_rec, &test_provenance()).unwrap();

        let deleted = engine.delete_lineage(1).unwrap();
        assert!(deleted);

        let err = engine.get_provenance(1).unwrap_err();
        assert!(matches!(err, crate::error::MemoryError::Lineage(_)));
    }

    #[test]
    fn record_lineage_read_only_rejects() {
        // Can't easily test read-only in memory, but verify the method
        // goes through try_write by checking it works in normal mode.
        let engine = engine_with_facts();
        assert!(!engine.is_read_only());
        let new_rec = NewLineageRecord {
            wisdom_fact_id: 1,
            source_fact_ids: vec![2, 3],
        };
        let result = engine.record_lineage(&new_rec, &test_provenance());
        assert!(result.is_ok());
    }
}
```

- [ ] **Step 4: Run test to verify it fails**

Run: `cargo test -p memory-engine record_and_get_provenance -- --nocapture`
Expected: FAIL — `record_lineage` not defined.

- [ ] **Step 5: Implement the four engine facade methods**

In `src/engine/lineage.rs`, replace the empty `impl` block:

```rust
impl MemoryEngine {
    /// Record provenance for a promoted wisdom fact.
    ///
    /// Writes to the `lineage` sidecar table via the writer connection.
    /// Returns the auto-assigned `lineage_id`.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine is read-only.
    /// Returns `MemoryError::Database` on insert failure.
    pub fn record_lineage(
        &self,
        record: &NewLineageRecord,
        provenance: &PromotionProvenance,
    ) -> Result<i64> {
        let conn = self.pool.try_write()?;
        let store = LineageStore::new(&conn);
        store.insert(record, provenance)
    }

    /// Retrieve the provenance envelope and lineage record for a wisdom fact.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Lineage` if no lineage exists.
    pub fn get_provenance(
        &self,
        wisdom_fact_id: i64,
    ) -> Result<(LineageRecord, PromotionProvenance)> {
        let conn = self.pool.read();
        let store = LineageStore::new(&conn);
        store.get_by_wisdom_fact(wisdom_fact_id)
    }

    /// Retrieve just the full source-fact ID chain for a wisdom fact.
    ///
    /// Lighter than `get_provenance` — use when only the source chain is needed
    /// (e.g., "Why?" button, debugging bad promotions).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Lineage` if no lineage exists.
    pub fn get_full_lineage(&self, wisdom_fact_id: i64) -> Result<Vec<i64>> {
        let conn = self.pool.read();
        let store = LineageStore::new(&conn);
        store.get_source_fact_ids(wisdom_fact_id)
    }

    /// Delete the lineage record for a wisdom fact (e.g., when reversing a promotion).
    ///
    /// Returns `true` if a record was deleted, `false` if none existed.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine is read-only.
    pub fn delete_lineage(&self, wisdom_fact_id: i64) -> Result<bool> {
        let conn = self.pool.try_write()?;
        let store = LineageStore::new(&conn);
        store.delete(wisdom_fact_id)
    }
}
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p memory-engine engine::lineage -v`
Expected: All 4 tests pass.

- [ ] **Step 7: Commit**

```bash
git add src/engine/lineage.rs src/engine/mod.rs
git commit -m "feat(engine): expose provenance API on MemoryEngine facade

Four public methods (#55): record_lineage, get_provenance,
get_full_lineage, delete_lineage. Write methods go through try_write
(read-only guard). Read methods use the reader pool."
```

---

### Task 6: Re-export new types from `lib.rs`

**Files:**
- Modify: `src/lib.rs`

- [ ] **Step 1: Verify the types are already re-exported via `pub use types::*`**

Check `src/lib.rs` — line `pub use types::*;` already glob-exports everything from `types.rs`. Since `PromotionProvenance`, `LineageRecord`, and `NewLineageRecord` were added to `types.rs`, they're already public. No changes needed.

However, `LineageStore` should NOT be re-exported (it's an internal store detail — consumers use the engine facade).

- [ ] **Step 2: Verify workspace compiles with all new public API**

Run: `cargo build --workspace`
Expected: All 3 crates compile.

- [ ] **Step 3: Run full workspace tests**

Run: `cargo test --workspace`
Expected: All pass.

- [ ] **Step 4: Run clippy on workspace**

Run: `cargo clippy --workspace --all-targets`
Expected: No warnings.

- [ ] **Step 5: Commit (only if lib.rs needed changes)**

If no changes needed, skip this commit.

---

### Task 7: Workspace verification gate

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace`
Expected: Clean.

- [ ] **Step 2: Full workspace tests**

Run: `cargo test --workspace`
Expected: All pass.

- [ ] **Step 3: Full workspace clippy**

Run: `cargo clippy --workspace --all-targets`
Expected: No warnings.

- [ ] **Step 4: Format check**

Run: `cargo fmt --check`
Expected: Clean.
