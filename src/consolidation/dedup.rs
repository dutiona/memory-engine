use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::error::Result;
use crate::search::vector::cosine_similarity;
use crate::store::edges::EdgeStore;
use crate::store::facts::FactStore;
use crate::types::Fact;

/// Outcome of a [`local_dedup`] pass.
///
/// Replaces the former `(usize, Vec<i64>)` return whose `usize::MAX` first element
/// was an in-band "skipped" sentinel (#272): that magic value collided with any
/// legitimate count `>= usize::MAX - 1` and forced the orchestrator to decode it
/// before use. The skip state now lives in the type, so the over-cap case can be
/// asserted directly and can never be mistaken for a real count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DedupOutcome {
    /// The pass ran to completion.
    Ran {
        /// Number of near-duplicate facts expired.
        removed: usize,
        /// Ids of the expired facts (so the caller can update vector indexes).
        expired_ids: Vec<i64>,
    },
    /// The pass was skipped because the active corpus exceeded the `max_facts`
    /// safety cap. The orchestrator must NOT advance the consolidation watermark,
    /// so the skipped facts are retried once the corpus shrinks.
    Skipped {
        /// Number of active facts that tripped the cap.
        active_count: usize,
    },
}

/// Local deduplication pass.
///
/// Compares facts created since `since` (or all if `None`) against all active facts.
/// Near-duplicates (cosine > threshold) are resolved by expiring the lower-importance
/// fact. Deterministic tie-break: on equal importance, the newer fact (higher id) is
/// expired.
///
/// When the active corpus exceeds `max_facts` the O(N*M) pairwise comparison is
/// skipped and [`DedupOutcome::Skipped`] is returned (the orchestrator then leaves
/// the watermark unadvanced); otherwise [`DedupOutcome::Ran`] carries the removed
/// count and the expired ids. The cap is injected so callers own the policy and
/// tests can exercise the skip path without a 50 000-fact corpus.
///
/// # Errors
///
/// Returns `MemoryError::Database` on SQL failure.
/// Returns `MemoryError::NotFound` if a fact to expire or update no longer exists.
pub fn local_dedup(
    conn: &Connection,
    embed_dim: usize,
    threshold: f32,
    max_facts: usize,
    since: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<DedupOutcome> {
    let fact_store = FactStore::new(conn, embed_dim);
    let edge_store = EdgeStore::new(conn);
    let active_facts = fact_store.list_active(None)?;

    if active_facts.len() > max_facts {
        tracing::warn!(
            count = active_facts.len(),
            max = max_facts,
            "dedup skipped: too many active facts for O(N*M) comparison; \
             watermark will NOT advance so skipped facts are retried when corpus shrinks"
        );
        return Ok(DedupOutcome::Skipped {
            active_count: active_facts.len(),
        });
    }

    // Split into "new" (to compare) and "all active" (to compare against)
    let new_facts: Vec<_> = since.map_or_else(
        || active_facts.iter().collect(),
        |since_dt| {
            active_facts
                .iter()
                .filter(|f| f.t_created > since_dt)
                .collect()
        },
    );

    let mut expired_ids = std::collections::HashSet::new();
    let mut duplicates_removed = 0;
    // Running maximum `importance_score` per surviving fact id (#264). The
    // in-memory `active_facts` Vec is never updated after a DB write, so within a
    // multi-duplicate chain a survivor's in-memory score goes stale once an earlier
    // merge has already written a higher value. This map is the live source of
    // truth consulted by `inherit_max_importance`.
    let mut running_scores: std::collections::HashMap<i64, f64> = std::collections::HashMap::new();

    for new_fact in &new_facts {
        if expired_ids.contains(&new_fact.id) || new_fact.is_pinned {
            continue; // pinned facts are never dedup candidates
        }

        for candidate in &active_facts {
            if candidate.id == new_fact.id
                || expired_ids.contains(&candidate.id)
                || candidate.is_pinned
            {
                continue; // skip pinned candidates too
            }

            let similarity = cosine_similarity(&new_fact.embedding, &candidate.embedding);
            if similarity > threshold {
                // Expire the lower-importance fact. Tie-break: higher id (newer) expires.
                let expire_id = if new_fact.importance < candidate.importance {
                    new_fact.id
                } else if new_fact.importance > candidate.importance {
                    candidate.id
                } else {
                    // Equal importance: expire the newer one (higher id)
                    new_fact.id.max(candidate.id)
                };

                fact_store.expire(expire_id, now)?;
                edge_store.expire_by_fact(expire_id, now)?;
                expired_ids.insert(expire_id);
                duplicates_removed += 1;

                // Update survivor's importance: inherit max from merged pair
                let (survivor, loser) = if expire_id == new_fact.id {
                    (&candidate, new_fact)
                } else {
                    (new_fact, &candidate)
                };
                inherit_max_importance(&fact_store, survivor, loser, &mut running_scores)?;

                // If the new_fact itself was expired, stop comparing it
                if expire_id == new_fact.id {
                    break;
                }
            }
        }
    }

    let expired_vec: Vec<i64> = expired_ids.into_iter().collect();
    Ok(DedupOutcome::Ran {
        removed: duplicates_removed,
        expired_ids: expired_vec,
    })
}

/// Inherit the higher importance values from `loser` into `survivor`.
///
/// Called after a dedup merge to ensure the surviving fact retains the maximum
/// base `importance` and `importance_score` across the merged pair — and,
/// crucially, across an entire chain of merges onto the same survivor.
///
/// `running_scores` tracks the live maximum `importance_score` per fact id so the
/// decision never reads a stale in-memory value (#264): the in-memory `Fact` is
/// not updated after a DB write, so an earlier merge's higher inherited score
/// would otherwise be invisible — and overwritten — by a later, lower one. A fact
/// absent from the map has never been written, so its in-memory score equals the
/// DB value and is a safe fallback. The `loser` is consulted through the map too,
/// so a fact that absorbed a high score before itself being expired passes that
/// score on to its own survivor.
fn inherit_max_importance(
    fact_store: &FactStore<'_>,
    survivor: &Fact,
    loser: &Fact,
    running_scores: &mut std::collections::HashMap<i64, f64>,
) -> Result<()> {
    // Base `importance` inheritance is a structural no-op under the current expiry
    // rule: dedup always expires the lower-importance fact (ties broken by id), so
    // the survivor's `importance` is always >= the loser's and the guard below can
    // never fire. It is kept as a defensive symmetric guard; the assert documents
    // and enforces the invariant so a future change to the expiry rule that breaks
    // it (re-introducing the #264 staleness for this field) fails loudly in tests.
    debug_assert!(
        loser.importance <= survivor.importance,
        "expiry invariant violated: loser.importance ({}) > survivor.importance ({}); \
         base-importance inheritance would need the same running-max fix as importance_score",
        loser.importance,
        survivor.importance
    );
    if loser.importance > survivor.importance {
        fact_store.update_importance(survivor.id, loser.importance)?;
    }

    // `importance_score`: compare live (not in-memory) maxima for both facts.
    let survivor_score = running_scores
        .get(&survivor.id)
        .copied()
        .unwrap_or(survivor.importance_score);
    let loser_score = running_scores
        .get(&loser.id)
        .copied()
        .unwrap_or(loser.importance_score);
    if loser_score > survivor_score {
        fact_store.update_importance_score(survivor.id, loser_score)?;
        running_scores.insert(survivor.id, loser_score);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    use crate::store::schema::{init_schema, open_memory};
    use crate::types::{FactType, NewFact};

    /// A cap so large the safety check never fires — for tests not exercising the
    /// over-cap skip path. (`usize::MAX` is now just a big number, no longer the
    /// "skipped" sentinel it used to be — that's the whole point of #272.)
    const NO_CAP: usize = usize::MAX;

    impl DedupOutcome {
        /// Assert the pass ran and return `(removed, expired_ids)`.
        fn expect_ran(self) -> (usize, Vec<i64>) {
            match self {
                Self::Ran {
                    removed,
                    expired_ids,
                } => (removed, expired_ids),
                Self::Skipped { active_count } => {
                    panic!(
                        "expected DedupOutcome::Ran, got Skipped {{ active_count: {active_count} }}"
                    )
                }
            }
        }
    }

    fn setup() -> (Connection, usize) {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        (conn, 4)
    }

    fn insert_fact(
        conn: &Connection,
        embed_dim: usize,
        content: &str,
        embedding: Vec<f32>,
        importance: f64,
    ) -> i64 {
        let store = FactStore::new(conn, embed_dim);
        store
            .insert(&NewFact {
                content: content.into(),
                content_hash: blake3::hash(content.as_bytes()).to_hex().as_str()[..32].to_string(),
                embedding,
                fact_type: FactType::Semantic,
                t_created: Utc::now(),
                t_expired: None,
                t_valid: None,
                t_invalid: None,
                source_event_id: None,
                scope_id: 1,
                importance,
                access_count: 0,
                last_accessed: Utc::now(),
                metadata: serde_json::json!({}),
                is_pinned: false,
            })
            .unwrap()
    }

    #[test]
    fn near_duplicates_detected() {
        let (conn, dim) = setup();
        // Two nearly identical embeddings
        insert_fact(&conn, dim, "fact A", vec![1.0, 0.0, 0.0, 0.0], 0.5);
        insert_fact(&conn, dim, "fact B", vec![0.99, 0.01, 0.0, 0.0], 0.3);

        let (removed, _) = local_dedup(&conn, dim, 0.90, NO_CAP, None, Utc::now())
            .unwrap()
            .expect_ran();
        assert_eq!(removed, 1);

        // Lower importance (B) should be expired
        let store = FactStore::new(&conn, dim);
        let active = store.list_active(None).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].content, "fact A");
    }

    #[test]
    fn dissimilar_facts_not_deduped() {
        let (conn, dim) = setup();
        // Orthogonal embeddings
        insert_fact(&conn, dim, "fact X", vec![1.0, 0.0, 0.0, 0.0], 0.5);
        insert_fact(&conn, dim, "fact Y", vec![0.0, 1.0, 0.0, 0.0], 0.5);

        let (removed, _) = local_dedup(&conn, dim, 0.90, NO_CAP, None, Utc::now())
            .unwrap()
            .expect_ran();
        assert_eq!(removed, 0);

        let store = FactStore::new(&conn, dim);
        assert_eq!(store.list_active(None).unwrap().len(), 2);
    }

    #[test]
    fn only_compares_new_facts_against_active() {
        let (conn, dim) = setup();
        let old_time = Utc::now() - Duration::days(10);

        // Insert an "old" fact
        let store = FactStore::new(&conn, dim);
        store
            .insert(&NewFact {
                content: "old fact".into(),
                content_hash: "h_old".into(),
                embedding: vec![1.0, 0.0, 0.0, 0.0],
                fact_type: FactType::Semantic,
                t_created: old_time,
                t_expired: None,
                t_valid: None,
                t_invalid: None,
                source_event_id: None,
                scope_id: 1,
                importance: 0.5,
                access_count: 0,
                last_accessed: old_time,
                metadata: serde_json::json!({}),
                is_pinned: false,
            })
            .unwrap();

        // Insert a "new" near-duplicate
        store
            .insert(&NewFact {
                content: "new duplicate".into(),
                content_hash: "h_new".into(),
                embedding: vec![0.99, 0.01, 0.0, 0.0],
                fact_type: FactType::Semantic,
                t_created: Utc::now(),
                t_expired: None,
                t_valid: None,
                t_invalid: None,
                source_event_id: None,
                scope_id: 1,
                importance: 0.3,
                access_count: 0,
                last_accessed: Utc::now(),
                metadata: serde_json::json!({}),
                is_pinned: false,
            })
            .unwrap();

        // Only compare facts created since `old_time + 1 day`
        let since = old_time + Duration::days(1);
        let (removed, _) = local_dedup(&conn, dim, 0.90, NO_CAP, Some(since), Utc::now())
            .unwrap()
            .expect_ran();
        assert_eq!(removed, 1); // new duplicate should be expired against old

        let active = store.list_active(None).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].content, "old fact"); // old survives (higher importance wins)
    }

    #[test]
    fn higher_importance_survives() {
        let (conn, dim) = setup();
        insert_fact(&conn, dim, "low importance", vec![1.0, 0.0, 0.0, 0.0], 0.2);
        insert_fact(
            &conn,
            dim,
            "high importance",
            vec![0.99, 0.01, 0.0, 0.0],
            0.8,
        );

        local_dedup(&conn, dim, 0.90, NO_CAP, None, Utc::now()).unwrap();

        let store = FactStore::new(&conn, dim);
        let active = store.list_active(None).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].content, "high importance");
    }

    #[test]
    fn equal_importance_newer_expires() {
        let (conn, dim) = setup();
        // Same importance — newer (higher id) should be expired
        insert_fact(&conn, dim, "older fact", vec![1.0, 0.0, 0.0, 0.0], 0.5);
        insert_fact(&conn, dim, "newer fact", vec![0.99, 0.01, 0.0, 0.0], 0.5);

        local_dedup(&conn, dim, 0.90, NO_CAP, None, Utc::now()).unwrap();

        let store = FactStore::new(&conn, dim);
        let active = store.list_active(None).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].content, "older fact");
    }

    fn insert_pinned_fact(
        conn: &Connection,
        embed_dim: usize,
        content: &str,
        embedding: Vec<f32>,
        importance: f64,
        is_pinned: bool,
    ) -> i64 {
        let store = FactStore::new(conn, embed_dim);
        store
            .insert(&NewFact {
                content: content.into(),
                content_hash: blake3::hash(content.as_bytes()).to_hex().as_str()[..32].to_string(),
                embedding,
                fact_type: FactType::Semantic,
                t_created: Utc::now(),
                t_expired: None,
                t_valid: None,
                t_invalid: None,
                source_event_id: None,
                scope_id: 1,
                importance,
                access_count: 0,
                last_accessed: Utc::now(),
                metadata: serde_json::json!({}),
                is_pinned,
            })
            .unwrap()
    }

    #[test]
    fn pinned_facts_not_deduped() {
        let (conn, dim) = setup();
        // Insert a pinned fact and a near-duplicate unpinned fact
        insert_pinned_fact(
            &conn,
            dim,
            "pinned fact",
            vec![1.0, 0.0, 0.0, 0.0],
            0.5,
            true,
        );
        insert_pinned_fact(
            &conn,
            dim,
            "pinned fact copy",
            vec![0.99, 0.01, 0.0, 0.0],
            0.5,
            false,
        );

        let (removed, _) = local_dedup(&conn, dim, 0.90, NO_CAP, None, Utc::now())
            .unwrap()
            .expect_ran();
        // Neither should be deduped because one is pinned
        assert_eq!(removed, 0);

        let store = FactStore::new(&conn, dim);
        let active = store.list_active(None).unwrap();
        assert_eq!(active.len(), 2);
    }

    #[test]
    fn both_pinned_not_deduped() {
        let (conn, dim) = setup();
        insert_pinned_fact(&conn, dim, "pinned A", vec![1.0, 0.0, 0.0, 0.0], 0.5, true);
        insert_pinned_fact(
            &conn,
            dim,
            "pinned B",
            vec![0.99, 0.01, 0.0, 0.0],
            0.8,
            true,
        );

        let (removed, _) = local_dedup(&conn, dim, 0.90, NO_CAP, None, Utc::now())
            .unwrap()
            .expect_ran();
        assert_eq!(removed, 0);

        let store = FactStore::new(&conn, dim);
        assert_eq!(store.list_active(None).unwrap().len(), 2);
    }

    #[test]
    fn survivor_inherits_max_importance() {
        let (conn, dim) = setup();
        // Low importance fact, high importance fact — near-duplicate embeddings
        insert_fact(&conn, dim, "low imp", vec![1.0, 0.0, 0.0, 0.0], 0.3);
        insert_fact(&conn, dim, "high imp", vec![0.99, 0.01, 0.0, 0.0], 0.9);

        // Set distinct importance_scores before dedup
        let store = FactStore::new(&conn, dim);
        store.update_importance_score(1, 0.8).unwrap(); // low imp fact gets higher score
        store.update_importance_score(2, 0.4).unwrap(); // high imp fact gets lower score

        local_dedup(&conn, dim, 0.90, NO_CAP, None, Utc::now()).unwrap();

        let active = store.list_active(None).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].content, "high imp");
        // Survivor inherits max base importance (0.9 > 0.3)
        assert!((active[0].importance - 0.9).abs() < f64::EPSILON);
        // Survivor inherits max importance_score (0.8 > 0.4)
        assert!(
            (active[0].importance_score - 0.8).abs() < f64::EPSILON,
            "expected importance_score 0.8, got {}",
            active[0].importance_score
        );
    }

    /// Regression for #264: when a chain of 3+ near-duplicates collapses onto a
    /// single survivor, that survivor must inherit the MAXIMUM `importance_score`
    /// across the whole chain — not whichever loser it happened to merge with last.
    ///
    /// The pre-fix bug read the survivor's *stale in-memory* score on every merge:
    /// the in-memory `active_facts` Vec is never updated after a DB write, so a
    /// later, lower loser score (C's 0.5) silently overwrote an earlier, higher one
    /// (B's 0.8). The existing `survivor_inherits_max_importance` test only exercises
    /// the two-fact case and cannot catch this.
    #[test]
    fn survivor_inherits_max_importance_across_multi_duplicate_chain() {
        let (conn, dim) = setup();
        // Three near-duplicates (all pairwise cosine > 0.90) with EQUAL base
        // importance, so the tie-break (higher id expires) makes the lowest-id fact
        // (A) the sole survivor that B and C both merge into.
        let a = insert_fact(&conn, dim, "A", vec![1.0, 0.0, 0.0, 0.0], 0.5);
        let b = insert_fact(&conn, dim, "B", vec![0.99, 0.01, 0.0, 0.0], 0.5);
        let c = insert_fact(&conn, dim, "C", vec![0.98, 0.02, 0.0, 0.0], 0.5);

        // Survivor A starts LOW; the chain-wide maximum is B's 0.8. C (merged last)
        // is 0.5 — the value the stale-read bug wrongly leaves behind.
        let store = FactStore::new(&conn, dim);
        store.update_importance_score(a, 0.3).unwrap();
        store.update_importance_score(b, 0.8).unwrap(); // the true maximum
        store.update_importance_score(c, 0.5).unwrap();

        let (removed, _) = local_dedup(&conn, dim, 0.90, NO_CAP, None, Utc::now())
            .unwrap()
            .expect_ran();
        assert_eq!(removed, 2, "B and C both collapse onto A");

        let active = store.list_active(None).unwrap();
        assert_eq!(active.len(), 1);
        // Pin the collapse topology: A is the survivor. The 0.8-vs-0.5 regression
        // value is order-sensitive — it assumes the merge order A→B then A→C, which
        // holds because `local_dedup` scans `list_active` in rowid (insertion) order.
        // That ordering is not yet guaranteed by the query (no ORDER BY — #495); when
        // that lands this coupling becomes a contract rather than an SQLite default.
        assert_eq!(active[0].id, a);
        assert_eq!(active[0].content, "A");
        assert!(
            (active[0].importance_score - 0.8).abs() < f64::EPSILON,
            "survivor must hold the chain-wide max importance_score 0.8, got {} \
             (the stale-read bug overwrites B's 0.8 with C's 0.5)",
            active[0].importance_score
        );
    }

    /// Regression for #264 (loser-side propagation): a fact that absorbs a high
    /// `importance_score` *as a survivor* and is then itself expired *as a loser*
    /// must pass the absorbed score on to its final survivor. This exercises the
    /// `running_scores.get(&loser.id)` branch — the half of the fix the pure-survivor
    /// chain test above cannot reach.
    #[test]
    fn survivor_then_loser_propagates_absorbed_score() {
        let (conn, dim) = setup();
        // Near-duplicates with ASCENDING base importance so the expiry rule (expire
        // the lower-importance fact) makes B survive L, then A survive B:
        //   L(imp 0.2) loses to B(imp 0.5) → B absorbs L's high score 0.9
        //   B(imp 0.5) loses to A(imp 0.8) → A must inherit that 0.9 via the map
        let l = insert_fact(&conn, dim, "L", vec![1.0, 0.0, 0.0, 0.0], 0.2);
        let b = insert_fact(&conn, dim, "B", vec![0.99, 0.01, 0.0, 0.0], 0.5);
        let a = insert_fact(&conn, dim, "A", vec![0.98, 0.02, 0.0, 0.0], 0.8);

        let store = FactStore::new(&conn, dim);
        store.update_importance_score(l, 0.9).unwrap(); // the score that must survive two hops
        store.update_importance_score(b, 0.1).unwrap();
        store.update_importance_score(a, 0.1).unwrap();

        let (removed, _) = local_dedup(&conn, dim, 0.90, NO_CAP, None, Utc::now())
            .unwrap()
            .expect_ran();
        assert_eq!(removed, 2, "L collapses onto B, then B onto A");

        let active = store.list_active(None).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, a);
        assert!(
            (active[0].importance_score - 0.9).abs() < f64::EPSILON,
            "final survivor must inherit the score absorbed by an intermediate \
             survivor (0.9), got {} (stale-read bug reads B's in-memory 0.1)",
            active[0].importance_score
        );
    }

    #[test]
    fn empty_db_dedup_is_noop() {
        let (conn, dim) = setup();
        // No facts in the DB — dedup must return (0, []) without error.
        let (removed, expired) = local_dedup(&conn, dim, 0.90, NO_CAP, None, Utc::now())
            .unwrap()
            .expect_ran();
        assert_eq!(removed, 0);
        assert!(expired.is_empty());

        let store = FactStore::new(&conn, dim);
        assert!(store.list_active(None).unwrap().is_empty());
    }

    /// #272/#345/#142: when the active corpus exceeds the injected cap, the pass
    /// returns `DedupOutcome::Skipped { active_count }` instead of the old
    /// `usize::MAX` magic value — assertable directly, and no facts are touched.
    /// The injected cap is what makes this testable without a 50 000-fact corpus.
    #[test]
    fn over_cap_returns_skipped_without_expiring() {
        let (conn, dim) = setup();
        // Two near-duplicates that WOULD collapse under the normal path...
        insert_fact(&conn, dim, "dup A", vec![1.0, 0.0, 0.0, 0.0], 0.5);
        insert_fact(&conn, dim, "dup B", vec![0.99, 0.01, 0.0, 0.0], 0.5);

        // ...but a cap of 1 (corpus is 2) trips the safety skip.
        let outcome = local_dedup(&conn, dim, 0.90, 1, None, Utc::now()).unwrap();
        assert_eq!(
            outcome,
            DedupOutcome::Skipped { active_count: 2 },
            "over-cap dedup must report Skipped with the tripping count, not run"
        );

        // Skipped means untouched: both facts remain active.
        let store = FactStore::new(&conn, dim);
        assert_eq!(store.list_active(None).unwrap().len(), 2);
    }
}
