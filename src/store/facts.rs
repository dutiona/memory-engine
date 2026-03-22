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
                source_event_id, importance, access_count, last_accessed, metadata, scope_id,
                is_pinned, importance_score, surfaced_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, NULL)",
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
                i64::from(fact.is_pinned),
                fact.importance, // seed importance_score from base importance
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
                    source_event_id, importance, access_count, last_accessed, metadata, scope_id,
                    is_pinned, importance_score, surfaced_at
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
                    source_event_id, importance, access_count, last_accessed, metadata, scope_id,
                    is_pinned, importance_score, surfaced_at
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
                    source_event_id, importance, access_count, last_accessed, metadata, scope_id,
                    is_pinned, importance_score, surfaced_at
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
                    source_event_id, importance, access_count, last_accessed, metadata, scope_id,
                    is_pinned, importance_score, surfaced_at
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
                    source_event_id, importance, access_count, last_accessed, metadata, scope_id,
                    is_pinned, importance_score, surfaced_at
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

    /// List active pinned (unforgettable) facts, optionally filtered by scope.
    /// Pass empty slice to get all pinned facts across all scopes.
    pub fn list_pinned(&self, scope_ids: &[i64]) -> Result<Vec<Fact>> {
        let base = "SELECT id, content, content_hash, embedding, fact_type,
                        t_created, t_expired, t_valid, t_invalid,
                        source_event_id, importance, access_count, last_accessed, metadata, scope_id,
                        is_pinned, importance_score, surfaced_at
                 FROM facts WHERE t_expired IS NULL AND is_pinned = 1";
        let dim = self.embed_dim;
        if scope_ids.is_empty() {
            let sql = format!("{base} ORDER BY importance_score DESC");
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map([], |row| row_to_fact(row, dim))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        } else {
            let scope_json = serde_json::to_string(scope_ids).expect("serialize scope_ids");
            let sql = format!(
                "{base} AND scope_id IN (SELECT value FROM json_each(?1)) ORDER BY importance_score DESC"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows =
                stmt.query_map(rusqlite::params![scope_json], |row| row_to_fact(row, dim))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        }
    }

    /// List active, valid facts where `t_valid <= now` and `t_valid IS NOT NULL`.
    /// Excludes facts where `t_invalid <= now` (bi-temporally invalidated).
    pub fn list_due(&self, now: DateTime<Utc>, scope_ids: &[i64]) -> Result<Vec<Fact>> {
        let now_str = now.to_rfc3339();
        let base = "SELECT id, content, content_hash, embedding, fact_type,
                    t_created, t_expired, t_valid, t_invalid,
                    source_event_id, importance, access_count, last_accessed, metadata, scope_id,
                    is_pinned, importance_score, surfaced_at
             FROM facts
             WHERE t_expired IS NULL AND t_valid IS NOT NULL AND t_valid <= ?1
             AND (t_invalid IS NULL OR t_invalid > ?1)";
        let sql = if scope_ids.is_empty() {
            format!("{base} ORDER BY t_valid ASC")
        } else {
            let placeholders: String = scope_ids
                .iter()
                .enumerate()
                .map(|(i, _)| format!("?{}", i + 2))
                .collect::<Vec<_>>()
                .join(",");
            format!("{base} AND scope_id IN ({placeholders}) ORDER BY t_valid ASC")
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(now_str)];
        for id in scope_ids {
            params.push(Box::new(*id));
        }
        let dim = self.embed_dim;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
            row_to_fact(row, dim)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Earliest future `t_valid` among active facts with `t_valid > now`.
    /// Excludes bi-temporally invalidated facts.
    pub fn next_due_time(
        &self,
        now: DateTime<Utc>,
        scope_ids: &[i64],
    ) -> Result<Option<DateTime<Utc>>> {
        let now_str = now.to_rfc3339();
        let base = "SELECT MIN(t_valid) FROM facts
             WHERE t_expired IS NULL AND t_valid IS NOT NULL AND t_valid > ?1
             AND (t_invalid IS NULL OR t_invalid > ?1)";
        let sql = if scope_ids.is_empty() {
            base.to_string()
        } else {
            let placeholders: String = scope_ids
                .iter()
                .enumerate()
                .map(|(i, _)| format!("?{}", i + 2))
                .collect::<Vec<_>>()
                .join(",");
            format!("{base} AND scope_id IN ({placeholders})")
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(now_str)];
        for id in scope_ids {
            params.push(Box::new(*id));
        }
        let result: Option<String> =
            stmt.query_row(rusqlite::params_from_iter(params), |r| r.get(0))?;
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

    /// Stamp `surfaced_at` for facts that have not yet been surfaced.
    /// Returns the persisted `(fact_id, surfaced_at)` pairs for ALL requested IDs
    /// (including those already stamped by a prior caller).
    pub fn stamp_surfaced(
        &self,
        fact_ids: &[i64],
        now: DateTime<Utc>,
    ) -> Result<Vec<(i64, DateTime<Utc>)>> {
        if fact_ids.is_empty() {
            return Ok(vec![]);
        }
        let now_str = now.to_rfc3339();
        let ids_json = serde_json::to_string(fact_ids)?;
        // Stamp only unsurfaced facts (idempotent for already-stamped ones)
        self.conn.execute(
            "UPDATE facts SET surfaced_at = ?1 WHERE id IN (SELECT value FROM json_each(?2)) AND surfaced_at IS NULL",
            params![now_str, ids_json],
        )?;
        // Re-read persisted values for ALL requested IDs (handles concurrent races)
        let mut stmt = self.conn.prepare(
            "SELECT id, surfaced_at FROM facts WHERE id IN (SELECT value FROM json_each(?1)) AND surfaced_at IS NOT NULL",
        )?;
        let rows = stmt.query_map(params![ids_json], |row| {
            let id: i64 = row.get(0)?;
            let ts_str: String = row.get(1)?;
            let ts = parse_timestamp(&ts_str)?;
            Ok((id, ts))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Set the pinned flag on a fact.
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

    /// List ALL facts (including expired). Used for state dumps.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn list_all(&self) -> Result<Vec<Fact>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, content_hash, embedding, fact_type,
                    t_created, t_expired, t_valid, t_invalid,
                    source_event_id, importance, access_count, last_accessed, metadata, scope_id,
                    is_pinned, importance_score, surfaced_at
             FROM facts ORDER BY id ASC",
        )?;
        let dim = self.embed_dim;
        let rows = stmt.query_map([], |row| row_to_fact(row, dim))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Iterate all facts row-by-row, calling `f` for each.
    ///
    /// Unlike [`Self::list_all`], this never allocates a `Vec` — each fact is
    /// deserialized, passed to the callback, and dropped before the next
    /// row is read.  Suitable for streaming serialization of large databases.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure, or propagates any
    /// error returned by `f`.
    pub fn for_each<F>(&self, mut f: F) -> Result<()>
    where
        F: FnMut(Fact) -> Result<()>,
    {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, content_hash, embedding, fact_type,
                    t_created, t_expired, t_valid, t_invalid,
                    source_event_id, importance, access_count, last_accessed, metadata, scope_id,
                    is_pinned, importance_score, surfaced_at
             FROM facts ORDER BY id ASC",
        )?;
        let dim = self.embed_dim;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let fact = row_to_fact(row, dim)?;
            f(fact)?;
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

    /// List active facts ordered by materialized importance_score, excluding IDs in `exclude`.
    /// Pass empty `scope_ids` to query across all scopes.
    pub fn list_by_importance_score(
        &self,
        scope_ids: &[i64],
        min_score: f64,
        limit: usize,
        exclude: &std::collections::HashSet<i64>,
    ) -> Result<Vec<Fact>> {
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        let exclude_json = serde_json::to_string(&exclude.iter().copied().collect::<Vec<_>>())
            .expect("serialize exclude_ids");
        let dim = self.embed_dim;

        let base = "SELECT id, content, content_hash, embedding, fact_type,
                        t_created, t_expired, t_valid, t_invalid,
                        source_event_id, importance, access_count, last_accessed, metadata, scope_id,
                        is_pinned, importance_score, surfaced_at
                 FROM facts
                 WHERE t_expired IS NULL AND importance_score >= ?1
                   AND id NOT IN (SELECT value FROM json_each(?2))";

        if scope_ids.is_empty() {
            let sql = format!("{base} ORDER BY importance_score DESC LIMIT ?3");
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params![min_score, exclude_json, limit_i64],
                |row| row_to_fact(row, dim),
            )?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        } else {
            let scope_json = serde_json::to_string(scope_ids).expect("serialize scope_ids");
            let sql = format!(
                "{base} AND scope_id IN (SELECT value FROM json_each(?3))
                 ORDER BY importance_score DESC LIMIT ?4"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params![min_score, exclude_json, scope_json, limit_i64],
                |row| row_to_fact(row, dim),
            )?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        }
    }

    // --- Co-session edge support ---

    /// List active facts belonging to a session (via `source_event_id → events.session_id`).
    ///
    /// Returns lightweight [`SessionFact`] structs (no embeddings) sorted by id.
    /// Facts without `source_event_id` are excluded by the INNER JOIN.
    ///
    /// When `scope_ids` is non-empty, only facts whose `scope_id` is in the
    /// provided set are returned. When empty, all scopes are included
    /// (backward-compatible global lookup).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub fn list_active_by_session(
        &self,
        session_id: &str,
        scope_ids: &[i64],
    ) -> Result<Vec<SessionFact>> {
        if scope_ids.is_empty() {
            let mut stmt = self.conn.prepare(
                "SELECT f.id
                 FROM facts f
                 INNER JOIN events e ON f.source_event_id = e.id
                 WHERE e.session_id = ?1
                   AND f.t_expired IS NULL
                 ORDER BY f.id",
            )?;
            let rows = stmt.query_map(params![session_id], |row| {
                Ok(SessionFact { id: row.get(0)? })
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(MemoryError::Database)
        } else {
            let scope_json = serde_json::to_string(scope_ids)?;
            let mut stmt = self.conn.prepare(
                "SELECT f.id
                 FROM facts f
                 INNER JOIN events e ON f.source_event_id = e.id
                 WHERE e.session_id = ?1
                   AND f.t_expired IS NULL
                   AND f.scope_id IN (SELECT value FROM json_each(?2))
                 ORDER BY f.id",
            )?;
            let rows = stmt.query_map(params![session_id, scope_json], |row| {
                Ok(SessionFact { id: row.get(0)? })
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(MemoryError::Database)
        }
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
                    source_event_id, importance, access_count, last_accessed, metadata, scope_id,
                    is_pinned, importance_score, surfaced_at
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

    /// List active facts whose validity interval overlaps the given period `[start, end)`.
    ///
    /// A fact with `[t_valid, t_invalid)` overlaps `[start, end)` when:
    /// `(t_valid IS NULL OR t_valid < end) AND (t_invalid IS NULL OR t_invalid > start)`
    ///
    /// NULL `t_valid` = unbounded start (valid since creation).
    /// NULL `t_invalid` = unbounded end (still valid).
    ///
    /// Optionally filters by scope and fact type.
    /// Ordered by `importance_score DESC`.
    pub fn list_active_in_period(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        scope_ids: &[i64],
        fact_type: Option<&FactType>,
    ) -> Result<Vec<Fact>> {
        let start_str = start.to_rfc3339();
        let end_str = end.to_rfc3339();
        let dim = self.embed_dim;

        let mut conditions = vec![
            "t_expired IS NULL".to_string(),
            "(t_valid IS NULL OR t_valid < ?1)".to_string(),
            "(t_invalid IS NULL OR t_invalid > ?2)".to_string(),
        ];

        let mut param_idx = 3u32;
        let scope_json;
        if !scope_ids.is_empty() {
            scope_json = serde_json::to_string(scope_ids).expect("serialize scope_ids");
            conditions.push(format!(
                "scope_id IN (SELECT value FROM json_each(?{param_idx}))"
            ));
            param_idx += 1;
        } else {
            scope_json = String::new();
        }

        let ft_str;
        if let Some(ft) = fact_type {
            ft_str = fact_type_to_str(ft).to_string();
            conditions.push(format!("fact_type = ?{param_idx}"));
        } else {
            ft_str = String::new();
        }

        let where_clause = conditions.join(" AND ");
        let sql = format!(
            "SELECT id, content, content_hash, embedding, fact_type,
                    t_created, t_expired, t_valid, t_invalid,
                    source_event_id, importance, access_count, last_accessed, metadata, scope_id,
                    is_pinned, importance_score, surfaced_at
             FROM facts
             WHERE {where_clause}
             ORDER BY importance_score DESC"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(end_str), Box::new(start_str)];
        if !scope_ids.is_empty() {
            params.push(Box::new(scope_json));
        }
        if fact_type.is_some() {
            params.push(Box::new(ft_str));
        }

        let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
            row_to_fact(row, dim)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

/// Lightweight fact info for session-based edge creation.
/// Avoids deserializing embeddings — only carries the fact id needed for pairwise edge wiring.
#[derive(Debug, Clone)]
pub struct SessionFact {
    pub id: i64,
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
    let surfaced_at_str: Option<String> = row.get("surfaced_at")?;
    let surfaced_at = parse_optional_timestamp(surfaced_at_str.as_deref())?;

    let fact_type = str_to_fact_type(&fact_type_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;

    let embedding = deserialize_embedding(&embedding_blob, embed_dim).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Blob, Box::new(e))
    })?;

    let metadata: serde_json::Value = serde_json::from_str(&metadata_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;

    let is_pinned_i64: i64 = row.get("is_pinned")?;

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
        is_pinned: is_pinned_i64 != 0,
        importance_score: row.get("importance_score")?,
        surfaced_at,
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
            is_pinned: false,
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
    fn list_pinned_returns_only_pinned_active_facts() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        let mut pinned = make_fact("pinned fact", vec![0.1; DIM]);
        pinned.is_pinned = true;
        fs.insert(&pinned).unwrap();
        fs.insert(&make_fact("normal fact", vec![0.2; DIM]))
            .unwrap();

        let result = fs.list_pinned(&[]).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].is_pinned);
    }

    #[test]
    fn list_due_surfaces_facts_with_past_t_valid() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        let now = Utc::now();

        let mut past = make_fact("past reminder", vec![0.1; DIM]);
        past.t_valid = Some(now - chrono::Duration::hours(1));
        fs.insert(&past).unwrap();

        let mut future = make_fact("future reminder", vec![0.2; DIM]);
        future.t_valid = Some(now + chrono::Duration::hours(1));
        fs.insert(&future).unwrap();

        fs.insert(&make_fact("regular fact", vec![0.3; DIM]))
            .unwrap();

        let result = fs.list_due(now, &[]).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].content.contains("past"));
    }

    #[test]
    fn next_due_time_returns_earliest_future_t_valid() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        let now = Utc::now();

        assert!(fs.next_due_time(now, &[]).unwrap().is_none());

        let mut future = make_fact("reminder", vec![0.1; DIM]);
        future.t_valid = Some(now + chrono::Duration::hours(2));
        fs.insert(&future).unwrap();

        let mut sooner = make_fact("sooner reminder", vec![0.2; DIM]);
        sooner.t_valid = Some(now + chrono::Duration::hours(1));
        fs.insert(&sooner).unwrap();

        let next = fs.next_due_time(now, &[]).unwrap().unwrap();
        assert!(next < now + chrono::Duration::hours(2));
    }

    #[test]
    fn set_pinned_toggles_flag() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        let id = fs.insert(&make_fact("toggleable", vec![0.1; DIM])).unwrap();

        let fact = fs.get(id).unwrap();
        assert!(!fact.is_pinned);

        fs.set_pinned(id, true).unwrap();
        let fact = fs.get(id).unwrap();
        assert!(fact.is_pinned);

        fs.set_pinned(id, false).unwrap();
        let fact = fs.get(id).unwrap();
        assert!(!fact.is_pinned);
    }

    // --- Co-session edge support tests ---

    /// Insert an event with a `session_id`, return the event id.
    fn insert_event(conn: &Connection, session_id: Option<&str>) -> i64 {
        conn.execute(
            "INSERT INTO events (timestamp, event_type, payload, source, session_id, scope_id, origin_node_id, sequence_id)
             VALUES (datetime('now'), 'interaction', '{}', 'test', ?1, 1, 'local', 0)",
            params![session_id],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// Insert a fact linked to an event via `source_event_id`.
    fn insert_fact_with_event(conn: &Connection, content: &str, event_id: i64) -> i64 {
        let store = FactStore::new(conn, DIM);
        let mut fact = make_fact(content, vec![0.1; DIM]);
        fact.source_event_id = Some(event_id);
        store.insert(&fact).unwrap()
    }

    #[test]
    fn list_active_by_session_returns_matching() {
        let conn = setup();
        let e1 = insert_event(&conn, Some("s1"));
        let e2 = insert_event(&conn, Some("s1"));
        let e3 = insert_event(&conn, Some("s2")); // different session

        let f1 = insert_fact_with_event(&conn, "fact a", e1);
        let f2 = insert_fact_with_event(&conn, "fact b", e2);
        let _f3 = insert_fact_with_event(&conn, "fact c", e3);

        let store = FactStore::new(&conn, DIM);
        let session_facts = store.list_active_by_session("s1", &[]).unwrap();
        assert_eq!(session_facts.len(), 2);
        assert_eq!(session_facts[0].id, f1);
        assert_eq!(session_facts[1].id, f2);
    }

    #[test]
    fn list_active_by_session_excludes_expired() {
        let conn = setup();
        let e1 = insert_event(&conn, Some("s1"));
        let e2 = insert_event(&conn, Some("s1"));

        let f1 = insert_fact_with_event(&conn, "active", e1);
        let f2 = insert_fact_with_event(&conn, "will expire", e2);

        let store = FactStore::new(&conn, DIM);
        store.expire(f2, Utc::now()).unwrap();

        let session_facts = store.list_active_by_session("s1", &[]).unwrap();
        assert_eq!(session_facts.len(), 1);
        assert_eq!(session_facts[0].id, f1);
    }

    #[test]
    fn list_active_by_session_empty_for_unknown() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let result = store.list_active_by_session("nonexistent", &[]).unwrap();
        assert!(result.is_empty());
    }

    fn insert_scoped_fact_with_event(
        conn: &Connection,
        content: &str,
        event_id: i64,
        scope_id: i64,
    ) -> i64 {
        let store = FactStore::new(conn, DIM);
        let mut fact = make_fact(content, vec![0.1; DIM]);
        fact.source_event_id = Some(event_id);
        fact.scope_id = scope_id;
        store.insert(&fact).unwrap()
    }

    #[test]
    fn list_active_by_session_filters_by_scope() {
        let conn = setup();

        // Create child scopes under root (id=1)
        conn.execute(
            "INSERT INTO scopes (id, parent_id, label, depth) VALUES (2, 1, 'alice', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scopes (id, parent_id, label, depth) VALUES (3, 1, 'bob', 1)",
            [],
        )
        .unwrap();

        let e1 = insert_event(&conn, Some("s1"));
        let e2 = insert_event(&conn, Some("s1"));
        let e3 = insert_event(&conn, Some("s1"));

        let f1 = insert_scoped_fact_with_event(&conn, "alice fact", e1, 2);
        let f2 = insert_scoped_fact_with_event(&conn, "alice fact 2", e2, 2);
        let _f3 = insert_scoped_fact_with_event(&conn, "bob fact", e3, 3);

        let store = FactStore::new(&conn, DIM);

        // Filter by alice's scope
        let alice_facts = store.list_active_by_session("s1", &[2]).unwrap();
        assert_eq!(alice_facts.len(), 2);
        assert_eq!(alice_facts[0].id, f1);
        assert_eq!(alice_facts[1].id, f2);

        // Filter by bob's scope
        let bob_facts = store.list_active_by_session("s1", &[3]).unwrap();
        assert_eq!(bob_facts.len(), 1);

        // No filter (empty slice) returns all
        let all_facts = store.list_active_by_session("s1", &[]).unwrap();
        assert_eq!(all_facts.len(), 3);

        // Multiple scopes
        let both = store.list_active_by_session("s1", &[2, 3]).unwrap();
        assert_eq!(both.len(), 3);
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

    // --- list_active_in_period tests ---

    #[test]
    fn period_overlap_fully_contained() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        let now = Utc::now();

        let mut fact = make_fact("contained", vec![0.1; DIM]);
        fact.t_valid = Some(now - TimeDelta::hours(2));
        fact.t_invalid = Some(now - TimeDelta::hours(1));
        fs.insert(&fact).unwrap();

        // Period [now-3h, now) fully contains [now-2h, now-1h)
        let results = fs
            .list_active_in_period(now - TimeDelta::hours(3), now, &[], None)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "contained");
    }

    #[test]
    fn period_overlap_partial() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        let now = Utc::now();

        let mut fact = make_fact("partial", vec![0.1; DIM]);
        fact.t_valid = Some(now - TimeDelta::hours(2));
        fact.t_invalid = Some(now + TimeDelta::hours(2));
        fs.insert(&fact).unwrap();

        // Period [now-1h, now+1h) partially overlaps [now-2h, now+2h)
        let results = fs
            .list_active_in_period(
                now - TimeDelta::hours(1),
                now + TimeDelta::hours(1),
                &[],
                None,
            )
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn period_no_overlap_before() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        let now = Utc::now();

        let mut fact = make_fact("future", vec![0.1; DIM]);
        fact.t_valid = Some(now + TimeDelta::hours(1));
        fact.t_invalid = Some(now + TimeDelta::hours(3));
        fs.insert(&fact).unwrap();

        // Period [now-2h, now-1h) is entirely before [now+1h, now+3h)
        let results = fs
            .list_active_in_period(
                now - TimeDelta::hours(2),
                now - TimeDelta::hours(1),
                &[],
                None,
            )
            .unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn period_null_t_valid_matches_any() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        let now = Utc::now();

        // No t_valid = unbounded start → always overlaps any period
        let fact = make_fact("unbounded start", vec![0.1; DIM]);
        fs.insert(&fact).unwrap();

        let results = fs
            .list_active_in_period(
                now - TimeDelta::hours(1),
                now + TimeDelta::hours(1),
                &[],
                None,
            )
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn period_null_t_invalid_matches_any() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        let now = Utc::now();

        let mut fact = make_fact("unbounded end", vec![0.1; DIM]);
        fact.t_valid = Some(now - TimeDelta::hours(2));
        // t_invalid is None → still valid → unbounded end
        fs.insert(&fact).unwrap();

        let results = fs
            .list_active_in_period(now - TimeDelta::hours(1), now, &[], None)
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn period_with_scope_filter() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        let now = Utc::now();

        // Create a second scope via SQL (root scope 1 exists from schema init)
        conn.execute(
            "INSERT INTO scopes (id, parent_id, label, depth) VALUES (2, 1, 'child', 1)",
            [],
        )
        .unwrap();

        let mut f1 = make_fact("scope 1", vec![0.1; DIM]);
        f1.scope_id = 1;
        f1.t_valid = Some(now - TimeDelta::hours(1));
        fs.insert(&f1).unwrap();

        let mut f2 = make_fact("scope 2", vec![0.2; DIM]);
        f2.scope_id = 2;
        f2.t_valid = Some(now - TimeDelta::hours(1));
        fs.insert(&f2).unwrap();

        let results = fs
            .list_active_in_period(
                now - TimeDelta::hours(2),
                now + TimeDelta::hours(1),
                &[1],
                None,
            )
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "scope 1");
    }

    #[test]
    fn period_with_fact_type_filter() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        let now = Utc::now();

        let mut f1 = make_fact("episodic", vec![0.1; DIM]);
        f1.t_valid = Some(now - TimeDelta::hours(1));
        fs.insert(&f1).unwrap();

        let mut f2 = make_fact("semantic", vec![0.2; DIM]);
        f2.fact_type = FactType::Semantic;
        f2.t_valid = Some(now - TimeDelta::hours(1));
        fs.insert(&f2).unwrap();

        let results = fs
            .list_active_in_period(
                now - TimeDelta::hours(2),
                now + TimeDelta::hours(1),
                &[],
                Some(&FactType::Semantic),
            )
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "semantic");
    }
}
