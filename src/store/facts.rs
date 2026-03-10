use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};

use crate::error::{MemoryError, Result};
use crate::store::{
    deserialize_embedding, parse_optional_timestamp, parse_timestamp, serialize_embedding,
};
use crate::types::{Fact, FactType, NewFact};

/// Store for bi-temporal facts with embedding BLOBs.
pub struct FactStore<'a> {
    conn: &'a Connection,
    embed_dim: usize,
}

pub(crate) const fn fact_type_to_str(ft: &FactType) -> &'static str {
    match ft {
        FactType::Episodic => "episodic",
        FactType::Semantic => "semantic",
        FactType::Procedural => "procedural",
    }
}

fn str_to_fact_type(s: &str) -> Result<FactType> {
    match s {
        "episodic" => Ok(FactType::Episodic),
        "semantic" => Ok(FactType::Semantic),
        "procedural" => Ok(FactType::Procedural),
        other => Err(MemoryError::NotFound(format!("unknown fact type: {other}"))),
    }
}

/// Compute blake3 hex hash of content, truncated to first 32 characters (128 bits).
fn content_hash(content: &str) -> String {
    let hash = blake3::hash(content.as_bytes());
    hash.to_hex()[..32].to_string()
}

impl<'a> FactStore<'a> {
    /// Create a new `FactStore` borrowing the given connection.
    #[must_use]
    pub const fn new(conn: &'a Connection, embed_dim: usize) -> Self {
        Self { conn, embed_dim }
    }

    /// Insert a new fact. Validates embedding dimension, computes `content_hash`
    /// via blake3 (hex, first 32 chars / 128 bits), serializes embedding as BLOB.
    /// Returns the auto-assigned id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::EmbeddingDimension` if embedding length != `embed_dim`.
    /// Returns `MemoryError::Database` on insert failure.
    pub fn insert(&self, fact: &NewFact) -> Result<i64> {
        if fact.embedding.len() != self.embed_dim {
            return Err(MemoryError::EmbeddingDimension {
                expected: self.embed_dim,
                actual: fact.embedding.len(),
            });
        }

        let hash = content_hash(&fact.content);
        let blob = serialize_embedding(&fact.embedding);
        let fact_type_str = fact_type_to_str(&fact.fact_type);
        let t_created = fact.t_created.to_rfc3339();
        let t_expired = fact.t_expired.map(|dt| dt.to_rfc3339());
        let t_valid = fact.t_valid.map(|dt| dt.to_rfc3339());
        let t_invalid = fact.t_invalid.map(|dt| dt.to_rfc3339());
        let last_accessed = fact.last_accessed.to_rfc3339();
        let metadata_str = serde_json::to_string(&fact.metadata)?;

        self.conn.execute(
            "INSERT INTO facts (content, content_hash, embedding, fact_type,
                t_created, t_expired, t_valid, t_invalid,
                source_event_id, importance, access_count, last_accessed, metadata, scope_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                fact.content,
                hash,
                blob,
                fact_type_str,
                t_created,
                t_expired,
                t_valid,
                t_invalid,
                fact.source_event_id,
                fact.importance,
                fact.access_count,
                last_accessed,
                metadata_str,
                fact.scope_id,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Get a fact by id, including full embedding deserialization.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if the id doesn't exist.
    pub fn get(&self, id: i64) -> Result<Fact> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, content_hash, embedding, fact_type,
                    t_created, t_expired, t_valid, t_invalid,
                    source_event_id, importance, access_count, last_accessed, metadata, scope_id
             FROM facts WHERE id = ?1",
        )?;
        let dim = self.embed_dim;
        let mut rows = stmt.query_map(params![id], |row| row_to_fact(row, dim))?;
        match rows.next() {
            Some(Ok(fact)) => Ok(fact),
            Some(Err(e)) => Err(e.into()),
            None => Err(MemoryError::NotFound(format!("fact {id}"))),
        }
    }

    /// List all active facts (`t_expired IS NULL`).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub fn list_active(&self) -> Result<Vec<Fact>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, content_hash, embedding, fact_type,
                    t_created, t_expired, t_valid, t_invalid,
                    source_event_id, importance, access_count, last_accessed, metadata, scope_id
             FROM facts WHERE t_expired IS NULL",
        )?;
        let dim = self.embed_dim;
        let rows = stmt.query_map([], |row| row_to_fact(row, dim))?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        Ok(facts)
    }

    /// List facts active at a given point in time (bi-temporal query).
    ///
    /// Active means: `t_expired IS NULL` AND valid at `valid_at`:
    /// `(t_valid IS NULL OR t_valid <= valid_at) AND (t_invalid IS NULL OR t_invalid > valid_at)`
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub fn list_active_at(&self, valid_at: DateTime<Utc>) -> Result<Vec<Fact>> {
        let valid_at_str = valid_at.to_rfc3339();
        let mut stmt = self.conn.prepare(
            "SELECT id, content, content_hash, embedding, fact_type,
                    t_created, t_expired, t_valid, t_invalid,
                    source_event_id, importance, access_count, last_accessed, metadata, scope_id
             FROM facts
             WHERE t_expired IS NULL
               AND (t_valid IS NULL OR t_valid <= ?1)
               AND (t_invalid IS NULL OR t_invalid > ?1)",
        )?;
        let dim = self.embed_dim;
        let rows = stmt.query_map(params![valid_at_str], |row| row_to_fact(row, dim))?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        Ok(facts)
    }

    /// Expire a fact by setting `t_expired`.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if no rows affected.
    pub fn expire(&self, id: i64, now: DateTime<Utc>) -> Result<()> {
        let now_str = now.to_rfc3339();
        let changed = self.conn.execute(
            "UPDATE facts SET t_expired = ?1 WHERE id = ?2 AND t_expired IS NULL",
            params![now_str, id],
        )?;
        if changed == 0 {
            return Err(MemoryError::NotFound(format!("fact {id}")));
        }
        Ok(())
    }

    /// Update the importance score for a fact.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if no rows affected.
    pub fn update_importance(&self, id: i64, importance: f64) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE facts SET importance = ?1 WHERE id = ?2",
            params![importance, id],
        )?;
        if changed == 0 {
            return Err(MemoryError::NotFound(format!("fact {id}")));
        }
        Ok(())
    }

    /// Increment the access count and update `last_accessed`.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if no rows affected.
    pub fn increment_access(&self, id: i64, now: DateTime<Utc>) -> Result<()> {
        let now_str = now.to_rfc3339();
        let changed = self.conn.execute(
            "UPDATE facts SET access_count = access_count + 1, last_accessed = ?1 WHERE id = ?2",
            params![now_str, id],
        )?;
        if changed == 0 {
            return Err(MemoryError::NotFound(format!("fact {id}")));
        }
        Ok(())
    }

    // --- Resume context queries ---

    /// List active facts in a specific scope, ordered by importance DESC.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub fn list_by_scope_importance(&self, scope_id: i64, limit: usize) -> Result<Vec<Fact>> {
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        let dim = self.embed_dim;
        let mut stmt = self.conn.prepare(
            "SELECT id, content, content_hash, embedding, fact_type,
                    t_created, t_expired, t_valid, t_invalid,
                    source_event_id, importance, access_count, last_accessed, metadata, scope_id
             FROM facts
             WHERE t_expired IS NULL AND scope_id = ?1
             ORDER BY importance DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![scope_id, limit_i64], |row| row_to_fact(row, dim))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(MemoryError::Database)
    }

    /// List active facts in a set of scopes with importance >= threshold,
    /// excluding specific fact IDs, ordered by importance DESC.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub fn list_by_scopes_importance(
        &self,
        scope_ids: &[i64],
        min_importance: f64,
        limit: usize,
        exclude_ids: &std::collections::HashSet<i64>,
    ) -> Result<Vec<Fact>> {
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        let scope_json = serde_json::to_string(scope_ids).expect("serialize scope_ids");
        let exclude_json = serde_json::to_string(&exclude_ids.iter().copied().collect::<Vec<_>>())
            .expect("serialize exclude_ids");
        let dim = self.embed_dim;
        let mut stmt = self.conn.prepare(
            "SELECT id, content, content_hash, embedding, fact_type,
                    t_created, t_expired, t_valid, t_invalid,
                    source_event_id, importance, access_count, last_accessed, metadata, scope_id
             FROM facts
             WHERE t_expired IS NULL
               AND scope_id IN (SELECT value FROM json_each(?1))
               AND importance >= ?2
               AND id NOT IN (SELECT value FROM json_each(?3))
             ORDER BY importance DESC
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![scope_json, min_importance, exclude_json, limit_i64],
            |row| row_to_fact(row, dim),
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(MemoryError::Database)
    }

    /// List active facts in a set of scopes, excluding specific fact IDs,
    /// ordered by `t_created` DESC (most recent first).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub fn list_by_scopes_recent(
        &self,
        scope_ids: &[i64],
        limit: usize,
        exclude_ids: &std::collections::HashSet<i64>,
    ) -> Result<Vec<Fact>> {
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        let scope_json = serde_json::to_string(scope_ids).expect("serialize scope_ids");
        let exclude_json = serde_json::to_string(&exclude_ids.iter().copied().collect::<Vec<_>>())
            .expect("serialize exclude_ids");
        let dim = self.embed_dim;
        let mut stmt = self.conn.prepare(
            "SELECT id, content, content_hash, embedding, fact_type,
                    t_created, t_expired, t_valid, t_invalid,
                    source_event_id, importance, access_count, last_accessed, metadata, scope_id
             FROM facts
             WHERE t_expired IS NULL
               AND scope_id IN (SELECT value FROM json_each(?1))
               AND id NOT IN (SELECT value FROM json_each(?2))
             ORDER BY t_created DESC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![scope_json, exclude_json, limit_i64], |row| {
            row_to_fact(row, dim)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(MemoryError::Database)
    }
}

fn row_to_fact(row: &rusqlite::Row<'_>, embed_dim: usize) -> rusqlite::Result<Fact> {
    let t_created_str: String = row.get("t_created")?;
    let t_expired_str: Option<String> = row.get("t_expired")?;
    let t_valid_str: Option<String> = row.get("t_valid")?;
    let t_invalid_str: Option<String> = row.get("t_invalid")?;
    let last_accessed_str: String = row.get("last_accessed")?;
    let fact_type_str: String = row.get("fact_type")?;
    let embedding_blob: Vec<u8> = row.get("embedding")?;
    let metadata_str: String = row.get("metadata")?;

    let t_created = parse_timestamp(&t_created_str)?;
    let t_expired = parse_optional_timestamp(t_expired_str.as_deref())?;
    let t_valid = parse_optional_timestamp(t_valid_str.as_deref())?;
    let t_invalid = parse_optional_timestamp(t_invalid_str.as_deref())?;
    let last_accessed = parse_timestamp(&last_accessed_str)?;

    let fact_type = str_to_fact_type(&fact_type_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;

    let embedding = deserialize_embedding(&embedding_blob, embed_dim).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Blob, Box::new(e))
    })?;

    let metadata: serde_json::Value = serde_json::from_str(&metadata_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;

    Ok(Fact {
        id: row.get("id")?,
        content: row.get("content")?,
        content_hash: row.get("content_hash")?,
        embedding,
        fact_type,
        t_created,
        t_expired,
        t_valid,
        t_invalid,
        source_event_id: row.get("source_event_id")?,
        importance: row.get("importance")?,
        access_count: row.get("access_count")?,
        last_accessed,
        metadata,
        scope_id: row.get("scope_id")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::{init_schema, open_memory};
    use chrono::TimeDelta;

    const DIM: usize = 768;

    fn setup() -> Connection {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        conn
    }

    fn make_fact(content: &str, embedding: Vec<f32>) -> NewFact {
        NewFact {
            content: content.into(),
            content_hash: String::new(), // store computes this
            embedding,
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
        }
    }

    #[test]
    fn insert_and_get_round_trip() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let embedding = vec![0.1_f32; DIM];
        let fact = make_fact("Rust is a systems language", embedding);
        let id = store.insert(&fact).unwrap();
        assert!(id > 0);

        let retrieved = store.get(id).unwrap();
        assert_eq!(retrieved.id, id);
        assert_eq!(retrieved.content, "Rust is a systems language");
        assert_eq!(retrieved.fact_type, FactType::Episodic);
        assert!(!retrieved.content_hash.is_empty());
    }

    #[test]
    fn embedding_blob_round_trip() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let mut embedding = vec![0.0_f32; DIM];
        for (i, val) in embedding.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            {
                *val = i as f32 * 0.001;
            }
        }
        let fact = make_fact("embedding test", embedding.clone());
        let id = store.insert(&fact).unwrap();
        let retrieved = store.get(id).unwrap();

        // Byte-exact: serialize original and compare
        let original_blob = serialize_embedding(&embedding);
        let retrieved_blob = serialize_embedding(&retrieved.embedding);
        assert_eq!(original_blob, retrieved_blob);
        assert_eq!(embedding, retrieved.embedding);
    }

    #[test]
    fn expire_excludes_from_active() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let id1 = store.insert(&make_fact("fact 1", vec![0.1; DIM])).unwrap();
        let id2 = store.insert(&make_fact("fact 2", vec![0.2; DIM])).unwrap();

        // Both active
        let active = store.list_active().unwrap();
        assert_eq!(active.len(), 2);

        // Expire fact 1
        store.expire(id1, Utc::now()).unwrap();
        let active = store.list_active().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, id2);
    }

    #[test]
    fn bi_temporal_query() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let now = Utc::now();
        let past = now - TimeDelta::hours(2);
        let future = now + TimeDelta::hours(2);

        // Fact valid from past to future
        let mut fact = make_fact("temporal fact", vec![0.1; DIM]);
        fact.t_valid = Some(past);
        fact.t_invalid = Some(future);
        store.insert(&fact).unwrap();

        // Fact valid only in the future
        let mut fact2 = make_fact("future fact", vec![0.2; DIM]);
        fact2.t_valid = Some(future);
        store.insert(&fact2).unwrap();

        // Query at "now" — only the first fact should match
        let results = store.list_active_at(now).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "temporal fact");
    }

    #[test]
    fn wrong_embedding_dimension() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let fact = make_fact("bad dim", vec![0.1; 512]); // wrong: 512 != 768
        let err = store.insert(&fact).unwrap_err();
        assert!(matches!(
            err,
            MemoryError::EmbeddingDimension {
                expected: 768,
                actual: 512
            }
        ));
    }

    #[test]
    fn update_importance_works() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let id = store.insert(&make_fact("test", vec![0.1; DIM])).unwrap();
        store.update_importance(id, 0.9).unwrap();
        let fact = store.get(id).unwrap();
        assert!((fact.importance - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn increment_access_works() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let id = store.insert(&make_fact("test", vec![0.1; DIM])).unwrap();
        let before = store.get(id).unwrap();
        assert_eq!(before.access_count, 0);

        store.increment_access(id, Utc::now()).unwrap();
        let after = store.get(id).unwrap();
        assert_eq!(after.access_count, 1);
        assert!(after.last_accessed >= before.last_accessed);
    }

    #[test]
    fn content_hash_is_blake3_hex_32() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let content = "hash test content";
        let id = store.insert(&make_fact(content, vec![0.1; DIM])).unwrap();
        let fact = store.get(id).unwrap();

        // Compute expected hash
        let expected = &blake3::hash(content.as_bytes()).to_hex()[..32];
        assert_eq!(fact.content_hash, expected);
        assert_eq!(fact.content_hash.len(), 32);
    }
}
