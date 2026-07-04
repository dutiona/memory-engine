//! HNSW-based approximate nearest neighbor search.
//!
//! Gated behind the `ann` feature flag.
//! Uses the `hnsw` crate (rust-cv) which owns inserted vectors,
//! avoiding lifetime issues with the HNSW graph.

use me_types::error::StorageError;
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

use crate::search::strategy::VectorSearchStrategy;
use crate::search::vector::VectorResult;
use crate::store::deserialize_embedding;
use me_types::error::Result;
use me_types::types::FactType;

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
    /// Maps `fact_id` → current HNSW index (latest insert wins).
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

    /// Construct an empty `hnsw::Hnsw` with the canonical seed + params.
    ///
    /// The single construction point for the index used by every build path
    /// ([`build_inner`](Self::build_inner) and [`from_snapshot`](Self::from_snapshot)),
    /// so the deterministic topology (`HNSW_SEED`, `ef_construction(200)`) can never
    /// drift between a freshly-opened, a rebuilt, and a snapshot-restored index — a
    /// drift would silently change which neighbors a query proposes.
    fn new_hnsw_index() -> Hnsw<CosineMetric, Vec<f32>, SmallRng, 16, 32> {
        use rand::SeedableRng;

        Hnsw::new_params_and_prng(
            CosineMetric,
            hnsw::Params::new().ef_construction(200),
            SmallRng::seed_from_u64(HNSW_SEED),
        )
    }

    /// Build a populated [`HnswInner`] from a fallible `(fact_id, embedding)` stream.
    ///
    /// The single insert-loop kernel shared by every index-construction path —
    /// [`build_inner`](Self::build_inner) (DB rows) and
    /// [`from_snapshot`](Self::from_snapshot) (snapshot entries) — so the
    /// dimension check, the sequential-ID invariant, and the
    /// `index_to_fact` / `fact_to_hnsw` mapping fill can never drift between them.
    /// Both callers supply their own source as an iterator; this owns the loop.
    ///
    /// `entries` yields **fallible** items: each `Result<(fact_id, embedding)>` is
    /// unwrapped with `?` inside the loop, so a per-item error (a rusqlite row
    /// error or a wrong-width blob from [`deserialize_embedding`]) short-circuits
    /// the build with the right [`MemoryError`] without the kernel ever holding the
    /// whole decoded set resident. The DB path ([`build_inner`](Self::build_inner))
    /// streams a lazy decode straight in — one row decoded at a time, exactly like
    /// the pre-refactor loop, so there is no peak-memory regression — while the
    /// snapshot path ([`from_snapshot`](Self::from_snapshot)) wraps each
    /// already-decoded entry in `Ok(...)`. The kernel enforces `embedding.len() ==
    /// embed_dim` itself.
    ///
    /// This guard is **only materially load-bearing for the snapshot path**: the
    /// DB path is already dim-checked upstream — [`build_inner`](Self::build_inner)
    /// runs each blob through [`deserialize_embedding`], which returns
    /// `EmbeddingDimension` on a width mismatch and otherwise yields a `Vec<f32>`
    /// of exactly `embed_dim` elements, so the kernel's check can never fire on a
    /// DB row (it is redundant defense-in-depth there). [`from_snapshot`](Self::from_snapshot),
    /// by contrast, clones `entry.embedding` straight through with no prior check,
    /// so this is the *sole* gate rejecting a wrong-width snapshot entry. Keeping
    /// the check in the shared kernel means both sources are rejected identically
    /// without the snapshot path needing its own bespoke guard.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::EmbeddingDimension` if any embedding has the wrong
    /// size, or `MemoryError::Internal` if HNSW does not assign sequential IDs
    /// starting from 0.
    fn build_hnsw_inner(
        entries: impl IntoIterator<Item = Result<(i64, Vec<f32>)>>,
        embed_dim: usize,
    ) -> Result<HnswInner> {
        let mut index = Self::new_hnsw_index();
        let mut searcher: Searcher<u32> = Searcher::default();
        let mut index_to_fact = Vec::new();
        let mut fact_to_hnsw = HashMap::new();

        for entry in entries {
            let (fact_id, embedding) = entry?;
            if embedding.len() != embed_dim {
                return Err(me_types::error::MemoryError::EmbeddingDimension {
                    expected: embed_dim,
                    actual: embedding.len(),
                });
            }
            let hnsw_id = index.insert(embedding, &mut searcher);
            if hnsw_id != index_to_fact.len() {
                return Err(me_types::error::MemoryError::Internal(format!(
                    "HNSW index must assign sequential IDs (got {hnsw_id}, expected {})",
                    index_to_fact.len()
                )));
            }
            index_to_fact.push(fact_id);
            fact_to_hnsw.insert(fact_id, hnsw_id);
        }

        Ok(HnswInner {
            index,
            index_to_fact,
            fact_to_hnsw,
            tombstones: HashSet::new(),
        })
    }

    /// Build a populated [`HnswInner`] from all active facts in `conn`.
    ///
    /// The shared core of [`build_from_db`](Self::build_from_db) (wraps it in a fresh
    /// `RwLock`) and [`rebuild_from_db`](Self::rebuild_from_db) (swaps it into the
    /// existing lock) — so both produce an index with identical topology from the
    /// same DB rows. Builds a **lazy** decode iterator that maps each rusqlite row
    /// to a `Result<(fact_id, Vec<f32>)>` (propagating row errors and the
    /// `EmbeddingDimension` from [`deserialize_embedding`]) and hands it straight to
    /// the shared [`build_hnsw_inner`](Self::build_hnsw_inner) kernel — no
    /// intermediate fully-decoded `Vec`. Each row is decoded one-at-a-time as the
    /// kernel pulls it, so the peak-memory profile matches the pre-refactor
    /// streaming insert (only the live HNSW graph holds every vector, never a second
    /// resident copy).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on query failure, `MemoryError::Internal` if
    /// HNSW does not assign sequential IDs starting from 0, or
    /// `MemoryError::EmbeddingDimension` if a stored embedding has the wrong size.
    fn build_inner(conn: &Connection, embed_dim: usize) -> Result<HnswInner> {
        let mut stmt = conn
            .prepare("SELECT id, embedding FROM facts WHERE t_expired IS NULL ORDER BY id")
            .map_err(StorageError::backend)?;
        // The mapped-rows iterator borrows `stmt`, which is owned here and outlives
        // the `build_hnsw_inner` call below — so the kernel can pull rows lazily.
        // Each item is fallible: a rusqlite row error or a wrong-width blob is
        // threaded through as `Err`, and `build_hnsw_inner` short-circuits on it.
        let rows = stmt
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let blob: Vec<u8> = row.get(1)?;
                Ok((id, blob))
            })
            .map_err(StorageError::backend)?;
        let decoded = rows.map(|row| {
            let (fact_id, blob) = row.map_err(StorageError::backend)?;
            let embedding = deserialize_embedding(&blob, embed_dim)?;
            Ok((fact_id, embedding))
        });

        Self::build_hnsw_inner(decoded, embed_dim)
    }

    /// Build an HNSW index from all active facts in the database.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on query failure, or
    /// `MemoryError::EmbeddingDimension` if a stored embedding has the wrong size.
    pub fn build_from_db(conn: &Connection, embed_dim: usize) -> Result<Self> {
        Ok(Self {
            inner: RwLock::new(Self::build_inner(conn, embed_dim)?),
            embed_dim,
        })
    }

    /// Rebuild the index **in place** from the current active facts in `conn`.
    ///
    /// Used after a same-dim reconstruction promote (#624): the active vectors were
    /// rewritten under a new embedding model, so the in-memory graph (built on the
    /// old vectors) is stale and must be rebuilt. Builds a fresh [`HnswInner`] via a
    /// full [`build_inner`](Self::build_inner) scan, then swaps it in — all-or-nothing:
    /// on `Err` the swap never happens (`?` returns before the assignment) and the
    /// old, stale-but-consistent index stays live.
    ///
    /// **The write lock spans the whole build, deliberately.** `notify_insert` /
    /// `notify_expire` take the *same* lock, so building under it serializes any
    /// concurrent fact write either *before* the scan (→ included in the rebuild) or
    /// *after* the swap (→ applied to the new index). Building off-lock and only
    /// locking for the swap would let a concurrent `notify` land on the
    /// about-to-be-discarded old index and be silently lost. A same-dim
    /// reconstruction is a rare, operator-driven event, so briefly blocking
    /// `vector_search` (which takes the read lock) for the O(N) build is the correct
    /// trade vs. a dropped index entry.
    ///
    /// `&self` interior mutability — the engine holds only `&dyn StorageBackend`
    /// post-#631, so a `&mut self` rebuild is unreachable. Same-dim only: it reuses
    /// `self.embed_dim`, so a wrong-dim rebuild is impossible by construction (a
    /// different-dim reconstruction fences + reopens instead, #742).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on query failure, or
    /// `MemoryError::EmbeddingDimension` if a stored embedding has the wrong size.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the write lock MUST span the build_inner scan, not just the swap: \
                  notify_insert/notify_expire take the same lock, so building under it \
                  serializes concurrent writes either before the scan or after the swap. \
                  Tightening to build-off-lock-then-swap reintroduces a lost-update window \
                  where a concurrent notify lands on the discarded old index."
    )]
    pub(crate) fn rebuild_from_db(&self, conn: &Connection) -> Result<()> {
        let mut guard = self.inner.write();
        *guard = Self::build_inner(conn, self.embed_dim)?;
        Ok(())
    }

    /// Number of tombstoned (logically-removed) HNSW slots — test-only white-box
    /// accessor. A full [`rebuild_from_db`](Self::rebuild_from_db) must reset this to
    /// `0` (it builds a fresh index with no tombstones), distinguishing a genuine
    /// rebuild from a `notify_insert`-replay (which only appends + tombstones, never
    /// reclaims).
    #[cfg(test)]
    pub(crate) fn tombstone_count(&self) -> usize {
        self.inner.read().tombstones.len()
    }

    /// Snapshot of the internal id↔hnsw mappings — test-only white-box accessor.
    ///
    /// Returns `(index_to_fact.clone(), fact_to_hnsw.clone())`. These are the
    /// mappings the build kernel fills in lock-step (`index_to_fact[hnsw_id] ==
    /// fact_id` and `fact_to_hnsw[fact_id] == hnsw_id`); comparing them across two
    /// independently-built indices proves a rebuild reproduced the *exact* graph
    /// membership and slot assignment — a dropped, duplicated, or mis-mapped entry
    /// shows up here even when a small-N black-box `search` cannot distinguish the
    /// topologies (it re-scores candidates against the live DB).
    #[cfg(test)]
    pub(crate) fn mapping_snapshot(&self) -> (Vec<i64>, HashMap<i64, usize>) {
        let inner = self.inner.read();
        (inner.index_to_fact.clone(), inner.fact_to_hnsw.clone())
    }

    /// Snapshot active embeddings for fast cold-start.
    ///
    /// Reads embeddings from DB (vectors are not kept in memory). Returns
    /// compact data: only active facts, ordered by `fact_id`. On load,
    /// `from_snapshot` rebuilds a fresh compact HNSW index from this data.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on query failure, or
    /// `MemoryError::EmbeddingDimension` if a stored embedding has the wrong size.
    #[allow(
        clippy::unused_self,
        reason = "future versions will read self.inner metadata (e.g. ef_construction) to store in the snapshot; keeping &self avoids an API break"
    )]
    pub(crate) fn to_snapshot(
        &self,
        conn: &Connection,
        embed_dim: usize,
    ) -> Result<me_types::types::snapshot::HnswSnapshot> {
        use me_types::types::snapshot::{HnswEntry, HnswSnapshot};

        let mut stmt = conn
            .prepare("SELECT id, embedding FROM facts WHERE t_expired IS NULL ORDER BY id")
            .map_err(StorageError::backend)?;
        let rows = stmt
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let blob: Vec<u8> = row.get(1)?;
                Ok((id, blob))
            })
            .map_err(StorageError::backend)?;

        let mut entries = Vec::new();
        for row in rows {
            let (fact_id, blob) = row.map_err(StorageError::backend)?;
            let embedding = deserialize_embedding(&blob, embed_dim)?;
            entries.push(HnswEntry { fact_id, embedding });
        }

        Ok(HnswSnapshot { entries })
    }

    /// Rebuild a compact HNSW index from snapshot data (no DB I/O needed).
    ///
    /// Delegates the insert loop to the shared
    /// [`build_hnsw_inner`](Self::build_hnsw_inner) kernel (which also constructs the
    /// empty index via [`new_hnsw_index`](Self::new_hnsw_index)), so a
    /// snapshot-restored index has the same deterministic topology, the same
    /// dimension check, and the same sequential-ID invariant as a freshly-built or
    /// rebuilt one.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::EmbeddingDimension` if a snapshot entry's embedding has
    /// the wrong size, or `MemoryError::Internal` if HNSW does not assign sequential
    /// IDs starting from 0.
    pub(crate) fn from_snapshot(
        snap: &me_types::types::snapshot::HnswSnapshot,
        embed_dim: usize,
    ) -> Result<Self> {
        // Snapshot entries are already decoded, so each is infallible — wrap in
        // `Ok(...)` to match the kernel's `Item = Result<...>` contract.
        let entries = snap
            .entries
            .iter()
            .map(|entry| Ok((entry.fact_id, entry.embedding.clone())));

        Ok(Self {
            inner: RwLock::new(Self::build_hnsw_inner(entries, embed_dim)?),
            embed_dim,
        })
    }
}

impl VectorSearchStrategy for HnswStrategy {
    #[allow(
        clippy::significant_drop_tightening,
        reason = "read lock is already scoped to an explicit block that ends before Phase 2; the inner variable binding is intentional for clarity"
    )]
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
            return Err(me_types::error::MemoryError::EmbeddingDimension {
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

            // Phase 2: Post-filter + exact-score ALL candidates in ONE batched
            // query (#288/#362). The old per-candidate `check_fact_filters` +
            // `load_embedding` pair issued 2N round-trips; a single query over the
            // candidate id set returns only the surviving rows (passing the SAME
            // expiry/fact_type/scope predicates) with their embeddings.
            let surviving = fetch_candidate_embeddings(
                conn,
                &candidates,
                fact_type,
                scope_ids,
                self.embed_dim,
            )?;

            results.clear();
            results.reserve(surviving.len());
            // Iterate `candidates` in HNSW-neighbor order so the pre-sort order is
            // identical to the old per-candidate loop (the final `sort_by` is
            // stable, so this preserves equal-score tie-breaking exactly).
            for fact_id in &candidates {
                if let Some(stored_emb) = surviving.get(fact_id) {
                    let score = crate::search::cosine_similarity(query_embedding, stored_emb);
                    results.push(VectorResult {
                        fact_id: *fact_id,
                        score,
                    });
                }
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
            return crate::search::vector::vector_search(
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

    fn name(&self) -> &'static str {
        "hnsw"
    }

    /// # Errors
    ///
    /// Returns [`MemoryError::IndexInconsistent`] if the sequential-ID invariant
    /// (`index.len() == index_to_fact.len()`) does not hold — the same invariant
    /// [`build_inner`](Self::build_inner) and [`from_snapshot`](Self::from_snapshot)
    /// enforce. This would indicate a bug in the `hnsw` crate or a concurrent
    /// modification; the `index_to_fact` / `fact_to_hnsw` mappings rely on `hnsw`
    /// handing out IDs equal to the current `index_to_fact.len()`.
    ///
    /// The invariant is checked **before** the graph is mutated, so the error
    /// path leaves the index unchanged — neither orphaning a graph entry nor
    /// wedging future inserts. Crucially, callers fire this **post-commit**: an
    /// `Err` here means the durable write already succeeded and only the
    /// in-memory vector index is now inconsistent. The correct recovery is to
    /// **rebuild the index** (e.g. reopen the engine), **not** to retry the
    /// write — which is exactly the [`IndexInconsistent`](MemoryError::IndexInconsistent)
    /// contract.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the write guard MUST span the whole read-check-mutate critical \
                  section: the sequential-ID precondition (index.len() == \
                  index_to_fact.len()) and the subsequent index.insert + mappings \
                  push must be atomic under one lock, or a concurrent notify could \
                  observe / wedge a half-updated index. The early `return Err` arms \
                  also still borrow `inner`, so a tail `drop(inner)` is unreachable."
    )]
    fn notify_insert(&self, fact_id: i64, embedding: &[f32]) -> Result<()> {
        let mut inner = self.inner.write();
        // Check the sequential-ID invariant against the *next* ID `hnsw` will
        // assign — `Hnsw::insert` hands out `len()` and grows by one — BEFORE
        // mutating the graph. `build_inner`/`from_snapshot` validate the same
        // invariant post-insert; here we validate pre-insert so a violation
        // aborts cleanly with no orphaned graph entry (the graph's len is the
        // ID the next insert returns, an `hnsw` 0.11 guarantee mirrored by
        // those build paths' `hnsw_id == index_to_fact.len()` checks).
        let expected_id = inner.index_to_fact.len();
        if inner.index.len() != expected_id {
            // Clean guard: fires BEFORE any mutation, so the index is left
            // untouched (no orphan, not wedged). Post-commit semantics — the
            // durable write already landed, so this surfaces as
            // `IndexInconsistent` ("rebuild the index, do not retry the write"),
            // never as a write failure.
            return Err(me_types::error::MemoryError::IndexInconsistent {
                fact_id,
                detail: format!(
                    "HNSW sequential ID invariant violated on insert: index has {} \
                     nodes, expected {expected_id}. This indicates a bug in the hnsw \
                     crate or a concurrent modification.",
                    inner.index.len()
                ),
            });
        }
        // Tombstone the old HNSW entry for this fact_id (if any) so the
        // stale embedding is excluded from future searches.
        if let Some(&old_hnsw_id) = inner.fact_to_hnsw.get(&fact_id) {
            inner.tombstones.insert(old_hnsw_id);
        }
        let vec = embedding.to_vec();
        let mut searcher: Searcher<u32> = Searcher::default();
        let hnsw_id = inner.index.insert(vec, &mut searcher);
        // The pre-check + `hnsw` 0.11's deterministic `len()` guarantee
        // `hnsw_id == expected_id`, so the mappings ALWAYS grow in lock-step with
        // the graph. Push unconditionally to keep the index internally consistent
        // (`index.len() == index_to_fact.len()` stays true). The earlier code
        // returned `Err` here when they differed, but that left the graph grown
        // while the mappings were not — permanently wedging the index (every
        // future insert would fail the pre-check). A `debug_assert_eq!` instead
        // catches a hypothetical `hnsw` contract break in dev/CI without panicking
        // or wedging in release.
        debug_assert_eq!(
            hnsw_id, expected_id,
            "hnsw broke its sequential-id contract mid-insert: got {hnsw_id}, expected {expected_id}"
        );
        inner.index_to_fact.push(fact_id);
        inner.fact_to_hnsw.insert(fact_id, hnsw_id);
        Ok(())
    }

    fn notify_expire(&self, fact_id: i64) {
        let mut inner = self.inner.write();
        if let Some(hnsw_id) = inner.fact_to_hnsw.remove(&fact_id) {
            inner.tombstones.insert(hnsw_id);
        }
    }
}

/// Fetch the embeddings of all `candidate_ids` that survive the HNSW post-filters
/// (active + `fact_type` + scope), in a **single** batched query (#288/#362).
///
/// Replaces the old per-candidate `check_fact_filters` (EXISTS) + `load_embedding`
/// (SELECT embedding) pair, which issued two round-trips per candidate — a `2N`
/// N+1 pattern across up to [`MAX_WIDEN_ATTEMPTS`] widening passes. The predicates
/// are byte-for-byte the same ones those two helpers enforced:
///
/// - `t_expired IS NULL` (active only),
/// - `?2 IS NULL OR fact_type = ?2` (optional `fact_type` filter),
/// - `?3 IS NULL OR scope_id IN (json_each(?3))` (optional scope filter).
///
/// The candidate id list is passed as a JSON array expanded via `json_each` — the
/// same binding technique the scope filter already uses, so a large candidate set
/// is one parameter, not `N` placeholders. Rows not matching the filters (or absent
/// — e.g. a stale HNSW graph entry whose fact was expired) are simply omitted from
/// the returned map, so the caller drops them exactly as the old EXISTS check did.
///
/// # Errors
///
/// Returns `MemoryError::Storage` on query failure, `MemoryError::Serialization`
/// if the id/scope JSON cannot be built, or `MemoryError::EmbeddingDimension` if a
/// stored embedding has the wrong width.
fn fetch_candidate_embeddings(
    conn: &Connection,
    candidate_ids: &[i64],
    fact_type: Option<&FactType>,
    scope_ids: Option<&[i64]>,
    embed_dim: usize,
) -> Result<HashMap<i64, Vec<f32>>> {
    use crate::search::serialize_scope_ids;
    use crate::store::facts::fact_type_to_str;

    if candidate_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let ids_json = serde_json::to_string(candidate_ids)
        .map_err(me_types::error::MemoryError::Serialization)?;
    let scope_json = serialize_scope_ids(scope_ids)?;

    let mut stmt = conn
        .prepare(
            "SELECT id, embedding FROM facts
         WHERE id IN (SELECT value FROM json_each(?1))
           AND t_expired IS NULL
           AND (?2 IS NULL OR fact_type = ?2)
           AND (?3 IS NULL OR scope_id IN (SELECT value FROM json_each(?3)))",
        )
        .map_err(StorageError::backend)?;
    let rows = stmt
        .query_map(
            rusqlite::params![ids_json, fact_type.map(fact_type_to_str), scope_json],
            |row| {
                let id: i64 = row.get(0)?;
                let blob: Vec<u8> = row.get(1)?;
                Ok((id, blob))
            },
        )
        .map_err(StorageError::backend)?;

    let mut out = HashMap::with_capacity(candidate_ids.len());
    for row in rows {
        let (fact_id, blob) = row.map_err(StorageError::backend)?;
        let embedding = deserialize_embedding(&blob, embed_dim)?;
        out.insert(fact_id, embedding);
    }
    Ok(out)
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
        use chrono::Utc;
        use me_types::types::{FactType, NewFact};

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
                    base_importance: 0.5,
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
                base_importance: 0.5,
                access_count: 0,
                last_accessed: Utc::now(),
                metadata: serde_json::json!({}),
                is_pinned: false,
            };
            let new_id = store.insert(&new_fact).unwrap();
            strategy.notify_insert(new_id, &new_emb).unwrap();

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
        fn hnsw_strategy_notify_insert_invariant_violation_errors_without_panic() {
            // #295 / #257 (Option C): a violated sequential-ID invariant on a
            // POST-COMMIT index update must return
            // `MemoryError::IndexInconsistent`, NOT panic — a panic in this
            // embedded lib aborts the consumer's process and leaves an orphaned
            // graph entry. The distinct variant tells the caller "the durable
            // write succeeded; rebuild the index, do not retry the write".
            //
            // White-box setup: desync `index_to_fact` from the graph so the
            // sequential-ID precondition (`index.len() == index_to_fact.len()`)
            // no longer holds. Pushing a phantom `index_to_fact` slot makes
            // `expected_id` one ahead of the graph's node count, so the
            // pre-mutation check fires.
            let (conn, _ids) = setup_with_facts();
            let strategy = HnswStrategy::build_from_db(&conn, DIM).unwrap();

            let graph_len_before = {
                let mut inner = strategy.inner.write();
                inner.index_to_fact.push(9999); // phantom slot: desync the mappings
                inner.index.len()
            };

            // Must return Err, not panic — and the new typed variant, not Internal.
            let err = strategy
                .notify_insert(42, &[0.1_f32, 0.2, 0.3, 0.4])
                .expect_err("desynced index must surface an IndexInconsistent error");
            assert!(
                matches!(
                    err,
                    me_types::error::MemoryError::IndexInconsistent { fact_id: 42, .. }
                ),
                "expected MemoryError::IndexInconsistent {{ fact_id: 42, .. }}, got {err:?}"
            );

            // The error path left the graph UNMUTATED — no orphaned entry: the
            // pre-check ran before `index.insert`, so the node count is unchanged.
            assert_eq!(
                strategy.inner.read().index.len(),
                graph_len_before,
                "error path must not have mutated the HNSW graph (no orphan)"
            );

            // The index is NOT wedged: undo the artificial desync (pop the phantom
            // slot to restore `index.len() == index_to_fact.len()`), and a real
            // post-commit `notify_insert` succeeds again. This proves the clean
            // pre-check guard never poisons future inserts (the wedge the old
            // post-insert `Err` arm would have caused).
            strategy.inner.write().index_to_fact.pop();
            assert_eq!(
                strategy.inner.read().index.len(),
                strategy.inner.read().index_to_fact.len(),
                "invariant restored: graph and mappings are back in lock-step"
            );

            let new_emb = vec![0.99_f32, 0.01, 0.0, 0.0];
            let store = FactStore::new(&conn, DIM);
            let new_fact = NewFact {
                content: "post-recovery fact".into(),
                content_hash: String::new(),
                embedding: new_emb.clone(),
                fact_type: FactType::Semantic,
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
            let new_id = store.insert(&new_fact).unwrap();
            strategy
                .notify_insert(new_id, &new_emb)
                .expect("a valid insert must still succeed — the index is not wedged");
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
                me_types::error::MemoryError::EmbeddingDimension {
                    expected: 4,
                    actual: 2
                }
            ));
        }

        #[test]
        #[allow(
            clippy::too_many_lines,
            reason = "exhaustive filter-matrix test: 5 fixture facts (one per excluded \
                      dimension) + per-result score verification; splitting would scatter \
                      the single behavioral assertion"
        )]
        fn hnsw_strategy_batched_fetch_respects_filters_and_expiry() {
            // #288 / #362: Phase 2 now fetches all surviving candidates in ONE
            // batched query per widening attempt (was 2 round-trips per candidate).
            // This proves the batched path preserves the exact filter semantics the
            // old per-candidate `check_fact_filters` + `load_embedding` pair enforced:
            // (a) fact_type filter, (b) scope filter, (c) expiry exclusion, and that
            // the scores match an independent cosine computation.
            let conn = open_memory().unwrap();
            init_schema(&conn).unwrap();
            let store = FactStore::new(&conn, DIM);

            // scope 1 (root) exists by default; create scope 2 for the wrong-scope fact
            // (facts.scope_id has a FK to scopes.id).
            let scope_store = crate::store::scopes::ScopeStore::new(&conn);
            let scope2 = scope_store.insert(1, "other", 1).unwrap().id;

            let make =
                |content: &str, emb: Vec<f32>, ft: FactType, scope: i64, expired: bool| -> i64 {
                    let fact = NewFact {
                        content: content.into(),
                        content_hash: String::new(),
                        embedding: emb,
                        fact_type: ft,
                        t_created: Utc::now(),
                        t_expired: if expired { Some(Utc::now()) } else { None },
                        t_valid: None,
                        t_invalid: None,
                        source_event_id: None,
                        scope_id: scope,
                        base_importance: 0.5,
                        access_count: 0,
                        last_accessed: Utc::now(),
                        metadata: serde_json::json!({}),
                        is_pinned: false,
                    };
                    store.insert(&fact).unwrap()
                };

            // Near-query semantic facts in scope 1 (the wanted matches).
            let near_a = make(
                "near a",
                vec![1.0, 0.0, 0.0, 0.0],
                FactType::Semantic,
                1,
                false,
            );
            let near_b = make(
                "near b",
                vec![0.9, 0.1, 0.0, 0.0],
                FactType::Semantic,
                1,
                false,
            );
            // Same scope/type but must be filtered OUT by expiry.
            let expired = make(
                "expired",
                vec![0.95, 0.05, 0.0, 0.0],
                FactType::Semantic,
                1,
                true,
            );
            // Wrong fact_type — excluded by the fact_type filter.
            let wrong_type = make(
                "wrong type",
                vec![0.98, 0.02, 0.0, 0.0],
                FactType::Episodic,
                1,
                false,
            );
            // Wrong scope — excluded by the scope filter.
            let wrong_scope = make(
                "wrong scope",
                vec![0.97, 0.03, 0.0, 0.0],
                FactType::Semantic,
                scope2,
                false,
            );

            let strategy = HnswStrategy::build_from_db(&conn, DIM).unwrap();
            // The HNSW build scans `t_expired IS NULL`, so `expired` was never indexed;
            // re-insert its (still-active-in-graph) embedding so a stale graph entry
            // could surface it — the batched query's `t_expired IS NULL` must still drop it.
            strategy
                .notify_insert(expired, &[0.95_f32, 0.05, 0.0, 0.0])
                .unwrap();

            let query = [1.0_f32, 0.0, 0.0, 0.0];
            let results = strategy
                .search(&conn, &query, DIM, 2, Some(&FactType::Semantic), Some(&[1]))
                .unwrap();

            let found: Vec<i64> = results.iter().map(|r| r.fact_id).collect();
            assert!(
                found.contains(&near_a) && found.contains(&near_b),
                "both in-scope, in-type, active facts must be returned, got {found:?}"
            );
            assert!(
                !found.contains(&expired),
                "expired fact must be excluded by t_expired IS NULL"
            );
            assert!(
                !found.contains(&wrong_type),
                "wrong fact_type must be excluded by the fact_type filter"
            );
            assert!(
                !found.contains(&wrong_scope),
                "wrong scope must be excluded by the scope filter"
            );

            // Scores must equal an independent cosine computation over the stored vector.
            for r in &results {
                let blob: Vec<u8> = conn
                    .query_row(
                        "SELECT embedding FROM facts WHERE id = ?1",
                        [r.fact_id],
                        |row| row.get(0),
                    )
                    .unwrap();
                let emb = deserialize_embedding(&blob, DIM).unwrap();
                let expected = crate::search::cosine_similarity(&query, &emb);
                assert!(
                    (r.score - expected).abs() < f32::EPSILON,
                    "score for {} must match cosine, got {} expected {expected}",
                    r.fact_id,
                    r.score
                );
            }
        }

        #[test]
        fn hnsw_strategy_one_batched_query_per_widening_attempt() {
            // #288 / #362: the batched candidate fetch must issue exactly ONE
            // candidate query (the `json_each` batch) per widening attempt — never
            // 2N per-candidate round-trips. We trace SQL statements during `search`
            // and count those that touch the batch query (identified by `json_each`).
            use rusqlite::trace::{TraceEvent, TraceEventCodes};
            use std::cell::Cell;

            // `trace_v2` takes a bare `fn` pointer (no captures), so the counter
            // lives in a thread-local the callback bumps. Single-threaded test.
            thread_local! {
                static BATCH_QUERIES: Cell<usize> = const { Cell::new(0) };
            }
            #[allow(
                clippy::needless_pass_by_value,
                reason = "signature is fixed by `Connection::trace_v2`, which takes a \
                          bare `fn(TraceEvent)` — a reference param would not match"
            )]
            fn on_stmt(ev: TraceEvent<'_>) {
                if let TraceEvent::Stmt(_, sql) = ev
                    && sql.contains("json_each")
                {
                    BATCH_QUERIES.with(|c| c.set(c.get() + 1));
                }
            }

            let (conn, _ids) = setup_with_facts();
            let strategy = HnswStrategy::build_from_db(&conn, DIM).unwrap();

            BATCH_QUERIES.with(|c| c.set(0));
            conn.trace_v2(TraceEventCodes::SQLITE_TRACE_STMT, Some(on_stmt));

            // A request the index can satisfy without widening: limit 2 over 3 facts,
            // no filters → first attempt yields >= limit, loop breaks after one pass.
            let query = [1.0_f32, 0.0, 0.0, 0.0];
            let results = strategy.search(&conn, &query, DIM, 2, None, None).unwrap();

            conn.trace_v2(TraceEventCodes::empty(), None);
            assert_eq!(results.len(), 2, "search must satisfy the request");
            assert_eq!(
                BATCH_QUERIES.with(Cell::get),
                1,
                "exactly one batched candidate query per widening attempt (1 attempt here)"
            );
        }

        #[test]
        fn hnsw_strategy_one_batched_query_per_widening_attempt_forces_widening() {
            // #288 / #362, INFO coverage: the previous test only exercises the
            // single-attempt path, so it cannot distinguish "one query per attempt"
            // from "one query total". Here we build a fixture that FORCES the widen
            // loop to take a second attempt and assert the batch-query count tracks
            // attempts (2 attempts → 2 batched queries), proving the per-attempt
            // invariant rather than the by-construction single-attempt case.
            //
            // Construction: the `OVERFETCH_FACTOR * limit` candidates HNSW returns on
            // the first attempt are all `Procedural` (filtered out by the `Semantic`
            // query), so attempt 1 underfills (0 < 1) and the loop widens; the wider
            // overfetch on attempt 2 reaches a farther `Semantic` fact that survives.
            // (We filter on `fact_type` rather than `scope_id` to avoid seeding a
            // second scope row — `init_schema` only provides the default scope 1.)
            use rusqlite::trace::{TraceEvent, TraceEventCodes};
            use std::cell::Cell;

            thread_local! {
                static BATCH_QUERIES: Cell<usize> = const { Cell::new(0) };
            }
            #[allow(
                clippy::needless_pass_by_value,
                reason = "signature is fixed by `Connection::trace_v2`, which takes a \
                          bare `fn(TraceEvent)` — a reference param would not match"
            )]
            fn on_stmt(ev: TraceEvent<'_>) {
                if let TraceEvent::Stmt(_, sql) = ev
                    && sql.contains("json_each")
                {
                    BATCH_QUERIES.with(|c| c.set(c.get() + 1));
                }
            }

            let conn = open_memory().unwrap();
            init_schema(&conn).unwrap();
            let store = FactStore::new(&conn, DIM);

            // Helper: insert a fact at `emb` of `fact_type`, returning its id.
            let insert = |emb: Vec<f32>, fact_type: FactType| -> i64 {
                let fact = NewFact {
                    content: "fact".into(),
                    content_hash: String::new(),
                    embedding: emb,
                    fact_type,
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
                store.insert(&fact).unwrap()
            };

            // Query is [1,0,0,0]. `limit = 1` → first overfetch = OVERFETCH_FACTOR = 3.
            // The 3 nearest facts are `Procedural` (excluded by the `Semantic` query);
            // a 4th, farther fact is the only `Semantic` match, reachable only once
            // overfetch widens to >= 4. We seed > OVERFETCH_FACTOR `Procedural` facts so
            // the first attempt's candidate window is entirely `Procedural` and
            // genuinely underfills the `Semantic` request.
            let procedural_near = vec![
                vec![1.0_f32, 0.0, 0.0, 0.0],
                vec![0.99_f32, 0.01, 0.0, 0.0],
                vec![0.98_f32, 0.02, 0.0, 0.0],
                vec![0.97_f32, 0.03, 0.0, 0.0],
            ];
            for emb in procedural_near {
                insert(emb, FactType::Procedural);
            }
            // The lone `Semantic` match: farther than every `Procedural` fact, so HNSW
            // only surfaces it after the overfetch window widens on the second attempt.
            let semantic_id = insert(vec![0.0_f32, 1.0, 0.0, 0.0], FactType::Semantic);

            let strategy = HnswStrategy::build_from_db(&conn, DIM).unwrap();

            BATCH_QUERIES.with(|c| c.set(0));
            conn.trace_v2(TraceEventCodes::SQLITE_TRACE_STMT, Some(on_stmt));

            let query = [1.0_f32, 0.0, 0.0, 0.0];
            let results = strategy
                .search(&conn, &query, DIM, 1, Some(&FactType::Semantic), None)
                .unwrap();

            conn.trace_v2(TraceEventCodes::empty(), None);

            assert_eq!(results.len(), 1, "the lone `Semantic` fact must be found");
            assert_eq!(
                results[0].fact_id, semantic_id,
                "the surviving result is the `Semantic` fact, reached only after widening"
            );
            assert_eq!(
                BATCH_QUERIES.with(Cell::get),
                2,
                "two widening attempts must issue exactly two batched candidate \
                 queries — one per attempt, never 2N per-candidate round-trips"
            );
        }

        #[test]
        fn hnsw_strategy_notify_expire_excludes_from_results() {
            let (conn, ids) = setup_with_facts();
            let strategy = HnswStrategy::build_from_db(&conn, DIM).unwrap();

            // Tombstone in HNSW index
            strategy.notify_expire(ids[0]);
            // Also expire in DB so the batched candidate fetch excludes it
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

        // --- #624: rebuild_from_db (live same-dim reconstruction rebuild) ---

        #[test]
        fn rebuild_from_db_reclaims_tombstones_and_matches_live_rows() {
            // White-box guard for #624 — the *rigorous* proof that `rebuild_from_db`
            // produces a fresh index, not a `notify_insert`-replay (which only appends
            // + tombstones, never reclaims). A black-box query test cannot distinguish
            // a stale index from a rebuilt one at this corpus size: `search` re-scores
            // every candidate against the current DB embedding and, with
            // `DEFAULT_EF_SEARCH = 100` exploring the whole small graph, returns the
            // correct top-k either way. The tombstone/active-count invariant is the
            // observable a no-op (or replay) rebuild fails. DO NOT "simplify" this into
            // a query assertion — it would silently stop guarding the rebuild.
            let (conn, ids) = setup_with_facts();
            let strategy = HnswStrategy::build_from_db(&conn, DIM).unwrap();

            // Churn the in-memory index so it accumulates tombstones: re-insert one
            // fact (tombstones its prior slot) and expire another.
            strategy
                .notify_insert(ids[0], &[0.5_f32, 0.5, 0.0, 0.0])
                .unwrap();
            strategy.notify_expire(ids[1]);
            assert!(
                strategy.tombstone_count() >= 2,
                "churn must create tombstones, got {}",
                strategy.tombstone_count()
            );

            // Expire ids[1] in the DB too, so the rebuild's active scan drops it.
            conn.execute(
                "UPDATE facts SET t_expired = datetime('now') WHERE id = ?1",
                [ids[1]],
            )
            .unwrap();

            strategy.rebuild_from_db(&conn).unwrap();

            // A genuine rebuild resets tombstones to 0 and the active set to live rows.
            assert_eq!(
                strategy.tombstone_count(),
                0,
                "rebuild must reclaim all tombstones"
            );
            assert_eq!(
                strategy.active_count(),
                2,
                "active set == live (non-expired) rows: ids[0], ids[2]"
            );
        }

        #[test]
        fn rebuild_from_db_on_empty_db_is_ok() {
            let conn = open_memory().unwrap();
            init_schema(&conn).unwrap();
            let strategy = HnswStrategy::build_from_db(&conn, DIM).unwrap();
            strategy.rebuild_from_db(&conn).unwrap();
            assert_eq!(strategy.active_count(), 0);
            let results = strategy
                .search(&conn, &[1.0_f32, 0.0, 0.0, 0.0], DIM, 5, None, None)
                .unwrap();
            assert!(results.is_empty(), "rebuilt empty index yields no results");
        }

        #[test]
        fn rebuild_from_db_reflects_swapped_embeddings() {
            // Behavioral companion: after the stored vectors change, a rebuilt index
            // searches over the NEW vectors. (At this corpus size the query also
            // resolves correctly on a stale index via re-scoring — the white-box test
            // above is the rigorous guard; this documents end-to-end intent and that a
            // rebuild does not break search.)
            let (conn, ids) = setup_with_facts();
            let strategy = HnswStrategy::build_from_db(&conn, DIM).unwrap(); // graph on OLD vectors

            // Swap stored vectors: ids[2] becomes the e0 match, ids[0] moves to e1.
            conn.execute(
                "UPDATE facts SET embedding = ?1 WHERE id = ?2",
                rusqlite::params![
                    crate::store::serialize_embedding(&[0.0_f32, 1.0, 0.0, 0.0]),
                    ids[0]
                ],
            )
            .unwrap();
            conn.execute(
                "UPDATE facts SET embedding = ?1 WHERE id = ?2",
                rusqlite::params![
                    crate::store::serialize_embedding(&[1.0_f32, 0.0, 0.0, 0.0]),
                    ids[2]
                ],
            )
            .unwrap();

            strategy.rebuild_from_db(&conn).unwrap(); // graph on NEW vectors

            let results = strategy
                .search(&conn, &[1.0_f32, 0.0, 0.0, 0.0], DIM, 1, None, None)
                .unwrap();
            assert_eq!(
                results[0].fact_id, ids[2],
                "after rebuild, the fact swapped to e0 is the nearest"
            );
        }

        // --- #499: to_snapshot / from_snapshot round-trip (search-equivalence) ---

        #[test]
        fn snapshot_roundtrip_preserves_search_results() {
            // #499 (`search/testing-hnsw-snapshot-roundtrip`): the snapshot path is a
            // cold-start optimization — `to_snapshot` dumps active embeddings, and
            // `from_snapshot` rebuilds a fresh compact index from them with NO DB I/O.
            //
            // WHITE-BOX guard (mirrors the #624 `rebuild_from_db` test above). A
            // black-box query CANNOT validate the round-trip at this corpus size:
            // `search` re-scores every candidate against the live DB embedding and
            // falls back to a brute-force DB scan, so the top-k it returns is
            // topology-independent — a reviewer proved that building the snapshot
            // index on all-zero vectors OR in reversed insertion order still passes a
            // pure-query assertion. The load-bearing observable is the internal
            // id↔hnsw mapping the build kernel fills: a dropped, duplicated, or
            // mis-mapped entry would diverge here. So we assert the rebuilt index'
            // `index_to_fact` / `fact_to_hnsw` (and tombstone count) reproduce the
            // original's exactly. The search block below is CORROBORATING only — it
            // documents end-to-end intent, it is not the guard.
            let (conn, ids) = setup_with_facts();
            let original = HnswStrategy::build_from_db(&conn, DIM).unwrap();

            // Probe the original index for the full top-k ordering (corroborating).
            let query = [1.0_f32, 0.0, 0.0, 0.0];
            let before = original.search(&conn, &query, DIM, 3, None, None).unwrap();
            assert_eq!(before.len(), 3, "fixture has 3 active facts");

            // Round-trip: snapshot the active embeddings, then rebuild from them.
            let snap = original.to_snapshot(&conn, DIM).unwrap();
            assert_eq!(
                snap.entries.len(),
                ids.len(),
                "snapshot must capture every active fact"
            );
            let restored = HnswStrategy::from_snapshot(&snap, DIM).unwrap();

            // --- White-box: the rebuilt mappings must equal the original's. ---
            // These depend on the rebuild being correct (right membership, right slot
            // assignment, no drift) and would catch a dropped/duplicated/mis-mapped
            // entry that the maskable black-box query cannot.
            assert_eq!(
                restored.active_count(),
                original.active_count(),
                "round-trip must preserve the active set size"
            );
            assert_eq!(
                restored.tombstone_count(),
                original.tombstone_count(),
                "a fresh snapshot rebuild carries no tombstones (both are 0)"
            );
            let (orig_i2f, orig_f2h) = original.mapping_snapshot();
            let (rest_i2f, rest_f2h) = restored.mapping_snapshot();
            assert_eq!(
                rest_i2f, orig_i2f,
                "round-trip must reproduce index_to_fact exactly (slot order + membership): \
                 {rest_i2f:?} vs {orig_i2f:?}"
            );
            assert_eq!(
                rest_f2h, orig_f2h,
                "round-trip must reproduce the fact_to_hnsw id↔slot mapping exactly: \
                 {rest_f2h:?} vs {orig_f2h:?}"
            );

            // --- Corroborating (NOT the guard): end-to-end search still returns the
            // same top-k. See the header comment — at this corpus size `search`
            // re-scores against the live DB and cannot distinguish topologies, so a
            // pass here proves nothing about the rebuild on its own. ---
            let after = restored.search(&conn, &query, DIM, 3, None, None).unwrap();
            assert_eq!(
                after.len(),
                before.len(),
                "round-trip must return the same number of results"
            );
            for (a, b) in after.iter().zip(before.iter()) {
                assert_eq!(
                    a.fact_id, b.fact_id,
                    "round-trip must preserve the top-k ordering: {after:?} vs {before:?}"
                );
                assert!(
                    (a.score - b.score).abs() < f32::EPSILON,
                    "round-trip must preserve scores: {} vs {}",
                    a.score,
                    b.score
                );
            }
        }

        #[test]
        fn from_snapshot_dim_mismatch_errors() {
            // The shared `build_hnsw_inner` kernel enforces the dimension check for
            // the snapshot path identically to the DB path: a wrong-width entry must
            // surface `EmbeddingDimension`, not corrupt the index.
            use me_types::types::snapshot::{HnswEntry, HnswSnapshot};

            let snap = HnswSnapshot {
                entries: vec![HnswEntry {
                    fact_id: 1,
                    embedding: vec![1.0_f32, 0.0], // len 2, DIM is 4
                }],
            };
            // `HnswStrategy` (the `Ok` variant) has no `Debug` impl, so
            // `unwrap_err()` is unavailable; match on the result explicitly.
            let result = HnswStrategy::from_snapshot(&snap, DIM);
            assert!(
                matches!(
                    result,
                    Err(me_types::error::MemoryError::EmbeddingDimension {
                        expected: 4,
                        actual: 2
                    })
                ),
                "wrong-width snapshot entry must surface EmbeddingDimension"
            );
        }

        #[test]
        fn rebuild_from_db_dim_mismatch_preserves_old_index() {
            // All-or-nothing: a rebuild that hits a wrong-width stored embedding errors
            // and leaves the previous in-memory index untouched (the `?` returns before
            // the swap assignment).
            let (conn, ids) = setup_with_facts();
            let strategy = HnswStrategy::build_from_db(&conn, DIM).unwrap();
            let before = strategy.active_count();

            conn.execute(
                "UPDATE facts SET embedding = ?1 WHERE id = ?2",
                rusqlite::params![
                    crate::store::serialize_embedding(&[0.5_f32; DIM + 1]),
                    ids[0]
                ],
            )
            .unwrap();

            let err = strategy.rebuild_from_db(&conn).unwrap_err();
            assert!(
                matches!(err, me_types::error::MemoryError::EmbeddingDimension { .. }),
                "wrong-width stored embedding must abort the rebuild, got {err:?}"
            );
            assert_eq!(
                strategy.active_count(),
                before,
                "a failed rebuild must leave the old index intact"
            );
        }
    }
}
