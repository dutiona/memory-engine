use rusqlite::Connection;

use crate::error::Result;

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
/// FTS5 syntax errors (e.g., unbalanced quotes) return an empty vec
/// rather than propagating an error.
///
/// # Errors
///
/// Returns `MemoryError::Database` for non-FTS5 database errors.
pub fn fts_search(conn: &Connection, query: &str, limit: usize) -> Result<Vec<FtsResult>> {
    let mut stmt = conn.prepare(
        "SELECT rowid, bm25(facts_fts) AS score \
         FROM facts_fts WHERE facts_fts MATCH ?1 \
         ORDER BY score LIMIT ?2",
    )?;

    let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);

    // FTS5 syntax errors surface at query execution time, not prepare time.
    // Catch them here and return empty results instead of propagating.
    let rows = match stmt.query_map(rusqlite::params![query, limit_i64], |row| {
        Ok(FtsResult {
            fact_id: row.get(0)?,
            score: row.get(1)?,
        })
    }) {
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
            importance: 0.5,
            access_count: 0,
            last_accessed: Utc::now(),
            metadata: serde_json::json!({}),
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

        let results = fts_search(&conn, "Rust", 10).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn search_empty_for_nonexistent_term() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        store
            .insert(&make_fact("Rust is a systems programming language"))
            .unwrap();

        let results = fts_search(&conn, "JavaScript", 10).unwrap();
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

        let results = fts_search(&conn, "Rust", 10).unwrap();
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
        let results = fts_search(&conn, "\"unbalanced", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn limit_respected() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        store.insert(&make_fact("Rust language one")).unwrap();
        store.insert(&make_fact("Rust language two")).unwrap();
        store.insert(&make_fact("Rust language three")).unwrap();

        let results = fts_search(&conn, "Rust", 2).unwrap();
        assert_eq!(results.len(), 2);
    }
}
