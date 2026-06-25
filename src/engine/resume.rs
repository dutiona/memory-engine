use std::collections::HashSet;

use chrono::Utc;

use crate::error::{MemoryError, Result};
use crate::resume::context::{ResumeConfig, ResumeContext};
use crate::types::Fact;

use super::{MemoryEngine, apply_surfaced_stamps};

impl MemoryEngine {
    // --- Public API: Resume ---

    /// Retrieve tiered context for resuming a session.
    ///
    /// Returns four tiers of facts (mutually exclusive):
    /// 1. **Pinned** — all pinned facts (cross-scope)
    /// 2. **High-importance** — top-N by materialized `importance_score`
    /// 3. **Due** — facts with `t_valid` <= now
    /// 4. **Recent** — most recent, from scope ancestors
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if the requested scope path doesn't exist,
    /// or `MemoryError::Conflict` if [`ResumeConfig::validate`] rejects the config
    /// (out-of-range `high_importance_min` or a zero tier cap).
    pub async fn resume_context(&self, config: &ResumeConfig) -> Result<ResumeContext> {
        // Fail fast at the public boundary: reject an invalid config before
        // acquiring the scope_tree lock or touching the DB (#359). The internal
        // tier walk below trusts its already-validated input.
        config.validate()?;

        // Resolve the evaluation instant ONCE, before the tier walk. `ResumeConfig`
        // defaults `now` to `None` (a pure `Default`); the wall-clock fallback lives
        // here so every tier read below — due-fact filtering, the bi-temporal
        // surfaced_at predicate, and the stamp — sees a single, consistent `now`.
        let now = config.now.unwrap_or_else(Utc::now);

        // Step 1: Resolve scope IDs from cache (short-lived read lock).
        // The scope_tree guard is taken and dropped entirely within this block —
        // no lock is held across the `.await`s that follow (keeps the future Send).
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

        // Step 2: Assemble the four tiers via the async storage port. This inlines
        // the former `resume::resume_context(conn, ...)` helper — the backend now
        // owns embed_dim, so the per-tier port calls drop it. Control flow, tier
        // ordering, dedup (`seen`), and caps are preserved verbatim.
        let mut seen: HashSet<i64> = HashSet::new();

        // Tier 1: Pinned facts (always present, cross-scope). The cap is pushed to
        // SQL (#395) — the DB no longer transmits/deserializes the embedding BLOBs
        // of pinned facts beyond `pinned_cap` only to discard them in Rust.
        let pinned = self
            .storage
            .list_pinned_facts(&[], Some(config.pinned_cap))
            .await?;
        seen.extend(pinned.iter().map(|f| f.id));

        // Tier 2: High-importance by materialized score
        let high_importance = self
            .storage
            .list_facts_by_importance_score(
                &scope_ids,
                config.high_importance_min,
                config.high_importance_cap,
                &seen,
            )
            .await?;
        seen.extend(high_importance.iter().map(|f| f.id));

        // Tier 3: Due facts (future memory now surfacing). The exclude(seen) set
        // and the cap are pushed to SQL (#396) — the DB no longer materializes (and
        // decodes the embedding BLOB of) every due fact only to drop the already-seen
        // ones and truncate to `due_cap` in Rust. json_each exclusion is
        // order-independent, and `ORDER BY t_valid ASC LIMIT` yields the identical
        // set the old `.filter(!seen).take(due_cap)` produced.
        let exclude: Vec<i64> = seen.iter().copied().collect();
        let due = self
            .storage
            .list_due_facts(now, &scope_ids, &exclude, Some(config.due_cap))
            .await?;
        seen.extend(due.iter().map(|f| f.id));

        // Tier 4: Scope-filtered recent
        let recent = self
            .storage
            .list_facts_by_scopes_recent(&scope_ids, config.recent_cap, &seen)
            .await?;

        let mut ctx = ResumeContext {
            pinned,
            high_importance,
            due,
            recent,
        };

        // Step 3: Stamp surfaced_at on ALL due facts across ALL tiers (#93).
        // A fact is "due" if t_valid <= now AND not bi-temporally invalidated,
        // regardless of which tier claimed it. The valid-time test is the shared
        // `Fact::is_temporally_due` predicate (#477) — the single source of truth
        // the SQL `list_due` and `explain`'s FactState::Due both mirror, so they
        // cannot silently drift. Here we additionally require the fact to be
        // *unsurfaced* (the surfacing concern stays at the call site).
        // Must use write_conn — read connections have query_only = ON.
        let is_unsurfaced_due =
            |f: &Fact| -> bool { f.surfaced_at.is_none() && f.is_temporally_due(now) };
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
            let stamped = self.stamp_surfaced_facts(&unsurfaced_ids, now).await?;
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
