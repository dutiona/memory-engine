//! HNSW-based approximate nearest neighbor search.
//!
//! Gated behind the `ann` feature flag.
//! Uses the `hnsw` crate (rust-cv) which owns inserted vectors,
//! avoiding lifetime issues with the HNSW graph.

use space::Metric;

use crate::search::cosine_similarity;

/// Cosine distance metric for the `hnsw` crate.
///
/// Converts cosine distance (1 - similarity) to `u32` via `f32::to_bits()`.
/// For non-negative f32 values, bit representation preserves total order,
/// satisfying `space::Metric`'s `Unit: Ord` requirement.
///
/// Edge cases:
/// - Zero-norm vectors return distance 1.0 (maximum cosine distance for
///   degenerate input, placing them far from all real vectors).
/// - Result clamped to \[0, 2\] to avoid NaN/negative from floating-point noise.
#[derive(Copy, Clone)]
pub struct CosineMetric;

#[allow(clippy::ptr_arg)] // space::Metric trait requires &P where P=Vec<f32>
impl Metric<Vec<f32>> for CosineMetric {
    type Unit = u32;

    fn distance(&self, a: &Vec<f32>, b: &Vec<f32>) -> u32 {
        let sim = cosine_similarity(a, b);
        // Guard against NaN from degenerate inputs (e.g. all-NaN vectors).
        // cosine_similarity returns 0.0 for zero-norm vectors, but NaN
        // can propagate if input elements are NaN. Treat NaN as orthogonal.
        let sim = if sim.is_nan() { 0.0 } else { sim };
        let dist = (1.0 - sim).clamp(0.0, 2.0);
        dist.to_bits()
    }
}

use std::collections::{HashMap, HashSet};

use hnsw::{Hnsw, Searcher};
use parking_lot::RwLock;
use rand::rngs::SmallRng;
use rusqlite::Connection;
use space::Neighbor;

use crate::error::Result;
use crate::search::strategy::VectorSearchStrategy;
use crate::search::vector::VectorResult;
use crate::store::deserialize_embedding;
use crate::types::FactType;

/// HNSW approximate nearest neighbor strategy.
///
/// Wraps an in-memory HNSW index with ID mappings (`hnsw_index → fact_id`)
/// and a tombstone set for lazily excluding expired facts from results.
pub struct HnswStrategy {
    inner: RwLock<HnswInner>,
    embed_dim: usize,
}

struct HnswInner {
    index: Hnsw<CosineMetric, Vec<f32>, SmallRng, 16, 32>,
    index_to_fact: Vec<i64>,
    /// Maps fact_id → current HNSW index (latest insert wins).
    fact_to_hnsw: HashMap<i64, usize>,
    /// Tombstoned HNSW indices (not fact IDs) — excludes stale entries
    /// when a fact is expired or replaced by a newer insert.
    tombstones: HashSet<usize>,
}

const OVERFETCH_FACTOR: usize = 3;
const DEFAULT_EF_SEARCH: usize = 100;
const MAX_WIDEN_ATTEMPTS: usize = 3;
const HNSW_SEED: u64 = 42;

impl HnswStrategy {
    /// Number of active (non-tombstoned) items in the index.
    #[must_use]
    pub fn active_count(&self) -> usize {
        let inner = self.inner.read();
        inner.fact_to_hnsw.len()
    }

    /// Build an HNSW index from all active facts in the database.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure, or
    /// `MemoryError::EmbeddingDimension` if a stored embedding has the wrong size.
    ///
    /// # Panics
    ///
    /// Panics if HNSW does not assign sequential IDs starting from 0.
    pub fn build_from_db(conn: &Connection, embed_dim: usize) -> Result<Self> {
        use rand::SeedableRng;

        let mut index: Hnsw<CosineMetric, Vec<f32>, SmallRng, 16, 32> = Hnsw::new_params_and_prng(
            CosineMetric,
            hnsw::Params::new().ef_construction(200),
            SmallRng::seed_from_u64(HNSW_SEED),
        );
        let mut searcher: Searcher<u32> = Searcher::default();
        let mut index_to_fact = Vec::new();

        let mut stmt =
            conn.prepare("SELECT id, embedding FROM facts WHERE t_expired IS NULL ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            let id: i64 = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            Ok((id, blob))
        })?;

        let mut fact_to_hnsw = HashMap::new();
        for row in rows {
            let (fact_id, blob) = row?;
            let embedding = deserialize_embedding(&blob, embed_dim)?;
            let hnsw_id = index.insert(embedding, &mut searcher);
            if hnsw_id != index_to_fact.len() {
                return Err(crate::error::MemoryError::Internal(format!(
                    "HNSW index must assign sequential IDs (got {hnsw_id}, expected {})",
                    index_to_fact.len()
                )));
            }
            index_to_fact.push(fact_id);
            fact_to_hnsw.insert(fact_id, hnsw_id);
        }

        Ok(Self {
            inner: RwLock::new(HnswInner {
                index,
                index_to_fact,
                fact_to_hnsw,
                tombstones: HashSet::new(),
            }),
            embed_dim,
        })
    }

    /// Snapshot active embeddings for fast cold-start.
    ///
    /// Reads embeddings from DB (vectors are not kept in memory). Returns
    /// compact data: only active facts, ordered by fact_id. On load,
    /// `from_snapshot` rebuilds a fresh compact HNSW index from this data.
    pub(crate) fn to_snapshot(
        &self,
        conn: &Connection,
        embed_dim: usize,
    ) -> Result<crate::engine::snapshot::HnswSnapshot> {
        use crate::engine::snapshot::{HnswEntry, HnswSnapshot};

        let mut stmt =
            conn.prepare("SELECT id, embedding FROM facts WHERE t_expired IS NULL ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            let id: i64 = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            Ok((id, blob))
        })?;

        let mut entries = Vec::new();
        for row in rows {
            let (fact_id, blob) = row?;
            let embedding = deserialize_embedding(&blob, embed_dim)?;
            entries.push(HnswEntry { fact_id, embedding });
        }

        Ok(HnswSnapshot { entries })
    }

    /// Rebuild a compact HNSW index from snapshot data (no DB I/O needed).
    ///
    /// Uses the same seed and parameters as `build_from_db` for deterministic
    /// topology.
    pub(crate) fn from_snapshot(
        snap: &crate::engine::snapshot::HnswSnapshot,
        embed_dim: usize,
    ) -> Result<Self> {
        use rand::SeedableRng;

        let mut index: Hnsw<CosineMetric, Vec<f32>, SmallRng, 16, 32> = Hnsw::new_params_and_prng(
            CosineMetric,
            hnsw::Params::new().ef_construction(200),
            SmallRng::seed_from_u64(HNSW_SEED),
        );
        let mut searcher: Searcher<u32> = Searcher::default();
        let mut index_to_fact = Vec::new();
        let mut fact_to_hnsw = HashMap::new();

        for entry in &snap.entries {
            if entry.embedding.len() != embed_dim {
                return Err(crate::error::MemoryError::EmbeddingDimension {
                    expected: embed_dim,
                    actual: entry.embedding.len(),
                });
            }
            let hnsw_id = index.insert(entry.embedding.clone(), &mut searcher);
            if hnsw_id != index_to_fact.len() {
                return Err(crate::error::MemoryError::Internal(format!(
                    "HNSW index must assign sequential IDs (got {hnsw_id}, expected {})",
                    index_to_fact.len()
                )));
            }
            index_to_fact.push(entry.fact_id);
            fact_to_hnsw.insert(entry.fact_id, hnsw_id);
        }

        Ok(Self {
            inner: RwLock::new(HnswInner {
                index,
                index_to_fact,
                fact_to_hnsw,
                tombstones: HashSet::new(),
            }),
            embed_dim,
        })
    }
}

impl VectorSearchStrategy for HnswStrategy {
    fn search(
        &self,
        conn: &Connection,
        query_embedding: &[f32],
        _embed_dim: usize,
        limit: usize,
        fact_type: Option<&FactType>,
        scope_ids: Option<&[i64]>,
    ) -> Result<Vec<VectorResult>> {
        if query_embedding.len() != self.embed_dim {
            return Err(crate::error::MemoryError::EmbeddingDimension {
                expected: self.embed_dim,
                actual: query_embedding.len(),
            });
        }

        let mut ef = DEFAULT_EF_SEARCH;
        let mut overfetch = limit.saturating_mul(OVERFETCH_FACTOR);
        let mut results = Vec::new();
        let query_vec = query_embedding.to_vec();

        for _attempt in 0..MAX_WIDEN_ATTEMPTS {
            // Phase 1: Collect HNSW candidates under read lock, then release.
            let candidates = {
                let inner = self.inner.read();
                let mut searcher: Searcher<u32> = Searcher::default();

                // dest length must not exceed the number of indexed items
                // (hnsw 0.11 panics on copy_from_slice if dest > item count)
                let n_items = inner.index_to_fact.len();
                if n_items == 0 {
                    return Ok(Vec::new());
                }
                let dest_len = overfetch.min(n_items);
                let mut dest = vec![
                    Neighbor {
                        index: !0,
                        distance: !0,
                    };
                    dest_len
                ];
                let neighbors = inner
                    .index
                    .nearest(&query_vec, ef, &mut searcher, &mut dest);

                let mut cands = Vec::new();
                for neighbor in neighbors {
                    if !inner.tombstones.contains(&neighbor.index) {
                        let fact_id = inner.index_to_fact[neighbor.index];
                        cands.push(fact_id);
                    }
                }
                cands
            }; // Read lock released here

            // Phase 2: Post-filter and exact-score ALL candidates via DB
            results.clear();
            results.reserve(candidates.len());
            for fact_id in candidates {
                let passes = check_fact_filters(conn, fact_id, fact_type, scope_ids)?;
                if !passes {
                    continue;
                }
                let stored_emb = load_embedding(conn, fact_id, self.embed_dim)?;
                let score = crate::search::cosine_similarity(query_embedding, &stored_emb);
                results.push(VectorResult { fact_id, score });
            }

            if results.len() >= limit {
                break;
            }
            // Widen both search accuracy and candidate count so aggressive
            // filters don't exhaust the same small candidate set each retry.
            ef = ef.saturating_mul(2);
            overfetch = overfetch.saturating_mul(2);
        }

        // If HNSW widening couldn't satisfy, fall back to brute-force.
        if results.len() < limit {
            return crate::search::vector_search(
                conn,
                query_embedding,
                self.embed_dim,
                limit,
                fact_type,
                scope_ids,
            );
        }

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);

        Ok(results)
    }

    fn name(&self) -> &str {
        "hnsw"
    }

    fn notify_insert(&self, fact_id: i64, embedding: &[f32]) {
        let mut inner = self.inner.write();
        // Tombstone the old HNSW entry for this fact_id (if any) so the
        // stale embedding is excluded from future searches.
        if let Some(&old_hnsw_id) = inner.fact_to_hnsw.get(&fact_id) {
            inner.tombstones.insert(old_hnsw_id);
        }
        let vec = embedding.to_vec();
        let mut searcher: Searcher<u32> = Searcher::default();
        let hnsw_id = inner.index.insert(vec, &mut searcher);
        assert_eq!(
            hnsw_id,
            inner.index_to_fact.len(),
            "HNSW sequential ID invariant violated on insert: got {hnsw_id}, \
             expected {}. This indicates a bug in the hnsw crate or a \
             concurrent modification. Index is now corrupt.",
            inner.index_to_fact.len()
        );
        inner.index_to_fact.push(fact_id);
        inner.fact_to_hnsw.insert(fact_id, hnsw_id);
    }

    fn notify_expire(&self, fact_id: i64) {
        let mut inner = self.inner.write();
        if let Some(hnsw_id) = inner.fact_to_hnsw.remove(&fact_id) {
            inner.tombstones.insert(hnsw_id);
        }
    }
}

fn check_fact_filters(
    conn: &Connection,
    fact_id: i64,
    fact_type: Option<&FactType>,
    scope_ids: Option<&[i64]>,
) -> Result<bool> {
    use crate::search::serialize_scope_ids;
    use crate::store::facts::fact_type_to_str;

    let scope_json = serialize_scope_ids(scope_ids)?;
    let exists: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM facts
            WHERE id = ?1
            AND t_expired IS NULL
            AND (?2 IS NULL OR fact_type = ?2)
            AND (?3 IS NULL OR scope_id IN (SELECT value FROM json_each(?3)))
        )",
        rusqlite::params![fact_id, fact_type.map(fact_type_to_str), scope_json,],
        |row| row.get(0),
    )?;

    Ok(exists)
}

fn load_embedding(conn: &Connection, fact_id: i64, embed_dim: usize) -> Result<Vec<f32>> {
    let blob: Vec<u8> = conn.query_row(
        "SELECT embedding FROM facts WHERE id = ?1",
        [fact_id],
        |row| row.get(0),
    )?;
    deserialize_embedding(&blob, embed_dim)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_metric_identical_vectors() {
        let m = CosineMetric;
        let a = vec![1.0_f32, 0.0, 0.0];
        let b = vec![1.0_f32, 0.0, 0.0];
        assert_eq!(m.distance(&a, &b), 0.0_f32.to_bits());
    }

    #[test]
    fn cosine_metric_orthogonal_vectors() {
        let m = CosineMetric;
        let a = vec![1.0_f32, 0.0, 0.0];
        let b = vec![0.0_f32, 1.0, 0.0];
        assert_eq!(m.distance(&a, &b), 1.0_f32.to_bits());
    }

    #[test]
    fn cosine_metric_preserves_distance_ordering() {
        let m = CosineMetric;
        let query = vec![1.0_f32, 0.0, 0.0];
        let close = vec![0.9_f32, 0.1, 0.0];
        let far = vec![0.0_f32, 1.0, 0.0];
        assert!(m.distance(&query, &close) < m.distance(&query, &far));
    }

    #[test]
    fn hnsw_spike_basic_search() {
        use hnsw::{Hnsw, Searcher};
        use rand::SeedableRng;
        use rand::rngs::SmallRng;
        use space::Neighbor;

        // Build a small index: 5 vectors, dim=4, M=8, M0=16
        let mut index: Hnsw<CosineMetric, Vec<f32>, SmallRng, 8, 16> = Hnsw::new_params_and_prng(
            CosineMetric,
            hnsw::Params::new().ef_construction(100),
            SmallRng::seed_from_u64(42),
        );
        let mut searcher = Searcher::default();

        let vectors = vec![
            vec![1.0, 0.0, 0.0, 0.0], // id 0: "north"
            vec![0.9, 0.1, 0.0, 0.0], // id 1: close to north
            vec![0.0, 1.0, 0.0, 0.0], // id 2: "east"
            vec![0.0, 0.0, 1.0, 0.0], // id 3: "up"
            vec![0.1, 0.0, 0.0, 1.0], // id 4: mostly "w" axis
        ];

        for v in &vectors {
            index.insert(v.clone(), &mut searcher);
        }

        // Search for "north" -- should find id 0 first, id 1 second
        let query = vec![1.0_f32, 0.0, 0.0, 0.0];
        let mut dest = vec![
            Neighbor {
                index: !0,
                distance: !0,
            };
            3
        ];
        let results = index.nearest(&query, 24, &mut searcher, &mut dest);

        assert!(results.len() >= 2, "should find at least 2 neighbors");
        // Closest should be id 0 (exact match, distance 0)
        assert_eq!(results[0].index, 0);
        assert_eq!(results[0].distance, 0);
        // Second closest should be id 1
        assert_eq!(results[1].index, 1);
    }

    #[test]
    fn hnsw_is_send_sync() {
        use hnsw::Hnsw;
        use rand::rngs::SmallRng;

        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Hnsw<CosineMetric, Vec<f32>, SmallRng, 16, 32>>();
    }

    mod hnsw_strategy_tests {
        use super::*;
        use crate::search::strategy::VectorSearchStrategy;
        use crate::store::facts::FactStore;
        use crate::store::schema::{init_schema, open_memory};
        use crate::types::{FactType, NewFact};
        use chrono::Utc;

        const DIM: usize = 4;

        fn setup_with_facts() -> (rusqlite::Connection, Vec<i64>) {
            let conn = open_memory().unwrap();
            init_schema(&conn).unwrap();
            let store = FactStore::new(&conn, DIM);
            let mut ids = Vec::new();
            let embeddings = vec![
                vec![1.0, 0.0, 0.0, 0.0],
                vec![0.9, 0.1, 0.0, 0.0],
                vec![0.0, 1.0, 0.0, 0.0],
            ];
            for (i, emb) in embeddings.into_iter().enumerate() {
                let fact = NewFact {
                    content: format!("fact {i}"),
                    content_hash: String::new(),
                    embedding: emb,
                    fact_type: FactType::Semantic,
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
                };
                let id = store.insert(&fact).unwrap();
                ids.push(id);
            }
            (conn, ids)
        }

        #[test]
        fn hnsw_strategy_finds_nearest() {
            let (conn, ids) = setup_with_facts();
            let strategy = HnswStrategy::build_from_db(&conn, DIM).unwrap();

            let query = [1.0_f32, 0.0, 0.0, 0.0];
            let results = strategy.search(&conn, &query, DIM, 2, None, None).unwrap();

            assert_eq!(results.len(), 2);
            assert_eq!(results[0].fact_id, ids[0]);
            assert_eq!(results[1].fact_id, ids[1]);
        }

        #[test]
        fn hnsw_strategy_notify_insert_updates_index() {
            let (conn, ids) = setup_with_facts();
            let strategy = HnswStrategy::build_from_db(&conn, DIM).unwrap();

            let new_emb = vec![0.99, 0.01, 0.0, 0.0];
            let store = FactStore::new(&conn, DIM);
            let new_fact = NewFact {
                content: "new close fact".into(),
                content_hash: String::new(),
                embedding: new_emb.clone(),
                fact_type: FactType::Semantic,
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
            };
            let new_id = store.insert(&new_fact).unwrap();
            strategy.notify_insert(new_id, &new_emb);

            let query = [1.0_f32, 0.0, 0.0, 0.0];
            let results = strategy.search(&conn, &query, DIM, 3, None, None).unwrap();

            let found_ids: Vec<i64> = results.iter().map(|r| r.fact_id).collect();
            assert!(
                found_ids.contains(&new_id),
                "newly inserted fact should appear in results"
            );
            assert!(
                found_ids.contains(&ids[0]),
                "original closest fact should still appear"
            );
        }

        #[test]
        fn hnsw_strategy_empty_index_returns_empty() {
            // DB with no facts -> index_to_fact is empty, so search() hits the
            // `n_items == 0` early return (ann.rs:248-253) before any nearest()
            // call, returning an empty vec.
            let conn = open_memory().unwrap();
            init_schema(&conn).unwrap();
            let strategy = HnswStrategy::build_from_db(&conn, DIM).unwrap();
            assert_eq!(strategy.active_count(), 0);

            let query = [1.0_f32, 0.0, 0.0, 0.0];
            let results = strategy.search(&conn, &query, DIM, 5, None, None).unwrap();
            assert!(results.is_empty(), "empty index must yield no results");
        }

        #[test]
        fn hnsw_strategy_dim_mismatch_errors() {
            let (conn, _ids) = setup_with_facts();
            let strategy = HnswStrategy::build_from_db(&conn, DIM).unwrap();

            // Query length != strategy.embed_dim -> EmbeddingDimension error
            // before any index access (ann.rs:230-235). The `_embed_dim`
            // parameter is intentionally ignored; the strategy's own dim wins.
            let wrong = [1.0_f32, 0.0]; // len 2, DIM is 4
            let err = strategy
                .search(&conn, &wrong, DIM, 5, None, None)
                .unwrap_err();
            assert!(matches!(
                err,
                crate::error::MemoryError::EmbeddingDimension {
                    expected: 4,
                    actual: 2
                }
            ));
        }

        #[test]
        fn hnsw_strategy_notify_expire_excludes_from_results() {
            let (conn, ids) = setup_with_facts();
            let strategy = HnswStrategy::build_from_db(&conn, DIM).unwrap();

            // Tombstone in HNSW index
            strategy.notify_expire(ids[0]);
            // Also expire in DB so check_fact_filters excludes it
            conn.execute(
                "UPDATE facts SET t_expired = datetime('now') WHERE id = ?1",
                [ids[0]],
            )
            .unwrap();

            let query = [1.0_f32, 0.0, 0.0, 0.0];
            let results = strategy.search(&conn, &query, DIM, 2, None, None).unwrap();

            assert!(
                !results.iter().any(|r| r.fact_id == ids[0]),
                "expired fact should be excluded"
            );
        }
    }
}
