//! Engine methods for activity stream and session lifecycle.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::Utc;

use crate::engine::activity_filter::{
    ActivityFilterConfig, ActivityFilterDecision, PromoteAction, apply_filter,
    format_promote_content,
};
use crate::error::{MemoryError, Result};
use crate::limits::{check_json_size, check_str_size};
use crate::traits::EmbeddingProvider;
use crate::types::{
    ActivityStatus, AddFactOptions, AddFactRequest, NewActivity, ProjectContext,
    RecordActivityRequest, RecordActivityResult, SessionCheckpoint,
};

use super::MemoryEngine;

/// Compute blake3 hex hash, truncated to 32 chars (128 bits).
/// Same policy as `FactStore::content_hash`.
fn args_hash(canonical: &str) -> String {
    blake3::hash(canonical.as_bytes()).to_hex()[..32].to_string()
}

impl MemoryEngine {
    // --- Public API: Activity stream ---

    /// Record a tool activity with server-side filtering.
    ///
    /// Flow:
    /// 1. Apply the ignore/promote filter (pre-storage).
    /// 2. Apply the capture contract (#1024, ADR-0020 §6): a call touching a sensitive path is
    ///    a stub keeping only its path; otherwise input and result are redacted, then cut.
    /// 3. Compute `args_hash` (blake3, 32 hex) over the redacted, uncut input.
    /// 4. Resolve scope if provided, then insert or dedup at the store layer.
    /// 5. If promote AND not deduplicated AND not a stub: create a fact from the redacted
    ///    result.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on write failure, or
    /// `MemoryError::ReadOnly` if the engine is read-only.
    pub async fn record_activity(
        &self,
        req: &RecordActivityRequest,
        embedder: Option<Arc<dyn EmbeddingProvider>>,
        filter_config: &ActivityFilterConfig,
    ) -> Result<RecordActivityResult> {
        self.ensure_writable()?; // #972: fail fast before filtering/embedding + the activity write
        // Step 1: Filter decision (no locks needed); an ignored tool is never scanned.
        let mut decision = apply_filter(&req.tool_name, &req.args, None, filter_config);
        if matches!(decision, ActivityFilterDecision::Ignore) {
            return Ok(RecordActivityResult {
                activity_id: None,
                was_deduplicated: false,
                promoted_fact_id: None,
                status: ActivityStatus::Ignored,
            });
        }

        // Step 2: the capture contract.
        let capture = crate::redact::capture_tool_use(
            &req.args,
            req.result.as_deref(),
            // `None` defaults to `OutcomeClass::Success` (the DB column default).
            req.outcome_class.clone().unwrap_or_default(),
            &req.touched_paths,
            req.cwd.as_deref(),
        )?;
        let partly_scanned_result = req.result.is_some() && capture.full_result.is_none();
        if let ActivityFilterDecision::Promote(action) = &mut decision {
            // Scrubbed again after its own 200-byte cut, which can expose a match the result did
            // not hold: the tombstone check below must read the text `add_fact` stores.
            action.fact_content = crate::redact::scrub(&format_promote_content(
                &req.tool_name,
                capture.full_result.as_deref(),
            ))?;
        }
        let input_found = capture.input_found;

        // Step 3: Compute args hash.
        let hash = args_hash(&capture.hash_input);

        // Steps 3+4: Resolve scope, then insert/dedup via the port.
        let scope_id = match req.scope_path.as_deref() {
            Some(path) => self.ensure_scope_path(path).await?,
            None => 1, // root scope
        };

        let new_activity = NewActivity {
            session_id: req.session_id.clone(),
            tool_name: req.tool_name.clone(),
            args_hash: hash,
            args: capture.args,
            result_summary: capture.result,
            outcome_class: capture.outcome,
            timestamp: req.timestamp,
            scope_id,
            machine: None,
        };

        let (activity_id, was_deduplicated) = self
            .storage
            .insert_or_dedup_activity(&new_activity, filter_config.dedup_window_secs)
            .await?;
        // A duplicate's input and outcome only matched its row; its result rewrote the row's.
        if was_deduplicated {
            self.tally(capture.result_found, 0).await;
        } else {
            self.tally(input_found + capture.result_found, u64::from(capture.stub))
                .await;
        }

        // Step 5: Promote (only if new, not deduplicated).
        let (status, promoted_fact_id) = if was_deduplicated {
            (ActivityStatus::Deduplicated, None)
        } else if capture.stub || partly_scanned_result {
            // A stub keeps nothing to promote; a result over the write limit was scanned in part.
            (ActivityStatus::Recorded, None)
        } else if let ActivityFilterDecision::Promote(action) = &decision {
            self.promote_activity(activity_id, action, req, embedder)
                .await?
        } else {
            (ActivityStatus::Recorded, None)
        };

        Ok(RecordActivityResult {
            activity_id: Some(activity_id),
            was_deduplicated,
            promoted_fact_id,
            status,
        })
    }

    /// Promote a newly recorded activity to a fact, best-effort: a failure records it as is.
    async fn promote_activity(
        &self,
        activity_id: i64,
        action: &PromoteAction,
        req: &RecordActivityRequest,
        embedder: Option<Arc<dyn EmbeddingProvider>>,
    ) -> Result<(ActivityStatus, Option<i64>)> {
        // A promotion is a derivation, so a forget's tombstone keeps its content out
        // (#1023). `add_fact` is the ungated assertion path, so the check is here; it
        // reads outside the write, and `record_activity` has no server or CLI caller.
        if self.storage.is_forgotten(&action.fact_content).await? {
            return Ok((ActivityStatus::Recorded, None));
        }
        // No embedder: skip promotion, record as normal.
        let Some(emb) = embedder else {
            return Ok((ActivityStatus::Recorded, None));
        };
        // Embed OUTSIDE the write lock.
        let request = AddFactRequest {
            content: action.fact_content.clone(),
            fact_type: action.fact_type,
            source_event_id: None,
            scope: req.scope_path.clone(),
            opts: Some(AddFactOptions {
                base_importance: Some(action.importance),
                ..Default::default()
            }),
        };
        match self.add_fact(&request, emb, None).await {
            Ok(fact_id) => {
                self.storage
                    .update_activity_status(activity_id, ActivityStatus::Promoted, Some(fact_id))
                    .await?;
                Ok((ActivityStatus::Promoted, Some(fact_id)))
            }
            Err(e) => {
                // Promotion failed (e.g., embedding error). Record as
                // normal, but surface the failure so it is not silent.
                tracing::warn!(
                    activity_id,
                    error = %e,
                    "activity promotion to fact failed; recording without promotion"
                );
                Ok((ActivityStatus::Recorded, None))
            }
        }
    }

    // --- Public API: Session lifecycle ---

    /// Checkpoint a session (last-write-wins upsert).
    ///
    /// Called by the Stop hook when a session ends or pauses.
    ///
    /// Redaction (#1024): `summary` and `metadata` are scrubbed and kept whole (no cut); a text
    /// that does not settle is refused with `Conflict(Redaction)`.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only.
    /// Returns `MemoryError::Storage` on write failure.
    pub async fn checkpoint_session(
        &self,
        session_id: &str,
        scope_path: Option<&str>,
        summary: Option<&str>,
        metadata: Option<serde_json::Value>,
    ) -> Result<()> {
        self.ensure_writable()?; // #972: fail fast before the activity read + the checkpoint write
        // #1024: redact what is kept in full, after the raw size checks and before the write.
        let mut found = 0;
        let summary = match summary {
            Some(text) => {
                check_str_size(text, "checkpoint summary")?;
                let (clean, n) = crate::redact::scrub_with(text, &[])?;
                check_str_size(&clean, "checkpoint summary")?;
                found += n;
                Some(clean)
            }
            None => None,
        };
        let mut metadata = metadata.unwrap_or_else(|| serde_json::json!({}));
        check_json_size(&metadata, "checkpoint metadata")?;
        found += crate::redact::scrub_json(&mut metadata, &[])?;
        check_json_size(&metadata, "checkpoint metadata")?;
        // Find last activity_id for this session. `list_activities_by_session`
        // orders by `last_seen DESC`, so the first row of a 1-row page is the
        // most-recent activity — same row the old raw SQL `ORDER BY last_seen
        // DESC LIMIT 1` returned.
        let last_activity_id: Option<i64> = self
            .storage
            .list_activities_by_session(session_id, Some(1))
            .await?
            .first()
            .map(|a| a.id);

        let checkpoint = SessionCheckpoint {
            session_id: session_id.to_string(),
            scope_path: scope_path.map(String::from),
            summary,
            last_activity_id,
            checkpoint_at: Utc::now(),
            metadata,
        };
        self.storage.upsert_checkpoint(&checkpoint).await?;
        self.tally(found, 0).await;
        Ok(())
    }

    /// Load project context for session bootstrap.
    ///
    /// Returns recent activities, last checkpoint, and scope-filtered facts.
    /// All queries run in a single read snapshot for consistency.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if the scope path doesn't exist.
    pub async fn load_context(
        &self,
        scope_path: &str,
        activity_limit: usize,
        fact_limit: usize,
    ) -> Result<ProjectContext> {
        // Resolve scope IDs from cache (short-lived read lock). The owned
        // `scope_ids` vec is materialized before any `.await`, so the
        // `scope_tree` guard is never held across the awaits below.
        let scope_ids = {
            let tree = self.scope_tree.read();
            let id = tree
                .resolve_path(scope_path)
                .ok_or_else(|| MemoryError::NotFound(format!("scope path: {scope_path}")))?;
            tree.subtree(id)
        }; // scope_tree read lock dropped

        let checkpoint = self.storage.get_checkpoint_by_scope(scope_path).await?;
        let recent_activities = self
            .storage
            .list_recent_activities_by_scope(&scope_ids, activity_limit)
            .await?;
        let relevant_facts = self
            .storage
            .list_facts_by_scopes_recent(&scope_ids, fact_limit, &HashSet::new())
            .await?;

        Ok(ProjectContext {
            scope_path: scope_path.to_string(),
            recent_activities,
            last_checkpoint: checkpoint,
            relevant_facts,
        })
    }
}
