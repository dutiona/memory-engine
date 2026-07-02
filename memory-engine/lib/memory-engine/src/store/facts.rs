use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{MemoryError, Result, StorageError};
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

/// Parse a `fact_type` string read back from the store into the core [`FactType`].
///
/// Delegates to core's canonical [`FactType::from_str`] (the single source of
/// truth shared with the CLI and MCP, #678) rather than re-matching the variants
/// here. Stored values are written as `snake_case` via [`fact_type_to_str`]; the
/// canonical parser accepts that (and is case-insensitive), so the DB round-trip
/// is unaffected.
///
/// A value that does not parse is therefore **data-integrity corruption**, not a
/// missing row: the store always writes a canonical token, so an unparseable one
/// means the `fact_type` column was tampered with or otherwise corrupted. It maps
/// to [`MemoryError::Internal`] (a terminal "this shouldn't happen" / corrupt
/// state, #560), **not** [`MemoryError::NotFound`] — `NotFound` means "the
/// requested row is absent", a semantically distinct, recoverable condition
/// (#366). The message embeds the offending token verbatim for diagnostics.
///
/// What a *read-path* caller sees, end-to-end: the read sites ([`row_to_fact`],
/// [`row_to_scoring_row`]) box this `Internal` into
/// `rusqlite::Error::FromSqlConversionFailure`; the call site maps that rusqlite
/// error via `.map_err(StorageError::backend)` and `?` lifts it to
/// [`MemoryError::Storage`] (#926). So the surfaced top-level variant is
/// `Storage(StorageError::Backend)` (a backend/data error — and crucially **not**
/// `NotFound`, which is exactly the conflation #366 reported), with this
/// `Internal`'s message preserved as a substring of the `Backend` string for
/// diagnostics. The structured `Internal` value here is therefore the
/// directly-observable error only for a *direct* caller of this helper; the read
/// path re-classifies it to `Storage(Backend)`. On the `SQLite` backend a
/// `CHECK(fact_type IN (…))` constraint additionally makes this arm unreachable
/// through ordinary SQL — it fires only on genuine on-disk tampering or a backend
/// without that constraint. Both surfaced behaviors are covered by tests
/// (`corrupt_fact_type_read_path_surfaces_storage_not_notfound` for the end-to-end
/// path, `str_to_fact_type_rejects_unknown_as_internal` for the direct call).
fn str_to_fact_type(s: &str) -> Result<FactType> {
    s.parse::<FactType>()
        .map_err(|e| MemoryError::Internal(format!("corrupt stored fact_type: {e}")))
}

/// Compute blake3 hex hash of content, truncated to first 32 characters (128 bits).
fn content_hash(content: &str) -> String {
    let hash = blake3::hash(content.as_bytes());
    hash.to_hex()[..32].to_string()
}

/// Collect the set of **existing** fact ids (`SELECT id FROM facts`, any
/// `t_expired`).
///
/// A free function — it needs neither a configured `embed_dim` nor a full
/// [`FactStore`], only the id column — used by snapshot referential validation
/// (#257) to reject any edge endpoint that does not reference a fact that exists
/// (a phantom-node injection).
///
/// This set is *exactly* the population whose edges the `SQLite` foreign key
/// (`edges.source_fact_id/target_fact_id REFERENCES facts(id)`) honors on a full
/// `load_from_db` rebuild — and that
/// is *all* facts, not just active ones. `load_from_db` loads every *active edge*
/// (`edges.t_expired IS NULL`), and an active edge can legitimately point at an
/// *expired* fact: the conflict-resolution `contradicts` edge `new → old` is
/// created active in the same transaction the `old` fact is expired, and the
/// dream-cycle `supersedes` edge `synthetic → src` stays active while every
/// source `src` is expired. The FK guarantees the endpoint fact *exists*, never
/// that it is active (`SQLite` FKs cannot be conditional on `t_expired`).
/// Restricting this set to active-only would therefore be *stricter* than
/// `load_from_db`'s trust boundary and would falsely reject a snapshot that
/// faithfully mirrors a real rebuild.
///
/// # Errors
///
/// Returns `MemoryError::Storage` on query failure.
pub fn existing_fact_ids(conn: &Connection) -> Result<std::collections::HashSet<i64>> {
    let mut stmt = conn
        .prepare("SELECT id FROM facts")
        .map_err(StorageError::backend)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, i64>(0))
        .map_err(StorageError::backend)?;
    let mut ids = std::collections::HashSet::new();
    for row in rows {
        ids.insert(row.map_err(StorageError::backend)?);
    }
    Ok(ids)
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
    /// Returns `MemoryError::Storage` on insert failure.
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

        self.conn
            .execute(
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
                    fact.base_importance, // -> DB column `importance`
                    fact.access_count,
                    last_accessed,
                    metadata_str,
                    fact.scope_id,
                    i64::from(fact.is_pinned),
                    fact.base_importance, // seed importance_score from base importance
                ],
            )
            .map_err(StorageError::backend)?;
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
    /// and `base_importance` move to the max across occurrences (a fact later judged
    /// pinned or more important stays so). Frequency does not inflate `base_importance`
    /// — only the independent per-occurrence score does.
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
    /// Returns `MemoryError::Storage` on query or insert failure.
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
            .optional()
            .map_err(StorageError::backend)?;

        match existing {
            Some(id) => {
                // to_rfc3339() emits a `+00:00` offset with AutoSi-padded (0/3/6/9-digit)
                // fractions; `+` (0x2B) sorts before any digit, so a shorter fraction
                // (chronologically <=) always sorts first — lexicographic == chronological,
                // and SQL min/max give earliest/latest. (Same invariant the rest of the
                // store relies on for t_created/t_valid string comparison.)
                let t_created = fact.t_created.to_rfc3339();
                let last_accessed = fact.last_accessed.to_rfc3339();
                self.conn
                    .execute(
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
                            fact.base_importance,
                            id
                        ],
                    )
                    .map_err(StorageError::backend)?;
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
            .prepare(&format!("SELECT {FACT_COLUMNS} FROM facts WHERE id = ?1"))
            .map_err(StorageError::backend)?;
        let dim = self.embed_dim;
        let mut rows = stmt
            .query_map(params![id], |row| row_to_fact(row, dim))
            .map_err(StorageError::backend)?;
        match rows.next() {
            Some(Ok(fact)) => Ok(fact),
            Some(Err(e)) => Err(StorageError::backend(e).into()),
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
    /// Returns `MemoryError::Storage` on query failure.
    pub fn get_many(&self, ids: &[i64]) -> Result<std::collections::HashMap<i64, Fact>> {
        if ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let ids_json = serde_json::to_string(ids)?;
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {FACT_COLUMNS} FROM facts WHERE id IN (SELECT value FROM json_each(?1))"
            ))
            .map_err(StorageError::backend)?;
        let dim = self.embed_dim;
        let rows = stmt
            .query_map(params![ids_json], |row| row_to_fact(row, dim))
            .map_err(StorageError::backend)?;
        let mut out = std::collections::HashMap::with_capacity(ids.len());
        for row in rows {
            let fact = row.map_err(StorageError::backend)?;
            out.insert(fact.id, fact);
        }
        Ok(out)
    }

    /// List active facts (`t_expired IS NULL`), optionally limited, in ascending
    /// `id` (insertion) order (#495).
    ///
    /// When `limit` is `Some(n)`, a SQL `LIMIT n` clause is pushed into the
    /// query so that at most `n` rows are materialized — the `n` **oldest** active
    /// facts, since results are ordered by `id`. `None` returns all.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on query failure.
    pub fn list_active(&self, limit: Option<usize>) -> Result<Vec<Fact>> {
        let base = format!("SELECT {FACT_COLUMNS} FROM facts WHERE t_expired IS NULL");
        let limit_i64: i64 = limit.map_or(-1, |n| i64::try_from(n).unwrap_or(i64::MAX));
        // ORDER BY id (#495): deterministic iteration order across SQLite versions,
        // vacuums, and query plans. Consolidation's greedy dedup/cluster passes are
        // order-sensitive, so a stable insertion (rowid) order makes their output
        // reproducible rather than dependent on the storage engine's scan choice.
        let sql = format!("{base} ORDER BY id LIMIT ?1");
        let mut stmt = self.conn.prepare(&sql).map_err(StorageError::backend)?;
        let dim = self.embed_dim;
        let rows = stmt
            .query_map(params![limit_i64], |row| row_to_fact(row, dim))
            .map_err(StorageError::backend)?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row.map_err(StorageError::backend)?);
        }
        Ok(facts)
    }

    /// Count active facts (`t_expired IS NULL`) without materializing any rows.
    ///
    /// A `COUNT(*)` companion to [`list_active`](Self::list_active): consolidation
    /// uses it to test its O(N·M) / O(N²) safety caps **before** the expensive
    /// `list_active` load — which would otherwise deserialize every embedding BLOB
    /// (~147 MB for 50k×768-dim) only to discover the corpus is over the cap and
    /// skip both passes (#659).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on query failure.
    pub fn count_active(&self) -> Result<usize> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM facts WHERE t_expired IS NULL",
                [],
                |row| row.get(0),
            )
            .map_err(StorageError::backend)?;
        Ok(usize::try_from(count).unwrap_or(0))
    }

    /// Whether a fact is currently active (`t_expired IS NULL`).
    ///
    /// A point liveness probe used by the consolidation apply phase to detect a fact
    /// that was concurrently expired between the lock-free snapshot and the write — so a
    /// dedup survivor removed in that gap does not cause its loser to be expired too,
    /// orphaning the duplicate group (#409 read→write gap).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on query failure.
    pub fn is_active(&self, id: i64) -> Result<bool> {
        let active: bool = self
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM facts WHERE id = ?1 AND t_expired IS NULL)",
                params![id],
                |row| row.get(0),
            )
            .map_err(StorageError::backend)?;
        Ok(active)
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
    /// Returns `MemoryError::Storage` on query failure.
    pub fn list_active_scoring(&self) -> Result<Vec<FactScoringRow>> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {SCORING_COLUMNS} FROM facts WHERE t_expired IS NULL"
            ))
            .map_err(StorageError::backend)?;
        let rows = stmt
            .query_map([], row_to_scoring_row)
            .map_err(StorageError::backend)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(StorageError::backend)?);
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
    /// Returns `MemoryError::Storage` on query failure.
    pub fn list_active_at(&self, valid_at: DateTime<Utc>) -> Result<Vec<Fact>> {
        let valid_at_str = valid_at.to_rfc3339();
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {FACT_COLUMNS} FROM facts
             WHERE t_expired IS NULL
               AND (t_valid IS NULL OR t_valid <= ?1)
               AND (t_invalid IS NULL OR t_invalid > ?1)"
            ))
            .map_err(StorageError::backend)?;
        let dim = self.embed_dim;
        let rows = stmt
            .query_map(params![valid_at_str], |row| row_to_fact(row, dim))
            .map_err(StorageError::backend)?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row.map_err(StorageError::backend)?);
        }
        Ok(facts)
    }

    /// List dormant facts: active, non-pinned, low importance, temporally valid.
    ///
    /// Used by `sample_dormant()` for resonance queries. Returns facts with
    /// `importance_score < threshold` that pass temporal validity checks
    /// **as of `as_of`** (`t_valid <= as_of < t_invalid`).
    ///
    /// `as_of` is the injected wall-clock instant (the facade passes
    /// [`Utc::now`]), mirroring [`list_due`](Self::list_due) and
    /// [`list_active_at`](Self::list_active_at) — the store never reads the clock
    /// itself, so temporal behavior is deterministically testable (#327).
    ///
    /// When `scope_ids` is `Some`, only facts in those scopes are returned.
    /// When `None`, all scopes are searched.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on query failure.
    pub fn list_dormant(
        &self,
        importance_threshold: f64,
        scope_ids: Option<&[i64]>,
        as_of: DateTime<Utc>,
    ) -> Result<Vec<Fact>> {
        let now_str = as_of.to_rfc3339();

        // Scope filtering via `json_each(?3)`: a single serialized-array param
        // (`?3`) regardless of list length — the unified IN-list strategy shared
        // by every scope-filtered query in this store (#405). `None` and the
        // empty slice both mean "all scopes" (no clause).
        let scope_json = match scope_ids {
            Some(ids) if !ids.is_empty() => Some(serde_json::to_string(ids)?),
            _ => None,
        };
        let scope_clause = if scope_json.is_some() {
            " AND scope_id IN (SELECT value FROM json_each(?3))"
        } else {
            ""
        };

        let sql = format!(
            "SELECT {FACT_COLUMNS} FROM facts
             WHERE t_expired IS NULL
               AND is_pinned = 0
               AND importance_score < ?1
               AND (t_valid IS NULL OR t_valid <= ?2)
               AND (t_invalid IS NULL OR t_invalid > ?2){scope_clause}"
        );

        let mut stmt = self.conn.prepare(&sql).map_err(StorageError::backend)?;
        let dim = self.embed_dim;

        // Build params: [threshold, now, scope_json?]. `?3` is bound only when a
        // scope filter is present (its clause is otherwise absent from the SQL).
        let mut all_params: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(importance_threshold), Box::new(now_str)];
        if let Some(json) = scope_json {
            all_params.push(Box::new(json));
        }

        let rows = stmt
            .query_map(rusqlite::params_from_iter(all_params), |row| {
                row_to_fact(row, dim)
            })
            .map_err(StorageError::backend)?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row.map_err(StorageError::backend)?);
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
        let changed = self
            .conn
            .execute(
                "UPDATE facts SET t_expired = ?1 WHERE id = ?2 AND t_expired IS NULL",
                params![now_str, id],
            )
            .map_err(StorageError::backend)?;
        if changed == 0 {
            return Err(MemoryError::NotFound(format!("fact {id}")));
        }
        Ok(())
    }

    /// Bi-temporally expire AND invalidate a fact: set both `t_expired` (system
    /// time: removed from the active set) and `t_invalid` (valid time: no longer
    /// true in the world). Used by conflict resolution's `Update`/`Delete` arms —
    /// a superseded/deleted fact is not merely forgotten, it is marked
    /// no-longer-valid. Idempotent guard: only an unexpired row is affected.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if no rows are affected. This covers two
    /// indistinguishable cases by design: the fact does not exist at all, or it
    /// was already expired (`t_expired IS NOT NULL`). The SQL `WHERE` clause
    /// filters on `t_expired IS NULL`, so both produce zero changed rows and map
    /// to the same `NotFound` error.
    pub fn expire_and_invalidate(&self, id: i64, now: DateTime<Utc>) -> Result<()> {
        let now_str = now.to_rfc3339();
        let changed = self.conn.execute(
            "UPDATE facts SET t_expired = ?1, t_invalid = ?1 WHERE id = ?2 AND t_expired IS NULL",
            params![now_str, id],
        ).map_err(StorageError::backend)?;
        if changed == 0 {
            return Err(MemoryError::NotFound(format!("fact {id}")));
        }
        Ok(())
    }

    /// Update the base importance prior (DB column `importance`) for a fact.
    /// This is the static seed, not the computed `importance_score`.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if no rows affected.
    pub fn update_base_importance(&self, id: i64, base_importance: f64) -> Result<()> {
        let changed = self
            .conn
            .execute(
                "UPDATE facts SET importance = ?1 WHERE id = ?2",
                params![base_importance, id],
            )
            .map_err(StorageError::backend)?;
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
        ).map_err(StorageError::backend)?;
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
    /// Returns `MemoryError::Storage` on query failure.
    pub fn list_by_scope_importance(&self, scope_id: i64, limit: usize) -> Result<Vec<Fact>> {
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        let dim = self.embed_dim;
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {FACT_COLUMNS} FROM facts
             WHERE t_expired IS NULL AND scope_id = ?1
             ORDER BY importance DESC
             LIMIT ?2"
            ))
            .map_err(StorageError::backend)?;
        let rows = stmt
            .query_map(params![scope_id, limit_i64], |row| row_to_fact(row, dim))
            .map_err(StorageError::backend)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| StorageError::backend(e).into())
    }

    /// List active facts in a set of scopes with importance >= threshold,
    /// excluding specific fact IDs, ordered by importance DESC.
    ///
    /// # Panics
    ///
    /// Panics if `scope_ids` or `exclude_ids` cannot be serialized to JSON —
    /// infallible in practice for `&[i64]` / `&HashSet<i64>`.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on query failure.
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
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {FACT_COLUMNS} FROM facts
             WHERE t_expired IS NULL
               AND scope_id IN (SELECT value FROM json_each(?1))
               AND importance >= ?2
               AND id NOT IN (SELECT value FROM json_each(?3))
             ORDER BY importance DESC
             LIMIT ?4"
            ))
            .map_err(StorageError::backend)?;
        let rows = stmt
            .query_map(
                params![scope_json, min_importance, exclude_json, limit_i64],
                |row| row_to_fact(row, dim),
            )
            .map_err(StorageError::backend)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| StorageError::backend(e).into())
    }

    /// List active pinned (unforgettable) facts, optionally filtered by scope,
    /// ordered by `importance_score` DESC and capped at `limit`.
    ///
    /// Pass empty `scope_ids` to get pinned facts across all scopes. Pass
    /// `usize::MAX` for `limit` to retrieve every pinned fact (no cap). The cap is
    /// pushed down to SQL (`LIMIT ?`) so the DB never transmits or deserializes the
    /// embedding BLOBs of facts beyond the cap (#395) — matching the pattern of
    /// `list_by_importance_score`.
    ///
    /// # Panics
    ///
    /// Panics if `scope_ids` cannot be serialized to JSON — infallible in practice
    /// for `&[i64]`.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on query failure.
    pub fn list_pinned(&self, scope_ids: &[i64], limit: usize) -> Result<Vec<Fact>> {
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        let base =
            format!("SELECT {FACT_COLUMNS} FROM facts WHERE t_expired IS NULL AND is_pinned = 1");
        let dim = self.embed_dim;
        if scope_ids.is_empty() {
            let sql = format!("{base} ORDER BY importance_score DESC LIMIT ?1");
            let mut stmt = self.conn.prepare(&sql).map_err(StorageError::backend)?;
            let rows = stmt
                .query_map(rusqlite::params![limit_i64], |row| row_to_fact(row, dim))
                .map_err(StorageError::backend)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| StorageError::backend(e).into())
        } else {
            let scope_json = serde_json::to_string(scope_ids).expect("serialize scope_ids");
            let sql = format!(
                "{base} AND scope_id IN (SELECT value FROM json_each(?1)) ORDER BY importance_score DESC LIMIT ?2"
            );
            let mut stmt = self.conn.prepare(&sql).map_err(StorageError::backend)?;
            let rows = stmt
                .query_map(rusqlite::params![scope_json, limit_i64], |row| {
                    row_to_fact(row, dim)
                })
                .map_err(StorageError::backend)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| StorageError::backend(e).into())
        }
    }

    /// List active, valid facts where `t_valid <= now` and `t_valid IS NOT NULL`.
    /// Excludes facts where `t_invalid <= now` (bi-temporally invalidated).
    ///
    /// `exclude` is an id set removed in SQL (`id NOT IN (json_each(?))`); pass an
    /// empty slice for no exclusion. `limit` caps the result in SQL (`LIMIT ?`);
    /// pass `None` for the uncapped scheduling contract (`MemoryEngine::list_due`
    /// returns ALL due facts). Pushing both down (#396) keeps the resume Tier-3
    /// path from materializing — and decoding the embedding BLOB of — every due
    /// fact only to filter+cap it in Rust, matching `list_by_importance_score`.
    ///
    /// # Panics
    ///
    /// Panics if `exclude` cannot be serialized to JSON — infallible in practice
    /// for `&[i64]`.
    ///
    /// # Errors
    ///
    /// - Returns `MemoryError::Serialization` if `scope_ids` cannot be serialized
    ///   to JSON (infallible in practice for `&[i64]`).
    /// - Returns `MemoryError::Storage` on query failure.
    pub fn list_due(
        &self,
        now: DateTime<Utc>,
        scope_ids: &[i64],
        exclude: &[i64],
        limit: Option<usize>,
    ) -> Result<Vec<Fact>> {
        let now_str = now.to_rfc3339();
        let base = format!(
            "SELECT {FACT_COLUMNS} FROM facts
             WHERE t_expired IS NULL AND t_valid IS NOT NULL AND t_valid <= ?1
             AND (t_invalid IS NULL OR t_invalid > ?1)"
        );

        // Build the dynamic clauses and the matching positional params together so
        // the `?N` indices stay in lock-step. `?1` is always `now`; subsequent
        // indices are assigned in append order. Scope and exclude both filter via
        // `json_each(?N)` over a single serialized-array param — the unified
        // IN-list strategy shared by every scope-filtered query (#405): no
        // per-element placeholder string, so the only `?N` bookkeeping left is one
        // index per optional clause.
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(now_str)];
        let mut next_idx = 2;

        let scope_clause = if scope_ids.is_empty() {
            String::new()
        } else {
            let scope_json = serde_json::to_string(scope_ids)?;
            let clause = format!(" AND scope_id IN (SELECT value FROM json_each(?{next_idx}))");
            next_idx += 1;
            params.push(Box::new(scope_json));
            clause
        };

        let exclude_clause = if exclude.is_empty() {
            String::new()
        } else {
            let exclude_json = serde_json::to_string(exclude).expect("serialize exclude_ids");
            let clause = format!(" AND id NOT IN (SELECT value FROM json_each(?{next_idx}))");
            next_idx += 1;
            params.push(Box::new(exclude_json));
            clause
        };

        let limit_clause = limit.map_or_else(String::new, |n| {
            let clause = format!(" LIMIT ?{next_idx}");
            params.push(Box::new(i64::try_from(n).unwrap_or(i64::MAX)));
            clause
        });

        let sql =
            format!("{base}{scope_clause}{exclude_clause} ORDER BY t_valid ASC{limit_clause}");
        let mut stmt = self.conn.prepare(&sql).map_err(StorageError::backend)?;
        let dim = self.embed_dim;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params), |row| {
                row_to_fact(row, dim)
            })
            .map_err(StorageError::backend)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| StorageError::backend(e).into())
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
        // Scope filtering via `json_each(?2)`: a single serialized-array param,
        // length-independent — the unified IN-list strategy shared by every
        // scope-filtered query in this store (#405).
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(now_str)];
        let sql = if scope_ids.is_empty() {
            base.to_string()
        } else {
            params.push(Box::new(serde_json::to_string(scope_ids)?));
            format!("{base} AND scope_id IN (SELECT value FROM json_each(?2))")
        };
        let mut stmt = self.conn.prepare(&sql).map_err(StorageError::backend)?;
        let result: Option<String> = stmt
            .query_row(rusqlite::params_from_iter(params), |r| r.get(0))
            .map_err(StorageError::backend)?;
        match result {
            Some(s) => Ok(Some(parse_timestamp(&s).map_err(StorageError::backend)?)),
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
        ).map_err(StorageError::backend)?;
        // Re-read persisted values for ALL requested IDs (handles concurrent races)
        let mut stmt = self.conn.prepare(
            "SELECT id, surfaced_at FROM facts WHERE id IN (SELECT value FROM json_each(?1)) AND surfaced_at IS NOT NULL",
        ).map_err(StorageError::backend)?;
        let rows = stmt
            .query_map(params![ids_json], |row| {
                let id: i64 = row.get(0)?;
                let ts_str: String = row.get(1)?;
                let ts = parse_timestamp(&ts_str)?;
                Ok((id, ts))
            })
            .map_err(StorageError::backend)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| StorageError::backend(e).into())
    }

    /// Set the pinned flag on a fact.
    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<()> {
        let rows = self
            .conn
            .execute(
                "UPDATE facts SET is_pinned = ?1 WHERE id = ?2",
                rusqlite::params![i64::from(pinned), id],
            )
            .map_err(StorageError::backend)?;
        if rows == 0 {
            return Err(crate::error::MemoryError::NotFound(format!("fact {id}")));
        }
        Ok(())
    }

    /// List ALL facts (including expired). Used for state dumps.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on SQL failure.
    pub fn list_all(&self) -> Result<Vec<Fact>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {FACT_COLUMNS} FROM facts ORDER BY id ASC"))
            .map_err(StorageError::backend)?;
        let dim = self.embed_dim;
        let rows = stmt
            .query_map([], |row| row_to_fact(row, dim))
            .map_err(StorageError::backend)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| StorageError::backend(e).into())
    }

    /// Iterate all facts row-by-row, calling `f` for each.
    ///
    /// Unlike [`Self::list_all`], this never allocates a `Vec` — each fact is
    /// deserialized, passed to the callback, and dropped before the next
    /// row is read.  Suitable for streaming serialization of large databases.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on SQL failure, or propagates any
    /// error returned by `f`.
    pub fn for_each<F>(&self, mut f: F) -> Result<()>
    where
        F: FnMut(Fact) -> Result<()>,
    {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {FACT_COLUMNS} FROM facts ORDER BY id ASC"))
            .map_err(StorageError::backend)?;
        let dim = self.embed_dim;
        let mut rows = stmt.query([]).map_err(StorageError::backend)?;
        while let Some(row) = rows.next().map_err(StorageError::backend)? {
            let fact = row_to_fact(row, dim).map_err(StorageError::backend)?;
            f(fact)?;
        }
        Ok(())
    }

    /// Update the materialized importance score for a fact.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if no row with `id` exists (rows-affected
    /// contract, mirroring [`update_base_importance`](Self::update_base_importance)
    /// and [`increment_access`](Self::increment_access)) — a silent no-op on a
    /// nonexistent id would let a caller mistake a missing fact for a successful
    /// write (#328).
    pub fn update_importance_score(&self, id: i64, score: f64) -> Result<()> {
        let changed = self
            .conn
            .execute(
                "UPDATE facts SET importance_score = ?1 WHERE id = ?2",
                rusqlite::params![score, id],
            )
            .map_err(StorageError::backend)?;
        if changed == 0 {
            return Err(MemoryError::NotFound(format!("fact {id}")));
        }
        Ok(())
    }

    /// Bulk-update materialized importance scores in a single statement.
    ///
    /// Collapses what the prune path used to do as an N+1 loop of per-row
    /// `update_importance_score` calls (one prepared-statement exec + one
    /// B-tree write each) into one `json_each`-driven UPDATE, regardless of
    /// corpus size. Behaviorally identical to the loop: only the ids present in
    /// `scores` are touched — rows outside the payload are left untouched. This
    /// is an `UPDATE ... FROM json_each(?1)` join (`SQLite` 3.33+): the payload is
    /// parsed exactly once and index-joined to `facts` on
    /// `facts.id = CAST(s.key AS INTEGER)`, so the cost is O(N) in the batch
    /// size — no per-row re-scan of the JSON. The join condition *is* the
    /// restriction: a fact with no matching `json_each` row is simply not joined,
    /// hence not updated (no NULL is ever produced for the `NOT NULL`
    /// `importance_score` column).
    ///
    /// `scores` is serialized as a JSON object mapping the id (as a string key,
    /// which is what `json_each.key` yields) to its score; the join casts the
    /// TEXT key back to INTEGER (`CAST(s.key AS INTEGER)`) so i64 ids round-trip
    /// correctly.
    ///
    /// Every score is validated finite *before* any SQL runs: `serde_json` maps a
    /// non-finite `f64` (`NaN` / `±Infinity`) to JSON `null`, which `json_each`
    /// would then yield as NULL and the UPDATE would try to write into the
    /// `NOT NULL` column — aborting the entire enclosing transaction. The guard is
    /// validate-all-then-execute: a single non-finite entry rejects the whole
    /// batch with [`ConflictError::PolicyParameter`](crate::error::ConflictError::PolicyParameter)
    /// and leaves **all** rows untouched. (Defense in depth: today's callers feed
    /// provably-finite `compute_importance` output, so this is unreachable — but it
    /// converts a latent opaque DB-constraint abort into a clean, typed error.)
    ///
    /// An empty slice is a no-op and returns `Ok(())` (no degenerate statement
    /// runs).
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Conflict`](crate::error::MemoryError::Conflict) wrapping
    /// [`ConflictError::PolicyParameter`](crate::error::ConflictError::PolicyParameter)
    /// if any score is non-finite (the statement does not run). Returns
    /// `MemoryError::Storage` on SQL failure (or `MemoryError` from JSON
    /// serialization, which cannot fail for the finite `id -> f64` map built here).
    pub fn update_importance_scores_bulk(&self, scores: &[(i64, f64)]) -> Result<()> {
        if scores.is_empty() {
            return Ok(());
        }
        // Validate-all-then-execute: a non-finite score would serialize to JSON
        // `null` and write NULL into the `NOT NULL` importance_score column,
        // aborting the enclosing transaction. Reject the whole batch up front so a
        // bad entry leaves every row untouched.
        for &(id, score) in scores {
            if !score.is_finite() {
                return Err(MemoryError::Conflict(
                    crate::error::ConflictError::PolicyParameter(format!(
                        "importance score must be finite, got {score} for fact {id}"
                    )),
                ));
            }
        }
        let mut map = serde_json::Map::with_capacity(scores.len());
        for &(id, score) in scores {
            map.insert(id.to_string(), serde_json::json!(score));
        }
        let payload = serde_json::to_string(&serde_json::Value::Object(map))?;
        self.conn
            .execute(
                "UPDATE facts \
             SET importance_score = s.value \
             FROM json_each(?1) AS s \
             WHERE facts.id = CAST(s.key AS INTEGER)",
                params![payload],
            )
            .map_err(StorageError::backend)?;
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
            .optional()
            .map_err(StorageError::backend)?;
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
        let changed = self
            .conn
            .execute(
                "UPDATE facts SET metadata = ?1 WHERE id = ?2",
                params![new_str, id],
            )
            .map_err(StorageError::backend)?;
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
    /// Returns `MemoryError::Storage` on query failure.
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
    /// Returns `MemoryError::Storage` on query failure.
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
            .map_err(|e| StorageError::backend(e).into())
    }

    /// List active facts ordered by materialized `importance_score`, excluding IDs in `exclude`.
    /// Pass empty `scope_ids` to query across all scopes.
    ///
    /// # Panics
    ///
    /// Panics if `exclude` or `scope_ids` cannot be serialized to JSON —
    /// infallible in practice for `&HashSet<i64>` / `&[i64]`.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on query failure.
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
            let mut stmt = self.conn.prepare(&sql).map_err(StorageError::backend)?;
            let rows = stmt
                .query_map(
                    rusqlite::params![min_score, exclude_json, limit_i64],
                    |row| row_to_fact(row, dim),
                )
                .map_err(StorageError::backend)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| StorageError::backend(e).into())
        } else {
            let scope_json = serde_json::to_string(scope_ids).expect("serialize scope_ids");
            let sql = format!(
                "{base} AND scope_id IN (SELECT value FROM json_each(?3))
                 ORDER BY importance_score DESC LIMIT ?4"
            );
            let mut stmt = self.conn.prepare(&sql).map_err(StorageError::backend)?;
            let rows = stmt
                .query_map(
                    rusqlite::params![min_score, exclude_json, scope_json, limit_i64],
                    |row| row_to_fact(row, dim),
                )
                .map_err(StorageError::backend)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| StorageError::backend(e).into())
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
    /// Returns `MemoryError::Storage` on query failure.
    pub fn list_active_by_session(
        &self,
        session_id: &str,
        scope_ids: &[i64],
    ) -> Result<Vec<SessionFact>> {
        if scope_ids.is_empty() {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT f.id
                 FROM facts f
                 INNER JOIN events e ON f.source_event_id = e.id
                 WHERE e.session_id = ?1
                   AND f.t_expired IS NULL
                 ORDER BY f.id",
                )
                .map_err(StorageError::backend)?;
            let rows = stmt
                .query_map(params![session_id], |row| {
                    Ok(SessionFact { id: row.get(0)? })
                })
                .map_err(StorageError::backend)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| StorageError::backend(e).into())
        } else {
            let scope_json = serde_json::to_string(scope_ids)?;
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT f.id
                 FROM facts f
                 INNER JOIN events e ON f.source_event_id = e.id
                 WHERE e.session_id = ?1
                   AND f.t_expired IS NULL
                   AND f.scope_id IN (SELECT value FROM json_each(?2))
                 ORDER BY f.id",
                )
                .map_err(StorageError::backend)?;
            let rows = stmt
                .query_map(params![session_id, scope_json], |row| {
                    Ok(SessionFact { id: row.get(0)? })
                })
                .map_err(StorageError::backend)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| StorageError::backend(e).into())
        }
    }

    /// List active facts in a set of scopes, excluding specific fact IDs,
    /// ordered by `t_created` DESC (most recent first).
    ///
    /// # Panics
    ///
    /// Panics if `scope_ids` or `exclude_ids` cannot be serialized to JSON —
    /// infallible in practice for `&[i64]` / `&HashSet<i64>`.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on query failure.
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
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {FACT_COLUMNS} FROM facts
             WHERE t_expired IS NULL
               AND scope_id IN (SELECT value FROM json_each(?1))
               AND id NOT IN (SELECT value FROM json_each(?2))
             ORDER BY t_created DESC
             LIMIT ?3"
            ))
            .map_err(StorageError::backend)?;
        let rows = stmt
            .query_map(params![scope_json, exclude_json, limit_i64], |row| {
                row_to_fact(row, dim)
            })
            .map_err(StorageError::backend)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| StorageError::backend(e).into())
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
    /// # Panics
    ///
    /// Panics if `scope_ids` cannot be serialized to JSON — infallible in practice
    /// for `&[i64]`.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Conflict(ConflictError::QueryValidation)` if
    /// `marker_key` is not a non-empty `[A-Za-z0-9_]+` identifier.
    /// Returns `MemoryError::Storage` on query failure.
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
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {FACT_COLUMNS} FROM facts
             WHERE t_expired IS NULL
               AND scope_id IN (SELECT value FROM json_each(?1))
               AND {marker_predicate}
             ORDER BY t_created DESC
             LIMIT ?2"
            ))
            .map_err(StorageError::backend)?;
        let rows = stmt
            .query_map(params![scope_json, limit_i64], |row| row_to_fact(row, dim))
            .map_err(StorageError::backend)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| StorageError::backend(e).into())
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

        let mut stmt = self.conn.prepare(&sql).map_err(StorageError::backend)?;
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(end_str), Box::new(start_str)];
        if !scope_ids.is_empty() {
            params.push(Box::new(scope_json));
        }
        if fact_type.is_some() {
            params.push(Box::new(ft_str));
        }

        let rows = stmt
            .query_map(rusqlite::params_from_iter(params), |row| {
                row_to_fact(row, dim)
            })
            .map_err(StorageError::backend)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| StorageError::backend(e).into())
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
    /// Returns `MemoryError::Storage` on SQL failure.
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
        let deleted = self
            .conn
            .execute(&sql, params.as_slice())
            .map_err(StorageError::backend)?;
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
    /// **Precondition:** this equivalence holds *iff* `t_expired` is stored as
    /// the rfc3339 form of a UTC instant (`…+00:00`) — the only form any
    /// production write path produces. A non-UTC offset or a space-separated
    /// `datetime('now')` value would break both lexicographic ordering here and
    /// the rfc3339 parse on the [`list_all`](Self::list_all) read path, so no
    /// path is robust to it; the SQL filter assumes the same storage invariant
    /// the rest of the store upholds.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on SQL failure.
    pub fn list_archive_candidates(&self, expired_before: DateTime<Utc>) -> Result<Vec<Fact>> {
        let cutoff = expired_before.to_rfc3339();
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {FACT_COLUMNS} FROM facts
             WHERE is_pinned = 0
               AND t_expired IS NOT NULL
               AND t_expired < ?1
             ORDER BY id ASC"
            ))
            .map_err(StorageError::backend)?;
        let dim = self.embed_dim;
        let rows = stmt
            .query_map(params![cutoff], |row| row_to_fact(row, dim))
            .map_err(StorageError::backend)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| StorageError::backend(e).into())
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
        base_importance: row.get("importance")?,
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
        base_importance: row.get("importance")?,
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
        crate::test_utils::new_fact(content, embedding)
    }

    /// #366 end-to-end (read path): corrupt the stored `fact_type` of a real row,
    /// then read it back through the normal `FactStore::get` path and assert the
    /// **actual user-visible** `MemoryError`.
    ///
    /// This is the test that proves the #366 fix where a user can observe it — the
    /// isolated [`str_to_fact_type_rejects_unknown_as_internal`] test below only
    /// exercises the private helper, but every production caller (`row_to_fact`,
    /// `row_to_scoring_row`) boxes the helper's error into
    /// `rusqlite::Error::FromSqlConversionFailure`; the call site maps that via
    /// `.map_err(StorageError::backend)` and `?` lifts it to
    /// [`MemoryError::Storage`] (#926). So the helper's `Internal` never reaches a
    /// read-path caller as the top-level variant.
    ///
    /// What #366 actually demanded — *do not surface [`MemoryError::NotFound`] for
    /// a corrupt enum* (which lets a caller matching `NotFound` mistake a corrupt
    /// store for "no such fact") — is met **end-to-end**: the surfaced variant is
    /// `Storage(StorageError::Backend)`, a backend/data error distinct from
    /// `NotFound`. The diagnostic `Internal("corrupt stored fact_type: …")`'s
    /// message is preserved as a substring of the `Backend` string.
    ///
    /// Note the `SQLite` schema carries `CHECK(fact_type IN (…))`, so this corruption
    /// is unreachable through ordinary SQL — the test must
    /// `PRAGMA ignore_check_constraints` to simulate genuine on-disk tampering or a
    /// backend without the constraint (the exact scenario the helper guards).
    #[test]
    fn corrupt_fact_type_read_path_surfaces_storage_not_notfound() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let id = store.insert(&make_fact("p", vec![0.0; DIM])).unwrap();
        // The `CHECK(fact_type IN (...))` constraint rejects a bad value on a plain
        // UPDATE; bypass it to simulate genuine on-disk corruption / a non-CHECK
        // backend — the only way `str_to_fact_type`'s corrupt arm is reachable.
        conn.execute_batch("PRAGMA ignore_check_constraints = ON")
            .unwrap();
        conn.execute(
            "UPDATE facts SET fact_type = 'bogus' WHERE id = ?1",
            params![id],
        )
        .unwrap();
        conn.execute_batch("PRAGMA ignore_check_constraints = OFF")
            .unwrap();

        let err = store.get(id).unwrap_err();
        // The user-visible top-level variant is Storage(Backend): the helper's
        // Internal is boxed by FromSqlConversionFailure, then the call site maps it
        // via StorageError::backend and `?` lifts StorageError into Storage (#926).
        assert!(
            matches!(
                err,
                MemoryError::Storage(crate::error::StorageError::Backend(_))
            ),
            "read-path corrupt fact_type must surface Storage(Backend), got {err:?}"
        );
        // #366's actual harm — surfacing NotFound for a corrupt enum — is gone.
        assert!(
            !matches!(err, MemoryError::NotFound(_)),
            "a corrupt stored fact_type must never read back as NotFound (#366), got {err:?}"
        );
        // The diagnostic helper message survives as the boxed source.
        assert!(
            err.to_string().contains("corrupt stored fact_type"),
            "the diagnostic message must be preserved through the box, got {err}"
        );
    }

    #[test]
    fn str_to_fact_type_rejects_unknown_as_internal() {
        // A `fact_type` string read back from the DB that names no known variant
        // is a data-integrity failure (the store wrote it via `fact_type_to_str`,
        // so a value that does not parse means the row is corrupt) — NOT a
        // missing-row condition. It must map to `MemoryError::Internal`, never
        // `MemoryError::NotFound` (#366).
        let err = str_to_fact_type("bogus").unwrap_err();
        assert!(
            matches!(err, MemoryError::Internal(_)),
            "expected Internal, got {err:?}"
        );
        // The message stays greppable: it carries the offending token verbatim.
        assert!(
            err.to_string().contains("bogus"),
            "message must include the offending string, got {err}"
        );
        assert!(
            !matches!(err, MemoryError::NotFound(_)),
            "an unparseable stored enum is not a NotFound condition"
        );
    }

    /// #257 regression: the snapshot referential-validation set MUST include
    /// **expired** facts, because that is exactly the fact population
    /// `MemoryGraph::load_from_db` trusts. `load_from_db` loads every *active*
    /// edge (`edges.t_expired IS NULL`), and an active edge can legitimately point
    /// at an *expired* fact — e.g. the conflict-resolution `contradicts` edge
    /// `new → old` (the old fact is expired in the same transaction the edge is
    /// created) and the dream-cycle `supersedes` edge `synthetic → src` (every
    /// source is expired). The `edges.source_fact_id/target_fact_id REFERENCES
    /// facts(id)` foreign key guarantees the endpoint fact *exists*, not that it is
    /// active (`SQLite` FKs cannot be conditional on `t_expired`). Validating against
    /// the active-only set would therefore be *stricter* than `load_from_db` and
    /// would falsely reject a snapshot that faithfully mirrors a real rebuild.
    #[test]
    fn existing_fact_ids_includes_expired_facts() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);

        let active_id = store.insert(&make_fact("active", vec![0.1; DIM])).unwrap();
        let expired_id = store.insert(&make_fact("expired", vec![0.2; DIM])).unwrap();
        store.expire(expired_id, Utc::now()).unwrap();

        let ids = existing_fact_ids(&conn).unwrap();
        assert!(
            ids.contains(&active_id),
            "the active fact must be in the validation set"
        );
        assert!(
            ids.contains(&expired_id),
            "the EXPIRED fact must also be in the validation set: load_from_db's \
             active edges can reference it, so excluding it would falsely reject a \
             legitimate snapshot edge (the #257 over-strict regression)"
        );
    }

    /// #659: `count_active` counts exactly the rows `list_active` would return
    /// (`t_expired IS NULL`) without materializing them, and tracks expiry.
    #[test]
    fn count_active_matches_list_active_and_tracks_expiry() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        assert_eq!(store.count_active().unwrap(), 0, "empty table → 0");

        let a = store.insert(&make_fact("a", vec![0.1; DIM])).unwrap();
        store.insert(&make_fact("b", vec![0.2; DIM])).unwrap();
        assert_eq!(store.count_active().unwrap(), 2);
        assert_eq!(
            store.count_active().unwrap(),
            store.list_active(None).unwrap().len(),
            "count must agree with list_active's row count"
        );

        // Expiring a fact drops it from the active count (soft delete).
        store.expire(a, Utc::now()).unwrap();
        assert_eq!(store.count_active().unwrap(), 1, "expired fact not counted");
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
        weak.base_importance = 0.3;
        let (id, _) = store.insert_or_reinforce(&weak).unwrap();

        // A later occurrence judged pinned + more important — the strongest signal wins.
        let mut strong = make_fact("shared note", vec![0.1; DIM]);
        strong.is_pinned = true;
        strong.base_importance = 0.9;
        store.insert_or_reinforce(&strong).unwrap();
        let got = store.get(id).unwrap();
        assert!(got.is_pinned, "is_pinned rises to pinned on reinforcement");
        assert!(
            (got.base_importance - 0.9).abs() < f64::EPSILON,
            "importance rises to the max"
        );

        // A weaker later occurrence lowers neither signal.
        let mut weaker = make_fact("shared note", vec![0.1; DIM]);
        weaker.is_pinned = false;
        weaker.base_importance = 0.2;
        store.insert_or_reinforce(&weaker).unwrap();
        let got = store.get(id).unwrap();
        assert!(got.is_pinned, "pin is not lost by a weaker reinforcement");
        assert!(
            (got.base_importance - 0.9).abs() < f64::EPSILON,
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
        pinned.base_importance = 0.9;
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
        assert!((pinned_row.base_importance - 0.9).abs() < f64::EPSILON);
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
        store.update_base_importance(id, 0.9).unwrap();
        let fact = store.get(id).unwrap();
        assert!((fact.base_importance - 0.9).abs() < f64::EPSILON);
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

        let result = fs.list_pinned(&[], usize::MAX).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].is_pinned);
    }

    /// #395: the SQL `LIMIT` returns the SAME set the old Rust
    /// `list_pinned(..).take(cap)` did — the top-`cap` pinned facts by
    /// `importance_score` DESC — and `usize::MAX` retrieves all of them.
    #[test]
    fn list_pinned_sql_limit_matches_rust_take() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        // Seed five pinned facts with strictly increasing importance_score so the
        // DESC ordering (and thus the top-N selected by LIMIT) is unambiguous.
        for i in 0..5 {
            let mut f = make_fact(&format!("pinned {i}"), vec![0.1; DIM]);
            f.is_pinned = true;
            let id = fs.insert(&f).unwrap();
            fs.update_importance_score(id, f64::from(i) / 10.0).unwrap();
        }
        // Oracle: fetch all, then take(cap) in Rust (the pre-#395 behavior).
        let all = fs.list_pinned(&[], usize::MAX).unwrap();
        assert_eq!(all.len(), 5, "usize::MAX = no cap");
        for cap in [0_usize, 1, 3, 5, 99] {
            let rust_take: Vec<i64> = all.iter().take(cap).map(|f| f.id).collect();
            let sql_limit: Vec<i64> = fs
                .list_pinned(&[], cap)
                .unwrap()
                .iter()
                .map(|f| f.id)
                .collect();
            assert_eq!(
                sql_limit, rust_take,
                "SQL LIMIT {cap} must equal Rust .take({cap}) (same order)"
            );
        }
    }

    /// #392: the bulk `importance_score` materialization (one
    /// `UPDATE ... FROM json_each` join) must be behavior-equivalent to the old
    /// per-row loop:
    /// (a) it sets exactly the scored subset,
    /// (b) it leaves an UNSCORED row untouched (the FROM-join condition
    ///     `facts.id = CAST(s.key AS INTEGER)` is the restriction: a fact with no
    ///     matching `json_each` row is not joined, hence not updated — no NULL is
    ///     ever produced for the `NOT NULL` column),
    /// (c) i64 ids round-trip the `json_each` TEXT key cast (a large id catches
    ///     a TEXT/INTEGER mismatch in the `CAST(s.key AS INTEGER)` join key), and
    /// (d) an empty slice is a no-op.
    #[test]
    fn update_importance_scores_bulk_sets_subset_and_round_trips_ids() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);

        // Three autoincrement facts (ids 1, 2, 3), each seeded at base_importance
        // 0.5 by `insert`.
        let a = fs.insert(&make_fact("fact a", vec![0.1; DIM])).unwrap();
        let b = fs.insert(&make_fact("fact b", vec![0.2; DIM])).unwrap();
        let c = fs.insert(&make_fact("fact c", vec![0.3; DIM])).unwrap();

        // A fourth fact carrying a large i64 id, to exercise the json_each key
        // cast on a value that would mismatch under a sloppy TEXT/INTEGER pairing.
        let big_id: i64 = 9_007_199_254_740_993; // 2^53 + 1
        let d = fs.insert(&make_fact("fact d", vec![0.4; DIM])).unwrap();
        conn.execute("UPDATE facts SET id = ?1 WHERE id = ?2", params![big_id, d])
            .unwrap();

        // Score a SUBSET: a, c, and the large-id fact — but NOT b.
        fs.update_importance_scores_bulk(&[(a, 0.11), (c, 0.33), (big_id, 0.99)])
            .unwrap();

        // (a) exactly the scored rows take their new values.
        assert!((fs.get(a).unwrap().importance_score - 0.11).abs() < f64::EPSILON);
        assert!((fs.get(c).unwrap().importance_score - 0.33).abs() < f64::EPSILON);
        // (c) the large id round-trips the cast and is updated.
        assert!((fs.get(big_id).unwrap().importance_score - 0.99).abs() < f64::EPSILON);

        // (b) the unscored row b keeps its seeded base_importance (0.5) — the
        // bulk UPDATE must not zero/NULL rows outside the payload.
        assert!(
            (fs.get(b).unwrap().importance_score - 0.5).abs() < f64::EPSILON,
            "unscored fact must be left untouched"
        );

        // (d) an empty slice changes nothing and returns Ok.
        fs.update_importance_scores_bulk(&[]).unwrap();
        assert!((fs.get(a).unwrap().importance_score - 0.11).abs() < f64::EPSILON);
        assert!((fs.get(b).unwrap().importance_score - 0.5).abs() < f64::EPSILON);
        assert!((fs.get(c).unwrap().importance_score - 0.33).abs() < f64::EPSILON);
        assert!((fs.get(big_id).unwrap().importance_score - 0.99).abs() < f64::EPSILON);
    }

    /// #392 hardening: a non-finite score (`NaN` / `±Infinity`) is rejected up
    /// front with `ConflictError::PolicyParameter` and leaves **all** rows
    /// untouched. `serde_json` would otherwise map the non-finite `f64` to JSON
    /// `null`, writing NULL into the `importance_score REAL NOT NULL` column and
    /// aborting the enclosing transaction — so this proves validate-all-then-
    /// execute: the bad batch errors cleanly and no row mutates.
    #[test]
    fn update_importance_scores_bulk_rejects_non_finite_leaves_rows_untouched() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let conn = setup();
            let fs = FactStore::new(&conn, DIM);

            // Two facts, each seeded at base_importance 0.5 by `insert`.
            let a = fs.insert(&make_fact("fact a", vec![0.1; DIM])).unwrap();
            let b = fs.insert(&make_fact("fact b", vec![0.2; DIM])).unwrap();

            // A batch mixing a valid score for `a` with a non-finite score for `b`.
            // The whole batch must be rejected before any UPDATE runs.
            let err = fs
                .update_importance_scores_bulk(&[(a, 0.11), (b, bad)])
                .expect_err("non-finite score must be rejected");
            assert!(
                matches!(
                    err,
                    MemoryError::Conflict(crate::error::ConflictError::PolicyParameter(_))
                ),
                "expected ConflictError::PolicyParameter, got {err:?}"
            );

            // Validate-all-then-execute: BOTH rows keep their seeded 0.5 — even
            // `a`, whose score was valid, must be left untouched.
            assert!(
                (fs.get(a).unwrap().importance_score - 0.5).abs() < f64::EPSILON,
                "valid-scored row must be untouched when the batch is rejected"
            );
            assert!(
                (fs.get(b).unwrap().importance_score - 0.5).abs() < f64::EPSILON,
                "non-finite-scored row must be untouched"
            );
        }
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

        let result = fs.list_due(now, &[], &[], None).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].content.contains("past"));
    }

    /// #396: the SQL `exclude` + `LIMIT` returns the SAME set the old resume Tier-3
    /// Rust path did — `list_due(now, scope).filter(|f| !seen.contains).take(cap)` —
    /// and the uncapped scheduling shape (`exclude=[]`, `limit=None`) is unaffected.
    #[test]
    fn list_due_exclude_and_limit_match_rust_filter_take() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        let now = Utc::now();

        // Six due facts, all in the past so all are due; ascending t_valid so the
        // `ORDER BY t_valid ASC` selection (and thus what LIMIT keeps) is unambiguous.
        let mut ids = Vec::new();
        for i in 0..6 {
            let mut f = make_fact(&format!("due {i}"), vec![0.1; DIM]);
            f.t_valid = Some(now - TimeDelta::hours(i64::from(6 - i)));
            ids.push(fs.insert(&f).unwrap());
        }
        // One future fact that must never be due.
        let mut future = make_fact("future", vec![0.2; DIM]);
        future.t_valid = Some(now + TimeDelta::hours(1));
        fs.insert(&future).unwrap();

        // Uncapped scheduling shape: ALL six due, none excluded.
        let all_due = fs.list_due(now, &[], &[], None).unwrap();
        assert_eq!(all_due.len(), 6, "scheduling shape returns ALL due facts");

        // Exclude the two earliest (the first two in t_valid ASC order) and cap.
        let seen: std::collections::HashSet<i64> = [ids[0], ids[1]].into_iter().collect();
        for cap in [0_usize, 1, 2, 4, 99] {
            // Oracle: the pre-#396 Rust path over the uncapped query.
            let rust: Vec<i64> = all_due
                .iter()
                .filter(|f| !seen.contains(&f.id))
                .take(cap)
                .map(|f| f.id)
                .collect();
            // SQL pushdown.
            let exclude: Vec<i64> = seen.iter().copied().collect();
            let sql: Vec<i64> = fs
                .list_due(now, &[], &exclude, Some(cap))
                .unwrap()
                .iter()
                .map(|f| f.id)
                .collect();
            assert_eq!(
                sql, rust,
                "SQL exclude+LIMIT {cap} must equal Rust .filter(!seen).take({cap})"
            );
        }
    }

    /// #477: the SQL `list_due` predicate and the shared in-Rust
    /// `Fact::is_temporally_due` predicate MUST agree on the same active fact
    /// population, over a corpus with varied `t_valid`/`t_invalid` combinations.
    /// This pins the two against silent drift (the resume walk and `explain`'s
    /// `FactState::Due` both route through `is_temporally_due`).
    #[test]
    fn list_due_membership_equals_is_temporally_due_predicate() {
        // Item-before-statements (clippy::items_after_statements): the alias keeps
        // the `cases` corpus type readable.
        type Ts = Option<chrono::DateTime<Utc>>;

        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        let now = Utc::now();
        let past = now - TimeDelta::hours(1);
        let future = now + TimeDelta::hours(1);

        // A corpus spanning every meaningful (t_valid, t_invalid) combination,
        // including the boundary instants (== now) the predicate hinges on.
        let cases: &[(&str, Ts, Ts)] = &[
            ("no_tvalid", None, None), // t_valid None → never due
            ("no_tvalid_invalidated", None, Some(future)),
            ("past_tvalid", Some(past), None),     // due
            ("tvalid_eq_now", Some(now), None),    // due (boundary: t_valid <= now)
            ("future_tvalid", Some(future), None), // not yet valid
            ("past_then_invalid_future", Some(past), Some(future)), // due (invalid still ahead)
            ("past_then_invalid_past", Some(past), Some(past)), // invalidated
            ("invalid_eq_now", Some(past), Some(now)), // invalidated (boundary: t_invalid > now is false)
        ];

        let mut active_facts = Vec::new();
        for (label, t_valid, t_invalid) in cases {
            let mut f = make_fact(label, vec![0.1; DIM]);
            f.t_valid = *t_valid;
            f.t_invalid = *t_invalid;
            let id = fs.insert(&f).unwrap();
            active_facts.push(fs.get(id).unwrap());
        }

        // Also seed an EXPIRED-but-otherwise-due fact: it must NOT appear in
        // list_due (system-time filter), and is excluded from the predicate oracle
        // because is_temporally_due deliberately does not test t_expired (its
        // documented scope — the caller's active-only read owns that).
        let mut expired = make_fact("expired_due", vec![0.2; DIM]);
        expired.t_valid = Some(past);
        let expired_id = fs.insert(&expired).unwrap();
        fs.expire(expired_id, now).unwrap();

        // SQL truth set (uncapped, unfiltered — the scheduling shape).
        let sql_ids: std::collections::HashSet<i64> = fs
            .list_due(now, &[], &[], None)
            .unwrap()
            .into_iter()
            .map(|f| f.id)
            .collect();

        // Predicate truth set over the ACTIVE corpus only (matches the predicate's
        // documented scope: valid-time only, system-time liveness owned by caller).
        let pred_ids: std::collections::HashSet<i64> = active_facts
            .iter()
            .filter(|f| f.is_temporally_due(now))
            .map(|f| f.id)
            .collect();

        assert_eq!(
            sql_ids, pred_ids,
            "SQL list_due and Fact::is_temporally_due must agree on the active corpus"
        );
        // The expired-but-due fact must be in NEITHER (SQL filters it, oracle omits it).
        assert!(
            !sql_ids.contains(&expired_id),
            "expired fact must not be due"
        );
        // Sanity: the due set is non-empty (3 due cases above).
        assert_eq!(sql_ids.len(), 3, "exactly the three due cases");
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
        // A row TEXT->timestamp conversion failure surfaces as Storage(Backend), NOT
        // a schema migration failure.
        assert!(
            !matches!(err, MemoryError::Migration(_)),
            "expected non-Migration error, got: {err:?}"
        );
        assert!(
            matches!(
                err,
                MemoryError::Storage(crate::error::StorageError::Backend(_))
            ),
            "expected Storage(Backend) error, got: {err:?}"
        );
    }

    // -------------------------------------------------------------------------
    // #405 — unified scope_id IN-list strategy (json_each) across the three
    // scheduling/forgetting queries that used to build positional placeholder
    // strings (list_dormant, list_due, next_due_time). These tests pin the
    // refactor against regressions for 0, 1, and N scope ids — N > 1 being the
    // case the per-element placeholder builder was hand-rolling.
    // -------------------------------------------------------------------------

    /// Seed scopes 1..=4 and one fact per scope, then return the fact ids keyed
    /// by their scope. Scope 1 already exists (created by `init_schema`); 2..=4
    /// are inserted to satisfy the FK on `facts.scope_id`. All facts share a past
    /// `t_valid` so they are due, and a low importance so they are dormant.
    fn seed_one_fact_per_scope(
        store: &FactStore<'_>,
        conn: &Connection,
        now: DateTime<Utc>,
    ) -> std::collections::HashMap<i64, i64> {
        for scope in 2..=4 {
            conn.execute(
                "INSERT INTO scopes (id, parent_id, label, depth) VALUES (?1, 1, ?2, 1)",
                params![scope, format!("scope{scope}")],
            )
            .unwrap();
        }
        let mut by_scope = std::collections::HashMap::new();
        for scope in 1..=4 {
            let mut f = make_fact(&format!("fact in scope {scope}"), vec![0.1; DIM]);
            f.scope_id = scope;
            f.base_importance = 0.1; // dormant: importance_score = 0.1 < threshold
            f.t_valid = Some(now - TimeDelta::hours(1)); // due
            by_scope.insert(scope, store.insert(&f).unwrap());
        }
        by_scope
    }

    /// `list_due` honors the scope filter for 0, 1, and N scope ids — the unified
    /// `json_each` IN-list returns exactly the facts in the requested scopes.
    #[test]
    fn list_due_scope_filter_handles_zero_one_and_n_scopes() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        let now = Utc::now();
        let by_scope = seed_one_fact_per_scope(&fs, &conn, now);

        let due_ids = |scopes: &[i64]| -> std::collections::HashSet<i64> {
            fs.list_due(now, scopes, &[], None)
                .unwrap()
                .into_iter()
                .map(|f| f.id)
                .collect()
        };

        // 0 scopes → all scopes (no filter): every seeded fact is due.
        assert_eq!(
            due_ids(&[]),
            by_scope
                .values()
                .copied()
                .collect::<std::collections::HashSet<_>>(),
            "empty scope slice means all scopes"
        );
        // 1 scope → exactly that scope's fact.
        assert_eq!(
            due_ids(&[2]),
            std::collections::HashSet::from([by_scope[&2]]),
            "single scope returns only its fact"
        );
        // N scopes → exactly those scopes' facts (and nothing from the omitted ones).
        assert_eq!(
            due_ids(&[1, 3, 4]),
            [by_scope[&1], by_scope[&3], by_scope[&4]]
                .into_iter()
                .collect::<std::collections::HashSet<_>>(),
            "multi-scope filter returns exactly the requested scopes"
        );
        // Mutation guard: an omitted scope's fact must NOT leak in.
        assert!(
            !due_ids(&[1, 3, 4]).contains(&by_scope[&2]),
            "scope 2 was not requested and must be excluded"
        );
    }

    /// `next_due_time` honors the scope filter for 0, 1, and N scope ids: the
    /// earliest FUTURE `t_valid` is computed only over the requested scopes.
    #[test]
    fn next_due_time_scope_filter_handles_zero_one_and_n_scopes() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        let now = Utc::now();
        for scope in 2..=4 {
            conn.execute(
                "INSERT INTO scopes (id, parent_id, label, depth) VALUES (?1, 1, ?2, 1)",
                params![scope, format!("scope{scope}")],
            )
            .unwrap();
        }
        // One future fact per scope; the lead time is distinct per scope so the
        // MIN(t_valid) of any scope subset is unambiguous.
        for (scope, hours) in [(1_i64, 4_i64), (2, 1), (3, 2), (4, 3)] {
            let mut f = make_fact(&format!("future scope {scope}"), vec![0.1; DIM]);
            f.scope_id = scope;
            f.t_valid = Some(now + TimeDelta::hours(hours));
            fs.insert(&f).unwrap();
        }

        // 0 scopes → global MIN is scope 2's +1h.
        let all = fs.next_due_time(now, &[]).unwrap().unwrap();
        assert!(
            all < now + TimeDelta::hours(1) + TimeDelta::minutes(1)
                && all > now + TimeDelta::minutes(59),
            "global next-due must be ~+1h (scope 2), got {all}"
        );
        // 1 scope → that scope's own next-due (scope 3 = +2h, not the global +1h).
        let one = fs.next_due_time(now, &[3]).unwrap().unwrap();
        assert!(
            one > now + TimeDelta::hours(1) + TimeDelta::minutes(1),
            "single-scope next-due must skip the omitted earlier scope, got {one}"
        );
        // N scopes → MIN over the subset {3,4} = scope 3's +2h, excluding scope 2's +1h.
        let many = fs.next_due_time(now, &[3, 4]).unwrap().unwrap();
        assert_eq!(many, one, "MIN over {{3,4}} equals scope 3's +2h");
        assert!(
            many > now + TimeDelta::hours(1) + TimeDelta::minutes(1),
            "multi-scope subset must exclude scope 2's earlier +1h, got {many}"
        );
        // A scope with no future facts → None.
        for scope in 5..=5 {
            conn.execute(
                "INSERT INTO scopes (id, parent_id, label, depth) VALUES (?1, 1, ?2, 1)",
                params![scope, format!("scope{scope}")],
            )
            .unwrap();
        }
        assert!(
            fs.next_due_time(now, &[5]).unwrap().is_none(),
            "a scope with no future-valid facts has no next-due time"
        );
    }

    /// `list_dormant` honors an N-scope (N > 1) filter via the unified `json_each`
    /// IN-list — complementing the existing single-scope/None coverage in
    /// `list_dormant_filters_importance_pinned_and_scope`.
    #[test]
    fn list_dormant_scope_filter_handles_n_scopes() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        let now = Utc::now();
        let by_scope = seed_one_fact_per_scope(&fs, &conn, now);

        let dormant_ids = |scopes: Option<&[i64]>| -> std::collections::HashSet<i64> {
            fs.list_dormant(0.5, scopes, now)
                .unwrap()
                .into_iter()
                .map(|f| f.id)
                .collect()
        };

        // None → all scopes.
        assert_eq!(
            dormant_ids(None),
            by_scope
                .values()
                .copied()
                .collect::<std::collections::HashSet<_>>(),
            "None spans every scope"
        );
        // N scopes → exactly the requested scopes' facts.
        assert_eq!(
            dormant_ids(Some(&[2, 4])),
            [by_scope[&2], by_scope[&4]]
                .into_iter()
                .collect::<std::collections::HashSet<_>>(),
            "multi-scope dormant filter returns exactly the requested scopes"
        );
        // Mutation guard: an omitted scope must not leak in.
        assert!(
            !dormant_ids(Some(&[2, 4])).contains(&by_scope[&1]),
            "scope 1 was not requested and must be excluded"
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

    // -------------------------------------------------------------------------
    // #326 — stamp_surfaced: batch, idempotent, returns persisted pairs
    // -------------------------------------------------------------------------

    /// Stamping a batch of never-surfaced facts sets `surfaced_at = now` on each
    /// and returns the persisted `(id, now)` pair for every requested id.
    #[test]
    fn stamp_surfaced_stamps_unsurfaced_and_returns_pairs() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let a = store.insert(&make_fact("a", vec![0.1; DIM])).unwrap();
        let b = store.insert(&make_fact("b", vec![0.2; DIM])).unwrap();
        // A third, unrequested fact must NOT be stamped or returned.
        let c = store.insert(&make_fact("c", vec![0.3; DIM])).unwrap();

        let now: DateTime<Utc> = "2024-03-01T12:00:00Z".parse().unwrap();
        let mut pairs = store.stamp_surfaced(&[a, b], now).unwrap();
        pairs.sort_by_key(|(id, _)| *id);

        assert_eq!(
            pairs,
            vec![(a, now), (b, now)],
            "every requested id must come back with the persisted surfaced_at"
        );
        // The persisted column matches what was returned (round-trip, not just the
        // in-memory return value).
        for id in [a, b] {
            let stored: Option<String> = conn
                .query_row(
                    "SELECT surfaced_at FROM facts WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                stored.map(|s| parse_timestamp(&s).unwrap()),
                Some(now),
                "fact {id} surfaced_at must be persisted as `now`"
            );
        }
        // The unrequested fact is untouched.
        let c_surfaced: Option<String> = conn
            .query_row(
                "SELECT surfaced_at FROM facts WHERE id = ?1",
                params![c],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            c_surfaced.is_none(),
            "an id not in the batch must not be stamped"
        );
    }

    /// Re-stamping is idempotent: a second call with a *different* clock must NOT
    /// overwrite the original `surfaced_at` (only `surfaced_at IS NULL` rows are
    /// touched), yet it still returns the original persisted pair.
    #[test]
    fn stamp_surfaced_is_idempotent_preserving_original_timestamp() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let id = store.insert(&make_fact("once", vec![0.1; DIM])).unwrap();

        let first: DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
        let second: DateTime<Utc> = "2024-12-31T23:59:59Z".parse().unwrap();
        assert!(first < second, "fixture sanity: the two clocks differ");

        let p1 = store.stamp_surfaced(&[id], first).unwrap();
        assert_eq!(p1, vec![(id, first)]);

        // Re-stamp with a later clock — the original must survive.
        let p2 = store.stamp_surfaced(&[id], second).unwrap();
        assert_eq!(
            p2,
            vec![(id, first)],
            "re-stamp must return the ORIGINAL surfaced_at, not the new clock"
        );
        let stored: String = conn
            .query_row(
                "SELECT surfaced_at FROM facts WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            parse_timestamp(&stored).unwrap(),
            first,
            "the persisted surfaced_at must not be overwritten on re-stamp"
        );
    }

    /// A partial batch where one id is already stamped and one is fresh: the fresh
    /// one is stamped with `now`, the already-stamped one keeps its original — both
    /// come back with their *persisted* (asymmetric) timestamps.
    #[test]
    fn stamp_surfaced_mixed_batch_returns_each_persisted_value() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let old = store.insert(&make_fact("old", vec![0.1; DIM])).unwrap();
        let fresh = store.insert(&make_fact("fresh", vec![0.2; DIM])).unwrap();

        let t_old: DateTime<Utc> = "2024-02-02T02:02:02Z".parse().unwrap();
        let t_new: DateTime<Utc> = "2024-08-08T08:08:08Z".parse().unwrap();
        store.stamp_surfaced(&[old], t_old).unwrap();

        let mut pairs = store.stamp_surfaced(&[old, fresh], t_new).unwrap();
        pairs.sort_by_key(|(id, _)| *id);
        // Distinct expected timestamps per id catch a swap or a "stamp everything
        // with now" regression.
        assert_eq!(pairs, vec![(old, t_old), (fresh, t_new)]);
    }

    /// Empty slice → `Ok(vec![])` with no DB writes (early-return contract).
    #[test]
    fn stamp_surfaced_empty_slice_is_ok_empty() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let id = store
            .insert(&make_fact("untouched", vec![0.1; DIM]))
            .unwrap();
        let pairs = store.stamp_surfaced(&[], Utc::now()).unwrap();
        assert!(pairs.is_empty(), "empty batch yields no pairs");
        // No collateral stamping happened.
        let surfaced: Option<String> = conn
            .query_row(
                "SELECT surfaced_at FROM facts WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(surfaced.is_none(), "empty batch must not stamp any fact");
    }

    // -------------------------------------------------------------------------
    // #327 — list_dormant: injectable `as_of` clock, deterministic temporal filter
    // -------------------------------------------------------------------------

    /// `list_dormant` respects the injected `as_of` for the temporal-validity
    /// window: a fact valid only in `[t_valid, t_invalid)` is dormant when `as_of`
    /// falls inside the window and excluded when it falls outside — with the SAME
    /// fact and threshold, so the only variable under test is the clock.
    #[test]
    fn list_dormant_honors_injected_as_of_window() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);

        let valid_from: DateTime<Utc> = "2024-06-01T00:00:00Z".parse().unwrap();
        let valid_until: DateTime<Utc> = "2024-07-01T00:00:00Z".parse().unwrap();
        let mut f = make_fact("windowed dormant", vec![0.1; DIM]);
        f.base_importance = 0.1; // seeds importance_score = 0.1 < threshold
        f.t_valid = Some(valid_from);
        f.t_invalid = Some(valid_until);
        let id = store.insert(&f).unwrap();

        let inside: DateTime<Utc> = "2024-06-15T00:00:00Z".parse().unwrap();
        let before: DateTime<Utc> = "2024-05-15T00:00:00Z".parse().unwrap();
        let after: DateTime<Utc> = "2024-07-15T00:00:00Z".parse().unwrap();

        // Inside the validity window → dormant.
        let hit = store.list_dormant(0.5, None, inside).unwrap();
        assert_eq!(
            hit.iter().map(|x| x.id).collect::<Vec<_>>(),
            vec![id],
            "as_of inside [t_valid, t_invalid) must include the fact"
        );
        // Before t_valid → excluded (proves t_valid <= as_of is enforced).
        assert!(
            store.list_dormant(0.5, None, before).unwrap().is_empty(),
            "as_of before t_valid must exclude the fact"
        );
        // At/after t_invalid → excluded (proves as_of < t_invalid is enforced).
        assert!(
            store.list_dormant(0.5, None, after).unwrap().is_empty(),
            "as_of at/after t_invalid must exclude the fact"
        );
    }

    /// `list_dormant` filters on importance, pinned, and scope — independent of the
    /// clock. Uses a far-future `as_of` so every fact is temporally valid, isolating
    /// the non-temporal predicates.
    #[test]
    fn list_dormant_filters_importance_pinned_and_scope() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        conn.execute(
            "INSERT INTO scopes (id, parent_id, label, depth) VALUES (2, 1, 'other', 1)",
            [],
        )
        .unwrap();

        // Low-importance, unpinned, scope 1 → dormant.
        let mut low = make_fact("low", vec![0.1; DIM]);
        low.base_importance = 0.2;
        low.scope_id = 1;
        let low_id = store.insert(&low).unwrap();
        // High-importance → above threshold, excluded.
        let mut high = make_fact("high", vec![0.1; DIM]);
        high.base_importance = 0.9;
        high.scope_id = 1;
        store.insert(&high).unwrap();
        // Low-importance but pinned → excluded.
        let mut pinned = make_fact("pinned", vec![0.1; DIM]);
        pinned.base_importance = 0.2;
        pinned.is_pinned = true;
        pinned.scope_id = 1;
        store.insert(&pinned).unwrap();
        // Low-importance in scope 2 → excluded by the scope filter.
        let mut other = make_fact("scope2", vec![0.1; DIM]);
        other.base_importance = 0.2;
        other.scope_id = 2;
        let other_id = store.insert(&other).unwrap();

        let as_of: DateTime<Utc> = "2100-01-01T00:00:00Z".parse().unwrap();

        // Scope 1 only → just the low/unpinned fact.
        let scoped = store.list_dormant(0.5, Some(&[1]), as_of).unwrap();
        assert_eq!(
            scoped.iter().map(|f| f.id).collect::<Vec<_>>(),
            vec![low_id],
            "only low-importance, unpinned, in-scope facts are dormant"
        );
        // None (all scopes) → both low facts across scopes, neither high nor pinned.
        let mut all = store
            .list_dormant(0.5, None, as_of)
            .unwrap()
            .iter()
            .map(|f| f.id)
            .collect::<Vec<_>>();
        all.sort_unstable();
        let mut expected = vec![low_id, other_id];
        expected.sort_unstable();
        assert_eq!(all, expected, "None scope spans every scope");
    }

    // -------------------------------------------------------------------------
    // #328 — update_importance_score: happy path + NotFound guard
    // -------------------------------------------------------------------------

    /// The happy path persists the new score (and updates `importance_score`, not
    /// the base `importance` column).
    #[test]
    fn update_importance_score_persists_score() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let mut f = make_fact("scored", vec![0.1; DIM]);
        f.base_importance = 0.3; // seeds both `importance` and `importance_score` to 0.3
        let id = store.insert(&f).unwrap();

        store.update_importance_score(id, 0.87).unwrap();

        let (new_score, base_imp): (f64, f64) = conn
            .query_row(
                "SELECT importance_score, importance FROM facts WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(
            (new_score - 0.87).abs() < f64::EPSILON,
            "importance_score must be updated to 0.87, got {new_score}"
        );
        assert!(
            (base_imp - 0.3).abs() < f64::EPSILON,
            "the base `importance` column must be untouched (only importance_score \
             changes), got {base_imp}"
        );
    }

    /// Updating a nonexistent id returns `NotFound` (rows-affected guard, #328) —
    /// previously a silent `Ok(())` no-op. Mirrors `update_base_importance`.
    #[test]
    fn update_importance_score_missing_id_is_not_found() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        // Insert one fact so the table is non-empty (proves the guard keys off the
        // matched row, not "table empty").
        store.insert(&make_fact("present", vec![0.1; DIM])).unwrap();

        let err = store.update_importance_score(9999, 0.5).unwrap_err();
        assert!(
            matches!(err, MemoryError::NotFound(_)),
            "updating a missing id must return NotFound, got {err:?}"
        );
        assert!(
            err.to_string().contains("9999"),
            "the error must name the offending id, got {err}"
        );
    }

    // -------------------------------------------------------------------------
    // #329 — query-method coverage: scope/importance/score/recent lists
    // -------------------------------------------------------------------------

    /// `list_by_scope_importance` returns active in-scope facts ordered by the base
    /// `importance` column DESC, capped at `limit`, and excludes other scopes /
    /// expired rows.
    #[test]
    fn list_by_scope_importance_orders_filters_and_caps() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        conn.execute(
            "INSERT INTO scopes (id, parent_id, label, depth) VALUES (2, 1, 'other', 1)",
            [],
        )
        .unwrap();

        // Scope 1: three active facts with distinct importance (so ordering is
        // unambiguous and a sort flip is observable).
        let mut a = make_fact("a", vec![0.1; DIM]);
        a.base_importance = 0.3;
        let a_id = store.insert(&a).unwrap();
        let mut b = make_fact("b", vec![0.1; DIM]);
        b.base_importance = 0.9;
        let b_id = store.insert(&b).unwrap();
        let mut c = make_fact("c", vec![0.1; DIM]);
        c.base_importance = 0.6;
        let c_id = store.insert(&c).unwrap();
        // Scope 1 but expired → excluded.
        let mut gone = make_fact("gone", vec![0.1; DIM]);
        gone.base_importance = 0.99;
        let gone_id = store.insert(&gone).unwrap();
        store.expire(gone_id, Utc::now()).unwrap();
        // Scope 2 high importance → excluded by scope filter.
        let mut other = make_fact("other", vec![0.1; DIM]);
        other.base_importance = 0.99;
        other.scope_id = 2;
        store.insert(&other).unwrap();

        // Full ordering: b (0.9) > c (0.6) > a (0.3).
        let ordered: Vec<i64> = store
            .list_by_scope_importance(1, 10)
            .unwrap()
            .iter()
            .map(|f| f.id)
            .collect();
        assert_eq!(
            ordered,
            vec![b_id, c_id, a_id],
            "active scope-1 facts, importance DESC"
        );
        // Limit pushes down: top-2 only.
        let top2: Vec<i64> = store
            .list_by_scope_importance(1, 2)
            .unwrap()
            .iter()
            .map(|f| f.id)
            .collect();
        assert_eq!(
            top2,
            vec![b_id, c_id],
            "LIMIT 2 keeps the two most important"
        );
        // Scope 2 isolation: querying it returns ONLY its single fact (proves the
        // scope filter both includes scope 2 and excludes all of scope 1).
        let scope2: Vec<String> = store
            .list_by_scope_importance(2, 10)
            .unwrap()
            .iter()
            .map(|f| f.content.clone())
            .collect();
        assert_eq!(
            scope2,
            vec!["other".to_owned()],
            "scope 2 holds only its own fact"
        );
    }

    /// `list_by_scopes_importance` honors the `json_each` scope-IN set, the
    /// `min_importance` floor, the `exclude_ids` set, importance-DESC ordering, and
    /// the LIMIT — each predicate is exercised with a distinct fact.
    #[test]
    fn list_by_scopes_importance_filters_threshold_exclude_scope_and_orders() {
        use std::collections::HashSet;
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        conn.execute(
            "INSERT INTO scopes (id, parent_id, label, depth) VALUES (2, 1, 's2', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scopes (id, parent_id, label, depth) VALUES (3, 1, 's3', 1)",
            [],
        )
        .unwrap();

        let mk = |store: &FactStore, content: &str, imp: f64, scope: i64| {
            let mut f = make_fact(content, vec![0.1; DIM]);
            f.base_importance = imp;
            f.scope_id = scope;
            store.insert(&f).unwrap()
        };

        let top = mk(&store, "top s2", 0.9, 2); // in-scope, high, but EXCLUDED below
        let mid = mk(&store, "mid s2", 0.7, 2); // in-scope, above floor → expected first
        let low = mk(&store, "low s2", 0.2, 2); // in-scope but below the 0.5 floor
        let s3 = mk(&store, "s3 high", 0.95, 3); // above floor, but scope 3 not queried
        let _s1 = mk(&store, "s1 high", 0.95, 1); // scope 1 not queried

        let exclude: HashSet<i64> = std::iter::once(top).collect();
        let got: Vec<i64> = store
            .list_by_scopes_importance(&[2], 0.5, 10, &exclude)
            .unwrap()
            .iter()
            .map(|f| f.id)
            .collect();
        assert_eq!(
            got,
            vec![mid],
            "scope 2, importance>=0.5, minus the excluded top, ordered DESC → just mid \
             (low is below floor; top is excluded; s3/s1 are out of scope)"
        );

        // Two scopes, no exclusion, no floor: ordering spans both scopes by importance.
        let across: Vec<i64> = store
            .list_by_scopes_importance(&[2, 3], 0.0, 10, &HashSet::new())
            .unwrap()
            .iter()
            .map(|f| f.id)
            .collect();
        assert_eq!(
            across,
            vec![s3, top, mid, low],
            "0.95 > 0.9 > 0.7 > 0.2 across scopes 2 and 3"
        );
        // LIMIT pushes down across the merged, ordered set.
        let top1: Vec<i64> = store
            .list_by_scopes_importance(&[2, 3], 0.0, 1, &HashSet::new())
            .unwrap()
            .iter()
            .map(|f| f.id)
            .collect();
        assert_eq!(top1, vec![s3], "LIMIT 1 keeps the single most important");
    }

    /// `list_by_importance_score` filters on the materialized `importance_score`
    /// column (NOT base `importance`), honoring `min_score`, `exclude`, scope, and
    /// score-DESC ordering. The score is set distinctly from `importance` so a
    /// column swap (`importance` vs `importance_score`) is caught.
    #[test]
    fn list_by_importance_score_uses_score_column_not_base_importance() {
        use std::collections::HashSet;
        let conn = setup();
        let store = FactStore::new(&conn, DIM);

        // Insert with a LOW base importance, then raise importance_score above the
        // floor. A method reading `importance` would miss this fact; one reading
        // `importance_score` finds it.
        let mut climber = make_fact("climber", vec![0.1; DIM]);
        climber.base_importance = 0.1; // base importance stays low
        let climber_id = store.insert(&climber).unwrap();
        store.update_importance_score(climber_id, 0.8).unwrap(); // score rises

        // Insert with a HIGH base importance but drop its score below the floor.
        let mut sinker = make_fact("sinker", vec![0.1; DIM]);
        sinker.base_importance = 0.9; // base importance high
        let sinker_id = store.insert(&sinker).unwrap();
        store.update_importance_score(sinker_id, 0.1).unwrap(); // score drops below 0.5

        // A second qualifying fact, to assert ordering AND exclusion.
        let mut mid = make_fact("mid", vec![0.1; DIM]);
        mid.base_importance = 0.5;
        let mid_id = store.insert(&mid).unwrap();
        store.update_importance_score(mid_id, 0.6).unwrap();

        // min_score 0.5: climber(0.8) and mid(0.6) qualify; sinker(0.1) does not.
        // Score-DESC → climber, then mid. If the method read `importance` instead,
        // sinker (0.9) would wrongly appear and climber (0.1) would vanish.
        let got: Vec<i64> = store
            .list_by_importance_score(&[], 0.5, 10, &HashSet::new())
            .unwrap()
            .iter()
            .map(|f| f.id)
            .collect();
        assert_eq!(
            got,
            vec![climber_id, mid_id],
            "filters/orders on importance_score, not the base importance column"
        );

        // Excluding the top leaves only mid.
        let exclude: HashSet<i64> = std::iter::once(climber_id).collect();
        let excluded: Vec<i64> = store
            .list_by_importance_score(&[], 0.5, 10, &exclude)
            .unwrap()
            .iter()
            .map(|f| f.id)
            .collect();
        assert_eq!(excluded, vec![mid_id], "exclude set removes the climber");

        // LIMIT pushes down.
        let top1: Vec<i64> = store
            .list_by_importance_score(&[], 0.5, 1, &HashSet::new())
            .unwrap()
            .iter()
            .map(|f| f.id)
            .collect();
        assert_eq!(top1, vec![climber_id], "LIMIT 1 keeps the top-scored fact");
    }

    /// `list_by_importance_score` with a non-empty `scope_ids` restricts to those
    /// scopes (the scoped SQL branch, distinct from the all-scopes branch above).
    #[test]
    fn list_by_importance_score_respects_scope_filter() {
        use std::collections::HashSet;
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        conn.execute(
            "INSERT INTO scopes (id, parent_id, label, depth) VALUES (2, 1, 'other', 1)",
            [],
        )
        .unwrap();

        let mut in_scope = make_fact("in", vec![0.1; DIM]);
        in_scope.base_importance = 0.7;
        let in_id = store.insert(&in_scope).unwrap();
        let mut out_scope = make_fact("out", vec![0.1; DIM]);
        out_scope.base_importance = 0.9;
        out_scope.scope_id = 2;
        store.insert(&out_scope).unwrap();

        let got: Vec<i64> = store
            .list_by_importance_score(&[1], 0.0, 10, &HashSet::new())
            .unwrap()
            .iter()
            .map(|f| f.id)
            .collect();
        assert_eq!(
            got,
            vec![in_id],
            "scope 1 only — the higher-scored scope-2 fact is excluded"
        );
    }

    /// `list_by_scopes_recent` returns active in-scope facts ordered by `t_created`
    /// DESC (newest first), honoring the exclude set, scope-IN, and LIMIT.
    #[test]
    fn list_by_scopes_recent_orders_by_t_created_and_filters() {
        use std::collections::HashSet;
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        conn.execute(
            "INSERT INTO scopes (id, parent_id, label, depth) VALUES (2, 1, 'other', 1)",
            [],
        )
        .unwrap();

        // Distinct creation times so DESC ordering is unambiguous.
        let mut oldest = make_fact("oldest", vec![0.1; DIM]);
        oldest.t_created = "2024-01-01T00:00:00Z".parse().unwrap();
        let oldest_id = store.insert(&oldest).unwrap();
        let mut middle = make_fact("middle", vec![0.1; DIM]);
        middle.t_created = "2024-06-01T00:00:00Z".parse().unwrap();
        let middle_id = store.insert(&middle).unwrap();
        let mut newest = make_fact("newest", vec![0.1; DIM]);
        newest.t_created = "2024-12-01T00:00:00Z".parse().unwrap();
        let newest_id = store.insert(&newest).unwrap();
        // Scope 2 → excluded by the scope filter even though it is the very newest.
        let mut other = make_fact("other", vec![0.1; DIM]);
        other.t_created = "2025-01-01T00:00:00Z".parse().unwrap();
        other.scope_id = 2;
        store.insert(&other).unwrap();

        // Newest-first across scope 1.
        let ordered: Vec<i64> = store
            .list_by_scopes_recent(&[1], 10, &HashSet::new())
            .unwrap()
            .iter()
            .map(|f| f.id)
            .collect();
        assert_eq!(
            ordered,
            vec![newest_id, middle_id, oldest_id],
            "active scope-1 facts, t_created DESC (the scope-2 newer fact is excluded)"
        );

        // Exclude the newest → middle becomes the head.
        let exclude: HashSet<i64> = std::iter::once(newest_id).collect();
        let after_exclude: Vec<i64> = store
            .list_by_scopes_recent(&[1], 10, &exclude)
            .unwrap()
            .iter()
            .map(|f| f.id)
            .collect();
        assert_eq!(
            after_exclude,
            vec![middle_id, oldest_id],
            "the excluded id is dropped, order preserved"
        );

        // LIMIT pushes down to the single newest.
        let top1: Vec<i64> = store
            .list_by_scopes_recent(&[1], 1, &HashSet::new())
            .unwrap()
            .iter()
            .map(|f| f.id)
            .collect();
        assert_eq!(top1, vec![newest_id], "LIMIT 1 keeps the newest");
    }

    /// `list_by_scopes_recent` excludes expired facts (active-only contract).
    #[test]
    fn list_by_scopes_recent_excludes_expired() {
        use std::collections::HashSet;
        let conn = setup();
        let store = FactStore::new(&conn, DIM);

        let live = store.insert(&make_fact("live", vec![0.1; DIM])).unwrap();
        let gone = store.insert(&make_fact("gone", vec![0.2; DIM])).unwrap();
        store.expire(gone, Utc::now()).unwrap();

        let got: Vec<i64> = store
            .list_by_scopes_recent(&[1], 10, &HashSet::new())
            .unwrap()
            .iter()
            .map(|f| f.id)
            .collect();
        assert_eq!(got, vec![live], "expired facts are not recent-listed");
    }

    // -------------------------------------------------------------------------
    // #329 — list_all and for_each: dump-path coverage
    // -------------------------------------------------------------------------

    /// `list_all` returns every inserted active fact (state-dump contract).
    ///
    /// Non-vacuous: three asymmetric facts are inserted and the returned `id`
    /// list is asserted verbatim in `id ASC` order, so a short/empty iteration
    /// or a missed row fails the equality.
    #[test]
    fn list_all_returns_all_active_facts() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let a = store.insert(&make_fact("alpha", vec![0.1; DIM])).unwrap();
        let b = store.insert(&make_fact("beta", vec![0.2; DIM])).unwrap();
        let c = store.insert(&make_fact("gamma", vec![0.3; DIM])).unwrap();

        let all = store.list_all().unwrap();
        let ids: Vec<i64> = all.iter().map(|f| f.id).collect();
        assert_eq!(
            ids,
            vec![a, b, c],
            "list_all must return every inserted fact in id ASC order"
        );
        // Content round-trips too — guards against a column-projection slip that
        // would return rows but with the wrong payload.
        let contents: Vec<&str> = all.iter().map(|f| f.content.as_str()).collect();
        assert_eq!(contents, vec!["alpha", "beta", "gamma"]);
    }

    /// `for_each` visits the same rows as `list_all`, in the same order.
    ///
    /// Non-vacuous: the collected stream is asserted equal to `list_all`'s
    /// output id-for-id over three asymmetric facts — a short iteration (e.g. a
    /// `while` that stops after one row) or a reordered scan fails the equality.
    #[test]
    fn for_each_matches_list_all_ordering() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        store.insert(&make_fact("one", vec![0.1; DIM])).unwrap();
        store.insert(&make_fact("two", vec![0.2; DIM])).unwrap();
        store.insert(&make_fact("three", vec![0.3; DIM])).unwrap();

        let expected: Vec<i64> = store.list_all().unwrap().iter().map(|f| f.id).collect();
        assert_eq!(expected.len(), 3, "fixture sanity: three facts present");

        let mut collected: Vec<i64> = Vec::new();
        store
            .for_each(|f| {
                collected.push(f.id);
                Ok(())
            })
            .unwrap();
        assert_eq!(
            collected, expected,
            "for_each must stream the same ids in the same order as list_all"
        );
    }

    /// A callback returning `Err` short-circuits iteration and propagates the
    /// error verbatim — the loop must not swallow it or run to completion.
    ///
    /// Non-vacuous: the callback errors on the FIRST visited row, so the visit
    /// count must be exactly 1 (a swallowed error would let all three through)
    /// and the surfaced error must be the callback's own.
    #[test]
    fn for_each_propagates_callback_error_and_short_circuits() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        store.insert(&make_fact("one", vec![0.1; DIM])).unwrap();
        store.insert(&make_fact("two", vec![0.2; DIM])).unwrap();
        store.insert(&make_fact("three", vec![0.3; DIM])).unwrap();

        let mut visited = 0_usize;
        let err = store
            .for_each(|_| {
                visited += 1;
                Err(MemoryError::Internal("boom".to_owned()))
            })
            .unwrap_err();

        assert_eq!(
            visited, 1,
            "iteration must stop at the first erroring row, not run to completion"
        );
        assert!(
            matches!(err, MemoryError::Internal(ref m) if m == "boom"),
            "the callback's own error must propagate verbatim, got {err:?}"
        );
    }

    /// All `FactType` variants survive an insert → `get()` roundtrip through the
    /// real `SQLite` store (#500).
    ///
    /// This exercises both the write path (`fact_type_to_str` → stored string) and
    /// the read path (`str_to_fact_type` → parsed enum) for every variant in one
    /// test. A mutation that returns the wrong stored string for any variant, or
    /// that maps the wrong stored string to the wrong variant on read-back, fails
    /// here. The three variants are exhaustive (no `#[non_exhaustive]`), so adding
    /// a new variant without updating the serialisation paths will be caught by a
    /// compile-time match exhaustiveness error before this test runs.
    #[test]
    fn all_fact_types_survive_store_roundtrip() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let variants = [FactType::Episodic, FactType::Semantic, FactType::Procedural];
        for expected in variants {
            let fact = crate::test_utils::new_fact_with_type(
                "roundtrip content",
                vec![0.1; DIM],
                expected,
            );
            let id = store.insert(&fact).unwrap();
            let retrieved = store
                .get(id)
                .unwrap_or_else(|e| panic!("get failed for variant {expected:?}: {e}"));
            assert_eq!(
                retrieved.fact_type, expected,
                "fact_type roundtrip failed: inserted {expected:?}, read back {:?}",
                retrieved.fact_type
            );
        }
    }
}
