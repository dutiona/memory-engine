//! Session & cognitive bookkeeping: the activity stream + checkpoint cursors.
//!
//! Folds `store/activities.rs` + `store/checkpoints.rs`. The read methods that are
//! `#[cfg(test)]` in the concrete stores today (`get_activity`,
//! `count_activities_by_session`, `list_activities_by_session`, `get_checkpoint`,
//! `list_recent_checkpoints`) become **unconditional** trait methods: a
//! `#[cfg(test)]` trait method would fork the trait's vtable shape between test and
//! release builds, and the #632 cross-backend conformance suite needs these
//! read-backs to assert writes *through* the trait.

use async_trait::async_trait;

use me_types::error::Result;
use me_types::types::{Activity, ActivityStatus, NewActivity, SessionCheckpoint};

/// Session activity stream + checkpoint cursors.
///
/// # Errors
/// Every method returns [`MemoryError::Storage`](me_types::error::MemoryError::Storage)
/// on a backend failure (or [`NotFound`](me_types::error::MemoryError::NotFound) for a
/// missing id).
#[async_trait]
pub trait SessionStore: Send + Sync {
    // --- activities ---
    async fn insert_or_dedup_activity(
        &self,
        activity: &NewActivity,
        dedup_window_secs: i64,
    ) -> Result<(i64, bool)>;
    async fn get_activity(&self, id: i64) -> Result<Activity>;
    async fn list_activities_by_session(
        &self,
        session_id: &str,
        limit: Option<usize>,
    ) -> Result<Vec<Activity>>;
    /// Recent activities in `scope_ids`, most-recent first. `scope_ids` empty =
    /// **no results** (an empty result set, NOT "all scopes" — unlike most
    /// `FactGraph` `&[i64]` methods; the #632 conformance suite pins this).
    async fn list_recent_activities_by_scope(
        &self,
        scope_ids: &[i64],
        limit: usize,
    ) -> Result<Vec<Activity>>;
    async fn update_activity_status(
        &self,
        id: i64,
        status: ActivityStatus,
        promoted_fact_id: Option<i64>,
    ) -> Result<()>;
    async fn count_activities_by_session(&self, session_id: &str) -> Result<i64>;

    // --- checkpoints ---
    async fn upsert_checkpoint(&self, checkpoint: &SessionCheckpoint) -> Result<()>;
    async fn get_checkpoint(&self, session_id: &str) -> Result<Option<SessionCheckpoint>>;
    async fn get_checkpoint_by_scope(&self, scope_path: &str) -> Result<Option<SessionCheckpoint>>;
    async fn list_recent_checkpoints(&self, limit: usize) -> Result<Vec<SessionCheckpoint>>;
}
