use rusqlite::Connection;

use crate::error::Result;
use crate::search::serialize_scope_ids;
use crate::store::facts::fact_type_to_str;
use crate::types::FactType;

/// A single FTS5 search result with the fact id and BM25 relevance score.
#[derive(Debug, Clone, PartialEq)]
pub struct FtsResult {
    pub fact_id: i64,
    pub score: f64,
}

/// Search the FTS5 index for facts matching the given query string.
///
/// Uses BM25 scoring (lower/more negative = better match).
/// Returns results sorted by relevance (best match first).
///
/// Filters are pushed into SQL:
/// - Only active facts (`t_expired IS NULL`) are returned.
/// - `fact_type` restricts results to a single fact type when `Some`.
/// - `scope_ids` restricts results to the given scope ids when `Some`.
///
/// FTS5 syntax errors (e.g., unbalanced quotes) return an empty vec
/// rather than propagating an error.
///
/// # Errors
///
/// Returns `MemoryError::Database` for non-FTS5 database errors.
pub fn fts_search(
    conn: &Connection,
    query: &str,
    limit: usize,
    fact_type: Option<&FactType>,
    scope_ids: Option<&[i64]>,
) -> Result<Vec<FtsResult>> {
    let mut stmt = conn.prepare(
        "SELECT f.id, bm25(facts_fts) AS score \
         FROM facts_fts \
         JOIN facts AS f ON f.id = facts_fts.rowid \
         WHERE facts_fts MATCH ?1 \
           AND f.t_expired IS NULL \
           AND (?2 IS NULL OR f.fact_type = ?2) \
           AND (?3 IS NULL OR f.scope_id IN (SELECT value FROM json_each(?3))) \
         ORDER BY score LIMIT ?4",
    )?;

    let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
    let fact_type_str: Option<&str> = fact_type.map(fact_type_to_str);
    let scope_ids_json = serialize_scope_ids(scope_ids)?;

    // FTS5 syntax errors surface at query_map execution time, not at prepare time.
    // They are indistinguishable from other rusqlite::Error variants at the type level,
    // so we catch all errors here. This is intentional: the only realistic failure mode
    // for a parameterized MATCH query is malformed FTS5 syntax from the caller.
    let rows = match stmt.query_map(
        rusqlite::params![query, fact_type_str, scope_ids_json, limit_i64],
        |row| {
            Ok(FtsResult {
                fact_id: row.get(0)?,
                score: row.get(1)?,
            })
        },
    ) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("FTS5 query failed (likely syntax error): {e}");
            return Ok(vec![]);
        }
    };

    let results: Vec<FtsResult> = rows
        .filter_map(|row| match row {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::warn!("FTS5 row mapping failed, skipping row: {e}");
                None
            }
        })
        .collect();
    Ok(results)
}

/// Count expired facts matching the FTS5 query (abstention diagnostics).
///
/// Mirrors [`fts_search`] but queries `t_expired IS NOT NULL` and returns
/// only a count — no row materialisation, no embedding deserialisation.
///
/// FTS5 syntax errors return `Ok(0)` (same fallback as `fts_search`).
///
/// # Errors
///
/// Returns `MemoryError::Database` for non-FTS5 database errors.
pub fn fts_count_expired(
    conn: &Connection,
    query: &str,
    fact_type: Option<&FactType>,
    scope_ids: Option<&[i64]>,
) -> Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT COUNT(*) \
         FROM facts_fts \
         JOIN facts AS f ON f.id = facts_fts.rowid \
         WHERE facts_fts MATCH ?1 \
           AND f.t_expired IS NOT NULL \
           AND (?2 IS NULL OR f.fact_type = ?2) \
           AND (?3 IS NULL OR f.scope_id IN (SELECT value FROM json_each(?3)))",
    )?;

    let fact_type_str: Option<&str> = fact_type.map(fact_type_to_str);
    let scope_ids_json = serialize_scope_ids(scope_ids)?;

    match stmt.query_row(
        rusqlite::params![query, fact_type_str, scope_ids_json],
        |row| row.get::<_, i64>(0),
    ) {
        Ok(n) => usize::try_from(n)
            .map_err(|e| crate::error::MemoryError::Internal(format!("invalid FTS count: {e}"))),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
        Err(e) => {
            // FTS5 syntax errors are caught here, same as fts_search.
            tracing::warn!("FTS5 count_expired query failed (likely syntax error): {e}");
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::facts::FactStore;
    use crate::store::schema::{init_schema, open_memory};
    use crate::types::{FactType, NewFact};
    use chrono::Utc;

    const DIM: usize = 4;

    fn setup() -> Connection {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        conn
    }

    fn make_fact(content: &str) -> NewFact {
        NewFact {
            content: content.into(),
            content_hash: String::new(),
            embedding: vec![0.1; DIM],
            fact_type: FactType::Episodic,
            t_created: Utc::now(),
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            scope_id: 1,
            importance: 0.5,
            access_count: 0,
            last_accessed: Utc::now(),
            metadata: serde_json::json!({}),
            is_pinned: false,
        }
    }

    #[test]
    fn search_finds_matching_facts() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        store
            .insert(&make_fact("Rust is a systems programming language"))
            .unwrap();
        store
            .insert(&make_fact("Python is great for machine learning"))
            .unwrap();
        store
            .insert(&make_fact("Rust has zero-cost abstractions"))
            .unwrap();

        let results = fts_search(&conn, "Rust", 10, None, None).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn search_empty_for_nonexistent_term() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        store
            .insert(&make_fact("Rust is a systems programming language"))
            .unwrap();

        let results = fts_search(&conn, "JavaScript", 10, None, None).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn bm25_ordering_most_relevant_first() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        // The fact mentioning "Rust" twice should score better than once
        store
            .insert(&make_fact("Python is great for data science"))
            .unwrap();
        store
            .insert(&make_fact("Rust Rust Rust systems programming in Rust"))
            .unwrap();
        store.insert(&make_fact("Rust is a language")).unwrap();

        let results = fts_search(&conn, "Rust", 10, None, None).unwrap();
        assert_eq!(results.len(), 2);
        // BM25 scores: lower (more negative) = better match
        // First result should have a lower (better) score
        assert!(results[0].score <= results[1].score);
    }

    #[test]
    fn fts5_syntax_error_returns_empty() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        store.insert(&make_fact("some content")).unwrap();

        // Unbalanced quotes are an FTS5 syntax error
        let results = fts_search(&conn, "\"unbalanced", 10, None, None).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn limit_respected() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        store.insert(&make_fact("Rust language one")).unwrap();
        store.insert(&make_fact("Rust language two")).unwrap();
        store.insert(&make_fact("Rust language three")).unwrap();

        let results = fts_search(&conn, "Rust", 2, None, None).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn fts_search_filters_by_fact_type() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        // Insert 2 semantic + 1 episodic facts matching "language"
        let mut semantic_fact = make_fact("Rust language");
        semantic_fact.fact_type = FactType::Semantic;
        store.insert(&semantic_fact).unwrap();
        let mut semantic_fact2 = make_fact("Python language");
        semantic_fact2.fact_type = FactType::Semantic;
        store.insert(&semantic_fact2).unwrap();
        store.insert(&make_fact("Go language")).unwrap(); // Episodic by default

        let results = fts_search(&conn, "language", 10, Some(&FactType::Semantic), None).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn fts_search_filters_by_scope() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let mut fact1 = make_fact("Rust language");
        fact1.scope_id = 1;
        store.insert(&fact1).unwrap();
        // Insert scope row for scope_id=2 (FK constraint)
        conn.execute(
            "INSERT OR IGNORE INTO scopes (id, parent_id, label, depth) VALUES (2, 1, 'test', 1)",
            [],
        )
        .unwrap();
        let mut fact2 = make_fact("Python language");
        fact2.scope_id = 2;
        store.insert(&fact2).unwrap();

        // scope_ids [1] should only return fact1
        let results = fts_search(&conn, "language", 10, None, Some(&[1])).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn fts_search_excludes_expired() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let id = store.insert(&make_fact("Rust language")).unwrap();
        store.expire(id, Utc::now()).unwrap();

        let results = fts_search(&conn, "Rust", 10, None, None).unwrap();
        assert!(results.is_empty());
    }

    // --- fts_count_expired tests ---

    #[test]
    fn fts_count_expired_returns_zero_when_no_expired() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        store.insert(&make_fact("Rust language")).unwrap();
        store.insert(&make_fact("Python language")).unwrap();

        let count = fts_count_expired(&conn, "language", None, None).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn fts_count_expired_counts_matching_expired() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let id1 = store.insert(&make_fact("Rust language")).unwrap();
        let id2 = store.insert(&make_fact("Rust compiler")).unwrap();
        store.insert(&make_fact("Python language")).unwrap(); // active, not expired
        store.expire(id1, Utc::now()).unwrap();
        store.expire(id2, Utc::now()).unwrap();

        // Both expired facts match "Rust"
        let count = fts_count_expired(&conn, "Rust", None, None).unwrap();
        assert_eq!(count, 2);

        // Only one expired fact matches "language" (id1, not id2="compiler")
        let count = fts_count_expired(&conn, "language", None, None).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn fts_count_expired_respects_scope_filter() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let mut fact1 = make_fact("Rust language");
        fact1.scope_id = 1;
        let id1 = store.insert(&fact1).unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO scopes (id, parent_id, label, depth) VALUES (2, 1, 'test', 1)",
            [],
        )
        .unwrap();
        let mut fact2 = make_fact("Rust compiler");
        fact2.scope_id = 2;
        let id2 = store.insert(&fact2).unwrap();
        store.expire(id1, Utc::now()).unwrap();
        store.expire(id2, Utc::now()).unwrap();

        // Scope [1] should only count fact1
        let count = fts_count_expired(&conn, "Rust", None, Some(&[1])).unwrap();
        assert_eq!(count, 1);

        // Scope [1, 2] should count both
        let count = fts_count_expired(&conn, "Rust", None, Some(&[1, 2])).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn fts_count_expired_respects_fact_type_filter() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let mut semantic = make_fact("Rust language");
        semantic.fact_type = FactType::Semantic;
        let id1 = store.insert(&semantic).unwrap();
        let id2 = store.insert(&make_fact("Rust compiler")).unwrap(); // Episodic
        store.expire(id1, Utc::now()).unwrap();
        store.expire(id2, Utc::now()).unwrap();

        let count = fts_count_expired(&conn, "Rust", Some(&FactType::Semantic), None).unwrap();
        assert_eq!(count, 1);

        let count = fts_count_expired(&conn, "Rust", Some(&FactType::Episodic), None).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn fts_count_expired_syntax_error_returns_zero() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let id = store.insert(&make_fact("some content")).unwrap();
        store.expire(id, Utc::now()).unwrap();

        let count = fts_count_expired(&conn, "\"unbalanced", None, None).unwrap();
        assert_eq!(count, 0);
    }
}
