use chrono::{DateTime, Utc};

use crate::error::Result;
use crate::types::Fact;

/// Configuration for [`MemoryEngine::resume_context`](crate::MemoryEngine::resume_context).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResumeConfig {
    /// Scope path to resume from. None = root only.
    pub scope_path: Option<String>,
    /// Current time for due-fact evaluation. `None` (the [`Default`]) defers
    /// resolution to [`MemoryEngine::resume_context`](crate::MemoryEngine::resume_context),
    /// which resolves it once to `Utc::now()` at call time — keeping the `Default`
    /// impl pure (no wall-clock capture at construction). Set `Some(_)` to pin a
    /// specific evaluation instant (e.g. for deterministic tests or replay).
    pub now: Option<DateTime<Utc>>,
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
            now: None,
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

/// Result of [`MemoryEngine::resume_context`](crate::MemoryEngine::resume_context).
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
}
