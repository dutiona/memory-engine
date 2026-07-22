//! T8 — the value-level realization proof.
//!
//! `#629`'s `backend.rs` test proved only that the trait family is *vtable-safe*
//! (a `&dyn StorageBackend` reference forms) and deferred constructing the full
//! `~90`-method impl to `#630`. These tests close that: a real `SqliteBackend`
//! coerces to `Arc<dyn StorageBackend>` and dispatches at least one method per
//! bounded trait through the object — exercising async-through-`dyn` for the whole
//! umbrella — and a bounded view (`&dyn FactGraph`) is shown independently usable
//! (the mockability the trait family promised).

use std::sync::Arc;

use chrono::Utc;

use super::SqliteBackend;
use crate::pool::ConnectionPool;
use crate::store::upcaster::UpcasterRegistry;
use me_storage::{FactFilter, FactGraph, StorageBackend};
use me_types::types::{EventType, FactType, NewEvent, NewFact};

const DIM: usize = 4;

fn backend() -> SqliteBackend {
    let pool = ConnectionPool::open_memory(DIM).unwrap();
    SqliteBackend::from_pool(Arc::new(pool), Arc::new(UpcasterRegistry::new()))
}

fn new_fact() -> NewFact {
    NewFact {
        content: "hello world".into(),
        content_hash: String::new(),
        embedding: vec![0.1; DIM],
        fact_type: FactType::Episodic,
        t_created: Utc::now(),
        t_expired: None,
        t_valid: None,
        t_invalid: None,
        source_event_id: None,
        scope_id: 1,
        base_importance: 0.5,
        access_count: 0,
        last_accessed: Utc::now(),
        metadata: serde_json::json!({}),
        is_pinned: false,
    }
}

fn new_event() -> NewEvent {
    NewEvent {
        timestamp: Utc::now(),
        event_type: EventType::Interaction,
        payload: serde_json::json!({ "k": "v" }),
        source: "t8".into(),
        session_id: None,
        scope_id: 1,
        origin_node_id: "local".into(),
        sequence_id: 0,
        created_at: None,
    }
}

#[tokio::test]
async fn arc_dyn_storage_backend_dispatches_every_bounded_trait() {
    let be: Arc<dyn StorageBackend> = Arc::new(backend());

    // SchemaManager — one sync method + one async method through the object.
    let _caps = be.capabilities();
    assert!(be.schema_version().await.unwrap() >= 1);

    // EventLog
    let eid = be.insert_event(&new_event()).await.unwrap();
    assert_eq!(be.get_event(eid).await.unwrap().id, eid);

    // FactGraph
    let fid = be.insert_fact(&new_fact()).await.unwrap();
    assert_eq!(be.get_fact(fid).await.unwrap().id, fid);

    // SearchIndex
    let hits = be
        .lexical_search("hello", &FactFilter::default(), 5)
        .await
        .unwrap();
    assert!(hits.len() <= 5);

    // ConsolidationStore
    assert!(be.list_all_summaries().await.unwrap().is_empty());

    // SessionStore
    assert!(be.list_recent_checkpoints(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn bounded_trait_is_independently_usable_as_dyn() {
    // The #629 mockability promise: a single bounded trait is usable in isolation
    // through a trait object, now against a real impl.
    let be = backend();
    let fg: &dyn FactGraph = &be;
    let fid = fg.insert_fact(&new_fact()).await.unwrap();
    assert_eq!(fg.get_fact(fid).await.unwrap().id, fid);
}
