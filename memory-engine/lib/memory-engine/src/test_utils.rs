/// Shared test utilities for `memory-engine` unit tests.
///
/// This module is only compiled in `#[cfg(test)]` mode. It provides common
/// test doubles and factory helpers that would otherwise be duplicated across
/// every `mod tests { … }` block.
///
/// # FK-pragma guarantee
///
/// [`setup_memory_db`] opens the connection through [`crate::store::schema::open_memory`],
/// which applies all pragmas including `PRAGMA foreign_keys = ON`. Raw
/// `Connection::open_in_memory()` skips this pragma and silently masks FK
/// violations; every store-level test setup must use this helper instead (#485).
use chrono::Utc;
use rusqlite::Connection;

use crate::error::Result;
use crate::store::schema::{init_schema, open_memory};
use crate::traits::EmbeddingProvider;
use crate::types::{EmbeddingFingerprint, EventType, FactType, NewEvent, NewFact};

// --- DB helpers ---

/// Open an in-memory `SQLite` connection with all pragmas (including
/// `foreign_keys = ON`) and the latest schema applied.
///
/// Use this in every store-level `fn setup() -> Connection` instead of
/// `Connection::open_in_memory()`, which skips the FK pragma (#485).
pub fn setup_memory_db() -> Connection {
    let conn = open_memory().expect("open in-memory db");
    init_schema(&conn).expect("init schema");
    conn
}

// --- EmbeddingProvider test doubles ---

/// Generic test double for [`EmbeddingProvider`] that produces a deterministic,
/// constant output vector of a given dimension.
///
/// Use [`MockEmbedder::new`] for the dominant pattern (`vec![0.5; dim]`) or
/// [`MockEmbedder::constant`] to choose a specific fill value. For tests that
/// need a fixed 4-element gradient vector `[0.1, 0.2, 0.3, 0.4]`, use
/// [`MockEmbedder::fixed4`].
///
/// **Not a replacement for purpose-built doubles.** If a test needs
/// dimension-mismatching, call-counting, error injection, or text-dependent
/// output, keep the local struct — those properties encode intent.
pub struct MockEmbedder {
    dim: usize,
    value: f32,
    /// When `Some`, overrides `value` and returns this exact vector.
    fixed: Option<Vec<f32>>,
}

impl MockEmbedder {
    /// Constant-vector embedder that returns `vec![0.5; dim]`.
    ///
    /// This is the most common pattern across store and consolidation tests.
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            value: 0.5,
            fixed: None,
        }
    }

    /// Constant-vector embedder that returns `vec![value; dim]`.
    pub fn constant(dim: usize, value: f32) -> Self {
        Self {
            dim,
            value,
            fixed: None,
        }
    }

    /// Fixed 4-element gradient vector `[0.1, 0.2, 0.3, 0.4]` with dim=4.
    ///
    /// Covers the family of `FakeEmbed` / `FixedEmbedder` / `FixedEmbed`
    /// structs that all produce this exact vector (inspect, ingest, lineage,
    /// apply, cognitive tests).
    pub fn fixed4() -> Self {
        Self {
            dim: 4,
            value: 0.0,
            fixed: Some(vec![0.1, 0.2, 0.3, 0.4]),
        }
    }
}

impl EmbeddingProvider for MockEmbedder {
    fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        self.fixed
            .as_ref()
            .map_or_else(|| Ok(vec![self.value; self.dim]), |v| Ok(v.clone()))
    }

    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint::new("mock", "test", self.dim)
    }
}

// --- NewFact helpers ---

/// Build a [`NewFact`] with `FactType::Episodic` and an empty `content_hash`.
///
/// This is the dominant shape used by store-level and search tests where the
/// content hash is not meaningful (the store computes or ignores it).
pub fn new_fact(content: &str, embedding: Vec<f32>) -> NewFact {
    new_fact_with_type(content, embedding, FactType::Episodic)
}

/// Build a [`NewFact`] with `FactType::Semantic` and a blake3 content hash.
///
/// Used by engine-level tests that go through dedup/conflict paths where the
/// hash is significant (e.g. `engine/tests.rs`, `engine/mod.rs` resolve tests).
pub fn new_fact_hashed(content: &str, embedding: Vec<f32>) -> NewFact {
    NewFact::builder(content, embedding, FactType::Semantic)
        .content_hash(blake3::hash(content.as_bytes()).to_hex().as_str()[..32].to_string())
        .build()
}

/// Build a [`NewFact`] with an explicit [`FactType`] and an empty `content_hash`.
///
/// Use this when the test needs to vary the fact type (e.g. `hybrid.rs`
/// multi-type filter tests).
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
