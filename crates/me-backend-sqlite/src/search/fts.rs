use me_types::error::StorageError;
use rusqlite::{Connection, ToSql, params_from_iter};

use crate::search::{FilterSql, serialize_scope_ids};
use crate::store::facts::fact_type_to_str;
use me_types::error::Result;
use me_types::types::FactType;

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
/// This is the verbatim active-only wrapper over [`fts_search_filtered`] used by
/// the engine's hybrid path; richer `FactFilter` dimensions are translated by the
/// backend (#684).
///
/// A query string FTS5 rejects (unbalanced quote, dangling operator, unknown column)
/// matches nothing and returns an empty vec.
///
/// # Errors
///
/// Returns `MemoryError::Storage` for every other failure, including a storage fault
/// and a row that fails to decode.
pub fn fts_search(
    conn: &Connection,
    query: &str,
    limit: usize,
    fact_type: Option<&FactType>,
    scope_ids: Option<&[i64]>,
) -> Result<Vec<FtsResult>> {
    let filter = FilterSql::active("f.", fact_type, scope_ids)?;
    fts_search_filtered(conn, query, limit, &filter)
}

/// Search the FTS5 index, applying a pre-rendered [`FilterSql`] fragment.
///
/// The single source of the BM25 query: the verbatim [`fts_search`] supplies an
/// active-only fragment, while the backend supplies the full `temporal`/`ids`/
/// `pinned`/`metadata` translation (#684). The fragment's `?` placeholders bind
/// between the `MATCH` query and the trailing `LIMIT`, so the parameter order is
/// `[query, ..filter, limit]`.
///
/// A query string FTS5 rejects matches nothing and returns an empty vec.
///
/// # Errors
///
/// Returns `MemoryError::Storage` for every other failure, including a storage fault
/// and a row that fails to decode.
pub fn fts_search_filtered(
    conn: &Connection,
    query: &str,
    limit: usize,
    filter: &FilterSql,
) -> Result<Vec<FtsResult>> {
    let sql = format!(
        "SELECT f.id, bm25(facts_fts) AS score \
         FROM facts_fts \
         JOIN facts AS f ON f.id = facts_fts.rowid \
         WHERE facts_fts MATCH ? \
           AND {} \
         ORDER BY score LIMIT ?",
        filter.where_clause
    );
    let mut stmt = conn.prepare(&sql).map_err(StorageError::backend)?;

    let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
    // Param order mirrors the `?` order in the SQL above: MATCH query first, then
    // the filter fragment's binds, then LIMIT.
    let query_param: Box<dyn ToSql> = Box::new(query.to_string());
    let limit_param: Box<dyn ToSql> = Box::new(limit_i64);
    let bound = std::iter::once(query_param.as_ref())
        .chain(filter.bind_refs())
        .chain(std::iter::once(limit_param.as_ref()));

    // `query_map` only binds: FTS5 parses the MATCH string when the statement first
    // steps, so a rejected query surfaces as the first row's error, like a fault.
    let rows = stmt
        .query_map(params_from_iter(bound), |row| {
            Ok(FtsResult {
                fact_id: row.get(0)?,
                score: row.get(1)?,
            })
        })
        .and_then(Iterator::collect);
    rows.or_else(|e| no_match_or_fault(conn, query, e, Vec::new()))
}

/// Classify a failed MATCH query: `no_match` when FTS5 rejected the query string, a
/// storage error otherwise.
///
/// FTS5 rejects a malformed string with a bare `SQLITE_ERROR` under varying messages,
/// but other failures raise the same code: the filter fragment's JSON functions (e.g. on
/// a tampered `metadata` value) and the index itself (an invalid format version, read
/// when another writer changes its config). So the error is the query's only when the
/// string fails alone with that code while a fixed valid query still runs; a string
/// that fails alone matches nothing.
fn no_match_or_fault<T>(
    conn: &Connection,
    query: &str,
    err: rusqlite::Error,
    no_match: T,
) -> Result<T> {
    let is_sqlite_error =
        |e: &rusqlite::Error| e.sqlite_extended_error_code() == Some(rusqlite::ffi::SQLITE_ERROR);
    let run_alone = |q: &str| {
        conn.query_row(
            "SELECT rowid FROM facts_fts WHERE facts_fts MATCH ?1 LIMIT 1",
            [q],
            |_| Ok(()),
        )
    };
    if is_sqlite_error(&err)
        && run_alone(query).as_ref().is_err_and(is_sqlite_error)
        && run_alone("\"a\"")
            .err()
            .and_then(|e| e.sqlite_extended_error_code())
            .is_none()
    {
        tracing::warn!("FTS5 rejected the query, reading it as no match: {err}");
        return Ok(no_match);
    }
    Err(StorageError::backend(err).into())
}

/// Count expired facts matching the FTS5 query (abstention diagnostics).
///
/// Mirrors [`fts_search`] but queries `t_expired IS NOT NULL` and returns
/// only a count — no row materialisation, no embedding deserialisation.
///
/// A query string FTS5 rejects counts `Ok(0)`, as in [`fts_search`].
///
/// # Errors
///
/// Returns `MemoryError::Storage` for every other failure.
pub fn fts_count_expired(
    conn: &Connection,
    query: &str,
    fact_type: Option<&FactType>,
    scope_ids: Option<&[i64]>,
) -> Result<usize> {
    let mut stmt = conn
        .prepare(
            "SELECT COUNT(*) \
         FROM facts_fts \
         JOIN facts AS f ON f.id = facts_fts.rowid \
         WHERE facts_fts MATCH ?1 \
           AND f.t_expired IS NOT NULL \
           AND (?2 IS NULL OR f.fact_type = ?2) \
           AND (?3 IS NULL OR f.scope_id IN (SELECT value FROM json_each(?3)))",
        )
        .map_err(StorageError::backend)?;

    let fact_type_str: Option<&str> = fact_type.map(fact_type_to_str);
    let scope_ids_json = serialize_scope_ids(scope_ids)?;

    let n = stmt
        .query_row(
            rusqlite::params![query, fact_type_str, scope_ids_json],
            |row| row.get::<_, i64>(0),
        )
        .or_else(|e| no_match_or_fault(conn, query, e, 0))?;
    usize::try_from(n)
        .map_err(|e| me_types::error::MemoryError::Internal(format!("invalid FTS count: {e}")))
}

/// Fuzz-only seam (`--cfg fuzzing`, set only by `cargo fuzz`).
///
/// Drives an arbitrary, attacker-controlled query string through the full FTS5
/// `MATCH` path on a freshly-seeded in-memory SQLite database. The seam owns the
/// DB setup so the harness can stay a thin `&[u8] -> ()` wrapper while still
/// reaching the crate-internal store/schema helpers (both `pub(crate)`).
///
/// The contract under test is total: any UTF-8 query string — balanced or not,
/// containing FTS5 operators, column filters, quotes, NUL bytes, or arbitrary
/// Unicode — must either return rows or be rejected by FTS5 and read as no match.
/// On this healthy store every error a query string can cause is such a
/// rejection, so it must NEVER panic and never propagate an error.
///
/// Compiled solely under `cargo fuzz` (`#[cfg(fuzzing)]`); adds nothing to the
/// shipped crate.
#[cfg(fuzzing)]
#[doc(hidden)]
pub fn fuzz_fts_query(query: &str) {
    use crate::store::facts::FactStore;
    use crate::store::schema::{init_schema, open_memory};
    use chrono::Utc;
    use me_types::types::{FactType, NewFact};

    const DIM: usize = 4;

    // These are fuzzer-setup invariants, not the fuzzed input: a failure here is a
    // broken fuzzing environment (out of memory, missing FTS5 build, etc.), not a
    // finding about the query path. Panic loudly so a broken env halts the fuzzer
    // instead of silently running empty iterations and masking a dead test.
    let conn = open_memory().expect("fuzzer DB setup failed: open_memory");
    init_schema(&conn).expect("fuzzer DB setup failed: init_schema");

    let store = FactStore::new(&conn, DIM);
    // Seed a couple of rows so the FTS5 index is non-empty: seeding a non-empty
    // index exercises the full row-materialisation + BM25 scoring path rather than
    // an empty-result fast path, so any parser-level panic in the query string is
    // reachable.
    for content in [
        "Rust systems programming language",
        "Python machine learning",
    ] {
        let fact = NewFact {
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
            base_importance: 0.5,
            access_count: 0,
            last_accessed: Utc::now(),
            metadata: serde_json::json!({}),
            is_pinned: false,
        };
        store
            .insert(&fact)
            .expect("fuzzer DB setup failed: seed insert");
    }

    // Drive the untrusted query through every public MATCH entry point: the
    // active-only search, the filtered variant (with a fact-type + scope
    // filter exercising the extra bind ordering), and the expired-count probe.
    //
    // The fuzzed property is the *total* contract: every query string — balanced
    // or malformed — must map to `Ok`, never propagate. A string FTS5 rejects
    // surfaces when the statement first steps and is folded into an empty result
    // (`Ok(vec![])` / `Ok(0)`), so `Err` here is a string the classifier failed
    // to attribute (or a genuine DB fault) — assert it loudly.
    fts_search(&conn, query, 10, None, None)
        .expect("malformed query must map to Ok, not propagate");
    fts_search(&conn, query, 10, Some(&FactType::Episodic), Some(&[1]))
        .expect("malformed query must map to Ok, not propagate");
    fts_count_expired(&conn, query, None, None)
        .expect("malformed query must map to Ok, not propagate");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::facts::FactStore;
    use crate::store::schema::{init_schema, open_memory};
    use chrono::Utc;
    use me_types::types::{FactType, NewFact};

    const DIM: usize = 4;

    fn setup() -> Connection {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        conn
    }

    fn make_fact(content: &str) -> NewFact {
        // kept: single-arg (embedding fixed to vec![0.1; DIM] using local DIM const)
        me_types::test_util::new_fact(content, vec![0.1; DIM])
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
        assert_eq!(results, []);
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
        assert_eq!(results, []);
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
        assert_eq!(results, []);
    }

    #[test]
    fn fts_search_empty_query_returns_empty() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        store.insert(&make_fact("Rust language")).unwrap();

        // An empty MATCH string is an FTS5 syntax error -> caught and mapped
        // to an empty result vec, never an Err.
        let results = fts_search(&conn, "", 10, None, None).unwrap();
        assert_eq!(results, []);
    }

    #[test]
    fn fts_search_empty_scope_slice_matches_nothing() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        store.insert(&make_fact("Rust language")).unwrap();

        // Some(&[]) serializes to "[]"; the `scope_id IN (SELECT ... json_each)`
        // subquery is empty, so no row can match — distinct
        // from None, which disables the filter entirely.
        let scoped = fts_search(&conn, "Rust", 10, None, Some(&[])).unwrap();
        assert!(
            scoped.is_empty(),
            "empty scope slice must exclude all facts"
        );
        // Sanity: the same query with no scope filter does find the fact.
        let unscoped = fts_search(&conn, "Rust", 10, None, None).unwrap();
        assert_eq!(unscoped.len(), 1);
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

    // --- error classification (#1067) ---

    /// The extended result code of the bare MATCH query, `None` when it succeeds.
    fn raw_match_code(conn: &Connection, query: &str) -> Option<i32> {
        conn.query_row(
            "SELECT count(*) FROM facts_fts WHERE facts_fts MATCH ?1",
            [query],
            |row| row.get::<_, i64>(0),
        )
        .err()
        .and_then(|e| e.sqlite_extended_error_code())
    }

    fn is_storage_error<T: std::fmt::Debug>(outcome: &Result<T>) -> bool {
        matches!(
            outcome,
            Err(me_types::error::MemoryError::Storage(
                StorageError::Backend(_)
            ))
        )
    }

    #[test]
    fn malformed_match_strings_read_as_no_match() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let id = store.insert(&make_fact("Rust language")).unwrap();
        store.expire(id, Utc::now()).unwrap();
        store.insert(&make_fact("Rust compiler")).unwrap();

        for query in [
            "\"unbalanced",
            "foo AND",
            "(",
            "NEAR(a b",
            "^",
            "a + ",
            "\"a\" OR",
            "nocol:rust",
            "*",
        ] {
            assert_eq!(
                raw_match_code(&conn, query),
                Some(rusqlite::ffi::SQLITE_ERROR),
                "premise: FTS5 rejects {query:?} with a bare SQLITE_ERROR"
            );
            assert_eq!(fts_search(&conn, query, 10, None, None).unwrap(), []);
            assert_eq!(fts_count_expired(&conn, query, None, None).unwrap(), 0);
        }
    }

    #[test]
    fn corrupt_fts_index_is_a_storage_error() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let id = store.insert(&make_fact("Rust language")).unwrap();
        store.expire(id, Utc::now()).unwrap();
        conn.execute("DELETE FROM facts_fts_data", []).unwrap();

        let code = raw_match_code(&conn, "Rust");
        assert!(
            code.is_some_and(|c| c != rusqlite::ffi::SQLITE_ERROR),
            "injection: the MATCH must fail with a storage code, got {code:?}"
        );
        assert!(is_storage_error(&fts_search(&conn, "Rust", 10, None, None)));
        assert!(is_storage_error(&fts_count_expired(
            &conn, "Rust", None, None
        )));
    }

    #[test]
    fn unreadable_fts_config_on_an_open_connection_is_a_storage_error() {
        // FTS5 reloads its config when another writer bumps the config cookie; an
        // invalid format version then fails every MATCH at step time with a bare
        // SQLITE_ERROR, valid query strings included.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fts.db");
        let conn = Connection::open(&path).unwrap();
        init_schema(&conn).unwrap();
        FactStore::new(&conn, DIM)
            .insert(&make_fact("Rust language"))
            .unwrap();
        assert_eq!(fts_search(&conn, "Rust", 10, None, None).unwrap().len(), 1);

        let writer = Connection::open(&path).unwrap();
        writer
            .execute(
                "INSERT INTO facts_fts(facts_fts, rank) VALUES('automerge', 4)",
                [],
            )
            .unwrap();
        writer
            .execute("UPDATE facts_fts_config SET v = 99 WHERE k = 'version'", [])
            .unwrap();

        assert_eq!(
            raw_match_code(&conn, "Rust"),
            Some(rusqlite::ffi::SQLITE_ERROR),
            "injection: a valid MATCH must now fail with a bare SQLITE_ERROR"
        );
        assert!(is_storage_error(&fts_search(&conn, "Rust", 10, None, None)));
        assert!(is_storage_error(&fts_search(
            &conn,
            "\"unbalanced",
            10,
            None,
            None
        )));
    }

    #[test]
    fn malformed_metadata_under_a_valid_query_is_a_storage_error() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let id = store.insert(&make_fact("Rust language")).unwrap();
        // CHECK(json_valid(metadata)) rejects a plain UPDATE; bypass it to simulate
        // on-disk tampering or a writer without the constraint.
        conn.execute_batch("PRAGMA ignore_check_constraints = ON")
            .unwrap();
        conn.execute("UPDATE facts SET metadata = '{' WHERE id = ?1", [id])
            .unwrap();
        conn.execute_batch("PRAGMA ignore_check_constraints = OFF")
            .unwrap();

        let filter = FilterSql {
            where_clause: "json_extract(f.metadata, ?) IS NOT NULL".into(),
            params: vec![Box::new("$.k".to_string())],
        };
        assert!(is_storage_error(&fts_search_filtered(
            &conn, "Rust", 10, &filter
        )));
    }
}
