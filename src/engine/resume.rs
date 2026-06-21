use crate::error::{MemoryError, Result};
use crate::resume::context::{ResumeConfig, ResumeContext};
use crate::types::Fact;

use super::{MemoryEngine, apply_surfaced_stamps};

impl MemoryEngine {
    // --- Public API: Resume ---

    /// Retrieve tiered context for resuming a session.
    ///
    /// Returns five tiers of facts (mutually exclusive):
    /// 1. **Pinned** — all pinned facts (cross-scope)
    /// 2. **High-importance** — top-N by materialized `importance_score`
    /// 3. **Due** — facts with `t_valid` <= now
    /// 4. **Recent** — most recent, from scope ancestors
    /// 5. **KB stubs** — placeholder for Phase 5
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if the requested scope path doesn't exist,
    /// or `MemoryError::Conflict` if [`ResumeConfig::validate`] rejects the config
    /// (out-of-range `high_importance_min` or a zero tier cap).
    pub fn resume_context(&self, config: &ResumeConfig) -> Result<ResumeContext> {
        // Fail fast at the public boundary: reject an invalid config before
        // acquiring the scope_tree lock or touching the DB (#359). The internal
        // `resume::resume_context` helper trusts its already-validated input.
        config.validate()?;

        // Step 1: Resolve scope IDs from cache (short-lived read lock)
        let scope_ids = {
            let tree = self.scope_tree.read();
            let root = crate::scope::ScopeTree::root_id();
            match config.scope_path.as_ref() {
                Some(path) => {
                    let id = tree
                        .resolve_path(path)
                        .ok_or_else(|| MemoryError::NotFound(format!("scope path: {path}")))?;
                    tree.ancestors(id)
                }
                None => vec![root],
            }
        }; // scope_tree read lock dropped here

        // Step 2: Query DB on read connection
        let mut ctx = self.with_read(|conn| {
            crate::resume::resume_context(conn, &scope_ids, self.embed_dim, config)
        })?;

        // Step 3: Stamp surfaced_at on ALL due facts across ALL tiers (#93).
        // A fact is "due" if t_valid <= now AND not bi-temporally invalidated,
        // regardless of which tier claimed it. Matches FactStore::list_due() predicate.
        // Must use write_conn — read connections have query_only = ON.
        let is_unsurfaced_due = |f: &Fact| -> bool {
            f.surfaced_at.is_none()
                && f.t_valid.is_some_and(|tv| tv <= config.now)
                && f.t_invalid.is_none_or(|ti| ti > config.now)
        };
        let unsurfaced_ids: Vec<i64> = ctx
            .pinned
            .iter()
            .chain(ctx.high_importance.iter())
            .chain(ctx.due.iter())
            .chain(ctx.recent.iter())
            .filter(|f| is_unsurfaced_due(f))
            .map(|f| f.id)
            .collect();

        if !unsurfaced_ids.is_empty() {
            let stamped = self.stamp_surfaced_facts(&unsurfaced_ids, config.now)?;
            apply_surfaced_stamps(
                ctx.pinned
                    .iter_mut()
                    .chain(ctx.high_importance.iter_mut())
                    .chain(ctx.due.iter_mut())
                    .chain(ctx.recent.iter_mut()),
                &stamped,
            );
        }

        Ok(ctx)
    }
}
