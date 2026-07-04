//! `impl SessionStore for SqliteBackend` — delegates to [`ActivityStore`] and
//! [`CheckpointStore`], the concrete SQL owners of the `activities` and
//! `session_checkpoints` tables.
//!
//! **Conn selection rule:**
//! - `insert_*` / `upsert_*` / `update_*` → [`super::SqliteBackend::block_write`].
//! - `get_*` / `list_*` / `count_*` → [`super::SqliteBackend::block_read`].
//!
//! **Empty-scope quirk (preserved verbatim):** `list_recent_activities_by_scope`
//! with an empty `scope_ids` slice returns an **empty** `Vec` — it does NOT mean
//! "all scopes". This is explicitly pinned by the #632 conformance suite and
//! matches the concrete [`ActivityStore::list_recent_by_scope`] behaviour.
//!
//! **`#[cfg(test)]` removal:** `ActivityStore::{get,list_by_session,count_by_session}`
//! and `CheckpointStore::{get,list_recent}` were previously test-only; the gate
//! was removed in the same commit so the trait can call them unconditionally (#630).

use async_trait::async_trait;

use super::SqliteBackend;
use me_types::error::Result;
use me_storage::session::SessionStore;
use crate::store::activities::ActivityStore;
use crate::store::checkpoints::CheckpointStore;
use me_types::types::{Activity, ActivityStatus, NewActivity, SessionCheckpoint};

#[async_trait]
impl SessionStore for SqliteBackend {
    // -------------------------------------------------------------------------
    // activities
    // -------------------------------------------------------------------------

    // WRITE
    async fn insert_or_dedup_activity(
        &self,
        activity: &NewActivity,
        dedup_window_secs: i64,
    ) -> Result<(i64, bool)> {
        let activity = activity.clone();
        self.block_write(move |c| {
            ActivityStore::new(c).insert_or_dedup(&activity, dedup_window_secs)
        })
        .await
    }

    // READ
    async fn get_activity(&self, id: i64) -> Result<Activity> {
        self.block_read(move |c| ActivityStore::new(c).get(id))
            .await
    }

    // READ
    async fn list_activities_by_session(
        &self,
        session_id: &str,
        limit: Option<usize>,
    ) -> Result<Vec<Activity>> {
        let session_id = session_id.to_owned();
        self.block_read(move |c| ActivityStore::new(c).list_by_session(&session_id, limit))
            .await
    }

    // READ — empty scope_ids ⇒ empty result (NOT "all scopes"); preserved verbatim.
    async fn list_recent_activities_by_scope(
        &self,
        scope_ids: &[i64],
        limit: usize,
    ) -> Result<Vec<Activity>> {
        let scope_ids = scope_ids.to_vec();
        self.block_read(move |c| ActivityStore::new(c).list_recent_by_scope(&scope_ids, limit))
            .await
    }

    // WRITE
    async fn update_activity_status(
        &self,
        id: i64,
        status: ActivityStatus,
        promoted_fact_id: Option<i64>,
    ) -> Result<()> {
        self.block_write(move |c| ActivityStore::new(c).update_status(id, status, promoted_fact_id))
            .await
    }

    // READ
    async fn count_activities_by_session(&self, session_id: &str) -> Result<i64> {
        let session_id = session_id.to_owned();
        self.block_read(move |c| ActivityStore::new(c).count_by_session(&session_id))
            .await
    }

    // -------------------------------------------------------------------------
    // checkpoints
    // -------------------------------------------------------------------------

    // WRITE
    async fn upsert_checkpoint(&self, checkpoint: &SessionCheckpoint) -> Result<()> {
        let checkpoint = checkpoint.clone();
        self.block_write(move |c| CheckpointStore::new(c).upsert(&checkpoint))
            .await
    }

    // READ
    async fn get_checkpoint(&self, session_id: &str) -> Result<Option<SessionCheckpoint>> {
        let session_id = session_id.to_owned();
        self.block_read(move |c| CheckpointStore::new(c).get(&session_id))
            .await
    }

    // READ
    async fn get_checkpoint_by_scope(&self, scope_path: &str) -> Result<Option<SessionCheckpoint>> {
        let scope_path = scope_path.to_owned();
        self.block_read(move |c| CheckpointStore::new(c).get_by_scope(&scope_path))
            .await
    }

    // READ
    async fn list_recent_checkpoints(&self, limit: usize) -> Result<Vec<SessionCheckpoint>> {
        self.block_read(move |c| CheckpointStore::new(c).list_recent(limit))
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;

    use super::super::SqliteBackend;
    use crate::pool::ConnectionPool;
    use me_storage::session::SessionStore;
    use crate::store::upcaster::UpcasterRegistry;
    use me_types::types::{ActivityStatus, NewActivity, OutcomeClass, SessionCheckpoint};

    const DIM: usize = 4;

    fn backend() -> SqliteBackend {
        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());
        SqliteBackend::from_pool(pool, Arc::new(UpcasterRegistry::new()))
    }

    fn make_activity(session_id: &str, tool: &str) -> NewActivity {
        NewActivity {
            session_id: session_id.into(),
            tool_name: tool.into(),
            args_hash: format!("{tool:0<32}"),
            args: serde_json::json!({}),
            result_summary: None,
            outcome_class: OutcomeClass::Success,
            timestamp: Utc::now(),
            scope_id: 1,
        }
    }

    // -------------------------------------------------------------------------
    // activities
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn activity_insert_then_get_parity() {
        let be = backend();
        let a = make_activity("sess-1", "Read");
        let (id, deduped) = be.insert_or_dedup_activity(&a, 300).await.unwrap();
        assert!(!deduped);
        assert!(id > 0);

        let got = be.get_activity(id).await.unwrap();
        assert_eq!(got.tool_name, "Read");
        assert_eq!(got.occurrence_count, 1);
        assert_eq!(got.status, ActivityStatus::Recorded);
    }

    #[tokio::test]
    async fn activity_dedup_within_window() {
        let be = backend();
        let a = make_activity("sess-1", "Bash");
        let (id1, d1) = be.insert_or_dedup_activity(&a, 300).await.unwrap();
        assert!(!d1);
        let (id2, d2) = be.insert_or_dedup_activity(&a, 300).await.unwrap();
        assert!(d2);
        assert_eq!(id1, id2);

        let got = be.get_activity(id1).await.unwrap();
        assert_eq!(got.occurrence_count, 2);
        assert_eq!(got.status, ActivityStatus::Deduplicated);
    }

    #[tokio::test]
    async fn list_activities_by_session_limit() {
        let be = backend();
        for i in 0..5 {
            be.insert_or_dedup_activity(&make_activity("sess-list", &format!("Tool{i}")), 300)
                .await
                .unwrap();
        }
        let all = be
            .list_activities_by_session("sess-list", None)
            .await
            .unwrap();
        assert_eq!(all.len(), 5);

        let limited = be
            .list_activities_by_session("sess-list", Some(3))
            .await
            .unwrap();
        assert_eq!(limited.len(), 3);
    }

    #[tokio::test]
    async fn count_activities_by_session() {
        let be = backend();
        for i in 0..4 {
            be.insert_or_dedup_activity(&make_activity("sess-count", &format!("T{i}")), 300)
                .await
                .unwrap();
        }
        let count = be.count_activities_by_session("sess-count").await.unwrap();
        assert_eq!(count, 4);
        let zero = be.count_activities_by_session("other").await.unwrap();
        assert_eq!(zero, 0);
    }

    #[tokio::test]
    async fn list_recent_by_scope_empty_slice_returns_empty() {
        // Pinned contract: empty scope_ids ⇒ NO results (not "all scopes").
        let be = backend();
        be.insert_or_dedup_activity(&make_activity("sess-scope", "Read"), 300)
            .await
            .unwrap();

        let empty = be.list_recent_activities_by_scope(&[], 10).await.unwrap();
        assert!(
            empty.is_empty(),
            "empty scope_ids must return empty, not all activities"
        );

        let found = be.list_recent_activities_by_scope(&[1], 10).await.unwrap();
        assert_eq!(found.len(), 1);
    }

    #[tokio::test]
    async fn update_activity_status_round_trip() {
        let be = backend();
        let (id, _) = be
            .insert_or_dedup_activity(&make_activity("sess-upd", "Bash"), 300)
            .await
            .unwrap();
        be.update_activity_status(id, ActivityStatus::Promoted, None)
            .await
            .unwrap();
        let got = be.get_activity(id).await.unwrap();
        assert_eq!(got.status, ActivityStatus::Promoted);
    }

    // -------------------------------------------------------------------------
    // checkpoints
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn checkpoint_upsert_get_by_scope() {
        let be = backend();
        let cp = SessionCheckpoint {
            session_id: "sess-cp".into(),
            scope_path: Some("project:memory-engine".into()),
            summary: Some("initial summary".into()),
            last_activity_id: None,
            checkpoint_at: Utc::now(),
            metadata: serde_json::json!({"count": 1}),
        };
        be.upsert_checkpoint(&cp).await.unwrap();

        let got = be.get_checkpoint("sess-cp").await.unwrap().unwrap();
        assert_eq!(got.session_id, "sess-cp");
        assert_eq!(got.summary, Some("initial summary".into()));

        let by_scope = be
            .get_checkpoint_by_scope("project:memory-engine")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(by_scope.session_id, "sess-cp");

        let none = be.get_checkpoint("nonexistent").await.unwrap();
        assert!(none.is_none());
    }

    #[tokio::test]
    async fn checkpoint_upsert_overwrites() {
        let be = backend();
        let cp1 = SessionCheckpoint {
            session_id: "sess-overwrite".into(),
            scope_path: None,
            summary: Some("first".into()),
            last_activity_id: None,
            checkpoint_at: Utc::now(),
            metadata: serde_json::json!({}),
        };
        be.upsert_checkpoint(&cp1).await.unwrap();
        let cp2 = SessionCheckpoint {
            summary: Some("second".into()),
            ..cp1
        };
        be.upsert_checkpoint(&cp2).await.unwrap();

        let got = be.get_checkpoint("sess-overwrite").await.unwrap().unwrap();
        assert_eq!(got.summary, Some("second".into()));
    }

    #[tokio::test]
    async fn list_recent_checkpoints() {
        let be = backend();
        for i in 0..5 {
            be.upsert_checkpoint(&SessionCheckpoint {
                session_id: format!("sess-{i}"),
                scope_path: None,
                summary: None,
                last_activity_id: None,
                checkpoint_at: Utc::now(),
                metadata: serde_json::json!({}),
            })
            .await
            .unwrap();
        }
        let list = be.list_recent_checkpoints(3).await.unwrap();
        assert_eq!(list.len(), 3);
    }
}
