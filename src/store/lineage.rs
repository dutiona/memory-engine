use rusqlite::{Connection, params};

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
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(MemoryError::Lineage(format!(
                "no lineage record for wisdom fact {wisdom_fact_id}"
            ))),
            Err(e) => Err(MemoryError::Database(e)),
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
            .prepare("SELECT source_fact_ids FROM lineage WHERE wisdom_fact_id = ?1")?;
        let result = stmt.query_row(params![wisdom_fact_id], |row| {
            let json: String = row.get(0)?;
            Ok(json)
        });
        match result {
            Ok(json) => Ok(serde_json::from_str(&json)?),
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(MemoryError::Lineage(format!(
                "no lineage record for wisdom fact {wisdom_fact_id}"
            ))),
            Err(e) => Err(MemoryError::Database(e)),
        }
    }

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
            lineage_id: 0,
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
        assert_eq!(prov.confidence, 0.85);
        assert_eq!(prov.method_version, "dreamcycle-v1");
        assert_eq!(prov.lineage_id, lineage_id);
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
        assert!(matches!(err, MemoryError::Database(_)));
    }
}
