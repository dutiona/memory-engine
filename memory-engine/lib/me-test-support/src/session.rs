//! `SessionStore` contract bodies.

use me_types::types::ActivityStatus;

use super::factory::ConformanceBackend;
use super::fixtures::{checkpoint, new_activity, new_fact, seed_facts};

/// Activity insert → get / count / list, and a second identical insert within the
/// dedup window deduplicates (same id, count unchanged).
pub async fn activity_insert_dedup_get_list_count<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let (id, deduped) = be
        .insert_or_dedup_activity(&new_activity("sess", "build"), 3600)
        .await
        .expect("insert activity");
    assert!(!deduped, "[{}] first insert must not be a dedup", f.name());
    let got = be.get_activity(id).await.expect("get_activity");
    assert_eq!(got.session_id, "sess", "[{}] activity session", f.name());
    assert_eq!(got.tool_name, "build", "[{}] activity tool", f.name());

    let (id2, deduped2) = be
        .insert_or_dedup_activity(&new_activity("sess", "build"), 3600)
        .await
        .expect("dedup");
    assert!(
        deduped2 && id2 == id,
        "[{}] an identical activity within the window must dedup to the same id",
        f.name()
    );
    assert_eq!(
        be.count_activities_by_session("sess").await.expect("count"),
        1,
        "[{}] dedup must keep the session count at 1",
        f.name()
    );
    assert!(
        be.list_activities_by_session("sess", None)
            .await
            .expect("list")
            .iter()
            .any(|a| a.id == id),
        "[{}] activity must list by session",
        f.name()
    );
}

/// `list_recent_activities_by_scope` empty `&[]` = **NONE** (NOT all) — the
/// `SessionStore` empty-slice contract (session.rs:36-43). A `&[scope]` control
/// proves the row is otherwise present.
pub async fn list_recent_activities_by_scope_empty_means_none<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    be.insert_or_dedup_activity(&new_activity("sess", "build"), 3600)
        .await
        .expect("insert");
    let in_scope = be
        .list_recent_activities_by_scope(&[1], 100)
        .await
        .expect("in scope");
    assert!(
        !in_scope.is_empty(),
        "[{}] &[scope] control must be non-empty",
        f.name()
    );
    let empty = be
        .list_recent_activities_by_scope(&[], 100)
        .await
        .expect("empty scope");
    assert!(
        empty.is_empty(),
        "[{}] list_recent_activities_by_scope empty scope_ids must mean NONE",
        f.name()
    );
}

/// `update_activity_status` updates the status and the promoted-fact link.
pub async fn update_activity_status<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    // A real promoted fact (the link is an FK-enforced reference).
    let fact_id = seed_facts(&be, &[new_fact("promoted")]).await[0];
    let (id, _) = be
        .insert_or_dedup_activity(&new_activity("sess", "build"), 3600)
        .await
        .expect("insert");
    be.update_activity_status(id, ActivityStatus::Promoted, Some(fact_id))
        .await
        .expect("update status");
    let got = be.get_activity(id).await.expect("get");
    assert_eq!(
        got.status,
        ActivityStatus::Promoted,
        "[{}] status must update",
        f.name()
    );
    assert_eq!(
        got.promoted_fact_id,
        Some(fact_id),
        "[{}] promoted_fact_id must update",
        f.name()
    );
}

/// Checkpoint upsert, read back via ALL THREE documented paths (`get_checkpoint`,
/// `get_checkpoint_by_scope`, `list_recent_checkpoints`), and overwrite is
/// last-write-wins (session.rs:5-9 names these read-backs as #632-needed).
pub async fn checkpoint_upsert_get_by_session_scope_recent<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    be.upsert_checkpoint(&checkpoint("sess-ck"))
        .await
        .expect("upsert");

    let by_session = be.get_checkpoint("sess-ck").await.expect("get by session");
    assert_eq!(
        by_session.map(|c| c.session_id),
        Some("sess-ck".to_owned()),
        "[{}] get_checkpoint by session",
        f.name()
    );
    let by_scope = be
        .get_checkpoint_by_scope("conformance")
        .await
        .expect("get by scope");
    assert_eq!(
        by_scope.map(|c| c.session_id),
        Some("sess-ck".to_owned()),
        "[{}] get_checkpoint_by_scope",
        f.name()
    );
    assert!(
        be.list_recent_checkpoints(10)
            .await
            .expect("recent")
            .iter()
            .any(|c| c.session_id == "sess-ck"),
        "[{}] list_recent_checkpoints",
        f.name()
    );

    // Overwrite (last-write-wins).
    let mut updated = checkpoint("sess-ck");
    updated.summary = Some("updated summary".into());
    be.upsert_checkpoint(&updated)
        .await
        .expect("upsert overwrite");
    let after = be
        .get_checkpoint("sess-ck")
        .await
        .expect("get after")
        .expect("checkpoint present");
    assert_eq!(
        after.summary,
        Some("updated summary".to_owned()),
        "[{}] checkpoint overwrite must be last-write-wins",
        f.name()
    );
    assert_eq!(
        be.list_recent_checkpoints(10)
            .await
            .expect("recent2")
            .iter()
            .filter(|c| c.session_id == "sess-ck")
            .count(),
        1,
        "[{}] overwrite must keep a single checkpoint for the session",
        f.name()
    );
}
