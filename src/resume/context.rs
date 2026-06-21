use std::collections::HashSet;

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::error::Result;
use crate::store::facts::FactStore;
use crate::types::Fact;

/// Configuration for [`resume_context`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResumeConfig {
    /// Scope path to resume from. None = root only.
    pub scope_path: Option<String>,
    /// Current time for due-fact evaluation.
    pub now: DateTime<Utc>,
    /// Max pinned facts. Default: 50.
    pub pinned_cap: usize,
    /// Max high-importance facts (by materialized score). Default: 20.
    pub high_importance_cap: usize,
    /// Minimum `importance_score` for high-importance tier. Default: 0.7.
    pub high_importance_min: f64,
    /// Max due facts (future memory now surfacing). Default: 10.
    pub due_cap: usize,
    /// Max recent facts (scope-filtered). Default: 10.
    pub recent_cap: usize,
}

impl Default for ResumeConfig {
    fn default() -> Self {
        Self {
            scope_path: None,
            now: Utc::now(),
            pinned_cap: 50,
            high_importance_cap: 20,
            high_importance_min: 0.7,
            due_cap: 10,
            recent_cap: 10,
        }
    }
}

impl ResumeConfig {
    /// Validate configuration parameters.
    ///
    /// `high_importance_min` is a materialized-score threshold that flows
    /// directly into [`FactStore::list_by_importance_score`]'s `min_score`
    /// comparison; it must be finite and within `[0.0, 1.0]` — a value like
    /// `2.0` would silently yield an empty high-importance tier with no
    /// diagnostic. The four tier caps must each be non-zero, since a `0` cap
    /// silently empties that tier.
    ///
    /// Mirrors the [`DreamCycleConfig::validate`](crate::types::DreamCycleConfig::validate)
    /// precedent and uses the same
    /// [`ConflictError::PolicyParameter`](crate::error::ConflictError::PolicyParameter)
    /// payload.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Conflict`](crate::error::MemoryError::Conflict)
    /// wrapping [`ConflictError::PolicyParameter`](crate::error::ConflictError::PolicyParameter)
    /// if `high_importance_min` is non-finite or outside `[0.0, 1.0]`, or if any
    /// of the tier caps (`pinned_cap`, `high_importance_cap`, `due_cap`,
    /// `recent_cap`) is zero.
    pub fn validate(&self) -> Result<()> {
        use crate::error::{ConflictError, MemoryError};

        if !self.high_importance_min.is_finite() || !(0.0..=1.0).contains(&self.high_importance_min)
        {
            return Err(MemoryError::Conflict(ConflictError::PolicyParameter(
                format!(
                    "high_importance_min must be in [0.0, 1.0], got {}",
                    self.high_importance_min
                ),
            )));
        }
        for (name, cap) in [
            ("pinned_cap", self.pinned_cap),
            ("high_importance_cap", self.high_importance_cap),
            ("due_cap", self.due_cap),
            ("recent_cap", self.recent_cap),
        ] {
            if cap == 0 {
                return Err(MemoryError::Conflict(ConflictError::PolicyParameter(
                    format!("{name} must be greater than 0, got 0"),
                )));
            }
        }
        Ok(())
    }
}

/// Result of [`resume_context`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResumeContext {
    /// Pinned (unforgettable) facts — agent identity, core beliefs.
    pub pinned: Vec<Fact>,
    /// High-importance facts by materialized score.
    pub high_importance: Vec<Fact>,
    /// Future-memory facts whose `t_valid` has arrived.
    pub due: Vec<Fact>,
    /// Most recent facts from active scopes.
    pub recent: Vec<Fact>,
    /// Placeholder: KB reference URIs for Phase 5.
    pub kb_stubs: Vec<String>,
}

/// Retrieve tiered context for resuming a session.
///
/// Four fact tiers, mutually exclusive (no fact appears in multiple tiers):
///
/// 1. **Pinned** — all pinned facts (always present, cross-scope)
/// 2. **High-importance** — top-N by materialized `importance_score`
/// 3. **Due** — active facts with `t_valid <= now` (future memory surfacing)
/// 4. **Scope-filtered recent** — newest by `t_created` from active scopes
///
/// Plus `kb_stubs` — placeholder `Vec<String>` for Phase 5 (not a fact tier).
///
/// Takes pre-resolved scope IDs to avoid holding cache locks across DB access.
///
/// # Errors
///
/// Returns [`crate::error::MemoryError`] if [`ResumeConfig::validate`] rejects
/// the config (out-of-range `high_importance_min` or a zero tier cap), or if any
/// underlying store query fails.
///
/// # Examples
///
/// This is a crate-internal helper; callers use the public
/// [`MemoryEngine::resume_context`](crate::MemoryEngine::resume_context).
/// The example is illustrative (`ignore`d) because `resume` is a `pub(crate)`
/// module — a compiled doctest cannot reach it from outside the crate.
///
/// ```ignore
/// use crate::resume::{ResumeConfig, resume_context};
/// use rusqlite::Connection;
/// let conn = Connection::open_in_memory().unwrap();
/// let config = ResumeConfig::default();
/// let ctx = resume_context(&conn, &[1], 384, &config).unwrap();
/// ```
pub fn resume_context(
    conn: &Connection,
    scope_ids: &[i64],
    embed_dim: usize,
    config: &ResumeConfig,
) -> Result<ResumeContext> {
    config.validate()?;

    let fact_store = FactStore::new(conn, embed_dim);
    let mut seen: HashSet<i64> = HashSet::new();

    // Tier 1: Pinned facts (always present, cross-scope)
    let pinned_all = fact_store.list_pinned(&[])?;
    let pinned: Vec<Fact> = pinned_all.into_iter().take(config.pinned_cap).collect();
    seen.extend(pinned.iter().map(|f| f.id));

    // Tier 2: High-importance by materialized score
    let high_importance = fact_store.list_by_importance_score(
        scope_ids,
        config.high_importance_min,
        config.high_importance_cap,
        &seen,
    )?;
    seen.extend(high_importance.iter().map(|f| f.id));

    // Tier 3: Due facts (future memory now surfacing)
    let due_all = fact_store.list_due(config.now, scope_ids)?;
    let due: Vec<Fact> = due_all
        .into_iter()
        .filter(|f| !seen.contains(&f.id))
        .take(config.due_cap)
        .collect();
    seen.extend(due.iter().map(|f| f.id));

    // Tier 4: Scope-filtered recent
    let recent = fact_store.list_by_scopes_recent(scope_ids, config.recent_cap, &seen)?;

    // Tier 5: KB stubs (Phase 5 placeholder)
    let kb_stubs = Vec::new();

    Ok(ResumeContext {
        pinned,
        high_importance,
        due,
        recent,
        kb_stubs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::facts::FactStore;
    use crate::store::schema::{init_schema, migrate, open_memory};
    use crate::types::{FactType, NewFact};
    use chrono::Utc;

    const DIM: usize = 4;

    fn setup() -> Connection {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn, None).unwrap();
        conn
    }

    fn make_fact(content: &str, importance: f64, scope_id: i64) -> NewFact {
        NewFact::builder(content, vec![0.1; DIM], FactType::Semantic)
            .importance(importance)
            .scope_id(scope_id)
            .build()
    }

    #[test]
    fn resume_config_serde_roundtrip() {
        let config = ResumeConfig {
            scope_path: Some("agents/assistant".into()),
            now: Utc::now(),
            pinned_cap: 25,
            high_importance_cap: 15,
            high_importance_min: 0.6,
            due_cap: 5,
            recent_cap: 8,
        };
        let json = serde_json::to_string(&config).expect("serialize ResumeConfig");
        let restored: ResumeConfig = serde_json::from_str(&json).expect("deserialize ResumeConfig");
        assert_eq!(config.scope_path, restored.scope_path);
        assert_eq!(config.pinned_cap, restored.pinned_cap);
        assert_eq!(config.high_importance_cap, restored.high_importance_cap);
        assert_eq!(config.due_cap, restored.due_cap);
        assert_eq!(config.recent_cap, restored.recent_cap);
        assert!((config.high_importance_min - restored.high_importance_min).abs() < f64::EPSILON);
    }

    #[test]
    fn resume_context_serde_roundtrip() {
        let ctx = ResumeContext {
            pinned: vec![],
            high_importance: vec![],
            due: vec![],
            recent: vec![],
            kb_stubs: vec!["kb://some/stub".into()],
        };
        let json = serde_json::to_string(&ctx).expect("serialize ResumeContext");
        let restored: ResumeContext =
            serde_json::from_str(&json).expect("deserialize ResumeContext");
        assert_eq!(ctx.kb_stubs, restored.kb_stubs);
        assert!(restored.pinned.is_empty());
        assert!(restored.high_importance.is_empty());
        assert!(restored.due.is_empty());
        assert!(restored.recent.is_empty());
    }

    #[test]
    fn resume_empty_db() {
        let conn = setup();
        let config = ResumeConfig::default();
        let ctx = resume_context(&conn, &[1], DIM, &config).unwrap();
        assert!(ctx.pinned.is_empty());
        assert!(ctx.high_importance.is_empty());
        assert!(ctx.due.is_empty());
        assert!(ctx.recent.is_empty());
        assert!(ctx.kb_stubs.is_empty());
    }

    #[test]
    fn resume_pinned_always_present() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        let mut pinned = make_fact("pinned identity", 0.9, 1);
        pinned.is_pinned = true;
        fs.insert(&pinned).unwrap();

        let config = ResumeConfig::default();
        let ctx = resume_context(&conn, &[1], DIM, &config).unwrap();
        assert_eq!(ctx.pinned.len(), 1);
        assert!(ctx.pinned[0].is_pinned);
    }

    #[test]
    fn resume_due_surfaces_at_time() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        let now = Utc::now();
        let past = now - chrono::Duration::hours(1);

        let mut due_fact = make_fact("reminder", 0.5, 1);
        due_fact.t_valid = Some(past);
        fs.insert(&due_fact).unwrap();

        let config = ResumeConfig {
            now,
            ..ResumeConfig::default()
        };
        let ctx = resume_context(&conn, &[1], DIM, &config).unwrap();
        assert_eq!(ctx.due.len(), 1);
    }

    #[test]
    fn resume_tiers_mutually_exclusive() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        let now = Utc::now();

        // Pinned fact
        let mut pinned = make_fact("pinned", 0.95, 1);
        pinned.is_pinned = true;
        fs.insert(&pinned).unwrap();

        // High importance (needs importance_score >= 0.7)
        let id2 = fs.insert(&make_fact("important", 0.9, 1)).unwrap();
        fs.update_importance_score(id2, 0.85).unwrap();

        // Due fact
        let mut due = make_fact("due item", 0.5, 1);
        due.t_valid = Some(now - chrono::Duration::hours(1));
        fs.insert(&due).unwrap();

        // Recent fact
        fs.insert(&make_fact("recent", 0.1, 1)).unwrap();

        let config = ResumeConfig {
            now,
            ..ResumeConfig::default()
        };
        let ctx = resume_context(&conn, &[1], DIM, &config).unwrap();

        let all_ids: Vec<i64> = ctx
            .pinned
            .iter()
            .chain(ctx.high_importance.iter())
            .chain(ctx.due.iter())
            .chain(ctx.recent.iter())
            .map(|f| f.id)
            .collect();
        let unique: HashSet<i64> = all_ids.iter().copied().collect();
        assert_eq!(all_ids.len(), unique.len(), "no duplicates across tiers");
    }

    #[test]
    fn validate_accepts_default_config() {
        assert!(ResumeConfig::default().validate().is_ok());
    }

    #[test]
    fn validate_rejects_out_of_range_high_importance_min() {
        use crate::error::{ConflictError, MemoryError};

        for bad in [2.0_f64, -0.1, f64::NAN, f64::INFINITY] {
            let config = ResumeConfig {
                high_importance_min: bad,
                ..ResumeConfig::default()
            };
            let err = config.validate().unwrap_err();
            assert!(
                matches!(
                    err,
                    MemoryError::Conflict(ConflictError::PolicyParameter(_))
                ),
                "expected PolicyParameter conflict for high_importance_min={bad}, got {err:?}"
            );
        }
    }

    #[test]
    fn validate_accepts_boundary_high_importance_min() {
        for ok in [0.0_f64, 1.0] {
            let config = ResumeConfig {
                high_importance_min: ok,
                ..ResumeConfig::default()
            };
            assert!(
                config.validate().is_ok(),
                "boundary high_importance_min={ok} must be accepted"
            );
        }
    }

    #[test]
    fn validate_rejects_zero_caps() {
        use crate::error::{ConflictError, MemoryError};

        let zero_setters: [fn(&mut ResumeConfig); 4] = [
            |c| c.pinned_cap = 0,
            |c| c.high_importance_cap = 0,
            |c| c.due_cap = 0,
            |c| c.recent_cap = 0,
        ];
        for set_zero in zero_setters {
            let mut config = ResumeConfig::default();
            set_zero(&mut config);
            let err = config.validate().unwrap_err();
            assert!(
                matches!(
                    err,
                    MemoryError::Conflict(ConflictError::PolicyParameter(_))
                ),
                "expected PolicyParameter conflict for a zero cap, got {err:?}"
            );
        }
    }

    #[test]
    fn resume_context_propagates_invalid_config() {
        let conn = setup();
        let config = ResumeConfig {
            high_importance_min: 5.0,
            ..ResumeConfig::default()
        };
        assert!(resume_context(&conn, &[1], DIM, &config).is_err());
    }

    #[test]
    fn resume_high_importance_uses_score() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);

        // Fact with high importance_score
        let id = fs.insert(&make_fact("important", 0.9, 1)).unwrap();
        fs.update_importance_score(id, 0.85).unwrap();

        // Fact with low importance_score
        let id2 = fs.insert(&make_fact("not important", 0.1, 1)).unwrap();
        fs.update_importance_score(id2, 0.3).unwrap();

        let config = ResumeConfig {
            high_importance_min: 0.7,
            ..ResumeConfig::default()
        };
        let ctx = resume_context(&conn, &[1], DIM, &config).unwrap();
        assert_eq!(ctx.high_importance.len(), 1);
        assert!(ctx.high_importance[0].importance_score >= 0.7);
    }
}
