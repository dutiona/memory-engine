use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::error::Result;
use crate::search::vector::cosine_similarity;
use crate::store::edges::EdgeStore;
use crate::store::facts::FactStore;

/// Local deduplication pass.
///
/// Compares facts created since `since` (or all if `None`) against all active facts.
/// Near-duplicates (cosine > threshold) are resolved by expiring the lower-importance
/// fact. Deterministic tie-break: on equal importance, the newer fact (higher id) is
/// expired.
///
/// Returns count of duplicates removed.
///
/// # Errors
///
/// Returns `MemoryError::Database` on SQL failure.
pub fn local_dedup(
    conn: &Connection,
    embed_dim: usize,
    threshold: f32,
    since: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<usize> {
    let fact_store = FactStore::new(conn, embed_dim);
    let edge_store = EdgeStore::new(conn);
    let active_facts = fact_store.list_active()?;

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

    for new_fact in &new_facts {
        if expired_ids.contains(&new_fact.id) {
            continue;
        }

        for candidate in &active_facts {
            if candidate.id == new_fact.id || expired_ids.contains(&candidate.id) {
                continue;
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

                // If the new_fact itself was expired, stop comparing it
                if expire_id == new_fact.id {
                    break;
                }
            }
        }
    }

    Ok(duplicates_removed)
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
            })
            .unwrap()
    }

    #[test]
    fn near_duplicates_detected() {
        let (conn, dim) = setup();
        // Two nearly identical embeddings
        insert_fact(&conn, dim, "fact A", vec![1.0, 0.0, 0.0, 0.0], 0.5);
        insert_fact(&conn, dim, "fact B", vec![0.99, 0.01, 0.0, 0.0], 0.3);

        let removed = local_dedup(&conn, dim, 0.90, None, Utc::now()).unwrap();
        assert_eq!(removed, 1);

        // Lower importance (B) should be expired
        let store = FactStore::new(&conn, dim);
        let active = store.list_active().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].content, "fact A");
    }

    #[test]
    fn dissimilar_facts_not_deduped() {
        let (conn, dim) = setup();
        // Orthogonal embeddings
        insert_fact(&conn, dim, "fact X", vec![1.0, 0.0, 0.0, 0.0], 0.5);
        insert_fact(&conn, dim, "fact Y", vec![0.0, 1.0, 0.0, 0.0], 0.5);

        let removed = local_dedup(&conn, dim, 0.90, None, Utc::now()).unwrap();
        assert_eq!(removed, 0);

        let store = FactStore::new(&conn, dim);
        assert_eq!(store.list_active().unwrap().len(), 2);
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
            })
            .unwrap();

        // Only compare facts created since `old_time + 1 day`
        let since = old_time + Duration::days(1);
        let removed = local_dedup(&conn, dim, 0.90, Some(since), Utc::now()).unwrap();
        assert_eq!(removed, 1); // new duplicate should be expired against old

        let active = store.list_active().unwrap();
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
        let active = store.list_active().unwrap();
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
        let active = store.list_active().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].content, "older fact");
    }
}
