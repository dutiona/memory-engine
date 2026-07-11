use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;

use me_index::graph::MemoryGraph;
use me_storage::StorageBackend;
use me_types::error::Result;
use me_types::types::FactScoringRow;
use me_types::types::forgetting::PruneStats;
use me_types::types::{Fact, FactType};

use super::types::ForgetPolicy;

/// Argument to `ln()` for access-frequency normalization: 100 + 1.
/// Gives `ln(101)` as the divisor so that 100 accesses produce a full score of 1.0.
const FREQUENCY_NORMALIZATION_ARG: f64 = 101.0;
/// Argument to `ln()` for graph-connectivity normalization: 50 + 1.
/// Gives `ln(51)` as the divisor so that 50 connections produce a full score of 1.0.
const CONNECTIVITY_NORMALIZATION_ARG: f64 = 51.0;

/// Ebbinghaus forgetting curve: retention = 2^(-age/half_life).
///
/// Returns 1.0 at `age=0`, 0.5 at `age=half_life`, 0.25 at `age=2×half_life`.
///
/// # Precondition
///
/// `half_life` must be **positive and finite**, and `age_days` must be
/// non-negative. Passing `half_life <= 0.0` or `NaN` produces `NaN` or values
/// outside the stated `[0, 1]` retention range **without panicking** (e.g.
/// `ebbinghaus_decay(0.0, 0.0)` is `-0.0 / 0.0 = NaN`; a negative `half_life`
/// yields a value above `1.0`). When reached through [`prune`], this is
/// guaranteed by [`ForgetPolicy::validate`]. Direct callers own the precondition.
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
    /// The static base importance prior (the consumer-supplied seed), not the
    /// computed `importance_score`.
    fn base_importance(&self) -> f64;
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
    fn base_importance(&self) -> f64 {
        self.base_importance
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
    fn base_importance(&self) -> f64 {
        self.base_importance
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
/// 4. **Base importance**: `fact.base_importance()` — already in \[0, 1\]
///
/// The result therefore lies in `[0, sum_of_weights]`, **not** `[0, 1]` in
/// general (the four weights need not sum to 1.0 — see ADR-0006). For
/// [`ForgetPolicy::default`] the weights sum to `1.0`, so the default-policy
/// score is in `[0, 1]`.
///
/// # Precondition
///
/// The effective half-life (`policy.half_life_days`, or the per-`FactType`
/// `half_life_overrides` entry) must be **positive and finite** — the same
/// precondition [`ebbinghaus_decay`] carries, since the recency term delegates
/// to it. A non-positive or `NaN` half-life propagates `NaN` into the score
/// **without panicking**. When reached through [`prune`], this is guaranteed by
/// [`ForgetPolicy::validate`]; direct callers own the precondition.
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
            policy.base_importance_weight * fact.base_importance(),
        )
}

/// Prune facts with importance below threshold (the async cutover orchestrator).
///
/// Loads the lightweight active-scoring projection through the port, scores every
/// fact against the in-memory graph degree, expires the sub-threshold unpinned /
/// non-exempt set atomically below the seam ([`me_storage::FactGraph::prune_atomic`]), then
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
///
/// # Example
///
/// `prune` is a crate-internal entry point (the `forgetting` module is
/// `pub(crate)`), so this sketch is `ignore`d rather than run as a doctest — it
/// shows the intended call shape, not a compilable public API:
///
/// ```ignore
/// use std::sync::Arc;
/// use chrono::Utc;
/// use parking_lot::RwLock;
///
/// let storage: Arc<dyn StorageBackend> = /* a backend */;
/// let graph = RwLock::new(MemoryGraph::new());
/// let policy = ForgetPolicy::default();
///
/// // Score every active fact and soft-expire the sub-threshold set.
/// let (stats, expired) = prune(&storage, &graph, &policy, Utc::now()).await?;
/// assert_eq!(expired.len(), stats.facts_expired);
/// ```
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
    use me_test_support::factory::ConformanceBackend;
    use me_types::types::FactType;
    use proptest::prelude::*;
    use std::collections::HashMap;

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
            base_importance: 0.8,
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
            base_importance: 0.3,
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
            base_importance: 0.5,
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
        use me_types::types::NewFact;

        let now = Utc::now();
        let old_time = now - Duration::days(100);
        let embed_dim = 4;

        let storage = me_test_support::factory::SqliteFactory.make().await;

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
                base_importance: 0.9,
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
                base_importance: 0.3,
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
                base_importance: 0.1,
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
        use me_types::types::NewFact;

        let now = Utc::now();
        let old_time = now - Duration::days(200);
        let embed_dim = 4;

        let storage = me_test_support::factory::SqliteFactory.make().await;

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
                base_importance: 0.01,
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
                base_importance: 0.01,
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
        use me_types::types::NewFact;

        let now = Utc::now();
        let embed_dim = 4;

        let storage = me_test_support::factory::SqliteFactory.make().await;

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
                base_importance: 0.8,
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
    ) -> me_types::types::NewFact {
        let old_time = now - Duration::days(200);
        me_types::types::NewFact {
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
            base_importance: 0.01,
            access_count: 0,
            last_accessed: old_time,
            metadata: serde_json::json!({}),
            is_pinned: false,
        }
    }

    #[tokio::test]
    async fn prune_exempts_knowledge_shaped_types_by_default() {
        let now = Utc::now();

        let storage = me_test_support::factory::SqliteFactory.make().await;

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
            base_importance: 0.5,
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

        let storage = me_test_support::factory::SqliteFactory.make().await;
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

    /// #312: the prune → graph-reconcile composition. Every prior prune test runs
    /// against an EMPTY `MemoryGraph`, so neither the SQL edge cascade
    /// (`prune_atomic` → `EdgeStore::expire_by_fact`) nor the in-memory reconcile
    /// loop (`graph.write().remove_edges_by_fact`) is ever exercised end-to-end.
    /// This seeds edges in BOTH the `SQLite` `EdgeStore` (via the port) and the
    /// `MemoryGraph` (mirroring the DB), prunes a low-importance victim, and
    /// asserts the cascade fires in both projections while a non-victim edge
    /// survives untouched.
    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "one cohesive composition test: seed (facts+edges in DB and graph), \
                  prune, then assert the cascade across both the SQLite and in-memory \
                  projections — splitting it would scatter the shared fixture"
    )]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the read guard is intentionally held across the closing assertion \
                  block; this is a single-threaded test with no contention"
    )]
    async fn prune_cascades_edges_and_reconciles_graph() {
        use me_index::graph::EdgeData;
        use me_types::types::{NewEdge, NewFact};

        let now = Utc::now();
        let old_time = now - Duration::days(200);
        let embed_dim = 4;

        let storage = me_test_support::factory::SqliteFactory.make().await;

        // Seed a fact with the shared embed dim + scope. Built fresh per call so
        // each fact can vary its type/importance/recency.
        let make_fact = |content: &str, hash: &str, fact_type, base, accessed| NewFact {
            content: content.into(),
            content_hash: hash.into(),
            embedding: vec![0.1; embed_dim],
            fact_type,
            t_created: old_time,
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            scope_id: 1,
            base_importance: base,
            access_count: 0,
            last_accessed: accessed,
            metadata: serde_json::json!({}),
            is_pinned: false,
        };

        // Victim: Episodic, ancient, near-zero importance → guaranteed pruned.
        let victim = storage
            .insert_fact(&make_fact(
                "victim",
                "h-victim",
                FactType::Episodic,
                0.01,
                old_time,
            ))
            .await
            .unwrap();
        // Survivor: Semantic (decay-exempt) → immune to the prune. Adjacent to the
        // victim (edge cascades) AND to the bystander (edge must survive).
        let survivor = storage
            .insert_fact(&make_fact(
                "survivor",
                "h-surv",
                FactType::Semantic,
                0.9,
                now,
            ))
            .await
            .unwrap();
        // Bystander: Semantic, only adjacent to the survivor — its edge must be
        // untouched by the victim's cascade.
        let bystander = storage
            .insert_fact(&make_fact(
                "bystander",
                "h-by",
                FactType::Semantic,
                0.9,
                now,
            ))
            .await
            .unwrap();

        // Mirror two edges into BOTH SQLite and the in-memory graph:
        //   e_cascade:  victim   → survivor   (must expire when victim is pruned)
        //   e_keep:     survivor → bystander  (neither endpoint pruned → survives)
        let new_edge = |src, tgt| NewEdge {
            source_fact_id: src,
            target_fact_id: tgt,
            relation_type: "related".into(),
            weight: 1.0,
            scope_id: 1,
            t_created: old_time,
            t_expired: None,
        };
        let e_cascade = storage
            .insert_edge(&new_edge(victim, survivor))
            .await
            .unwrap();
        let e_keep = storage
            .insert_edge(&new_edge(survivor, bystander))
            .await
            .unwrap();

        let graph = parking_lot::RwLock::new(MemoryGraph::new());
        {
            let mut g = graph.write();
            g.add_edge(
                victim,
                survivor,
                EdgeData {
                    edge_id: e_cascade,
                    relation_type: "related".into(),
                    weight: 1.0,
                },
            );
            g.add_edge(
                survivor,
                bystander,
                EdgeData {
                    edge_id: e_keep,
                    relation_type: "related".into(),
                    weight: 1.0,
                },
            );
        }

        // Pre-conditions: the graph mirrors the two seeded edges.
        assert_eq!(graph.read().degree(victim), 1, "victim starts connected");
        assert_eq!(
            graph.read().degree(survivor),
            2,
            "survivor bridges victim and bystander"
        );

        let policy = ForgetPolicy {
            min_importance: 0.3,
            ..ForgetPolicy::default()
        };
        let (stats, expired) = prune(&storage, &graph, &policy, now).await.unwrap();

        // Only the victim is pruned (survivor + bystander are decay-exempt Semantic).
        assert_eq!(stats.facts_evaluated, 3);
        assert_eq!(stats.facts_expired, 1, "only the episodic victim expires");
        assert_eq!(expired, vec![victim]);

        // (a) SQLite cascade: the victim's edge is expired, the other edge stays active.
        let active_edges = storage.list_active_edges().await.unwrap();
        let active_ids: std::collections::HashSet<i64> =
            active_edges.iter().map(|e| e.id).collect();
        assert!(
            !active_ids.contains(&e_cascade),
            "victim's edge must be expired in SQLite (cascade)"
        );
        assert!(
            active_ids.contains(&e_keep),
            "survivor↔bystander edge must remain active in SQLite"
        );
        // It is soft-deleted, not hard-deleted: still present with t_expired set.
        let cascaded = storage.get_edge(e_cascade).await.unwrap();
        assert!(
            cascaded.t_expired.is_some(),
            "cascaded edge is soft-deleted (t_expired set), not removed"
        );

        // (b) In-memory reconcile loop ran + (c) the surviving adjacent fact keeps
        // its non-victim edge in the graph. Scope the read guard tightly so it is
        // released immediately after the assertions (no contention held to fn end).
        {
            let g = graph.read();
            // (b) The victim is fully disconnected.
            assert_eq!(g.degree(victim), 0, "in-memory graph reconciled the victim");
            assert!(
                g.neighbors(victim).is_empty(),
                "victim has no outgoing neighbors after reconcile"
            );
            // (c) The cascade is scoped to the victim, not a blanket wipe.
            assert_eq!(
                g.degree(survivor),
                1,
                "survivor keeps exactly its bystander edge (victim edge gone)"
            );
            assert_eq!(g.neighbors(survivor), vec![bystander]);
            assert_eq!(g.degree(bystander), 1);
        }
    }

    /// #454: `prune` on a store with zero active facts. Exercises the empty
    /// `list_active_facts_scoring` path — the score/expire loops never iterate,
    /// `prune_atomic` commits a no-op, and the graph reconcile is skipped — and
    /// asserts the clean zero-stats result. A regression here (e.g. an `Err` from
    /// `list_active_facts_scoring` on an empty table) would otherwise be invisible.
    #[tokio::test]
    async fn prune_empty_store_returns_zero_stats() {
        let now = Utc::now();

        let storage = me_test_support::factory::SqliteFactory.make().await;

        // Insert nothing — the store is empty.
        let graph = parking_lot::RwLock::new(MemoryGraph::new());
        let policy = ForgetPolicy::default();
        let (stats, expired) = prune(&storage, &graph, &policy, now).await.unwrap();

        assert_eq!(stats.facts_evaluated, 0);
        assert_eq!(stats.facts_expired, 0);
        assert!(expired.is_empty(), "expired set must be empty: {expired:?}");
    }

    /// #497 (sub-finding 4): the negative-age clamp in `compute_importance`. A
    /// fact whose `last_accessed` is in the **future** yields a negative
    /// `age_days`, which `age_days.max(0.0)` pins to `0.0` before the recency
    /// term is computed — so recency lands at exactly `1.0` (the `age=0` value),
    /// never above it and never `NaN`.
    #[tokio::test]
    async fn future_last_accessed_clamps_recency_to_one() {
        let now = Utc::now();
        // recency-only policy: isolates the recency term so the score *is* recency.
        let policy = ForgetPolicy {
            recency_weight: 1.0,
            frequency_weight: 0.0,
            graph_degree_weight: 0.0,
            base_importance_weight: 0.0,
            ..ForgetPolicy::default()
        };

        // Episodic (non-exempt → recency actually decays) but accessed 30 days
        // in the FUTURE relative to `now` ⇒ negative age ⇒ clamped to 0.0.
        let future_fact = Fact {
            id: 1,
            content: "from the future".into(),
            content_hash: "hfut".into(),
            embedding: vec![0.1; 4],
            fact_type: FactType::Episodic,
            t_created: now,
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            scope_id: 1,
            base_importance: 0.0,
            access_count: 0,
            last_accessed: now + Duration::days(30),
            metadata: serde_json::json!({}),
            is_pinned: false,
            importance_score: 0.5,
            surfaced_at: None,
        };

        let score = compute_importance(&future_fact, 0, now, &policy);
        assert!(score.is_finite(), "clamp must not yield NaN, got {score}");
        // recency_weight == 1.0 and all others 0.0 ⇒ score == recency == 1.0.
        assert!(
            (score - 1.0).abs() < f64::EPSILON,
            "future access must clamp recency to exactly 1.0, got {score}"
        );
        assert!(score <= 1.0, "recency must never exceed 1.0, got {score}");
    }

    proptest! {
        /// #455 (a): `ebbinghaus_decay(age >= 0, half_life > 0)` is always in the
        /// closed interval `[0, 1]`. The lower bound is closed, not open: for very
        /// large `age / half_life` ratios `2^(-age/hl)` underflows to exactly `+0.0`
        /// in f64 (once the ratio exceeds ~1074), which is legitimate IEEE-754
        /// behavior, not a defect. The declared `age in 0..3650`, `hl in 0.001..3650`
        /// strategy admits ratios up to 3.65e6, so `+0.0` is reachable.
        #[test]
        fn prop_decay_in_unit_interval(
            age in 0.0f64..3650.0,
            hl in 0.001f64..3650.0,
        ) {
            let r = ebbinghaus_decay(age, hl);
            prop_assert!((0.0..=1.0).contains(&r), "decay({age}, {hl}) = {r} outside [0, 1]");
        }

        /// #455 (b): decay is monotone **non-increasing** in age for a fixed half-life.
        #[test]
        fn prop_decay_monotone_non_increasing_in_age(
            age1 in 0.0f64..1000.0,
            age2 in 0.0f64..1000.0,
            hl in 0.001f64..3650.0,
        ) {
            prop_assume!(age1 < age2);
            let older = ebbinghaus_decay(age2, hl);
            let newer = ebbinghaus_decay(age1, hl);
            // newer (smaller age) retains at least as much as older.
            prop_assert!(
                newer + 1e-12 >= older,
                "decay not non-increasing in age: decay({age1})={newer} < decay({age2})={older}"
            );
        }

        /// #455 (c): decay is monotone **non-decreasing** in half-life for a fixed
        /// positive age (age == 0 is excluded — there decay is flat at 1.0).
        #[test]
        fn prop_decay_monotone_non_decreasing_in_half_life(
            age in 0.001f64..1000.0,
            hl1 in 0.001f64..1000.0,
            hl2 in 0.001f64..1000.0,
        ) {
            prop_assume!(hl1 < hl2);
            let shorter = ebbinghaus_decay(age, hl1);
            let longer = ebbinghaus_decay(age, hl2);
            // a longer half-life retains at least as much at the same age.
            prop_assert!(
                longer + 1e-12 >= shorter,
                "decay not non-decreasing in half-life: decay(hl={hl1})={shorter} > decay(hl={hl2})={longer}"
            );
        }

        /// #455 + #497 (sub-finding 3): `compute_importance` output bounds. With the
        /// non-negative-weight default policy and arbitrary valid inputs, the score
        /// is in `[0, SUM_OF_WEIGHTS]`. For `ForgetPolicy::default()` the four
        /// weights sum to `1.0`, so the bound is `[0, 1]` — asserted as the ACTUAL
        /// computed weight sum, not a blind `[0, 1]`.
        #[test]
        fn prop_compute_importance_within_weight_sum(
            // Episodic so recency genuinely decays (not pinned by exemption); any
            // valid scoring inputs within sane ranges.
            base_importance in 0.0f64..=1.0,
            access_count in 0i64..1_000_000,
            graph_degree in 0usize..1_000_000,
            age_secs in 0i64..(3650 * 86400),
        ) {
            let policy = ForgetPolicy::default();
            let weight_sum = policy.recency_weight
                + policy.frequency_weight
                + policy.graph_degree_weight
                + policy.base_importance_weight;

            let now = Utc::now();
            let fact = Fact {
                id: 1,
                content: "prop".into(),
                content_hash: "hp".into(),
                embedding: vec![0.1; 4],
                fact_type: FactType::Episodic,
                t_created: now,
                t_expired: None,
                t_valid: None,
                t_invalid: None,
                source_event_id: None,
                scope_id: 1,
                base_importance,
                access_count,
                last_accessed: now - Duration::seconds(age_secs),
                metadata: serde_json::json!({}),
                is_pinned: false,
                importance_score: 0.5,
                surfaced_at: None,
            };

            let score = compute_importance(&fact, graph_degree, now, &policy);
            prop_assert!(score.is_finite(), "score must be finite, got {score}");
            prop_assert!(score >= 0.0, "score must be non-negative, got {score}");
            prop_assert!(
                score <= weight_sum + 1e-9,
                "score {score} exceeds weight sum {weight_sum}"
            );
        }
    }
}
