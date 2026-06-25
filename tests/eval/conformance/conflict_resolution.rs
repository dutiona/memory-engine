//! B5: Conflict resolution conformance tests.
//!
//! Verifies all four `CrudDecision` variants (`Add`, `Update`, `Delete`, `Noop`)
//! per `conflict/temporal.rs` semantics, bi-temporal expiry fields, and
//! the evidence provenance round-trip via `ingest` → `add_fact` → `explain_fact`.

use chrono::Utc;

use memory_engine::traits::{CrudDecision, EmbeddingProvider};
use memory_engine::types::{AddFactRequest, EventType, FactType, NewEvent, NewFact};

use crate::helpers::{FixedArbiter, TestEmbedder, eval_engine};

/// Build a `NewFact` suitable for `resolve_conflict`, using the `TestEmbedder`
/// for the embedding vector.
fn make_new_fact(content: &str) -> NewFact {
    let now = Utc::now();
    let embedding = TestEmbedder.embed(content).expect("embed failed");
    let hash = blake3::hash(content.as_bytes());
    NewFact {
        content: content.to_string(),
        content_hash: hash.to_hex().as_str()[..32].to_string(),
        embedding,
        fact_type: FactType::Semantic,
        t_created: now,
        t_expired: None,
        t_valid: None,
        t_invalid: None,
        source_event_id: None,
        scope_id: 1,
        base_importance: 0.5,
        access_count: 0,
        last_accessed: now,
        metadata: serde_json::json!({}),
        is_pinned: false,
    }
}

// ---------------------------------------------------------------------------
// CrudDecision::Add — keeps both, creates "supplements" edge
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conflict_add_keeps_both_facts_active() {
    let engine = eval_engine();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> = std::sync::Arc::new(TestEmbedder);

    let old_id = engine
        .add_fact(
            &AddFactRequest {
                content: "server runs on port 8080".to_string(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            embedder.clone(),
            None,
        )
        .await
        .expect("add old fact");

    let arbiter = FixedArbiter {
        decision: CrudDecision::Add,
    };
    let new_fact = make_new_fact("server also listens on port 8443");

    let resolution = engine
        .resolve_conflict(&arbiter, old_id, &new_fact)
        .await
        .expect("resolve_conflict failed");

    assert_eq!(resolution.decision, CrudDecision::Add);
    assert!(
        resolution.new_fact_id.is_some(),
        "Add should insert new fact"
    );

    let new_id = resolution.new_fact_id.unwrap();

    // Both facts should be active (no t_expired).
    let old = engine.get_fact(old_id).await.expect("get old");
    let new = engine.get_fact(new_id).await.expect("get new");
    assert!(old.t_expired.is_none(), "old fact should remain active");
    assert!(new.t_expired.is_none(), "new fact should be active");
}

// ---------------------------------------------------------------------------
// CrudDecision::Update — expires+invalidates old, inserts new, "contradicts" edge
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conflict_update_expires_and_invalidates_old() {
    let engine = eval_engine();
    let embedder = TestEmbedder;

    let old_id = engine
        .add_fact(
            &AddFactRequest {
                content: "database uses PostgreSQL 14".to_string(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            std::sync::Arc::new(embedder) as std::sync::Arc<dyn EmbeddingProvider>,
            None,
        )
        .await
        .expect("add old fact");

    let arbiter = FixedArbiter {
        decision: CrudDecision::Update,
    };
    let new_fact = make_new_fact("database upgraded to PostgreSQL 16");

    let resolution = engine
        .resolve_conflict(&arbiter, old_id, &new_fact)
        .await
        .expect("resolve_conflict");

    assert_eq!(resolution.decision, CrudDecision::Update);
    let new_id = resolution
        .new_fact_id
        .expect("Update should produce new_fact_id");

    // Old fact: t_expired AND t_invalid should both be set (bi-temporal).
    let old = engine.get_fact(old_id).await.expect("get old");
    assert!(old.t_expired.is_some(), "old fact must have t_expired set");
    assert!(
        old.t_invalid.is_some(),
        "old fact must have t_invalid set (bi-temporal invalidation)"
    );

    // New fact should be active.
    let new = engine.get_fact(new_id).await.expect("get new");
    assert!(new.t_expired.is_none(), "new fact should be active");
}

// ---------------------------------------------------------------------------
// CrudDecision::Delete — expires+invalidates old, does NOT insert new
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conflict_delete_expires_old_no_new_fact() {
    let engine = eval_engine();
    let embedder = TestEmbedder;

    let old_id = engine
        .add_fact(
            &AddFactRequest {
                content: "legacy endpoint /api/v1 is available".to_string(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            std::sync::Arc::new(embedder) as std::sync::Arc<dyn EmbeddingProvider>,
            None,
        )
        .await
        .expect("add old fact");

    let arbiter = FixedArbiter {
        decision: CrudDecision::Delete,
    };
    let new_fact = make_new_fact("endpoint /api/v1 has been removed");

    let resolution = engine
        .resolve_conflict(&arbiter, old_id, &new_fact)
        .await
        .expect("resolve_conflict");

    assert_eq!(resolution.decision, CrudDecision::Delete);
    assert!(
        resolution.new_fact_id.is_none(),
        "Delete should NOT insert a new fact"
    );

    // Old fact: t_expired AND t_invalid should both be set.
    let old = engine.get_fact(old_id).await.expect("get old");
    assert!(old.t_expired.is_some(), "old fact must be expired");
    assert!(old.t_invalid.is_some(), "old fact must be invalidated");
}

// ---------------------------------------------------------------------------
// CrudDecision::Noop — no changes at all
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conflict_noop_changes_nothing() {
    let engine = eval_engine();
    let embedder = TestEmbedder;

    let old_id = engine
        .add_fact(
            &AddFactRequest {
                content: "application version is 3.2.1".to_string(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            std::sync::Arc::new(embedder) as std::sync::Arc<dyn EmbeddingProvider>,
            None,
        )
        .await
        .expect("add old fact");

    let old_before = engine.get_fact(old_id).await.expect("get old before");

    let arbiter = FixedArbiter {
        decision: CrudDecision::Noop,
    };
    let new_fact = make_new_fact("application version is 3.2.1");

    let resolution = engine
        .resolve_conflict(&arbiter, old_id, &new_fact)
        .await
        .expect("resolve_conflict");

    assert_eq!(resolution.decision, CrudDecision::Noop);
    assert!(
        resolution.new_fact_id.is_none(),
        "Noop should not create a new fact"
    );

    let old_after = engine.get_fact(old_id).await.expect("get old after");
    assert!(
        old_after.t_expired.is_none(),
        "Noop should not expire the old fact"
    );
    assert!(
        old_after.t_invalid.is_none(),
        "Noop should not invalidate the old fact"
    );
    assert_eq!(
        old_before.content, old_after.content,
        "Noop should not change the fact"
    );
}

// ---------------------------------------------------------------------------
// Evidence provenance round-trip: ingest → add_fact(source_event_id) → explain_fact
// ---------------------------------------------------------------------------

#[tokio::test]
async fn evidence_provenance_round_trip() {
    let engine = eval_engine();
    let embedder = TestEmbedder;

    // Step 1: Ingest an event.
    let event = NewEvent {
        timestamp: Utc::now(),
        event_type: EventType::Interaction,
        payload: serde_json::json!({"user": "alice", "action": "deploy"}),
        source: "test-harness".to_string(),
        session_id: Some("session-provenance".to_string()),
        scope_id: 1,
        origin_node_id: "node-provenance".to_string(),
        sequence_id: 1,
        created_at: None,
    };
    let event_id = engine.ingest(&event).await.expect("ingest event");

    // Step 2: Add a fact linked to that event.
    let fact_id = engine
        .add_fact(
            &AddFactRequest {
                content: "Alice deployed version 4.0 to production".to_string(),
                fact_type: FactType::Episodic,
                source_event_id: Some(event_id),
                scope: None,
                opts: None,
            },
            std::sync::Arc::new(embedder) as std::sync::Arc<dyn EmbeddingProvider>,
            None,
        )
        .await
        .expect("add fact with source_event_id");

    // Step 3: Verify the fact carries the source_event_id.
    let fact = engine.get_fact(fact_id).await.expect("get fact");
    assert_eq!(
        fact.source_event_id,
        Some(event_id),
        "fact should reference the originating event"
    );

    // Step 4: explain_fact should trace provenance back to the event.
    let explanation = engine.explain_fact(fact_id).await.expect("explain_fact");
    assert_eq!(
        explanation.provenance.source_event_id,
        Some(event_id),
        "explanation should reference the source event"
    );
    assert!(
        explanation.provenance.source_event.is_some(),
        "explanation should include the resolved source event"
    );

    let resolved_event = explanation.provenance.source_event.unwrap();
    assert_eq!(resolved_event.id, event_id);
    assert_eq!(resolved_event.event_type, EventType::Interaction);
}
