//! Port-only fixtures: `New*` builders + seeding + the full-state `StoreSnapshot`.
//!
//! NOTHING SQLite-private appears here — every seed goes through `FactGraph` /
//! `EventLog` / … trait methods, the backend-agnostic replacement for the oracle
//! tests' `FactStore` + `pool.write()` loop. This is the keystone that lets a
//! behavior body read identically across backends.

use std::sync::Arc;

use chrono::Utc;

use crate::storage::StorageBackend;
use crate::types::{
    ConsolidationLevel, EmbeddingFingerprint, EventFilter, EventType, FactType,
    LineageSnapshotEntry, NewActivity, NewEvent, NewFact, NewSummary, SessionCheckpoint,
};

/// The embedding dimension every conformance backend uses (matches the `SQLite` oracle).
pub const DIM: usize = 4;

/// A minimal active fact via the public builder (scope = root).
pub fn new_fact(content: &str) -> NewFact {
    NewFact::builder(content, vec![0.1_f32; DIM], FactType::Episodic)
        .scope_id(1)
        .build()
}

/// The conformance embedding identity.
pub fn fingerprint() -> EmbeddingFingerprint {
    EmbeddingFingerprint::new("conformance-model", "test", DIM)
}

/// A minimal interaction event tagged with `session_id` (root scope).
pub fn new_event(session_id: &str) -> NewEvent {
    NewEvent {
        timestamp: Utc::now(),
        event_type: EventType::Interaction,
        payload: serde_json::json!({}),
        source: "conformance".into(),
        session_id: Some(session_id.to_owned()),
        scope_id: 1,
        origin_node_id: "conformance-node".into(),
        sequence_id: 0,
        created_at: None,
    }
}

/// A cluster-level summary (root scope).
pub fn new_summary(content: &str) -> NewSummary {
    NewSummary {
        content: content.into(),
        embedding: vec![0.1_f32; DIM],
        level: ConsolidationLevel::Cluster,
        source_fact_ids: Vec::new(),
        created_at: Utc::now(),
        scope_id: 1,
    }
}

/// A session activity (root scope).
pub fn new_activity(session_id: &str, tool: &str) -> NewActivity {
    NewActivity {
        session_id: session_id.to_owned(),
        tool_name: tool.to_owned(),
        args_hash: format!("{tool}-hash"),
        args: serde_json::json!({}),
        result_summary: None,
        outcome_class: "success".into(),
        timestamp: Utc::now(),
        scope_id: 1,
    }
}

/// A session checkpoint (root scope path).
pub fn checkpoint(session_id: &str) -> SessionCheckpoint {
    SessionCheckpoint {
        session_id: session_id.to_owned(),
        scope_path: Some("conformance".into()),
        summary: Some("checkpoint summary".into()),
        last_activity_id: None,
        checkpoint_at: Utc::now(),
        metadata: serde_json::json!({}),
    }
}

/// Seed facts THROUGH THE PORT, returning their ids.
///
/// Establishes the embedding identity first (idempotent) so vector ops /
/// `require_embedding_fingerprint_present` don't fault on a missing fingerprint —
/// `insert_fact` (unlike `insert_fact_atomic`) does not record it. The `schema`
/// fingerprint-contract bodies deliberately do NOT use this helper: they need a
/// fresh `make()` with no stamped identity to assert the absent→record→stored path.
pub async fn seed_facts(be: &Arc<dyn StorageBackend>, facts: &[NewFact]) -> Vec<i64> {
    be.record_embedding_fingerprint_if_absent(&fingerprint(), DIM)
        .await
        .expect("establish embedding identity");
    let mut ids = Vec::with_capacity(facts.len());
    for f in facts {
        ids.push(be.insert_fact(f).await.expect("seed insert_fact"));
    }
    ids
}

/// A full-state, port-only capture of the store, canonicalized for total `Eq`.
///
/// Each table is serialized row-by-row (every domain type derives `Serialize`, so
/// identical values produce identical JSON — capturing timestamps, embeddings, and
/// flags without hand-rolled bit-casting) and **sorted**, so two captures are equal
/// iff the observable store state is identical across EVERY table an atomic method
/// can touch. This is what makes the rollback assertion catch the F5 partial-commit
/// class (an expired-but-undeleted fact flips `t_expired`) AND a leaked
/// event/lineage/config row (the review BLOCKER).
///
/// Use ONLY for rollback tests that inject via a TYPED fault (wrong-dim embedding,
/// mismatched fingerprint) — those leave every table readable. A `DROP TABLE`
/// injection makes that table unreadable afterward, so those tests read the specific
/// non-dropped tables directly instead.
#[derive(Debug, PartialEq, Eq)]
pub struct StoreSnapshot {
    facts: Vec<String>,
    edges: Vec<String>,
    summaries: Vec<String>,
    scopes: Vec<String>,
    events: Vec<String>,
    lineage: Vec<String>,
    fingerprint: Option<String>,
    config: Vec<(String, Option<String>)>,
}

/// Watermark/cursor keys an atomic method might write — captured so a leaked config
/// write after a rolled-back transaction is caught.
const CONFIG_KEYS: [&str; 3] = [
    "last_dream_cycle_at",
    "last_caller_write_fact_id",
    "dream_cycle_history",
];

fn ser_rows<T: serde::Serialize>(rows: &[T]) -> Vec<String> {
    let mut v: Vec<String> = rows
        .iter()
        .map(|r| serde_json::to_string(r).expect("serialize snapshot row"))
        .collect();
    v.sort();
    v
}

/// Capture the full store state via PORT READS ONLY.
pub async fn snapshot(be: &Arc<dyn StorageBackend>) -> StoreSnapshot {
    let facts = ser_rows(&be.list_all_facts().await.expect("list_all_facts"));
    let edges = ser_rows(&be.list_all_edges().await.expect("list_all_edges"));
    let summaries = ser_rows(&be.list_all_summaries().await.expect("list_all_summaries"));
    let scopes = ser_rows(&be.list_all_scopes().await.expect("list_all_scopes"));
    let events = ser_rows(
        &be.list_events(&EventFilter::default())
            .await
            .expect("list_events"),
    );
    let mut lineage_rows: Vec<LineageSnapshotEntry> = Vec::new();
    be.for_each_lineage(&mut |e| {
        lineage_rows.push(e);
        Ok(())
    })
    .await
    .expect("for_each_lineage");
    let lineage = ser_rows(&lineage_rows);
    let fingerprint = be
        .load_embedding_fingerprint()
        .await
        .expect("load_embedding_fingerprint")
        .map(|fp| serde_json::to_string(&fp).expect("serialize fingerprint"));
    let mut config = Vec::with_capacity(CONFIG_KEYS.len());
    for key in CONFIG_KEYS {
        config.push((
            key.to_string(),
            be.get_config(key).await.expect("get_config"),
        ));
    }
    StoreSnapshot {
        facts,
        edges,
        summaries,
        scopes,
        events,
        lineage,
        fingerprint,
        config,
    }
}
