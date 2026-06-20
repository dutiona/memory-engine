use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};

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

/// The full ordered column list selected by every fact-reading query.
///
/// Centralized so the projection stays in lockstep with [`row_to_fact`], which
/// reads columns by name. The column set and order are identical to the
/// previous per-query literals (only inter-column whitespace is normalized;
/// `SQLite` ignores it, so the queries are behaviorally unchanged).
const FACT_COLUMNS: &str = "id, content, content_hash, embedding, fact_type, \
     t_created, t_expired, t_valid, t_invalid, \
     source_event_id, importance, access_count, last_accessed, metadata, scope_id, \
     is_pinned, importance_score, surfaced_at";

/// The minimal column set needed to score a fact's importance during a prune
/// pass — identity, decay inputs, and the pin flag. Deliberately excludes the
/// heavy `content`, `embedding`, and `metadata` columns. Kept in lockstep with
/// [`row_to_scoring_row`].
const SCORING_COLUMNS: &str = "id, fact_type, last_accessed, access_count, importance, is_pinned";

// `FactScoringRow` and `SessionFact` relocated to `crate::types` (#629 — the
// dialect-free storage port must not reference types living inside the SQLite
// store). Shims preserve the original `crate::store::facts::{FactScoringRow,
// SessionFact}` paths (e.g. `forgetting::policy` keeps its import + trait impl).
pub use crate::types::{FactScoringRow, SessionFact};

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "used as Option<&FactType>.map(fact_type_to_str) in search paths; changing to by-value would break those call sites"
)]
pub const fn fact_type_to_str(ft: &FactType) -> &'static str {
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

#[allow(dead_code)] // complete CRUD API — not all methods called through engine facade yet
impl<'a> FactStore<'a> {
    /// Create a new `FactStore` borrowing the given connection.
    #[must_use]
    pub(crate) const fn new(conn: &'a Connection, embed_dim: usize) -> Self {
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

    /// Insert a fact, or reinforce an existing active fact with identical content
    /// in the same scope.
    ///
    /// A bulk backfill (autonomous-agent-project#53) re-encounters recurring facts
    /// (conventions, decisions) across many sessions; inserting each occurrence
    /// duplicates rows. Instead, if an active (`t_expired IS NULL`) fact with the
    /// same `content_hash` **and** identical content exists in the same `scope_id`,
    /// this reinforces it — increments `access_count`, advances `last_accessed` to
    /// the later timestamp, and rolls `t_created` back to the earlier one (so
    /// first-seen and last-seen are independent of import order) — and returns its
    /// id. Otherwise it inserts a new row.
    ///
    /// Reinforcement also keeps the **strongest** per-occurrence signal: `is_pinned`
    /// and `importance` move to the max across occurrences (a fact later judged pinned
    /// or more important stays so). Frequency does not inflate `importance` — only the
    /// independent per-occurrence score does.
    ///
    /// This instantiates "memory decays unless reinforced": a re-mention strengthens
    /// the recency/frequency signal rather than spawning a duplicate. It is
    /// deliberately **not** a global `UNIQUE` constraint — identical content in
    /// different scopes is legitimate, and the reinforce-vs-insert decision is
    /// per-scope and active-only.
    ///
    /// The lookup-then-write is non-atomic but safe under the engine's single-writer
    /// pool (one mutex-guarded write connection); a caller outside that serialization
    /// must provide its own.
    ///
    /// Returns `(id, reinforced)`: `reinforced` is `true` when an existing fact was
    /// reinforced instead of a new row inserted.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::EmbeddingDimension` if embedding length != `embed_dim`.
    /// Returns `MemoryError::Database` on query or insert failure.
    pub fn insert_or_reinforce(&self, fact: &NewFact) -> Result<(i64, bool)> {
        if fact.embedding.len() != self.embed_dim {
            return Err(MemoryError::EmbeddingDimension {
                expected: self.embed_dim,
                actual: fact.embedding.len(),
            });
        }
        let hash = content_hash(&fact.content);
        // content_hash is index-backed (idx_facts_hash); the content equality guard
        // rejects the astronomically-unlikely 128-bit hash collision.
        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM facts
                 WHERE content_hash = ?1 AND content = ?2 AND scope_id = ?3 AND t_expired IS NULL
                 ORDER BY id LIMIT 1",
                params![hash, fact.content, fact.scope_id],
                |row| row.get(0),
            )
            .optional()?;

        match existing {
            Some(id) => {
                // to_rfc3339() emits a `+00:00` offset with AutoSi-padded (0/3/6/9-digit)
                // fractions; `+` (0x2B) sorts before any digit, so a shorter fraction
                // (chronologically <=) always sorts first — lexicographic == chronological,
                // and SQL min/max give earliest/latest. (Same invariant the rest of the
                // store relies on for t_created/t_valid string comparison.)
                let t_created = fact.t_created.to_rfc3339();
                let last_accessed = fact.last_accessed.to_rfc3339();
                self.conn.execute(
                    "UPDATE facts
                     SET access_count = access_count + 1,
                         t_created = min(t_created, ?1),
                         last_accessed = max(last_accessed, ?2),
                         is_pinned = max(is_pinned, ?3),
                         importance = max(importance, ?4)
                     WHERE id = ?5",
                    params![
                        t_created,
                        last_accessed,
                        i64::from(fact.is_pinned),
                        fact.importance,
                        id
                    ],
                )?;
                Ok((id, true))
            }
            None => Ok((self.insert(fact)?, false)),
        }
    }

    /// Get a fact by id, including full embedding deserialization.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if the id doesn't exist.
    pub fn get(&self, id: i64) -> Result<Fact> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {FACT_COLUMNS} FROM facts WHERE id = ?1"))?;
        let dim = self.embed_dim;
        let mut rows = stmt.query_map(params![id], |row| row_to_fact(row, dim))?;
        match rows.next() {
            Some(Ok(fact)) => Ok(fact),
            Some(Err(e)) => Err(e.into()),
            None => Err(MemoryError::NotFound(format!("fact {id}"))),
        }
    }

    /// Get multiple facts by id in a single round-trip.
    ///
    /// Materializes all requested ids with one `WHERE id IN (...)` query
    /// (via `json_each`, so `SQLite`'s bound-variable limit is never hit
    /// regardless of `ids` length) and returns them keyed by id. Missing
    /// ids are simply absent from the map — callers reconcile against the
    /// requested set (e.g. to preserve a ranked order and skip dropped rows).
    ///
    /// Returns an empty map for an empty `ids` slice (no query issued).
    /// Order of the returned map is unspecified; callers that need a
    /// particular order must re-index by id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub fn get_many(&self, ids: &[i64]) -> Result<std::collections::HashMap<i64, Fact>> {
        if ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let ids_json = serde_json::to_string(ids)?;
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {FACT_COLUMNS} FROM facts WHERE id IN (SELECT value FROM json_each(?1))"
        ))?;
        let dim = self.embed_dim;
        let rows = stmt.query_map(params![ids_json], |row| row_to_fact(row, dim))?;
        let mut out = std::collections::HashMap::with_capacity(ids.len());
        for row in rows {
            let fact = row?;
            out.insert(fact.id, fact);
        }
        Ok(out)
    }

    /// List active facts (`t_expired IS NULL`), optionally limited.
    ///
    /// When `limit` is `Some(n)`, a SQL `LIMIT n` clause is pushed into the
    /// query so that at most `n` rows are materialized. `None` returns all.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub fn list_active(&self, limit: Option<usize>) -> Result<Vec<Fact>> {
        let base = format!("SELECT {FACT_COLUMNS} FROM facts WHERE t_expired IS NULL");
        let limit_i64: i64 = limit.map_or(-1, |n| i64::try_from(n).unwrap_or(i64::MAX));
        let sql = format!("{base} LIMIT ?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let dim = self.embed_dim;
        let rows = stmt.query_map(params![limit_i64], |row| row_to_fact(row, dim))?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        Ok(facts)
    }

    /// List the importance-scoring projection of all active facts
    /// (`t_expired IS NULL`).
    ///
    /// Like [`list_active`](Self::list_active) but selects only the columns the
    /// forgetting pass needs (see [`FactScoringRow`]), so the working set never
    /// materializes `content`, `embedding`, or `metadata`. Used by `prune`,
    /// which must scan the entire active set to compute global importance.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub fn list_active_scoring(&self) -> Result<Vec<FactScoringRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SCORING_COLUMNS} FROM facts WHERE t_expired IS NULL"
        ))?;
        let rows = stmt.query_map([], row_to_scoring_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
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
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {FACT_COLUMNS} FROM facts
             WHERE t_expired IS NULL
               AND (t_valid IS NULL OR t_valid <= ?1)
               AND (t_invalid IS NULL OR t_invalid > ?1)"
        ))?;
        let dim = self.embed_dim;
        let rows = stmt.query_map(params![valid_at_str], |row| row_to_fact(row, dim))?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        Ok(facts)
    }

    /// List dormant facts: active, non-pinned, low importance, temporally valid.
    ///
    /// Used by `sample_dormant()` for resonance queries. Returns facts with
    /// `importance_score < threshold` that pass temporal validity checks.
    ///
    /// When `scope_ids` is `Some`, only facts in those scopes are returned.
    /// When `None`, all scopes are searched.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub fn list_dormant(
        &self,
        importance_threshold: f64,
        scope_ids: Option<&[i64]>,
    ) -> Result<Vec<Fact>> {
        let now_str = Utc::now().to_rfc3339();

        let (scope_clause, scope_params) = match scope_ids {
            Some(ids) if !ids.is_empty() => {
                let placeholders: Vec<String> = ids
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format!("?{}", i + 3))
                    .collect();
                let clause = format!(" AND scope_id IN ({})", placeholders.join(","));
                let params: Vec<Box<dyn rusqlite::types::ToSql>> = ids
                    .iter()
                    .map(|id| Box::new(*id) as Box<dyn rusqlite::types::ToSql>)
                    .collect();
                (clause, params)
            }
            _ => (String::new(), Vec::new()),
        };

        let sql = format!(
            "SELECT {FACT_COLUMNS} FROM facts
             WHERE t_expired IS NULL
               AND is_pinned = 0
               AND importance_score < ?1
               AND (t_valid IS NULL OR t_valid <= ?2)
               AND (t_invalid IS NULL OR t_invalid > ?2){scope_clause}"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let dim = self.embed_dim;

        // Build params: [threshold, now, ...scope_ids]
        let mut all_params: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(importance_threshold), Box::new(now_str)];
        all_params.extend(scope_params);
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            all_params.iter().map(std::convert::AsRef::as_ref).collect();

        let rows = stmt.query_map(param_refs.as_slice(), |row| row_to_fact(row, dim))?;
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
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {FACT_COLUMNS} FROM facts
             WHERE t_expired IS NULL AND scope_id = ?1
             ORDER BY importance DESC
             LIMIT ?2"
        ))?;
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
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {FACT_COLUMNS} FROM facts
             WHERE t_expired IS NULL
               AND scope_id IN (SELECT value FROM json_each(?1))
               AND importance >= ?2
               AND id NOT IN (SELECT value FROM json_each(?3))
             ORDER BY importance DESC
             LIMIT ?4"
        ))?;
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
        let base =
            format!("SELECT {FACT_COLUMNS} FROM facts WHERE t_expired IS NULL AND is_pinned = 1");
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
        let base = format!(
            "SELECT {FACT_COLUMNS} FROM facts
             WHERE t_expired IS NULL AND t_valid IS NOT NULL AND t_valid <= ?1
             AND (t_invalid IS NULL OR t_invalid > ?1)"
        );
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
            Some(s) => Ok(Some(parse_timestamp(&s)?)),
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
            rusqlite::params![i64::from(pinned), id],
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
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {FACT_COLUMNS} FROM facts ORDER BY id ASC"))?;
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
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {FACT_COLUMNS} FROM facts ORDER BY id ASC"))?;
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

    /// Shallow-merge a JSON object `patch` into a fact's `metadata` column.
    ///
    /// Existing keys are overwritten by colliding patch keys; other keys are
    /// preserved. Used by the dream-cycle to stamp the `dream_cycle` marker and
    /// `quarantine` annotations without a schema migration (the `metadata` column
    /// is free-form JSON).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Internal` if `patch` is not a JSON object, and
    /// `MemoryError::NotFound` if no fact with `id` exists.
    pub(crate) fn merge_metadata(&self, id: i64, patch: &serde_json::Value) -> Result<()> {
        let patch_obj = patch.as_object().ok_or_else(|| {
            MemoryError::Internal("merge_metadata patch must be a JSON object".to_owned())
        })?;

        let current: Option<String> = self
            .conn
            .query_row(
                "SELECT metadata FROM facts WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        let current = current.ok_or_else(|| MemoryError::NotFound(format!("fact {id}")))?;

        let mut value: serde_json::Value = serde_json::from_str(&current)?;
        if !value.is_object() {
            value = serde_json::Value::Object(serde_json::Map::new());
        }
        if let serde_json::Value::Object(ref mut map) = value {
            for (k, v) in patch_obj {
                map.insert(k.clone(), v.clone());
            }
        }

        let new_str = serde_json::to_string(&value)?;
        let changed = self.conn.execute(
            "UPDATE facts SET metadata = ?1 WHERE id = ?2",
            params![new_str, id],
        )?;
        if changed == 0 {
            return Err(MemoryError::NotFound(format!("fact {id}")));
        }
        Ok(())
    }

    /// Stamp each fact with the `dream_cycle` marker so a later cycle excludes it
    /// (idempotency). No schema change — the marker lives in `metadata` JSON.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if any id does not exist.
    pub(crate) fn mark_dream_cycled(
        &self,
        ids: &[i64],
        cycle_id: u64,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let marker = serde_json::json!({
            "dream_cycle": { "cycled_at": now.to_rfc3339(), "cycle_id": cycle_id }
        });
        for &id in ids {
            self.merge_metadata(id, &marker)?;
        }
        Ok(())
    }

    /// Like [`Self::list_active_in_period`] but excludes facts already stamped with
    /// the `dream_cycle` marker — the cycle's input-selection query.
    ///
    /// The exclusion is pushed into SQL (`json_extract(metadata, '$.dream_cycle') IS
    /// NULL`) so already-dream-cycled rows — and their embedding BLOBs — are never
    /// materialized, rather than fetched and discarded Rust-side.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub(crate) fn list_undreamt_in_period(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        scope_ids: &[i64],
        fact_type: Option<&FactType>,
    ) -> Result<Vec<Fact>> {
        // `json_type` (not `json_extract`) keeps this bit-for-bit equivalent to the
        // prior Rust filter `metadata.get("dream_cycle").is_none()`: `json_extract`
        // collapses an absent key and a present-`null` value both to SQL NULL, whereas
        // serde's `get().is_none()` is true only for an *absent* key. `json_type`
        // returns the text `'null'` for a present-null value (so `IS NULL` is false)
        // and SQL NULL only when the key is absent or the path doesn't resolve.
        self.list_active_in_period_inner(
            start,
            end,
            scope_ids,
            fact_type,
            &["json_type(metadata, '$.dream_cycle') IS NULL"],
        )
    }

    /// Highest `id` among **caller-written** active facts — the #209 caller-write
    /// cursor probe. "Caller-written" = active (`t_expired IS NULL`), not pinned
    /// (`is_pinned = 0`, so promoted wisdom is excluded), and not dream-cycled
    /// (`json_type(metadata, '$.dream_cycle') IS NULL`, so the cycle's own marked
    /// outputs are excluded — this is why invariant M must mark every cycle-created
    /// fact). Returns `None` when no such fact exists (empty or fully-excluded table),
    /// which the caller treats as "no caller writes".
    ///
    /// Reads no embedding BLOBs — a scalar `MAX(id)`, not a row materialization. Uses
    /// `json_type` (not `json_extract`) to match `list_undreamt_in_period`'s
    /// absent-vs-present-null distinction.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub fn max_caller_written_fact_id(&self) -> Result<Option<i64>> {
        self.conn
            .query_row(
                "SELECT MAX(id) FROM facts
                 WHERE t_expired IS NULL
                   AND is_pinned = 0
                   AND json_type(metadata, '$.dream_cycle') IS NULL",
                [],
                |r| r.get::<_, Option<i64>>(0),
            )
            .map_err(MemoryError::Database)
    }

    /// List active facts ordered by materialized `importance_score`, excluding IDs in `exclude`.
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

        let base = format!(
            "SELECT {FACT_COLUMNS} FROM facts
                 WHERE t_expired IS NULL AND importance_score >= ?1
                   AND id NOT IN (SELECT value FROM json_each(?2))"
        );

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
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {FACT_COLUMNS} FROM facts
             WHERE t_expired IS NULL
               AND scope_id IN (SELECT value FROM json_each(?1))
               AND id NOT IN (SELECT value FROM json_each(?2))
             ORDER BY t_created DESC
             LIMIT ?3"
        ))?;
        let rows = stmt.query_map(params![scope_json, exclude_json, limit_i64], |row| {
            row_to_fact(row, dim)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(MemoryError::Database)
    }

    /// List active facts in `scope_ids` whose `metadata` JSON carries the top-level
    /// key `marker_key` with a non-null value, ordered by `t_created` DESC (newest
    /// first), capped at `limit`.
    ///
    /// Matched with `json_extract(metadata, '$.<marker_key>') IS NOT NULL`.
    /// `json_extract` collapses an **absent** key and a present-`null` value both to
    /// SQL NULL, returning the value only when the key is present with a non-null
    /// value — exactly "key present with a non-null value" (the marker is always a
    /// non-null object, e.g. `{"flushed_at": …}`). (Note: `json_type` would NOT work
    /// here — it returns the text `'null'` for a present-null value, so
    /// `json_type(...) IS NOT NULL` would wrongly include `{"<key>": null}`.)
    ///
    /// Contrast `list_undreamt_in_period`, which uses `json_type(...) IS NULL` for the
    /// *complementary* (absence) predicate — there `json_type` is the correct idiom
    /// because `json_extract(...) IS NULL` would conflate absent with present-`null`.
    /// The two methods diverge by design: `json_extract` for presence, `json_type` for
    /// absence.
    ///
    /// `marker_key` **MUST** be a trusted caller-supplied literal (an engine const
    /// such as [`INSIGHT_MARKER_KEY`](crate::INSIGHT_MARKER_KEY)): it is interpolated
    /// into the SQL JSON path, **never bound**, so it must never carry client input.
    /// A runtime guard rejects a non-identifier key in **all** build profiles
    /// (not just `debug`), since the key is interpolated into SQL.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Conflict(ConflictError::QueryValidation)` if
    /// `marker_key` is not a non-empty `[A-Za-z0-9_]+` identifier.
    /// Returns `MemoryError::Database` on query failure.
    pub fn list_active_by_metadata_key_recent(
        &self,
        scope_ids: &[i64],
        marker_key: &str,
        limit: usize,
    ) -> Result<Vec<Fact>> {
        // Defense in depth: although every current caller passes a trusted engine
        // const, this is a runtime check (not a `debug_assert`, which compiles out in
        // release) because `marker_key` is interpolated into the SQL JSON path. A
        // future caller wiring client input here cannot silently open an injection.
        if marker_key.is_empty()
            || !marker_key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(MemoryError::Conflict(
                crate::error::ConflictError::QueryValidation(format!(
                    "marker_key must be a non-empty alphanumeric/underscore identifier, got {marker_key:?}"
                )),
            ));
        }
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        let scope_json = serde_json::to_string(scope_ids).expect("serialize scope_ids");
        let dim = self.embed_dim;
        // marker_key is a trusted const (see doc) — interpolated, not bound, because
        // `json_extract` paths cannot be parameterized portably. `json_extract` (not
        // `json_type`) gives "present with a non-null value" semantics.
        let marker_predicate = format!("json_extract(metadata, '$.{marker_key}') IS NOT NULL");
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {FACT_COLUMNS} FROM facts
             WHERE t_expired IS NULL
               AND scope_id IN (SELECT value FROM json_each(?1))
               AND {marker_predicate}
             ORDER BY t_created DESC
             LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![scope_json, limit_i64], |row| row_to_fact(row, dim))?;
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
        self.list_active_in_period_inner(start, end, scope_ids, fact_type, &[])
    }

    /// Shared core for [`Self::list_active_in_period`] and
    /// [`Self::list_undreamt_in_period`]. `extra_conditions` are additional
    /// **non-parameterized** SQL predicates `ANDed` into the `WHERE` clause (they
    /// must not reference bind parameters — they are appended verbatim, so callers
    /// pass only trusted literals like a `json_extract(metadata, …) IS NULL` filter).
    fn list_active_in_period_inner(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        scope_ids: &[i64],
        fact_type: Option<&FactType>,
        extra_conditions: &[&str],
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
        if scope_ids.is_empty() {
            scope_json = String::new();
        } else {
            scope_json = serde_json::to_string(scope_ids).expect("serialize scope_ids");
            conditions.push(format!(
                "scope_id IN (SELECT value FROM json_each(?{param_idx}))"
            ));
            param_idx += 1;
        }

        let ft_str;
        if let Some(ft) = fact_type {
            ft_str = fact_type_to_str(ft).to_string();
            conditions.push(format!("fact_type = ?{param_idx}"));
        } else {
            ft_str = String::new();
        }

        // Non-parameterized predicates (e.g. the dream-cycle exclusion filter) are
        // appended last so they never disturb the `?N` bind-parameter numbering above.
        for cond in extra_conditions {
            conditions.push((*cond).to_owned());
        }

        let where_clause = conditions.join(" AND ");
        // `, id ASC` is a deterministic tiebreaker: `importance_score` values tie often
        // (e.g. after decay), and SQLite leaves tie order otherwise unspecified. A stable
        // order makes downstream DBSCAN clustering reproducible across DB states.
        let sql = format!(
            "SELECT {FACT_COLUMNS} FROM facts
             WHERE {where_clause}
             ORDER BY importance_score DESC, id ASC"
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

    /// Hard-delete facts by ID. Used after successful archival.
    ///
    /// Breaks from the soft-deletion pattern because the `.pak` file IS the
    /// preservation. The FTS5 DELETE trigger fires automatically via the
    /// `facts_fts_ad` trigger, keeping the FTS index consistent.
    ///
    /// Returns the number of rows deleted.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn hard_delete_ids(&self, ids: &[i64]) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let placeholders: String = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("DELETE FROM facts WHERE id IN ({placeholders})");
        let params: Vec<&dyn rusqlite::types::ToSql> = ids
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        let deleted = self.conn.execute(&sql, params.as_slice())?;
        Ok(deleted)
    }

    /// List archive candidates: non-pinned facts whose system-time validity has
    /// expired before `expired_before` (`t_expired IS NOT NULL AND t_expired <
    /// expired_before`), ordered by id ascending.
    ///
    /// This pushes the archive selection predicate into SQL so the engine never
    /// materializes every fact (and every embedding BLOB) just to discard the
    /// ones that don't qualify. Equivalent to the prior Rust-side filter
    /// `!f.is_pinned && f.t_expired.is_some_and(|te| te < expired_before)` over
    /// `list_all()`.
    ///
    /// The `t_expired < ?` comparison is a string comparison on the stored
    /// rfc3339 timestamps; this is correct because `DateTime::to_rfc3339()` emits
    /// a lexicographically-ordered encoding (the same invariant
    /// [`insert_or_reinforce`](Self::insert_or_reinforce) relies on for its
    /// `min`/`max` over `t_created`).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn list_archive_candidates(&self, expired_before: DateTime<Utc>) -> Result<Vec<Fact>> {
        let cutoff = expired_before.to_rfc3339();
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {FACT_COLUMNS} FROM facts
             WHERE is_pinned = 0
               AND t_expired IS NOT NULL
               AND t_expired < ?1
             ORDER BY id ASC"
        ))?;
        let dim = self.embed_dim;
        let rows = stmt.query_map(params![cutoff], |row| row_to_fact(row, dim))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
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

/// Map a [`SCORING_COLUMNS`] row to a [`FactScoringRow`]. Kept in lockstep with
/// the column list; reads by name so column order is irrelevant.
fn row_to_scoring_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FactScoringRow> {
    let fact_type_str: String = row.get("fact_type")?;
    let last_accessed_str: String = row.get("last_accessed")?;
    let last_accessed = parse_timestamp(&last_accessed_str)?;
    let fact_type = str_to_fact_type(&fact_type_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let is_pinned_i64: i64 = row.get("is_pinned")?;

    Ok(FactScoringRow {
        id: row.get("id")?,
        fact_type,
        last_accessed,
        access_count: row.get("access_count")?,
        importance: row.get("importance")?,
        is_pinned: is_pinned_i64 != 0,
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

    /// #209 cursor probe: `max_caller_written_fact_id` returns the highest id among
    /// active, unpinned, non-dream-marked facts — excluding pinned (promoted wisdom),
    /// dream-marked (the cycle's own outputs), and expired facts.
    #[test]
    fn max_caller_written_fact_id_excludes_pinned_marked_expired() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);

        assert_eq!(
            store.max_caller_written_fact_id().unwrap(),
            None,
            "empty table → None"
        );

        // Two caller writes — the max wins.
        let c1 = store
            .insert(&make_fact("caller one", vec![0.1; DIM]))
            .unwrap();
        let c2 = store
            .insert(&make_fact("caller two", vec![0.2; DIM]))
            .unwrap();
        assert!(c2 > c1);
        assert_eq!(store.max_caller_written_fact_id().unwrap(), Some(c2));

        // A pinned fact (promoted wisdom) with a HIGHER id must be excluded.
        let mut pinned = make_fact("pinned wisdom", vec![0.3; DIM]);
        pinned.is_pinned = true;
        let p = store.insert(&pinned).unwrap();
        assert!(p > c2);
        assert_eq!(
            store.max_caller_written_fact_id().unwrap(),
            Some(c2),
            "pinned fact excluded"
        );

        // A dream-marked fact (the cycle's own output) with a HIGHER id must be excluded.
        let m = store
            .insert(&make_fact("cycle output", vec![0.4; DIM]))
            .unwrap();
        store.mark_dream_cycled(&[m], 7, Utc::now()).unwrap();
        assert_eq!(
            store.max_caller_written_fact_id().unwrap(),
            Some(c2),
            "dream-marked fact excluded"
        );

        // An expired caller fact with a HIGHER id must be excluded.
        let e = store.insert(&make_fact("expired", vec![0.5; DIM])).unwrap();
        store.expire(e, Utc::now()).unwrap();
        assert_eq!(
            store.max_caller_written_fact_id().unwrap(),
            Some(c2),
            "expired fact excluded"
        );
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
    fn merge_metadata_preserves_and_overwrites_keys() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let mut fact = make_fact("m", vec![0.1; DIM]);
        fact.metadata = serde_json::json!({"keep": 1, "collide": "old"});
        let id = store.insert(&fact).unwrap();

        store
            .merge_metadata(id, &serde_json::json!({"collide": "new", "added": true}))
            .unwrap();

        let md = store.get(id).unwrap().metadata;
        assert_eq!(md["keep"], 1, "existing non-colliding key preserved");
        assert_eq!(md["collide"], "new", "colliding key overwritten");
        assert_eq!(md["added"], true, "new key added");
    }

    #[test]
    fn merge_metadata_errors_on_missing_fact_and_non_object_patch() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        assert!(matches!(
            store.merge_metadata(999, &serde_json::json!({"a": 1})),
            Err(MemoryError::NotFound(_))
        ));
        let id = store.insert(&make_fact("x", vec![0.1; DIM])).unwrap();
        assert!(matches!(
            store.merge_metadata(id, &serde_json::json!("not-an-object")),
            Err(MemoryError::Internal(_))
        ));
    }

    #[test]
    fn list_archive_candidates_matches_pinned_and_expiry_predicate() {
        // Equivalence guard for the SQL-pushdown refactor (#349): the new
        // `list_archive_candidates` must select exactly the rows the prior
        // Rust filter did — `!is_pinned && t_expired.is_some_and(|te| te < cutoff)`.
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let cutoff = Utc::now();

        // Helper: insert a fact with explicit t_expired and is_pinned values
        // (both are persisted verbatim by insert()).
        let insert_expired =
            |content: &str, embed: f32, expired: Option<DateTime<Utc>>, pinned: bool| {
                let mut f = make_fact(content, vec![embed; DIM]);
                f.t_expired = expired;
                f.is_pinned = pinned;
                store.insert(&f).unwrap()
            };

        // (1) expired before cutoff, not pinned → INCLUDED
        let qualifies = insert_expired("qualifies", 0.1, Some(cutoff - TimeDelta::hours(1)), false);
        // (2) active (t_expired NULL) → EXCLUDED
        let _active = insert_expired("active", 0.2, None, false);
        // (3) expired AFTER cutoff → EXCLUDED
        let _too_recent =
            insert_expired("too recent", 0.3, Some(cutoff + TimeDelta::hours(1)), false);
        // (4) expired before cutoff but PINNED → EXCLUDED
        let _pinned = insert_expired("pinned", 0.4, Some(cutoff - TimeDelta::hours(1)), true);
        // (5) a second qualifying fact (higher id) to assert id-ascending order
        let qualifies2 = insert_expired(
            "qualifies two",
            0.5,
            Some(cutoff - TimeDelta::minutes(30)),
            false,
        );

        let candidates = store.list_archive_candidates(cutoff).unwrap();
        let ids: Vec<i64> = candidates.iter().map(|f| f.id).collect();
        assert_eq!(
            ids,
            vec![qualifies, qualifies2],
            "only non-pinned facts expired strictly before the cutoff, id-ascending"
        );
    }

    #[test]
    fn mark_dream_cycled_excludes_from_undreamt_selection() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let a = store.insert(&make_fact("a", vec![0.1; DIM])).unwrap();
        let b = store.insert(&make_fact("b", vec![0.2; DIM])).unwrap();

        let start = "2000-01-01T00:00:00Z".parse().unwrap();
        let end = "2100-01-01T00:00:00Z".parse().unwrap();

        // Both visible before marking.
        let before = store
            .list_undreamt_in_period(start, end, &[], None)
            .unwrap();
        assert_eq!(before.len(), 2);

        store.mark_dream_cycled(&[a], 1, Utc::now()).unwrap();

        let after = store
            .list_undreamt_in_period(start, end, &[], None)
            .unwrap();
        let ids: Vec<i64> = after.iter().map(|f| f.id).collect();
        assert_eq!(
            ids,
            vec![b],
            "the dream-cycled fact is excluded; the other remains"
        );

        // The marker carries the cycle id and is queryable on the row.
        let marked = store.get(a).unwrap();
        assert_eq!(marked.metadata["dream_cycle"]["cycle_id"], 1);
    }

    #[test]
    fn list_undreamt_in_period_composes_with_fact_type_filter() {
        // Regression guard for the SQL-pushdown refactor: the appended (non-param)
        // dream-cycle predicate must not disturb the `?N` bind numbering of the
        // fact_type filter. Mix types, mark one, and query a single type.
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let mut epi = make_fact("episodic", vec![0.1; DIM]);
        epi.fact_type = FactType::Episodic;
        let mut sem1 = make_fact("semantic one", vec![0.2; DIM]);
        sem1.fact_type = FactType::Semantic;
        let mut sem2 = make_fact("semantic two", vec![0.3; DIM]);
        sem2.fact_type = FactType::Semantic;
        store.insert(&epi).unwrap();
        let s1 = store.insert(&sem1).unwrap();
        let s2 = store.insert(&sem2).unwrap();

        let start = "2000-01-01T00:00:00Z".parse().unwrap();
        let end = "2100-01-01T00:00:00Z".parse().unwrap();

        // Semantic-only, both undreamt → exactly the two semantic facts (no episodic).
        let before = store
            .list_undreamt_in_period(start, end, &[], Some(&FactType::Semantic))
            .unwrap();
        let mut ids: Vec<i64> = before.iter().map(|f| f.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![s1, s2]);

        // Mark one semantic fact → it drops out; the type filter still holds.
        store.mark_dream_cycled(&[s1], 1, Utc::now()).unwrap();
        let after = store
            .list_undreamt_in_period(start, end, &[], Some(&FactType::Semantic))
            .unwrap();
        assert_eq!(after.iter().map(|f| f.id).collect::<Vec<_>>(), vec![s2]);
    }

    #[test]
    fn list_undreamt_filter_matches_serde_is_none_on_null_value() {
        // Equivalence guard: the SQL filter must match the prior Rust filter
        // `metadata.get("dream_cycle").is_none()` even for a present-but-null value.
        // serde's is_none() is true only for an ABSENT key, so a fact carrying
        // `{"dream_cycle": null}` is treated as already-dreamt (excluded) — which
        // `json_type(...) IS NULL` reproduces (json_extract would NOT).
        let conn = setup();
        let store = FactStore::new(&conn, DIM);

        let mut absent = make_fact("absent key", vec![0.1; DIM]);
        absent.metadata = serde_json::json!({"other": 1});
        let mut present_null = make_fact("present null", vec![0.2; DIM]);
        present_null.metadata = serde_json::json!({"dream_cycle": null});
        let mut present_obj = make_fact("present object", vec![0.3; DIM]);
        present_obj.metadata = serde_json::json!({"dream_cycle": {"cycle_id": 9}});

        let a = store.insert(&absent).unwrap();
        store.insert(&present_null).unwrap();
        store.insert(&present_obj).unwrap();

        let start = "2000-01-01T00:00:00Z".parse().unwrap();
        let end = "2100-01-01T00:00:00Z".parse().unwrap();
        let undreamt: Vec<i64> = store
            .list_undreamt_in_period(start, end, &[], None)
            .unwrap()
            .iter()
            .map(|f| f.id)
            .collect();

        // Only the absent-key fact is undreamt; present-null and present-object are excluded.
        assert_eq!(
            undreamt,
            vec![a],
            "present-null must be treated as dreamt (matching serde get().is_none())"
        );
    }

    #[test]
    fn list_active_by_metadata_key_recent_filters_marker_scope_active_recency() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);

        // Two marked active in-scope facts at distinct t_created (newest = m2).
        let mut m1 = make_fact("marked older", vec![0.1; DIM]);
        m1.metadata = serde_json::json!({"insight": {"flushed_at": "x"}});
        m1.t_created = "2024-01-01T00:00:00Z".parse().unwrap();
        let mut m2 = make_fact("marked newer", vec![0.2; DIM]);
        m2.metadata = serde_json::json!({"insight": {"flushed_at": "y"}});
        m2.t_created = "2024-06-01T00:00:00Z".parse().unwrap();
        // Marked but expired → excluded.
        let mut expired = make_fact("marked expired", vec![0.3; DIM]);
        expired.metadata = serde_json::json!({"insight": {"flushed_at": "z"}});
        expired.t_expired = Some("2024-07-01T00:00:00Z".parse().unwrap());
        // Active in-scope but unmarked → excluded.
        let unmarked = make_fact("unmarked", vec![0.4; DIM]);
        // Marker key present but null → excluded (presence-of-non-null contract).
        let mut null_marker = make_fact("null marker", vec![0.5; DIM]);
        null_marker.metadata = serde_json::json!({"insight": null});

        let id1 = store.insert(&m1).unwrap();
        let id2 = store.insert(&m2).unwrap();
        store.insert(&expired).unwrap();
        store.insert(&unmarked).unwrap();
        store.insert(&null_marker).unwrap();

        // All facts are in root scope (id 1).
        let got: Vec<i64> = store
            .list_active_by_metadata_key_recent(&[1], "insight", 10)
            .unwrap()
            .iter()
            .map(|f| f.id)
            .collect();
        assert_eq!(
            got,
            vec![id2, id1],
            "only marked active in-scope, newest-first"
        );

        // limit truncates to the newest.
        let top1: Vec<i64> = store
            .list_active_by_metadata_key_recent(&[1], "insight", 1)
            .unwrap()
            .iter()
            .map(|f| f.id)
            .collect();
        assert_eq!(top1, vec![id2]);

        // A scope with no facts → empty (scope filter holds).
        assert!(
            store
                .list_active_by_metadata_key_recent(&[999], "insight", 10)
                .unwrap()
                .is_empty()
        );
    }

    /// The runtime guard rejects a non-identifier `marker_key` in ALL build profiles
    /// (the key is interpolated into SQL) — a release build must not skip it.
    #[test]
    fn list_active_by_metadata_key_recent_rejects_non_identifier_key() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        for bad in ["", "in'sight", "$.x", "a b", "a;b"] {
            let err = store
                .list_active_by_metadata_key_recent(&[1], bad, 10)
                .unwrap_err();
            assert!(
                matches!(
                    err,
                    MemoryError::Conflict(crate::error::ConflictError::QueryValidation(_))
                ),
                "key {bad:?} should be rejected as QueryValidation, got {err:?}"
            );
        }
    }

    #[test]
    fn insert_or_reinforce_dedups_and_reinforces_in_scope() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let mut first = make_fact("always use rustfmt", vec![0.1; DIM]);
        first.t_created = "2024-07-20T14:00:00Z".parse().unwrap();
        first.last_accessed = first.t_created;
        let (id, reinforced) = store.insert_or_reinforce(&first).unwrap();
        assert!(!reinforced, "first occurrence inserts");

        let mut again = make_fact("always use rustfmt", vec![0.9; DIM]);
        again.t_created = "2025-01-12T10:00:00Z".parse().unwrap();
        again.last_accessed = again.t_created;
        let (id2, reinforced2) = store.insert_or_reinforce(&again).unwrap();
        assert_eq!(id2, id, "reinforce returns the existing id");
        assert!(reinforced2, "second occurrence reinforces");

        assert_eq!(
            store.list_active(None).unwrap().len(),
            1,
            "deduped to one row"
        );
        let got = store.get(id).unwrap();
        assert_eq!(got.access_count, 1, "reinforced once");
        assert_eq!(
            got.t_created.timestamp(),
            first.t_created.timestamp(),
            "t_created rolls back to the earliest occurrence"
        );
        assert_eq!(
            got.last_accessed.timestamp(),
            again.last_accessed.timestamp(),
            "last_accessed advances to the latest occurrence"
        );
    }

    #[test]
    fn insert_or_reinforce_timestamps_are_order_independent() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        // Insert the LATER occurrence first, then the EARLIER one.
        let mut late = make_fact("conv", vec![0.1; DIM]);
        late.t_created = "2025-01-12T10:00:00Z".parse().unwrap();
        late.last_accessed = late.t_created;
        let (id, _) = store.insert_or_reinforce(&late).unwrap();
        let mut early = make_fact("conv", vec![0.1; DIM]);
        early.t_created = "2024-07-20T14:00:00Z".parse().unwrap();
        early.last_accessed = early.t_created;
        store.insert_or_reinforce(&early).unwrap();
        let got = store.get(id).unwrap();
        assert_eq!(
            got.t_created.timestamp(),
            early.t_created.timestamp(),
            "earliest t_created wins regardless of import order"
        );
        assert_eq!(
            got.last_accessed.timestamp(),
            late.last_accessed.timestamp(),
            "latest last_accessed wins"
        );
    }

    #[test]
    fn insert_or_reinforce_distinguishes_scope_and_content() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        conn.execute(
            "INSERT INTO scopes (id, parent_id, label, depth) VALUES (2, 1, 'other', 1)",
            [],
        )
        .unwrap();
        let mut a = make_fact("same text", vec![0.1; DIM]);
        a.scope_id = 1;
        let mut b = make_fact("same text", vec![0.1; DIM]);
        b.scope_id = 2;
        let (_, ra) = store.insert_or_reinforce(&a).unwrap();
        let (_, rb) = store.insert_or_reinforce(&b).unwrap();
        assert!(
            !ra && !rb,
            "identical content in different scopes is not deduped"
        );
        let (_, rc) = store
            .insert_or_reinforce(&make_fact("different text", vec![0.1; DIM]))
            .unwrap();
        assert!(!rc, "different content is not deduped");
        assert_eq!(store.list_active(None).unwrap().len(), 3);
    }

    #[test]
    fn insert_or_reinforce_ignores_expired_facts() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let (id, _) = store
            .insert_or_reinforce(&make_fact("convention", vec![0.1; DIM]))
            .unwrap();
        store.expire(id, Utc::now()).unwrap();
        let (id2, reinforced) = store
            .insert_or_reinforce(&make_fact("convention", vec![0.1; DIM]))
            .unwrap();
        assert!(!reinforced, "an expired fact must not be reinforced");
        assert_ne!(id2, id, "a fresh row is inserted instead");
    }

    #[test]
    fn insert_or_reinforce_keeps_strongest_pin_and_importance() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let mut weak = make_fact("shared note", vec![0.1; DIM]);
        weak.is_pinned = false;
        weak.importance = 0.3;
        let (id, _) = store.insert_or_reinforce(&weak).unwrap();

        // A later occurrence judged pinned + more important — the strongest signal wins.
        let mut strong = make_fact("shared note", vec![0.1; DIM]);
        strong.is_pinned = true;
        strong.importance = 0.9;
        store.insert_or_reinforce(&strong).unwrap();
        let got = store.get(id).unwrap();
        assert!(got.is_pinned, "is_pinned rises to pinned on reinforcement");
        assert!(
            (got.importance - 0.9).abs() < f64::EPSILON,
            "importance rises to the max"
        );

        // A weaker later occurrence lowers neither signal.
        let mut weaker = make_fact("shared note", vec![0.1; DIM]);
        weaker.is_pinned = false;
        weaker.importance = 0.2;
        store.insert_or_reinforce(&weaker).unwrap();
        let got = store.get(id).unwrap();
        assert!(got.is_pinned, "pin is not lost by a weaker reinforcement");
        assert!(
            (got.importance - 0.9).abs() < f64::EPSILON,
            "importance does not drop"
        );
    }

    #[test]
    fn insert_or_reinforce_subsecond_timestamps_sort_chronologically() {
        // Regression guard for the RFC-3339 min/max invariant: variable sub-second
        // precision (3-digit vs 6-digit fraction sharing a prefix) is the lexicographic-
        // vs-chronological trap. The `+00:00` offset + AutoSi padding must keep min/max
        // chronologically correct.
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let early: DateTime<Utc> = "2024-07-20T14:00:00.500+00:00".parse().unwrap();
        let late: DateTime<Utc> = "2024-07-20T14:00:00.500123+00:00".parse().unwrap();
        assert!(early < late, "fixture sanity: early precedes late");

        // Insert the LATER occurrence first, then reinforce with the EARLIER.
        let mut first = make_fact("conv", vec![0.1; DIM]);
        first.t_created = late;
        first.last_accessed = late;
        let (id, _) = store.insert_or_reinforce(&first).unwrap();
        let mut second = make_fact("conv", vec![0.1; DIM]);
        second.t_created = early;
        second.last_accessed = early;
        store.insert_or_reinforce(&second).unwrap();

        let got = store.get(id).unwrap();
        assert_eq!(
            got.t_created.timestamp_micros(),
            early.timestamp_micros(),
            "t_created = earliest even at sub-second precision"
        );
        assert_eq!(
            got.last_accessed.timestamp_micros(),
            late.timestamp_micros(),
            "last_accessed = latest even at sub-second precision"
        );

        // The zero-fraction-vs-fractional case (the one the review flagged): to_rfc3339()
        // uses a numeric `+00:00` offset, NOT `Z`. The whole-second string ends in `+...`
        // where the fractional one has `.123`; `+` (0x2B) < `.` (0x2E), so the whole second
        // (earlier) still sorts first. (With a `Z` suffix this WOULD invert — hence the
        // explicit format assertion below guarding the invariant.)
        let whole: DateTime<Utc> = "2024-07-20T15:00:00+00:00".parse().unwrap();
        let frac: DateTime<Utc> = "2024-07-20T15:00:00.123+00:00".parse().unwrap();
        assert!(
            whole.to_rfc3339().ends_with("+00:00") && !whole.to_rfc3339().contains('Z'),
            "to_rfc3339() must use a numeric offset, not Z: {}",
            whole.to_rfc3339()
        );
        let mut w = make_fact("conv2", vec![0.1; DIM]);
        w.t_created = frac;
        w.last_accessed = whole;
        let (id2, _) = store.insert_or_reinforce(&w).unwrap();
        let mut x = make_fact("conv2", vec![0.1; DIM]);
        x.t_created = whole;
        x.last_accessed = frac;
        store.insert_or_reinforce(&x).unwrap();
        let got2 = store.get(id2).unwrap();
        assert_eq!(
            got2.t_created.timestamp_micros(),
            whole.timestamp_micros(),
            "t_created = whole-second (earliest) in the zero-vs-fractional case"
        );
        assert_eq!(
            got2.last_accessed.timestamp_micros(),
            frac.timestamp_micros(),
            "last_accessed = fractional (latest) in the zero-vs-fractional case"
        );
    }

    #[test]
    fn get_many_round_trips_and_skips_missing() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let id1 = store.insert(&make_fact("alpha", vec![0.1; DIM])).unwrap();
        let id2 = store.insert(&make_fact("beta", vec![0.2; DIM])).unwrap();

        // Empty slice issues no query and returns an empty map.
        assert!(store.get_many(&[]).unwrap().is_empty());

        // A nonexistent id is simply absent — no error, no placeholder.
        let missing = id2 + 9999;
        let fetched = store.get_many(&[id1, missing, id2]).unwrap();
        assert_eq!(fetched.len(), 2);
        assert_eq!(fetched[&id1].content, "alpha");
        assert_eq!(fetched[&id2].content, "beta");
        assert!(!fetched.contains_key(&missing));

        // Each returned fact round-trips its full payload (embedding included).
        assert_eq!(fetched[&id1].embedding, vec![0.1_f32; DIM]);
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
        let active = store.list_active(None).unwrap();
        assert_eq!(active.len(), 2);

        // Expire fact 1
        store.expire(id1, Utc::now()).unwrap();
        let active = store.list_active(None).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, id2);
    }

    #[test]
    fn list_active_scoring_returns_projection_and_excludes_expired() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);

        let mut pinned = make_fact("pinned semantic", vec![0.1; DIM]);
        pinned.fact_type = FactType::Semantic;
        pinned.is_pinned = true;
        pinned.importance = 0.9;
        pinned.access_count = 7;
        let pinned_id = store.insert(&pinned).unwrap();

        let plain_id = store.insert(&make_fact("plain", vec![0.2; DIM])).unwrap();

        let expired_id = store.insert(&make_fact("gone", vec![0.3; DIM])).unwrap();
        store.expire(expired_id, Utc::now()).unwrap();

        let rows = store.list_active_scoring().unwrap();
        assert_eq!(rows.len(), 2, "expired facts must be excluded");

        let pinned_row = rows.iter().find(|r| r.id == pinned_id).unwrap();
        assert_eq!(pinned_row.fact_type, FactType::Semantic);
        assert!(pinned_row.is_pinned);
        assert!((pinned_row.importance - 0.9).abs() < f64::EPSILON);
        assert_eq!(pinned_row.access_count, 7);

        let plain_row = rows.iter().find(|r| r.id == plain_id).unwrap();
        assert!(!plain_row.is_pinned);
        assert_eq!(plain_row.fact_type, FactType::Episodic);
    }

    #[test]
    fn list_active_with_limit() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        store.insert(&make_fact("fact 1", vec![0.1; DIM])).unwrap();
        store.insert(&make_fact("fact 2", vec![0.2; DIM])).unwrap();
        store.insert(&make_fact("fact 3", vec![0.3; DIM])).unwrap();

        // No limit returns all
        assert_eq!(store.list_active(None).unwrap().len(), 3);

        // Limit returns at most N
        assert_eq!(store.list_active(Some(2)).unwrap().len(), 2);
        assert_eq!(store.list_active(Some(1)).unwrap().len(), 1);

        // Limit larger than corpus returns all
        assert_eq!(store.list_active(Some(100)).unwrap().len(), 3);

        // Limit zero returns empty
        assert_eq!(store.list_active(Some(0)).unwrap().len(), 0);
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
        #[allow(
            clippy::items_after_statements,
            reason = "const scoped to the statement that uses it for readability"
        )]
        const VALID_DURATION_HOURS: i64 = 1;
        future.t_valid = Some(now + TimeDelta::hours(VALID_DURATION_HOURS));
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
    fn next_due_time_corrupt_t_valid_is_database_error_not_migration() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        let now = Utc::now();

        let mut future = make_fact("reminder", vec![0.1; DIM]);
        future.t_valid = Some(now + chrono::Duration::hours(1));
        let id = fs.insert(&future).unwrap();

        // Corrupt the stored t_valid to a non-RFC3339 string.
        conn.execute(
            "UPDATE facts SET t_valid = 'not-a-timestamp' WHERE id = ?1",
            params![id],
        )
        .unwrap();

        let err = fs.next_due_time(now, &[]).unwrap_err();
        // A row TEXT->timestamp conversion failure is a Database error, NOT a
        // schema migration failure.
        assert!(
            !matches!(err, MemoryError::Migration(_)),
            "expected non-Migration error, got: {err:?}"
        );
        assert!(
            matches!(err, MemoryError::Database(_)),
            "expected Database error, got: {err:?}"
        );
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

    mod proptest_content_hash {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn deterministic(s in ".*") {
                let h1 = content_hash(&s);
                let h2 = content_hash(&s);
                prop_assert_eq!(&h1, &h2, "content_hash is not deterministic");
            }

            #[test]
            fn always_32_hex_chars(s in ".*") {
                let h = content_hash(&s);
                prop_assert_eq!(h.len(), 32, "expected 32 chars, got {}", h.len());
                prop_assert!(h.chars().all(|c| c.is_ascii_hexdigit()),
                    "non-hex character in hash: {h}");
            }
        }
    }

    // --- hard_delete_ids tests ---

    #[test]
    fn hard_delete_ids_removes_facts() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let id1 = store
            .insert(&make_fact("fact alpha", vec![0.1; DIM]))
            .unwrap();
        let id2 = store
            .insert(&make_fact("fact beta", vec![0.2; DIM]))
            .unwrap();
        let id3 = store
            .insert(&make_fact("fact gamma", vec![0.3; DIM]))
            .unwrap();

        let deleted = store.hard_delete_ids(&[id1, id2]).unwrap();
        assert_eq!(deleted, 2);

        // fact gamma still exists
        let remaining = store.list_active(None).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, id3);

        // deleted facts are truly gone
        assert!(store.get(id1).is_err());
        assert!(store.get(id2).is_err());
    }

    #[test]
    fn hard_delete_ids_empty_slice_is_noop() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        store.insert(&make_fact("fact", vec![0.1; DIM])).unwrap();

        let deleted = store.hard_delete_ids(&[]).unwrap();
        assert_eq!(deleted, 0);
        assert_eq!(store.list_active(None).unwrap().len(), 1);
    }

    #[test]
    fn hard_delete_ids_cleans_fts5() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);

        let unique_token = "xyzzy_unique_archival_token";
        let id = store
            .insert(&make_fact(
                &format!("content containing {unique_token}"),
                vec![0.1; DIM],
            ))
            .unwrap();

        // FTS5 finds it before deletion
        let fts_count_before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts_fts WHERE facts_fts MATCH ?1",
                rusqlite::params![unique_token],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fts_count_before, 1);

        store.hard_delete_ids(&[id]).unwrap();

        // FTS5 is clean after deletion (trigger fired)
        let fts_count_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts_fts WHERE facts_fts MATCH ?1",
                rusqlite::params![unique_token],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fts_count_after, 0);
    }
}
