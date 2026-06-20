use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::error::Result;
use crate::search::vector::cosine_similarity;
use crate::store::edges::EdgeStore;
use crate::store::facts::FactStore;
use crate::types::Fact;

/// Local deduplication pass.
///
/// Compares facts created since `since` (or all if `None`) against all active facts.
/// Near-duplicates (cosine > threshold) are resolved by expiring the lower-importance
/// fact. Deterministic tie-break: on equal importance, the newer fact (higher id) is
/// expired.
///
/// Returns `(count, expired_ids)`: the number of duplicates removed and the
/// ids of the facts that were expired (so the caller can update vector
/// indexes). As a sentinel, when the active corpus exceeds the internal
/// `MAX_DEDUP_FACTS` cap the pass is skipped and the count is `usize::MAX`
/// with an empty id list, signaling the orchestrator NOT to advance the
/// consolidation watermark.
///
/// # Errors
///
/// Returns `MemoryError::Database` on SQL failure.
/// Returns `MemoryError::NotFound` if a fact to expire or update no longer exists.
pub fn local_dedup(
    conn: &Connection,
    embed_dim: usize,
    threshold: f32,
    since: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<(usize, Vec<i64>)> {
    /// Maximum number of active facts for dedup. Beyond this, the O(N*M)
    /// pairwise comparison becomes impractical. Skip with a warning.
    const MAX_DEDUP_FACTS: usize = 50_000;

    let fact_store = FactStore::new(conn, embed_dim);
    let edge_store = EdgeStore::new(conn);
    let active_facts = fact_store.list_active(None)?;

    if active_facts.len() > MAX_DEDUP_FACTS {
        tracing::warn!(
            count = active_facts.len(),
            max = MAX_DEDUP_FACTS,
            "dedup skipped: too many active facts for O(N*M) comparison; \
             watermark will NOT advance so skipped facts are retried when corpus shrinks"
        );
        // Return sentinel value usize::MAX to signal "skipped" to the orchestrator.
        // The orchestrator must NOT advance last_consolidated_at in this case.
        return Ok((usize::MAX, vec![]));
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
    Ok((duplicates_removed, expired_vec))
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

        let (removed, _) = local_dedup(&conn, dim, 0.90, None, Utc::now()).unwrap();
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

        let (removed, _) = local_dedup(&conn, dim, 0.90, None, Utc::now()).unwrap();
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
        let (removed, _) = local_dedup(&conn, dim, 0.90, Some(since), Utc::now()).unwrap();
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

        local_dedup(&conn, dim, 0.90, None, Utc::now()).unwrap();

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

        local_dedup(&conn, dim, 0.90, None, Utc::now()).unwrap();

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

        let (removed, _) = local_dedup(&conn, dim, 0.90, None, Utc::now()).unwrap();
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

        let (removed, _) = local_dedup(&conn, dim, 0.90, None, Utc::now()).unwrap();
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

        local_dedup(&conn, dim, 0.90, None, Utc::now()).unwrap();

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

        let (removed, _) = local_dedup(&conn, dim, 0.90, None, Utc::now()).unwrap();
        assert_eq!(removed, 2, "B and C both collapse onto A");

        let active = store.list_active(None).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].content, "A");
        assert!(
            (active[0].importance_score - 0.8).abs() < f64::EPSILON,
            "survivor must hold the chain-wide max importance_score 0.8, got {} \
             (the stale-read bug overwrites B's 0.8 with C's 0.5)",
            active[0].importance_score
        );
    }

    #[test]
    fn empty_db_dedup_is_noop() {
        let (conn, dim) = setup();
        // No facts in the DB — dedup must return (0, []) without error.
        let (removed, expired) = local_dedup(&conn, dim, 0.90, None, Utc::now()).unwrap();
        assert_eq!(removed, 0);
        assert!(expired.is_empty());

        let store = FactStore::new(&conn, dim);
        assert!(store.list_active(None).unwrap().is_empty());
    }
}
