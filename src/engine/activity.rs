//! Engine methods for activity stream and session lifecycle.

use std::collections::HashSet;

use chrono::Utc;

use crate::engine::activity_filter::{apply_filter, ActivityFilterConfig, ActivityFilterDecision};
use crate::error::{MemoryError, Result};
use crate::store::activities::ActivityStore;
use crate::store::checkpoints::CheckpointStore;
use crate::store::facts::FactStore;
use crate::traits::EmbeddingProvider;
use crate::types::{
    ActivityStatus, AddFactOptions, AddFactRequest, NewActivity, ProjectContext,
    RecordActivityRequest, RecordActivityResult, SessionCheckpoint,
};

use super::MemoryEngine;

/// Compute blake3 hex hash, truncated to 32 chars (128 bits).
/// Same policy as `FactStore::content_hash`.
fn args_hash(args: &serde_json::Value) -> String {
    let canonical = serde_json::to_string(args).unwrap_or_default();
    let hash = blake3::hash(canonical.as_bytes());
    hash.to_hex()[..32].to_string()
}

impl MemoryEngine {
    // --- Public API: Activity stream ---

    /// Record a tool activity with server-side filtering.
    ///
    /// Flow:
    /// 1. Apply ignore/promote filter (pre-storage).
    /// 2. Compute `args_hash` (blake3, 32 hex).
    /// 3. Resolve scope if provided.
    /// 4. Insert or dedup at store layer.
    /// 5. If promote AND not deduplicated: create a fact.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on write failure, or
    /// `MemoryError::ReadOnly` if the engine is read-only.
    pub fn record_activity(
        &self,
        req: &RecordActivityRequest,
        embedder: Option<&dyn EmbeddingProvider>,
        filter_config: &ActivityFilterConfig,
    ) -> Result<RecordActivityResult> {
        // Step 1: Filter decision (no locks needed).
        let decision = apply_filter(
            &req.tool_name,
            &req.args,
            req.result.as_deref(),
            filter_config,
        );
        if matches!(decision, ActivityFilterDecision::Ignore) {
            return Ok(RecordActivityResult {
                activity_id: None,
                was_deduplicated: false,
                promoted_fact_id: None,
                status: ActivityStatus::Ignored,
            });
        }

        // Step 2: Compute args hash.
        let hash = args_hash(&req.args);
        let outcome = req
            .outcome_class
            .as_deref()
            .unwrap_or("success")
            .to_string();

        // Steps 3+4: Resolve scope and insert/dedup under one lock acquisition.
        let conn = self.write_conn()?;
        let scope_id = match req.scope_path.as_deref() {
            Some(path) => self.ensure_scope_with_conn(&conn, path)?,
            None => 1, // root scope
        };

        let new_activity = NewActivity {
            session_id: req.session_id.clone(),
            tool_name: req.tool_name.clone(),
            args_hash: hash,
            args: req.args.clone(),
            result_summary: req.result.as_ref().map(|r| truncate(r, 512).to_string()),
            outcome_class: outcome,
            timestamp: req.timestamp,
            scope_id,
        };

        let (activity_id, was_deduplicated) = {
            let store = ActivityStore::new(&conn);
            store.insert_or_dedup(&new_activity, filter_config.dedup_window_secs)?
        };
        drop(conn);

        // Step 5: Promote (only if new, not deduplicated).
        let mut promoted_fact_id = None;
        let status = if was_deduplicated {
            ActivityStatus::Deduplicated
        } else if let ActivityFilterDecision::Promote(action) = &decision {
            // Embed OUTSIDE the write lock.
            if let Some(emb) = embedder {
                match self.add_fact(
                    &AddFactRequest {
                        content: action.fact_content.clone(),
                        fact_type: action.fact_type.clone(),
                        source_event_id: None,
                        scope: req.scope_path.clone(),
                        opts: Some(AddFactOptions {
                            importance: Some(action.importance),
                            ..Default::default()
                        }),
                    },
                    emb,
                    None, // no persistence classifier
                ) {
                    Ok(fact_id) => {
                        promoted_fact_id = Some(fact_id);
                        // Update activity status to promoted.
                        let conn = self.write_conn()?;
                        let store = ActivityStore::new(&conn);
                        store.update_status(
                            activity_id,
                            ActivityStatus::Promoted,
                            Some(fact_id),
                        )?;
                        ActivityStatus::Promoted
                    }
                    Err(_) => {
                        // Promotion failed (e.g., embedding error). Record as normal.
                        ActivityStatus::Recorded
                    }
                }
            } else {
                // No embedder — skip promotion, record as normal.
                ActivityStatus::Recorded
            }
        } else {
            ActivityStatus::Recorded
        };

        Ok(RecordActivityResult {
            activity_id: Some(activity_id),
            was_deduplicated,
            promoted_fact_id,
            status,
        })
    }

    // --- Public API: Session lifecycle ---

    /// Checkpoint a session (last-write-wins upsert).
    ///
    /// Called by the Stop hook when a session ends or pauses.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on write failure.
    pub fn checkpoint_session(
        &self,
        session_id: &str,
        scope_path: Option<&str>,
        summary: Option<&str>,
        metadata: Option<serde_json::Value>,
    ) -> Result<()> {
        let conn = self.write_conn()?;

        // Find last activity_id for this session.
        let last_activity_id: Option<i64> = conn
            .query_row(
                "SELECT id FROM activities WHERE session_id = ?1 ORDER BY last_seen DESC LIMIT 1",
                rusqlite::params![session_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(MemoryError::Database)?;

        let checkpoint = SessionCheckpoint {
            session_id: session_id.to_string(),
            scope_path: scope_path.map(String::from),
            summary: summary.map(String::from),
            last_activity_id,
            checkpoint_at: Utc::now(),
            metadata: metadata.unwrap_or(serde_json::json!({})),
        };
        CheckpointStore::new(&conn).upsert(&checkpoint)
    }

    /// Load project context for session bootstrap.
    ///
    /// Returns recent activities, last checkpoint, and scope-filtered facts.
    /// All queries run in a single read snapshot for consistency.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if the scope path doesn't exist.
    pub fn load_context(
        &self,
        scope_path: &str,
        activity_limit: usize,
        fact_limit: usize,
    ) -> Result<ProjectContext> {
        // Resolve scope IDs from cache (short-lived read lock).
        let scope_ids = {
            let tree = self.scope_tree.read();
            let id = tree
                .resolve_path(scope_path)
                .ok_or_else(|| MemoryError::NotFound(format!("scope path: {scope_path}")))?;
            tree.subtree(id)
        }; // scope_tree read lock dropped

        // Single read snapshot for consistency.
        self.with_read(|conn| {
            let checkpoint = CheckpointStore::new(conn).get_by_scope(scope_path)?;
            let recent_activities =
                ActivityStore::new(conn).list_recent_by_scope(&scope_ids, activity_limit)?;
            let relevant_facts = FactStore::new(conn, self.embed_dim).list_by_scopes_recent(
                &scope_ids,
                fact_limit,
                &HashSet::new(),
            )?;

            Ok(ProjectContext {
                scope_path: scope_path.to_string(),
                recent_activities,
                last_checkpoint: checkpoint,
                relevant_facts,
            })
        })
    }
}

/// Truncate a string to at most `max_len` characters, respecting UTF-8 boundaries.
fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        return s;
    }
    let mut end = max_len;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

use rusqlite::OptionalExtension;
