use me_types::error::StorageError;
use rusqlite::{Connection, params};

use me_types::error::{MemoryError, Result};
use me_types::types::{LineageRecord, LineageSnapshotEntry, NewLineageRecord, PromotionProvenance};

/// Store for the `lineage` sidecar table — provenance tracking for promoted wisdom facts.
pub struct LineageStore<'a> {
    conn: &'a Connection,
}

#[allow(dead_code)] // complete CRUD API — engine facade wires methods incrementally
impl<'a> LineageStore<'a> {
    /// Create a new `LineageStore` borrowing the given connection.
    // Stays `pub` (Wave 2 #816, me-backend-sqlite carve): `storage/sqlite/` joined
    // this crate in sub-PR 2b, but the facade's own `inspect/restore.rs` still
    // constructs `LineageStore` directly across the crate boundary.
    #[must_use]
    pub const fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Insert a new lineage record with its provenance envelope.
    /// Returns the auto-assigned `lineage_id`.
    ///
    /// Validates that the `wisdom_fact_id` references an **active** (non-expired)
    /// fact and that all `source_fact_ids` reference existing facts before
    /// inserting — so a lineage row can never be orphaned against a missing or
    /// soft-deleted wisdom fact. The row PK is the authoritative `lineage_id`; the
    /// provenance envelope no longer carries one (see [`PromotionProvenance`]).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Lineage` if the `wisdom_fact_id` does not exist or
    /// is expired, or if any source fact ID does not exist.
    /// Returns `MemoryError::Storage` on insert failure (e.g., duplicate
    /// `wisdom_fact_id`).
    pub fn insert(
        &self,
        record: &NewLineageRecord,
        provenance: &PromotionProvenance,
    ) -> Result<i64> {
        // Validate that the wisdom fact exists and is active (not soft-deleted).
        // The FK only proves the row exists; an expired (`t_expired IS NOT NULL`)
        // fact is a tombstone and must not anchor fresh lineage. Doing the check
        // here turns an opaque FK `Database` error into a clear `Lineage` cause
        // and additionally rejects the expired case the FK cannot catch.
        let wisdom_active: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM facts WHERE id = ?1 AND t_expired IS NULL",
                params![record.wisdom_fact_id],
                |r| r.get(0),
            )
            .map_err(StorageError::backend)?;
        if wisdom_active == 0 {
            return Err(MemoryError::Lineage(format!(
                "wisdom fact {} does not exist or is expired; cannot record lineage",
                record.wisdom_fact_id
            )));
        }

        // Validate that all distinct source fact IDs exist.
        if !record.source_fact_ids.is_empty() {
            let unique_ids: std::collections::BTreeSet<i64> =
                record.source_fact_ids.iter().copied().collect();
            let ids_json = serde_json::to_string(&unique_ids)?;
            let count: i64 = self
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM facts WHERE id IN (SELECT value FROM json_each(?1))",
                    params![ids_json],
                    |r| r.get(0),
                )
                .map_err(StorageError::backend)?;
            let expected = i64::try_from(unique_ids.len())
                .map_err(|e| MemoryError::Internal(e.to_string()))?;
            if count != expected {
                return Err(MemoryError::Lineage(format!(
                    "source_fact_ids contains nonexistent fact IDs \
                     (expected {} distinct IDs to exist, found {count})",
                    unique_ids.len()
                )));
            }
        }

        let source_ids_json = serde_json::to_string(&record.source_fact_ids)?;
        let prov_json = serde_json::to_string(provenance)?;

        self.conn
            .execute(
                "INSERT INTO lineage (wisdom_fact_id, source_fact_ids, provenance)
             VALUES (?1, ?2, ?3)",
                params![record.wisdom_fact_id, source_ids_json, prov_json],
            )
            .map_err(StorageError::backend)?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Insert a raw lineage row with an explicit `lineage_id` (for snapshot restore).
    ///
    /// Skips source fact validation — the caller (restore) is responsible for
    /// inserting facts before lineage.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on insert failure.
    pub fn insert_raw(&self, entry: &LineageSnapshotEntry) -> Result<()> {
        let source_ids_json = serde_json::to_string(&entry.source_fact_ids)?;
        let prov_json = serde_json::to_string(&entry.provenance)?;

        self.conn
            .execute(
                "INSERT INTO lineage (lineage_id, wisdom_fact_id, source_fact_ids, provenance)
             VALUES (?1, ?2, ?3, ?4)",
                params![
                    entry.lineage_id,
                    entry.wisdom_fact_id,
                    source_ids_json,
                    prov_json,
                ],
            )
            .map_err(StorageError::backend)?;
        Ok(())
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
        let mut stmt = self
            .conn
            .prepare(
                "SELECT lineage_id, wisdom_fact_id, source_fact_ids, provenance
             FROM lineage WHERE wisdom_fact_id = ?1",
            )
            .map_err(StorageError::backend)?;
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
                let provenance: PromotionProvenance = serde_json::from_str(&prov_json)?;
                // The PK is the authoritative lineage_id; it lives on the
                // companion record, not (re)written onto the envelope.
                let record = LineageRecord {
                    lineage_id,
                    wisdom_fact_id: wfid,
                    source_fact_ids,
                };
                Ok((record, provenance))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(MemoryError::Lineage(format!(
                "no lineage record for wisdom fact {wisdom_fact_id}"
            ))),
            Err(e) => Err(StorageError::backend(e).into()),
        }
    }

    /// Return just the source fact IDs for a wisdom fact's lineage.
    ///
    /// Lighter than `get_by_wisdom_fact` — skips provenance deserialization.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Lineage` if no lineage exists.
    pub fn get_source_fact_ids(&self, wisdom_fact_id: i64) -> Result<Vec<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT source_fact_ids FROM lineage WHERE wisdom_fact_id = ?1")
            .map_err(StorageError::backend)?;
        let result = stmt.query_row(params![wisdom_fact_id], |row| {
            let json: String = row.get(0)?;
            Ok(json)
        });
        match result {
            Ok(json) => Ok(serde_json::from_str(&json)?),
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(MemoryError::Lineage(format!(
                "no lineage record for wisdom fact {wisdom_fact_id}"
            ))),
            Err(e) => Err(StorageError::backend(e).into()),
        }
    }

    /// Delete the lineage record for a wisdom fact.
    ///
    /// Returns `true` if a record was deleted, `false` if none existed.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on SQL failure.
    pub fn delete(&self, wisdom_fact_id: i64) -> Result<bool> {
        let rows = self
            .conn
            .execute(
                "DELETE FROM lineage WHERE wisdom_fact_id = ?1",
                params![wisdom_fact_id],
            )
            .map_err(StorageError::backend)?;
        Ok(rows > 0)
    }

    /// Check whether a wisdom fact has a lineage record.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on SQL failure.
    pub fn has_lineage(&self, wisdom_fact_id: i64) -> Result<bool> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM lineage WHERE wisdom_fact_id = ?1",
                params![wisdom_fact_id],
                |r| r.get(0),
            )
            .map_err(StorageError::backend)?;
        Ok(count > 0)
    }

    /// Iterate over all lineage rows (for snapshot dump).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on SQL failure.
    /// Returns `MemoryError::Serialization` on malformed stored JSON.
    pub fn for_each<F>(&self, mut f: F) -> Result<()>
    where
        F: FnMut(LineageSnapshotEntry) -> Result<()>,
    {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT lineage_id, wisdom_fact_id, source_fact_ids, provenance
             FROM lineage ORDER BY lineage_id ASC",
            )
            .map_err(StorageError::backend)?;
        let mut rows = stmt.query([]).map_err(StorageError::backend)?;
        while let Some(row) = rows.next().map_err(StorageError::backend)? {
            let lineage_id: i64 = row.get(0).map_err(StorageError::backend)?;
            let wisdom_fact_id: i64 = row.get(1).map_err(StorageError::backend)?;
            let source_ids_json: String = row.get(2).map_err(StorageError::backend)?;
            let prov_json: String = row.get(3).map_err(StorageError::backend)?;
            let source_fact_ids: Vec<i64> = serde_json::from_str(&source_ids_json)?;
            let provenance: PromotionProvenance = serde_json::from_str(&prov_json)?;
            // `lineage_id` lives on the snapshot entry (the row PK), not on the
            // envelope — no reconstruction onto `provenance` needed.
            f(LineageSnapshotEntry {
                lineage_id,
                wisdom_fact_id,
                source_fact_ids,
                provenance,
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::{init_schema, open_memory};

    fn setup() -> rusqlite::Connection {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        // Insert dummy facts so FK constraints are satisfied
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
        }
    }

    #[test]
    fn insert_and_get_by_wisdom_fact() {
        let conn = setup();
        let store = LineageStore::new(&conn);

        let new_rec = NewLineageRecord {
            wisdom_fact_id: 1,
            source_fact_ids: vec![10, 20],
        };

        let lineage_id = store.insert(&new_rec, &test_provenance()).unwrap();
        assert!(lineage_id > 0);

        let (record, prov) = store.get_by_wisdom_fact(1).unwrap();
        assert_eq!(record.wisdom_fact_id, 1);
        assert_eq!(record.source_fact_ids, vec![10, 20]);
        assert_eq!(prov.source_count, 2);
        assert!((prov.confidence - 0.85).abs() < f64::EPSILON);
        assert_eq!(prov.method_version, "dreamcycle-v1");
        // The authoritative lineage_id now lives on the companion record (the row
        // PK), not reconstructed onto the provenance envelope.
        assert_eq!(record.lineage_id, lineage_id);
    }

    #[test]
    fn get_source_fact_ids_returns_only_ids() {
        let conn = setup();
        let store = LineageStore::new(&conn);
        let new_rec = NewLineageRecord {
            wisdom_fact_id: 1,
            source_fact_ids: vec![10, 20],
        };
        store.insert(&new_rec, &test_provenance()).unwrap();

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

    #[test]
    fn delete_removes_lineage() {
        let conn = setup();
        let store = LineageStore::new(&conn);
        let new_rec = NewLineageRecord {
            wisdom_fact_id: 1,
            source_fact_ids: vec![10, 20],
        };
        store.insert(&new_rec, &test_provenance()).unwrap();

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

    #[test]
    fn has_lineage_check() {
        let conn = setup();
        let store = LineageStore::new(&conn);
        assert!(!store.has_lineage(1).unwrap());

        let new_rec = NewLineageRecord {
            wisdom_fact_id: 1,
            source_fact_ids: vec![10, 20],
        };
        store.insert(&new_rec, &test_provenance()).unwrap();
        assert!(store.has_lineage(1).unwrap());
    }

    #[test]
    fn duplicate_wisdom_fact_id_rejected() {
        let conn = setup();
        let store = LineageStore::new(&conn);
        let new_rec = NewLineageRecord {
            wisdom_fact_id: 1,
            source_fact_ids: vec![10, 20],
        };
        store.insert(&new_rec, &test_provenance()).unwrap();

        // Second insert with same wisdom_fact_id should fail (unique index)
        let err = store.insert(&new_rec, &test_provenance()).unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Storage(me_types::error::StorageError::Backend(_))
        ));
    }

    #[test]
    fn insert_rejects_nonexistent_source_facts() {
        let conn = setup();
        let store = LineageStore::new(&conn);
        let new_rec = NewLineageRecord {
            wisdom_fact_id: 1,
            source_fact_ids: vec![10, 999], // 999 does not exist
        };
        let err = store.insert(&new_rec, &test_provenance()).unwrap_err();
        assert!(matches!(err, MemoryError::Lineage(_)));
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn insert_rejects_nonexistent_wisdom_fact() {
        // A lineage row must point at an existing wisdom fact. Without the
        // up-front check this would surface as an opaque FK `Database` error;
        // with it, the caller gets a clear `Lineage` error naming the cause.
        let conn = setup();
        let store = LineageStore::new(&conn);
        let new_rec = NewLineageRecord {
            wisdom_fact_id: 12345, // no such fact
            source_fact_ids: vec![10, 20],
        };
        let err = store.insert(&new_rec, &test_provenance()).unwrap_err();
        assert!(matches!(err, MemoryError::Lineage(_)), "{err}");
        assert!(err.to_string().contains("12345"), "{err}");
    }

    #[test]
    fn insert_rejects_expired_wisdom_fact() {
        // The FK only proves the row exists, not that it is active. Recording
        // lineage against a soft-deleted (expired) wisdom fact would orphan the
        // provenance against a tombstone — reject it.
        let conn = setup();
        // Expire wisdom fact 1.
        conn.execute(
            "UPDATE facts SET t_expired = '2026-02-01T00:00:00Z' WHERE id = 1",
            [],
        )
        .unwrap();
        let store = LineageStore::new(&conn);
        let new_rec = NewLineageRecord {
            wisdom_fact_id: 1,
            source_fact_ids: vec![10, 20],
        };
        let err = store.insert(&new_rec, &test_provenance()).unwrap_err();
        assert!(matches!(err, MemoryError::Lineage(_)), "{err}");
        assert!(err.to_string().contains("expired"), "{err}");
    }

    #[test]
    fn insert_with_empty_source_facts_skips_validation_and_stores_empty() {
        // #482: when `source_fact_ids` is empty, `insert` short-circuits the
        // FK-existence loop (`if !record.source_fact_ids.is_empty()`) and inserts a
        // row with `source_fact_ids = '[]'`. This pins the *current, intentional*
        // permissive behavior — an empty source chain is accepted, not rejected —
        // so a future change to that contract (e.g. erroring on empty sources) is a
        // deliberate, test-visible decision rather than a silent drift.
        //
        // Non-vacuous: it asserts BOTH that the insert succeeds (a regression that
        // made empty sources error would fail here) AND that the round-tripped
        // sources are exactly empty (a regression that, say, validated an empty set
        // against existing facts and substituted a non-empty default would fail the
        // equality). The wisdom-fact existence check still runs and must pass.
        let conn = setup();
        let store = LineageStore::new(&conn);

        let new_rec = NewLineageRecord {
            wisdom_fact_id: 1,
            source_fact_ids: vec![],
        };
        let lineage_id = store
            .insert(&new_rec, &test_provenance())
            .expect("empty source_fact_ids is accepted by the current contract");
        assert!(lineage_id > 0);

        // Read back through the typed getter: the stored chain is exactly empty.
        let ids = store.get_source_fact_ids(1).unwrap();
        assert!(
            ids.is_empty(),
            "empty source_fact_ids must round-trip as empty, got {ids:?}"
        );
        // And the raw column is the empty-JSON-array literal, not NULL or absent.
        let raw: String = conn
            .query_row(
                "SELECT source_fact_ids FROM lineage WHERE wisdom_fact_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(raw, "[]", "empty sources stored as '[]'");
    }

    #[test]
    fn for_each_iterates_all_rows() {
        let conn = setup();
        let store = LineageStore::new(&conn);
        // Insert two lineage records (need a second wisdom fact)
        conn.execute(
            "INSERT INTO facts (id, content, content_hash, embedding, fact_type, t_created, last_accessed, scope_id, importance_score)
             VALUES (2, 'wisdom 2', 'w2', X'00000000', 'semantic', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 1, 0.8)",
            [],
        ).unwrap();

        let rec1 = NewLineageRecord {
            wisdom_fact_id: 1,
            source_fact_ids: vec![10],
        };
        let rec2 = NewLineageRecord {
            wisdom_fact_id: 2,
            source_fact_ids: vec![20],
        };
        store.insert(&rec1, &test_provenance()).unwrap();
        store.insert(&rec2, &test_provenance()).unwrap();

        let mut entries = Vec::new();
        store
            .for_each(|entry| {
                entries.push(entry);
                Ok(())
            })
            .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].wisdom_fact_id, 1);
        assert_eq!(entries[1].wisdom_fact_id, 2);
    }
}
