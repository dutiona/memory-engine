use me_types::error::StorageError;
use rusqlite::{Connection, params_from_iter};

use crate::search::FilterSql;
use crate::store::deserialize_embedding;
use me_types::error::Result;
use me_types::types::FactType;

/// A single vector search result with fact id and cosine similarity score.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorResult {
    pub fact_id: i64,
    pub score: f32,
}

/// Candidate-set size above which a brute-force scan is flagged as a scaling
/// signal.
///
/// Brute-force cosine search is inherently O(N) per query — every active
/// embedding in the (post-SQL-filter) candidate set is deserialized and scored.
/// There is therefore **no correctness-preserving way to cap the scan**: a SQL
/// `LIMIT` would silently drop candidates and corrupt recall. The bound is
/// instead observational — past this many candidates we emit a one-time `warn`
/// directing operators at the `ann` feature (HNSW), which provides the actual
/// sublinear path.
///
/// Kept equal to the default [`SearchConfig::ann_threshold`](crate::search::strategy::SearchConfig)
/// (50,000) — the documented fact count at which ANN should take over.
// `search` is a crate-private module, so `pub(crate)` is the honest visibility;
// the `redundant_pub_crate` lint fires only because nothing here is `pub`, but
// the marker documents intent (vs widening to `pub` to dodge the lint).
#[allow(clippy::redundant_pub_crate)]
pub(crate) const BRUTE_FORCE_WARN_THRESHOLD: usize = 50_000;

/// Whether a brute-force candidate set is large enough to warrant the scaling
/// warning. Split out as a pure predicate so the boundary is unit-testable
/// without materializing a 50k-fact corpus.
// `pub(crate)` is honest (crate-private module); `#[allow]` keeps the lint quiet
// without widening visibility.
#[allow(clippy::redundant_pub_crate)]
#[must_use]
pub(crate) const fn brute_force_scan_is_oversized(candidates: usize) -> bool {
    candidates > BRUTE_FORCE_WARN_THRESHOLD
}

/// Process-lifetime guard so the scaling warning fires at most once, rather than
/// spamming a line per query once a deployment has outgrown brute-force.
static BRUTE_FORCE_WARN_ONCE: std::sync::Once = std::sync::Once::new();

/// Compute the cosine similarity between two vectors.
///
/// Relocated to [`me_types::math::cosine_similarity`] (Wave 2 #816 / S4,
/// sub-PR 3a) — it is pure math with no SQL/driver dependency, so keeping it
/// here would have forced the forthcoming `me-archive` crate (an L3 leaf) to
/// depend on this concrete backend crate just to reuse a scoring function.
/// Re-exported so every existing `me_backend_sqlite::search::vector::cosine_similarity`
/// / `crate::search::cosine_similarity` call site in this crate keeps resolving
/// unchanged.
pub use me_types::math::cosine_similarity;

/// Brute-force vector similarity search over active facts.
///
/// Streams all active facts (`t_expired IS NULL`) from `SQLite`, deserializes
/// their embeddings, computes cosine similarity against `query_embedding`,
/// and returns the top `limit` results sorted descending by score.
///
/// # Errors
///
/// Returns `MemoryError::Storage` on query failure, or
/// `MemoryError::EmbeddingDimension` if a stored embedding has the wrong size.
//
// The live SQLite path goes through `vector_search_filtered`; this unfiltered
// convenience wrapper is reached only by the `ann` brute-force fallback and by
// tests, so it is `dead_code` in a no-`ann` release build — suppress the lint
// there rather than widen visibility.
#[cfg_attr(not(any(test, feature = "ann")), allow(dead_code))]
// `search` is a crate-private module, so `pub(crate)` is the honest visibility;
// the lint only fires because the module isn't `pub`.
#[allow(clippy::redundant_pub_crate)]
pub(crate) fn vector_search(
    conn: &Connection,
    query_embedding: &[f32],
    embed_dim: usize,
    limit: usize,
    fact_type: Option<&FactType>,
    scope_ids: Option<&[i64]>,
) -> Result<Vec<VectorResult>> {
    let filter = FilterSql::active("", fact_type, scope_ids)?;
    vector_search_filtered(conn, query_embedding, embed_dim, limit, &filter)
}

/// Brute-force vector similarity search applying a pre-rendered [`FilterSql`].
///
/// The single source of the brute-force scan: the verbatim [`vector_search`]
/// supplies an active-only fragment, while the backend supplies the full
/// `temporal`/`ids`/`pinned`/`metadata` translation (#684). The fragment's bare
/// (un-aliased) column references match this query's single-table `FROM facts`.
///
/// # Errors
///
/// Returns `MemoryError::Storage` on query failure, or
/// `MemoryError::EmbeddingDimension` if a stored (or the query) embedding has the
/// wrong size.
pub fn vector_search_filtered(
    conn: &Connection,
    query_embedding: &[f32],
    embed_dim: usize,
    limit: usize,
    filter: &FilterSql,
) -> Result<Vec<VectorResult>> {
    if query_embedding.len() != embed_dim {
        return Err(me_types::error::MemoryError::EmbeddingDimension {
            expected: embed_dim,
            actual: query_embedding.len(),
        });
    }

    let sql = format!(
        "SELECT id, embedding FROM facts WHERE {}",
        filter.where_clause
    );
    let mut stmt = conn.prepare(&sql).map_err(StorageError::backend)?;

    let rows = stmt
        .query_map(params_from_iter(filter.bind_refs()), |row| {
            let id: i64 = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            Ok((id, blob))
        })
        .map_err(StorageError::backend)?;

    let mut scored: Vec<VectorResult> = Vec::new();
    for row in rows {
        let (id, blob) = row.map_err(StorageError::backend)?;
        let embedding = deserialize_embedding(&blob, embed_dim)?;
        let score = cosine_similarity(query_embedding, &embedding);
        scored.push(VectorResult { fact_id: id, score });
    }

    // Scaling signal (issue #572 / L9): brute force is O(N) per query and cannot
    // be capped without dropping recall, so flag — once — when the candidate set
    // has outgrown it. The `ann` feature is the sublinear remedy.
    if brute_force_scan_is_oversized(scored.len()) {
        let candidates = scored.len();
        BRUTE_FORCE_WARN_ONCE.call_once(|| {
            tracing::warn!(
                candidates,
                threshold = BRUTE_FORCE_WARN_THRESHOLD,
                "brute-force vector scan exceeded the scaling threshold; \
                 enable the `ann` feature for sublinear search"
            );
        });
    }

    // Partial sort: O(N) partition then sort only top `limit` elements
    if scored.len() > limit {
        scored.select_nth_unstable_by(limit, |a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(limit);
    }
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(scored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::facts::FactStore;
    use crate::store::schema::{init_schema, open_memory};
    use me_types::types::{FactType, NewFact};

    const DIM: usize = 4;

    fn setup() -> Connection {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        conn
    }

    fn make_fact_with_embedding(content: &str, embedding: Vec<f32>) -> NewFact {
        me_types::test_util::new_fact(content, embedding)
    }

    #[test]
    fn brute_force_oversized_predicate_boundary() {
        // Strictly greater than the threshold is "oversized"; equal is not.
        assert!(!brute_force_scan_is_oversized(0));
        assert!(!brute_force_scan_is_oversized(BRUTE_FORCE_WARN_THRESHOLD));
        assert!(brute_force_scan_is_oversized(
            BRUTE_FORCE_WARN_THRESHOLD + 1
        ));
    }

    #[test]
    fn brute_force_threshold_tracks_ann_threshold_default() {
        // The scaling warning must fire at the same corpus size the engine
        // documents for switching to ANN, so the two never drift apart.
        use crate::search::strategy::SearchConfig;
        assert_eq!(
            BRUTE_FORCE_WARN_THRESHOLD,
            SearchConfig::default().ann_threshold
        );
    }

    #[test]
    fn vector_search_rejects_wrong_query_dimension() {
        let conn = setup();
        let wrong_dim_query = [1.0_f32, 0.0]; // DIM is 4, query is 2
        let result = vector_search(&conn, &wrong_dim_query, DIM, 3, None, None);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(
            err,
            me_types::error::MemoryError::EmbeddingDimension {
                expected: 4,
                actual: 2
            }
        ));
    }

    #[test]
    fn vector_search_returns_top_k_descending() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);

        // Query embedding: [1, 0, 0, 0]
        let query = [1.0_f32, 0.0, 0.0, 0.0];

        // Insert 5 facts with known embeddings of varying similarity to query
        store
            .insert(&make_fact_with_embedding("exact", vec![1.0, 0.0, 0.0, 0.0]))
            .unwrap(); // cosine = 1.0
        store
            .insert(&make_fact_with_embedding("close", vec![0.9, 0.1, 0.0, 0.0]))
            .unwrap(); // high
        store
            .insert(&make_fact_with_embedding(
                "medium",
                vec![0.5, 0.5, 0.0, 0.0],
            ))
            .unwrap(); // medium
        store
            .insert(&make_fact_with_embedding("far", vec![0.0, 1.0, 0.0, 0.0]))
            .unwrap(); // cosine = 0.0
        store
            .insert(&make_fact_with_embedding(
                "opposite",
                vec![-1.0, 0.0, 0.0, 0.0],
            ))
            .unwrap(); // cosine = -1.0

        let results = vector_search(&conn, &query, DIM, 3, None, None).unwrap();
        assert_eq!(results.len(), 3);
        // Descending order by score
        assert!(results[0].score >= results[1].score);
        assert!(results[1].score >= results[2].score);
        // Top result should be the exact match
        assert!((results[0].score - 1.0).abs() < 1e-5);
    }

    #[test]
    fn vector_search_filters_by_scope() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        // Need scope 2 to exist for FK
        conn.execute(
            "INSERT OR IGNORE INTO scopes (id, parent_id, label, depth) VALUES (2, 1, 'test', 1)",
            [],
        )
        .unwrap();

        let mut fact1 = make_fact_with_embedding("fact one", vec![1.0, 0.0, 0.0, 0.0]);
        fact1.scope_id = 1;
        store.insert(&fact1).unwrap();
        let mut fact2 = make_fact_with_embedding("fact two", vec![0.9, 0.1, 0.0, 0.0]);
        fact2.scope_id = 2;
        store.insert(&fact2).unwrap();

        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let results = vector_search(&conn, &query, DIM, 10, None, Some(&[1])).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn vector_search_filters_by_fact_type() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let mut fact1 = make_fact_with_embedding("fact one", vec![1.0, 0.0, 0.0, 0.0]);
        fact1.fact_type = FactType::Semantic;
        store.insert(&fact1).unwrap();
        store
            .insert(&make_fact_with_embedding(
                "fact two",
                vec![0.9, 0.1, 0.0, 0.0],
            ))
            .unwrap(); // Episodic

        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let results =
            vector_search(&conn, &query, DIM, 10, Some(&FactType::Semantic), None).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn vector_search_empty_scope_slice_matches_nothing() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        store
            .insert(&make_fact_with_embedding(
                "fact one",
                vec![1.0, 0.0, 0.0, 0.0],
            ))
            .unwrap();

        let query = [1.0_f32, 0.0, 0.0, 0.0];
        // Some(&[]) serializes to "[]"; the json_each subquery is empty so the
        // `scope_id IN (...)` predicate excludes every fact (vector.rs:61-63),
        // unlike None which disables the filter.
        let scoped = vector_search(&conn, &query, DIM, 10, None, Some(&[])).unwrap();
        assert!(
            scoped.is_empty(),
            "empty scope slice must exclude all facts"
        );
        // Sanity: with no scope filter the fact is found.
        let unscoped = vector_search(&conn, &query, DIM, 10, None, None).unwrap();
        assert_eq!(unscoped.len(), 1);
    }
}
