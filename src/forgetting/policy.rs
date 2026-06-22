use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;

use crate::error::Result;
use crate::graph::MemoryGraph;
use crate::storage::StorageBackend;
use crate::store::facts::FactScoringRow;
use crate::traits::{ForgetPolicy, PruneStats};
use crate::types::{Fact, FactType};

/// Argument to `ln()` for access-frequency normalization: 100 + 1.
/// Gives `ln(101)` as the divisor so that 100 accesses produce a full score of 1.0.
const FREQUENCY_NORMALIZATION_ARG: f64 = 101.0;
/// Argument to `ln()` for graph-connectivity normalization: 50 + 1.
/// Gives `ln(51)` as the divisor so that 50 connections produce a full score of 1.0.
const CONNECTIVITY_NORMALIZATION_ARG: f64 = 51.0;

/// Ebbinghaus forgetting curve: retention = 2^(-age/half_life).
///
/// Returns 1.0 at `age=0`, 0.5 at `age=half_life`, 0.25 at `age=2×half_life`.
#[must_use]
pub fn ebbinghaus_decay(age_days: f64, half_life: f64) -> f64 {
    f64::exp2(-age_days / half_life)
}

/// The fact attributes [`compute_importance`] reads.
///
/// Implemented by both the full [`Fact`] and the lightweight [`FactScoringRow`]
/// projection, so the prune pass can score the entire active set without
/// materializing `content`/`embedding`/`metadata` (see [`prune`]).
///
/// `id` and `is_pinned` are intentionally absent — `prune` reads those directly
/// off the concrete row; the trait captures only the scoring inputs.
pub trait ImportanceInputs {
    fn fact_type(&self) -> &FactType;
    fn last_accessed(&self) -> DateTime<Utc>;
    fn access_count(&self) -> i64;
    fn importance(&self) -> f64;
}

impl ImportanceInputs for Fact {
    fn fact_type(&self) -> &FactType {
        &self.fact_type
    }
    fn last_accessed(&self) -> DateTime<Utc> {
        self.last_accessed
    }
    fn access_count(&self) -> i64 {
        self.access_count
    }
    fn importance(&self) -> f64 {
        self.importance
    }
}

impl ImportanceInputs for FactScoringRow {
    fn fact_type(&self) -> &FactType {
        &self.fact_type
    }
    fn last_accessed(&self) -> DateTime<Utc> {
        self.last_accessed
    }
    fn access_count(&self) -> i64 {
        self.access_count
    }
    fn importance(&self) -> f64 {
        self.importance
    }
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
    fact: &impl ImportanceInputs,
    graph_degree: usize,
    now: DateTime<Utc>,
    policy: &ForgetPolicy,
) -> f64 {
    // Knowledge-shaped types don't lose validity with age: recency stays 1.0.
    // An explicit half-life override wins and re-enables decay.
    let recency = if policy.is_decay_exempt(fact.fact_type()) {
        1.0
    } else {
        let half_life = policy
            .half_life_overrides
            .get(fact.fact_type())
            .copied()
            .unwrap_or(policy.half_life_days);

        #[allow(clippy::cast_precision_loss)]
        let age_days = (now - fact.last_accessed()).num_seconds() as f64 / 86400.0;
        ebbinghaus_decay(age_days.max(0.0), half_life)
    };

    // Normalization: log_base(count+1), capped at 1.0.
    // Using ln_1p for numerical accuracy near zero.
    #[allow(clippy::cast_precision_loss)]
    let frequency =
        (f64::ln_1p(fact.access_count() as f64) / FREQUENCY_NORMALIZATION_ARG.ln()).min(1.0);
    #[allow(clippy::cast_precision_loss)]
    let connectivity =
        (f64::ln_1p(graph_degree as f64) / CONNECTIVITY_NORMALIZATION_ARG.ln()).min(1.0);

    policy
        .recency_weight
        .mul_add(recency, policy.frequency_weight * frequency)
        + policy.graph_degree_weight.mul_add(
            connectivity,
            policy.base_importance_weight * fact.importance(),
        )
}

/// Prune facts with importance below threshold (the async cutover orchestrator).
///
/// Loads the lightweight active-scoring projection through the port, scores every
/// fact against the in-memory graph degree, expires the sub-threshold unpinned /
/// non-exempt set atomically below the seam ([`StorageBackend::prune_atomic`]), then
/// reconciles the in-memory graph. Pinned facts and decay-exempt fact types are
/// unforgettable — they still get a materialized score but bypass the expiry filter.
///
/// # `Send`-safety
///
/// The `parking_lot` graph guards are scoped strictly *between* the `.await`s — a
/// read guard for scoring, a write guard for reconciliation — so no guard is ever
/// held across an `.await` and the returned future stays `Send`.
///
/// # Returns
///
/// `(PruneStats, Vec<i64>)` — `PruneStats` carries `facts_evaluated`/`facts_expired`;
/// the vec is the set the backend **actually** expired (mirrored into the graph here).
///
/// # Errors
///
/// Returns `MemoryError::Conflict` if the policy fails validation, or
/// `MemoryError::Storage` on a backend failure.
pub async fn prune(
    storage: &Arc<dyn StorageBackend>,
    graph: &RwLock<MemoryGraph>,
    policy: &ForgetPolicy,
    now: DateTime<Utc>,
) -> Result<(PruneStats, Vec<i64>)> {
    policy.validate()?;

    // Prune must evaluate the *entire* active set (importance is global). The
    // lightweight scoring projection bounds the working set to a few scalars per
    // fact (issue #572 / L8). Awaited up front so the graph guard below is not held
    // across it.
    let active_facts = storage.list_active_facts_scoring().await?;

    // Score every active fact against its in-memory graph degree, and pick the
    // sub-threshold unpinned/non-exempt set — all under one brief read guard with
    // no `.await` inside (keeps the future `Send`).
    let (scored, to_expire) = {
        let g = graph.read();
        let scored: Vec<(i64, f64)> = active_facts
            .iter()
            .map(|fact| {
                (
                    fact.id,
                    compute_importance(fact, g.degree(fact.id), now, policy),
                )
            })
            .collect();
        let to_expire: Vec<i64> = active_facts
            .iter()
            .zip(&scored)
            .filter_map(|(fact, &(_, score))| {
                if fact.is_pinned || policy.is_decay_exempt(&fact.fact_type) {
                    return None;
                }
                (score < policy.min_importance).then_some(fact.id)
            })
            .collect();
        (scored, to_expire)
    };

    // Atomic write phase below the seam: materialize all scores + expire the
    // sub-threshold set + cascade edge expiry, in one transaction. Returns the ids
    // it actually expired (the backend also fires HNSW `notify_expire`).
    let (stats, expired) = storage.prune_atomic(&scored, &to_expire, now).await?;

    // Reconcile the in-memory graph after the commit (write guard, no `.await`).
    if !expired.is_empty() {
        let mut g = graph.write();
        for &fact_id in &expired {
            g.remove_edges_by_fact(fact_id);
        }
    }

    Ok((stats, expired))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use std::collections::HashMap;

    use crate::pool::ConnectionPool;
    use crate::storage::StorageBackend;
    use crate::storage::sqlite::SqliteBackend;
    use crate::store::upcaster::UpcasterRegistry;
    use crate::types::FactType;

    #[tokio::test]
    async fn decay_at_zero_is_one() {
        let result = ebbinghaus_decay(0.0, 69.0);
        assert!((result - 1.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn decay_at_half_life_is_half() {
        let result = ebbinghaus_decay(69.0, 69.0);
        assert!((result - 0.5).abs() < 1e-10);
    }

    #[tokio::test]
    async fn decay_at_two_half_lives_is_quarter() {
        let result = ebbinghaus_decay(138.0, 69.0);
        assert!((result - 0.25).abs() < 1e-10);
    }

    #[tokio::test]
    async fn high_access_recent_connected_beats_neglected_isolated() {
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

    #[tokio::test]
    async fn per_fact_type_half_life_override() {
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

    #[tokio::test]
    async fn prune_expires_low_importance_facts() {
        use crate::types::NewFact;

        let now = Utc::now();
        let old_time = now - Duration::days(100);
        let embed_dim = 4;

        let backend = std::sync::Arc::new(SqliteBackend::from_pool(
            std::sync::Arc::new(ConnectionPool::open_memory(embed_dim).unwrap()),
            std::sync::Arc::new(UpcasterRegistry::new()),
        ));
        let storage: std::sync::Arc<dyn StorageBackend> = backend.clone();

        // Insert 3 facts directly with varying importance and recency.

        // Fact 1: high importance, recently accessed
        storage
            .insert_fact(&NewFact {
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
            .await
            .unwrap();

        // Fact 2: medium importance, old
        storage
            .insert_fact(&NewFact {
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
            .await
            .unwrap();

        // Fact 3: low importance, very old
        storage
            .insert_fact(&NewFact {
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
            .await
            .unwrap();

        let graph = parking_lot::RwLock::new(MemoryGraph::new());
        let policy = ForgetPolicy {
            min_importance: 0.3,
            ..ForgetPolicy::default()
        };

        let (stats, _expired_ids) = prune(&storage, &graph, &policy, now).await.unwrap();
        assert_eq!(stats.facts_evaluated, 3);
        // At least 1 fact should be pruned (fact 3 with low importance + old age)
        assert!(
            stats.facts_expired >= 1,
            "Expected at least 1 fact expired, got {}",
            stats.facts_expired
        );

        // Verify expired facts still exist in DB (soft delete, not hard delete):
        // list_all_facts returns active + expired.
        let all_count = storage.list_all_facts().await.unwrap().len();
        assert_eq!(all_count, 3, "All facts should still exist in DB");
    }

    #[tokio::test]
    async fn prune_skips_pinned_facts() {
        use crate::types::NewFact;

        let now = Utc::now();
        let old_time = now - Duration::days(200);
        let embed_dim = 4;

        let backend = std::sync::Arc::new(SqliteBackend::from_pool(
            std::sync::Arc::new(ConnectionPool::open_memory(embed_dim).unwrap()),
            std::sync::Arc::new(UpcasterRegistry::new()),
        ));
        let storage: std::sync::Arc<dyn StorageBackend> = backend.clone();

        // Pinned fact with low importance and old age — would normally be pruned
        storage
            .insert_fact(&NewFact {
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
            .await
            .unwrap();

        // Unpinned fact with same characteristics — should be pruned
        storage
            .insert_fact(&NewFact {
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
            .await
            .unwrap();

        let graph = parking_lot::RwLock::new(MemoryGraph::new());
        let policy = ForgetPolicy {
            min_importance: 0.3,
            ..ForgetPolicy::default()
        };
        let (stats, _pruned_ids) = prune(&storage, &graph, &policy, now).await.unwrap();

        assert_eq!(stats.facts_expired, 1); // only unpinned
        assert_eq!(stats.facts_evaluated, 2);

        // Pinned fact still active
        let active = storage.list_active_facts(None).await.unwrap();
        assert_eq!(active.len(), 1);
        assert!(active[0].is_pinned);
    }

    #[tokio::test]
    async fn prune_materializes_importance_scores() {
        use crate::types::NewFact;

        let now = Utc::now();
        let embed_dim = 4;

        let backend = std::sync::Arc::new(SqliteBackend::from_pool(
            std::sync::Arc::new(ConnectionPool::open_memory(embed_dim).unwrap()),
            std::sync::Arc::new(UpcasterRegistry::new()),
        ));
        let storage: std::sync::Arc<dyn StorageBackend> = backend.clone();

        storage
            .insert_fact(&NewFact {
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
            .await
            .unwrap();

        let graph = parking_lot::RwLock::new(MemoryGraph::new());
        let policy = ForgetPolicy::default();
        prune(&storage, &graph, &policy, now).await.unwrap();

        // After prune, importance_score should be updated from default
        let fact = storage.get_fact(1).await.unwrap();
        assert!(
            (fact.importance_score - 0.5).abs() > f64::EPSILON,
            "importance_score should have been updated from default 0.5, got {}",
            fact.importance_score
        );
    }

    #[tokio::test]
    #[allow(clippy::field_reassign_with_default)]
    async fn policy_validation_rejects_invalid() {
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

    /// Ancient, neglected, isolated, unpinned fact — guaranteed below any
    /// reasonable importance threshold once its recency signal has decayed.
    fn neglected_fact(
        fact_type: FactType,
        hash: &str,
        now: DateTime<Utc>,
    ) -> crate::types::NewFact {
        let old_time = now - Duration::days(200);
        crate::types::NewFact {
            content: format!("neglected {fact_type}"),
            content_hash: hash.into(),
            embedding: vec![0.1; 4],
            fact_type,
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
        }
    }

    #[tokio::test]
    async fn prune_exempts_knowledge_shaped_types_by_default() {
        let now = Utc::now();
        let embed_dim = 4;

        let backend = std::sync::Arc::new(SqliteBackend::from_pool(
            std::sync::Arc::new(ConnectionPool::open_memory(embed_dim).unwrap()),
            std::sync::Arc::new(UpcasterRegistry::new()),
        ));
        let storage: std::sync::Arc<dyn StorageBackend> = backend.clone();

        // Identical neglect across the three types; only Episodic may decay away.
        storage
            .insert_fact(&neglected_fact(FactType::Semantic, "ks", now))
            .await
            .unwrap();
        storage
            .insert_fact(&neglected_fact(FactType::Procedural, "kp", now))
            .await
            .unwrap();
        storage
            .insert_fact(&neglected_fact(FactType::Episodic, "ke", now))
            .await
            .unwrap();

        let graph = parking_lot::RwLock::new(MemoryGraph::new());
        let policy = ForgetPolicy {
            min_importance: 0.3,
            ..ForgetPolicy::default()
        };
        let (stats, expired) = prune(&storage, &graph, &policy, now).await.unwrap();

        assert_eq!(stats.facts_evaluated, 3);
        assert_eq!(
            stats.facts_expired, 1,
            "only the episodic fact may expire, expired ids: {expired:?}"
        );
        let active = storage.list_active_facts(None).await.unwrap();
        let types: Vec<FactType> = active.iter().map(|f| f.fact_type).collect();
        assert!(
            types.contains(&FactType::Semantic),
            "semantic must survive neglect (supersession governs it, not decay)"
        );
        assert!(
            types.contains(&FactType::Procedural),
            "procedural must survive neglect (revision governs it, not decay)"
        );
    }

    #[tokio::test]
    async fn exempt_type_recency_does_not_decay() {
        let now = Utc::now();
        let policy = ForgetPolicy::default();

        let ancient = Fact {
            id: 1,
            content: "knowledge".into(),
            content_hash: "h".into(),
            embedding: vec![0.1; 4],
            fact_type: FactType::Semantic,
            t_created: now - Duration::days(500),
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            scope_id: 1,
            importance: 0.5,
            access_count: 5,
            last_accessed: now - Duration::days(500),
            metadata: serde_json::json!({}),
            is_pinned: false,
            importance_score: 0.5,
            surfaced_at: None,
        };
        let mut fresh = ancient.clone();
        fresh.last_accessed = now;

        let ancient_importance = compute_importance(&ancient, 0, now, &policy);
        let fresh_importance = compute_importance(&fresh, 0, now, &policy);
        assert!(
            (ancient_importance - fresh_importance).abs() < 1e-9,
            "age must not change an exempt fact's importance: ancient={ancient_importance}, fresh={fresh_importance}"
        );
    }

    #[tokio::test]
    async fn explicit_half_life_override_wins_over_exemption() {
        let now = Utc::now();
        let embed_dim = 4;

        let backend = std::sync::Arc::new(SqliteBackend::from_pool(
            std::sync::Arc::new(ConnectionPool::open_memory(embed_dim).unwrap()),
            std::sync::Arc::new(UpcasterRegistry::new()),
        ));
        let storage: std::sync::Arc<dyn StorageBackend> = backend.clone();
        storage
            .insert_fact(&neglected_fact(FactType::Semantic, "ks", now))
            .await
            .unwrap();

        let mut overrides = HashMap::new();
        overrides.insert(FactType::Semantic, 30.0);
        let policy = ForgetPolicy {
            half_life_overrides: overrides,
            min_importance: 0.3,
            ..ForgetPolicy::default()
        };

        let graph = parking_lot::RwLock::new(MemoryGraph::new());
        let (stats, _) = prune(&storage, &graph, &policy, now).await.unwrap();
        assert_eq!(
            stats.facts_expired, 1,
            "an explicit half-life override re-enables decay for an exempt type"
        );
    }

    #[tokio::test]
    async fn default_exemption_set_and_override_interaction() {
        let policy = ForgetPolicy::default();
        assert!(policy.is_decay_exempt(&FactType::Semantic));
        assert!(policy.is_decay_exempt(&FactType::Procedural));
        assert!(!policy.is_decay_exempt(&FactType::Episodic));

        let mut overridden = ForgetPolicy::default();
        overridden
            .half_life_overrides
            .insert(FactType::Semantic, 30.0);
        assert!(
            !overridden.is_decay_exempt(&FactType::Semantic),
            "explicit override must win over the default exemption"
        );
    }
}
