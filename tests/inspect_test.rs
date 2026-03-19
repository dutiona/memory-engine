//! Integration tests for Phase 4a Inspection APIs.
//!
//! Tests exercise all 5 inspection methods through the public `MemoryEngine` API.
//! No direct store access — edges are generated via `consolidate()` or `resolve_conflict()`.

use chrono::{Duration, Utc};
use memory_engine::engine::MemoryEngine;
use memory_engine::error::Result;
use memory_engine::inspect::types::*;
use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::{AddFactOptions, FactType};

const DIM: usize = 4;

struct TestEmbed;
impl EmbeddingProvider for TestEmbed {
    fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![0.25; DIM])
    }
}

/// Full lifecycle: add facts with various states, then exercise all inspection APIs.
#[test]
fn inspection_lifecycle() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();

    // --- Setup: create facts in different states ---

    // Active fact
    let active_id = engine
        .add_fact(
            "active fact",
            FactType::Semantic,
            None,
            &TestEmbed,
            None,
            None,
            None,
        )
        .unwrap();

    // Pinned fact
    let pin_opts = AddFactOptions {
        pinned: Some(true),
        ..Default::default()
    };
    let pinned_id = engine
        .add_fact(
            "pinned fact",
            FactType::Semantic,
            None,
            &TestEmbed,
            None,
            Some(&pin_opts),
            None,
        )
        .unwrap();

    // Due fact (t_valid in the past)
    let due_opts = AddFactOptions {
        t_valid: Some(Utc::now() - Duration::hours(1)),
        ..Default::default()
    };
    let due_id = engine
        .add_fact(
            "due fact",
            FactType::Episodic,
            None,
            &TestEmbed,
            None,
            Some(&due_opts),
            None,
        )
        .unwrap();

    // --- statistics() ---
    let stats = engine.statistics().unwrap();
    assert_eq!(stats.facts.total, 3);
    assert_eq!(stats.facts.active, 3);
    assert_eq!(stats.facts.pinned, 1);
    assert_eq!(stats.facts.due, 1); // only the due fact
    assert!(stats.scopes.total >= 1); // at least root
    assert!(stats.storage.main_db_bytes > 0);

    // --- explain_fact() ---
    let active_exp = engine.explain_fact(active_id).unwrap();
    assert!(matches!(active_exp.state, FactState::Active));
    assert_eq!(active_exp.scope_path, "/");

    let pinned_exp = engine.explain_fact(pinned_id).unwrap();
    assert!(matches!(pinned_exp.state, FactState::Pinned));
    assert!(pinned_exp.provenance.is_pinned);

    let due_exp = engine.explain_fact(due_id).unwrap();
    assert!(matches!(due_exp.state, FactState::Due { .. }));

    // --- fact_history() ---
    let due_hist = engine.fact_history(due_id).unwrap();
    assert_eq!(due_hist.fact_id, due_id);
    // Due fact has t_created + t_valid = 2 entries
    assert_eq!(due_hist.timeline.len(), 2);
    // First entry should be BecameValid (t_valid is 1 hour ago, before t_created=now)
    assert!(matches!(
        due_hist.timeline[0].kind,
        HistoryEventKind::BecameValid
    ));
    assert!(matches!(
        due_hist.timeline[1].kind,
        HistoryEventKind::Created
    ));

    let active_hist = engine.fact_history(active_id).unwrap();
    assert_eq!(active_hist.timeline.len(), 1);
    assert!(matches!(
        active_hist.timeline[0].kind,
        HistoryEventKind::Created
    ));

    // --- replay_events() ---
    // No events ingested via ingest() — replay should return empty with default filter
    let events = engine.replay_events(&ReplayFilter::default()).unwrap();
    // Events from add_fact are not logged (add_fact doesn't call ingest), so 0
    assert_eq!(events.len(), 0);

    // --- dump_state() ---
    let dir = tempfile::tempdir().unwrap();
    let json_path = dir.path().join("snapshot.json");
    engine
        .dump_state(&DumpFormat::Json(json_path.clone()))
        .unwrap();

    // Verify the dump is valid JSON containing our facts
    let content = std::fs::read_to_string(&json_path).unwrap();
    let snapshot: EngineSnapshot = serde_json::from_str(&content).unwrap();
    assert_eq!(snapshot.facts.len(), 3);
    assert_eq!(snapshot.embed_dim, DIM);
    assert!(snapshot.schema_version > 0);

    // SQLite dump should fail for in-memory engine
    let sqlite_path = dir.path().join("snapshot.db");
    assert!(engine.dump_state(&DumpFormat::Sqlite(sqlite_path)).is_err());
}

/// Test explain_fact for a not-found ID returns proper error.
#[test]
fn explain_fact_not_found() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let err = engine.explain_fact(999);
    assert!(err.is_err());
}

/// Test fact_history for a not-found ID returns proper error.
#[test]
fn fact_history_not_found() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let err = engine.fact_history(999);
    assert!(err.is_err());
}
