use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::error::Result;
use crate::graph::MemoryGraph;
use crate::store::edges::EdgeStore;
use crate::store::facts::FactStore;
use crate::traits::{ForgetPolicy, PruneStats};
use crate::types::Fact;

/// Normalization ceiling for access frequency: ln(100+1).
/// 100 accesses = full score.
const FREQUENCY_NORMALIZATION_CEILING: f64 = 101.0;
/// Normalization ceiling for graph connectivity: ln(50+1).
/// 50 connections = full score.
const CONNECTIVITY_NORMALIZATION_CEILING: f64 = 51.0;

/// Ebbinghaus forgetting curve: retention = 2^(-age/half_life).
///
/// Returns 1.0 at `age=0`, 0.5 at `age=half_life`, 0.25 at `age=2×half_life`.
#[must_use]
pub fn ebbinghaus_decay(age_days: f64, half_life: f64) -> f64 {
    f64::exp2(-age_days / half_life)
}

/// Compute composite importance score for a fact.
///
/// Weighted sum of 4 signals, each normalized to \[0, 1\]:
/// 1. **Recency**: `ebbinghaus_decay(age, half_life)` — already in \[0, 1\]
/// 2. **Frequency**: `ln(access_count + 1) / ln(101)` — capped at 1.0
///    (100 accesses = full score, chosen as a reasonable ceiling)
/// 3. **Graph degree**: `ln(degree + 1) / ln(51)` — capped at 1.0
///    (50 connections = full score)
/// 4. **Base importance**: `fact.importance` — already in \[0, 1\]
#[must_use]
pub fn compute_importance(
    fact: &Fact,
    graph_degree: usize,
    now: DateTime<Utc>,
    policy: &ForgetPolicy,
) -> f64 {
    let half_life = policy
        .half_life_overrides
        .get(&fact.fact_type)
        .copied()
        .unwrap_or(policy.half_life_days);

    #[allow(clippy::cast_precision_loss)]
    let age_days = (now - fact.last_accessed).num_seconds() as f64 / 86400.0;
    let recency = ebbinghaus_decay(age_days.max(0.0), half_life);

    // Normalization: log_base(count+1), capped at 1.0.
    // Using ln_1p for numerical accuracy near zero.
    #[allow(clippy::cast_precision_loss)]
    let frequency =
        (f64::ln_1p(fact.access_count as f64) / FREQUENCY_NORMALIZATION_CEILING.ln()).min(1.0);
    #[allow(clippy::cast_precision_loss)]
    let connectivity =
        (f64::ln_1p(graph_degree as f64) / CONNECTIVITY_NORMALIZATION_CEILING.ln()).min(1.0);

    policy
        .recency_weight
        .mul_add(recency, policy.frequency_weight * frequency)
        + policy.graph_degree_weight.mul_add(
            connectivity,
            policy.base_importance_weight * fact.importance,
        )
}

/// Prune facts with importance below threshold.
///
/// Iterates all active facts, computes importance, soft-deletes those below
/// `min_importance`. For each expired fact, cascades edge expiry in `SQLite`
/// and removes edges from the in-memory graph to keep it consistent.
///
/// All mutations happen in a single transaction.
///
/// # Errors
///
/// Returns `MemoryError::Conflict` if the policy fails validation.
/// Returns `MemoryError::Database` on SQL failure.
pub fn prune(
    conn: &Connection,
    graph: &mut MemoryGraph,
    policy: &ForgetPolicy,
    embed_dim: usize,
    now: DateTime<Utc>,
) -> Result<(PruneStats, Vec<i64>)> {
    policy.validate()?;

    let fact_store = FactStore::new(conn, embed_dim);
    let active_facts = fact_store.list_active(None)?;
    let facts_evaluated = active_facts.len();

    // Score all facts before mutating, so degree values are consistent.
    // Pinned facts are unforgettable — they bypass the decay filter entirely.
    let to_expire: Vec<i64> = active_facts
        .iter()
        .filter(|fact| {
            if fact.is_pinned {
                return false;
            }
            let degree = graph.degree(fact.id);
            compute_importance(fact, degree, now, policy) < policy.min_importance
        })
        .map(|f| f.id)
        .collect();

    let tx = conn.unchecked_transaction()?;
    let fact_store = FactStore::new(&tx, embed_dim);
    let edge_store = EdgeStore::new(&tx);

    // Materialize importance scores for all active facts
    for fact in &active_facts {
        let degree = graph.degree(fact.id);
        let score = compute_importance(fact, degree, now, policy);
        fact_store.update_importance_score(fact.id, score)?;
    }

    // Expire low-importance unpinned facts
    for &fact_id in &to_expire {
        fact_store.expire(fact_id, now)?;
        edge_store.expire_by_fact(fact_id, now)?;
    }

    tx.commit()?;

    // Update in-memory graph after successful commit
    for &fact_id in &to_expire {
        graph.remove_edges_by_fact(fact_id);
    }

    let stats = PruneStats {
        facts_expired: to_expire.len(),
        facts_evaluated,
    };
    Ok((stats, to_expire))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use std::collections::HashMap;

    use crate::store::schema::{init_schema, open_memory};
    use crate::types::FactType;

    #[test]
    fn decay_at_zero_is_one() {
        let result = ebbinghaus_decay(0.0, 69.0);
        assert!((result - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn decay_at_half_life_is_half() {
        let result = ebbinghaus_decay(69.0, 69.0);
        assert!((result - 0.5).abs() < 1e-10);
    }

    #[test]
    fn decay_at_two_half_lives_is_quarter() {
        let result = ebbinghaus_decay(138.0, 69.0);
        assert!((result - 0.25).abs() < 1e-10);
    }

    #[test]
    fn high_access_recent_connected_beats_neglected_isolated() {
        let now = Utc::now();
        let policy = ForgetPolicy::default();

        // Fact A: recently accessed, high access count, connected, high importance
        let fact_a = Fact {
            id: 1,
            content: "important".into(),
            content_hash: "h1".into(),
            embedding: vec![0.1; 4],
            fact_type: FactType::Semantic,
            t_created: now,
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            scope_id: 1,
            importance: 0.8,
            access_count: 100,
            last_accessed: now - Duration::hours(1),
            metadata: serde_json::json!({}),
            is_pinned: false,
            importance_score: 0.5,
            surfaced_at: None,
        };

        // Fact B: old, rarely accessed, isolated, low importance
        let fact_b = Fact {
            id: 2,
            content: "neglected".into(),
            content_hash: "h2".into(),
            embedding: vec![0.2; 4],
            fact_type: FactType::Episodic,
            t_created: now - Duration::days(180),
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            scope_id: 1,
            importance: 0.3,
            access_count: 1,
            last_accessed: now - Duration::days(90),
            metadata: serde_json::json!({}),
            is_pinned: false,
            importance_score: 0.5,
            surfaced_at: None,
        };

        let importance_a = compute_importance(&fact_a, 5, now, &policy);
        let importance_b = compute_importance(&fact_b, 0, now, &policy);
        assert!(
            importance_a > importance_b,
            "A ({importance_a}) should be more important than B ({importance_b})"
        );
    }

    #[test]
    fn per_fact_type_half_life_override() {
        let now = Utc::now();
        let mut overrides = HashMap::new();
        overrides.insert(FactType::Episodic, 30.0);
        overrides.insert(FactType::Procedural, 365.0);

        let policy = ForgetPolicy {
            half_life_overrides: overrides,
            ..ForgetPolicy::default()
        };

        let base_fact = Fact {
            id: 1,
            content: "test".into(),
            content_hash: "h".into(),
            embedding: vec![0.1; 4],
            fact_type: FactType::Episodic,
            t_created: now - Duration::days(60),
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            scope_id: 1,
            importance: 0.5,
            access_count: 5,
            last_accessed: now - Duration::days(60),
            metadata: serde_json::json!({}),
            is_pinned: false,
            importance_score: 0.5,
            surfaced_at: None,
        };

        let mut procedural_fact = base_fact.clone();
        procedural_fact.fact_type = FactType::Procedural;

        // Same age, same access, but Episodic decays faster (half_life=30)
        // than Procedural (half_life=365)
        let episodic_importance = compute_importance(&base_fact, 0, now, &policy);
        let procedural_importance = compute_importance(&procedural_fact, 0, now, &policy);

        assert!(
            procedural_importance > episodic_importance,
            "Procedural ({procedural_importance}) should decay slower than Episodic ({episodic_importance})"
        );
    }

    #[test]
    fn prune_expires_low_importance_facts() {
        use crate::types::NewFact;

        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();

        let now = Utc::now();
        let old_time = now - Duration::days(100);
        let embed_dim = 4;

        // Insert 3 facts directly with varying importance and recency
        let fact_store = FactStore::new(&conn, embed_dim);

        // Fact 1: high importance, recently accessed
        fact_store
            .insert(&NewFact {
                content: "very important".into(),
                content_hash: "h1".into(),
                embedding: vec![0.1; embed_dim],
                fact_type: FactType::Semantic,
                t_created: now,
                t_expired: None,
                t_valid: None,
                t_invalid: None,
                source_event_id: None,
                scope_id: 1,
                importance: 0.9,
                access_count: 50,
                last_accessed: now,
                metadata: serde_json::json!({}),
                is_pinned: false,
            })
            .unwrap();

        // Fact 2: medium importance, old
        fact_store
            .insert(&NewFact {
                content: "somewhat important".into(),
                content_hash: "h2".into(),
                embedding: vec![0.2; embed_dim],
                fact_type: FactType::Episodic,
                t_created: old_time,
                t_expired: None,
                t_valid: None,
                t_invalid: None,
                source_event_id: None,
                scope_id: 1,
                importance: 0.3,
                access_count: 2,
                last_accessed: old_time,
                metadata: serde_json::json!({}),
                is_pinned: false,
            })
            .unwrap();

        // Fact 3: low importance, very old
        fact_store
            .insert(&NewFact {
                content: "not important".into(),
                content_hash: "h3".into(),
                embedding: vec![0.3; embed_dim],
                fact_type: FactType::Episodic,
                t_created: old_time,
                t_expired: None,
                t_valid: None,
                t_invalid: None,
                source_event_id: None,
                scope_id: 1,
                importance: 0.1,
                access_count: 0,
                last_accessed: old_time,
                metadata: serde_json::json!({}),
                is_pinned: false,
            })
            .unwrap();

        let mut graph = MemoryGraph::new();
        let policy = ForgetPolicy {
            min_importance: 0.3,
            ..ForgetPolicy::default()
        };

        let (stats, _expired_ids) = prune(&conn, &mut graph, &policy, embed_dim, now).unwrap();
        assert_eq!(stats.facts_evaluated, 3);
        // At least 1 fact should be pruned (fact 3 with low importance + old age)
        assert!(
            stats.facts_expired >= 1,
            "Expected at least 1 fact expired, got {}",
            stats.facts_expired
        );

        // Verify expired facts still exist in DB (soft delete, not hard delete)
        let all_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM facts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(all_count, 3, "All facts should still exist in DB");
    }

    #[test]
    fn prune_skips_pinned_facts() {
        use crate::types::NewFact;

        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();

        let now = Utc::now();
        let old_time = now - Duration::days(200);
        let embed_dim = 4;
        let fact_store = FactStore::new(&conn, embed_dim);

        // Pinned fact with low importance and old age — would normally be pruned
        fact_store
            .insert(&NewFact {
                content: "pinned identity".into(),
                content_hash: "hp".into(),
                embedding: vec![0.1; embed_dim],
                fact_type: FactType::Semantic,
                t_created: old_time,
                t_expired: None,
                t_valid: None,
                t_invalid: None,
                source_event_id: None,
                scope_id: 1,
                importance: 0.01,
                access_count: 0,
                last_accessed: old_time,
                metadata: serde_json::json!({}),
                is_pinned: true,
            })
            .unwrap();

        // Unpinned fact with same characteristics — should be pruned
        fact_store
            .insert(&NewFact {
                content: "forgettable".into(),
                content_hash: "hf".into(),
                embedding: vec![0.2; embed_dim],
                fact_type: FactType::Episodic,
                t_created: old_time,
                t_expired: None,
                t_valid: None,
                t_invalid: None,
                source_event_id: None,
                scope_id: 1,
                importance: 0.01,
                access_count: 0,
                last_accessed: old_time,
                metadata: serde_json::json!({}),
                is_pinned: false,
            })
            .unwrap();

        let mut graph = MemoryGraph::new();
        let policy = ForgetPolicy {
            min_importance: 0.3,
            ..ForgetPolicy::default()
        };
        let (stats, _pruned_ids) = prune(&conn, &mut graph, &policy, embed_dim, now).unwrap();

        assert_eq!(stats.facts_expired, 1); // only unpinned
        assert_eq!(stats.facts_evaluated, 2);

        // Pinned fact still active
        let active = fact_store.list_active(None).unwrap();
        assert_eq!(active.len(), 1);
        assert!(active[0].is_pinned);
    }

    #[test]
    fn prune_materializes_importance_scores() {
        use crate::types::NewFact;

        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();

        let now = Utc::now();
        let embed_dim = 4;
        let fact_store = FactStore::new(&conn, embed_dim);

        fact_store
            .insert(&NewFact {
                content: "scored fact".into(),
                content_hash: "hs".into(),
                embedding: vec![0.1; embed_dim],
                fact_type: FactType::Semantic,
                t_created: now,
                t_expired: None,
                t_valid: None,
                t_invalid: None,
                source_event_id: None,
                scope_id: 1,
                importance: 0.8,
                access_count: 10,
                last_accessed: now,
                metadata: serde_json::json!({}),
                is_pinned: false,
            })
            .unwrap();

        let mut graph = MemoryGraph::new();
        let policy = ForgetPolicy::default();
        prune(&conn, &mut graph, &policy, embed_dim, now).unwrap();

        // After prune, importance_score should be updated from default
        let fact = fact_store.get(1).unwrap();
        assert!(
            (fact.importance_score - 0.5).abs() > f64::EPSILON,
            "importance_score should have been updated from default 0.5, got {}",
            fact.importance_score
        );
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn policy_validation_rejects_invalid() {
        let mut policy = ForgetPolicy::default();

        // Zero half-life
        policy.half_life_days = 0.0;
        assert!(policy.validate().is_err());
        policy.half_life_days = 69.0;

        // Negative weight
        policy.recency_weight = -1.0;
        assert!(policy.validate().is_err());
        policy.recency_weight = 0.3;

        // min_importance out of range
        policy.min_importance = 1.5;
        assert!(policy.validate().is_err());
        policy.min_importance = 0.1;

        // Negative half-life override
        let mut overrides = HashMap::new();
        overrides.insert(FactType::Episodic, -10.0);
        policy.half_life_overrides = overrides;
        assert!(policy.validate().is_err());
    }
}
