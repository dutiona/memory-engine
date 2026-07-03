//! Shared test-only factory helpers for `me-types` DTOs (Wave 2 #816).
//!
//! Relocated from the monolith's `test_utils.rs` — the `NewFact`/`NewEvent`
//! factory functions build only `me-types` DTOs, so they live here behind the
//! `test-util` feature and are re-exported by every consumer that needs them
//! (the facade's own `#[cfg(test)]` code, `me-backend-sqlite`'s store tests).
use chrono::Utc;

use crate::types::{EventType, FactType, NewEvent, NewFact};

// --- NewFact helpers ---

/// Build a [`NewFact`] with `FactType::Episodic` and an empty `content_hash`.
///
/// This is the dominant shape used by store-level and search tests where the
/// content hash is not meaningful (the store computes or ignores it).
#[must_use]
pub fn new_fact(content: &str, embedding: Vec<f32>) -> NewFact {
    new_fact_with_type(content, embedding, FactType::Episodic)
}

/// Build a [`NewFact`] with `FactType::Semantic` and a blake3 content hash.
///
/// Used by engine-level tests that go through dedup/conflict paths where the
/// hash is significant (e.g. `engine/tests.rs`, `engine/mod.rs` resolve tests).
#[must_use]
pub fn new_fact_hashed(content: &str, embedding: Vec<f32>) -> NewFact {
    NewFact::builder(content, embedding, FactType::Semantic)
        .content_hash(blake3::hash(content.as_bytes()).to_hex().as_str()[..32].to_string())
        .build()
}

/// Build a [`NewFact`] with an explicit [`FactType`] and an empty `content_hash`.
///
/// Use this when the test needs to vary the fact type (e.g. `hybrid.rs`
/// multi-type filter tests).
#[must_use]
pub fn new_fact_with_type(content: &str, embedding: Vec<f32>, fact_type: FactType) -> NewFact {
    NewFact {
        content: content.into(),
        content_hash: String::new(),
        embedding,
        fact_type,
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

// --- NewEvent helpers ---

/// Build a [`NewEvent`] with `EventType::Interaction`.
///
/// Covers `store/events.rs` and `storage/sqlite/event_log.rs` which both had
/// identical two-argument `make_event(source, session_id)` helpers.
#[must_use]
pub fn new_event(source: &str, session_id: Option<&str>) -> NewEvent {
    NewEvent {
        timestamp: Utc::now(),
        event_type: EventType::Interaction,
        payload: serde_json::json!({"key": "value"}),
        source: source.into(),
        session_id: session_id.map(Into::into),
        scope_id: 1,
        origin_node_id: "local".into(),
        sequence_id: 0,
        created_at: None,
    }
}
