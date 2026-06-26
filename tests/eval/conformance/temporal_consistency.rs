//! B2: Temporal consistency conformance tests.
//!
//! Verifies bi-temporal invariants: future-dated facts are hidden by default,
//! past-invalidated facts are excluded, period overlap semantics are correct,
//! and event sequence IDs are monotonically increasing.

use chrono::{Duration, Utc};

use memory_engine::MemoryQuery;
use memory_engine::types::{AddFactOptions, EventType, FactType, NewEvent};

use crate::helpers::{add_fact_with_opts, days_ago, eval_engine};

#[tokio::test]
async fn future_t_valid_fact_invisible_at_now() {
    let engine = eval_engine();

    let future = Utc::now() + Duration::days(30);
    let _future_id = add_fact_with_opts(
        &engine,
        "future deployment schedule",
        FactType::Semantic,
        None,
        AddFactOptions {
            t_valid: Some(future),
            ..Default::default()
        },
    )
    .await;

    // Default store-path query hides future-dated facts (temporal safety invariant).
    let response = engine
        .execute_query(&MemoryQuery::new())
        .await
        .expect("query failed");

    assert!(
        response.results.is_empty(),
        "future t_valid fact should be invisible at now; got {} results",
        response.results.len()
    );
}

#[tokio::test]
async fn future_t_valid_fact_visible_with_valid_at() {
    let engine = eval_engine();

    let future = Utc::now() + Duration::days(30);
    let future_id = add_fact_with_opts(
        &engine,
        "future deployment schedule visible",
        FactType::Semantic,
        None,
        AddFactOptions {
            t_valid: Some(future),
            ..Default::default()
        },
    )
    .await;

    // valid_at set to a point after t_valid should reveal the fact.
    let at = future + Duration::hours(1);
    let response = engine
        .execute_query(&MemoryQuery::new().valid_at(at))
        .await
        .expect("query failed");

    assert!(
        response.results.iter().any(|r| r.fact.id == future_id),
        "future fact should be visible when valid_at is after t_valid"
    );
}

#[tokio::test]
async fn past_t_invalid_fact_invisible() {
    let engine = eval_engine();

    let past_invalid = days_ago(5);
    let past_valid = days_ago(30);
    let _expired_id = add_fact_with_opts(
        &engine,
        "deprecated API endpoint configuration",
        FactType::Semantic,
        None,
        AddFactOptions {
            t_valid: Some(past_valid),
            t_invalid: Some(past_invalid),
            ..Default::default()
        },
    )
    .await;

    // Default query (effective cutoff = now) should exclude facts with t_invalid in the past.
    let response = engine
        .execute_query(&MemoryQuery::new())
        .await
        .expect("query failed");

    assert!(
        response.results.is_empty(),
        "fact with past t_invalid should be invisible"
    );
}

#[tokio::test]
async fn period_overlap_includes_overlapping_facts() {
    let engine = eval_engine();

    // Fact valid from 20 days ago to 5 days ago.
    let id_past = add_fact_with_opts(
        &engine,
        "past sprint review completed",
        FactType::Episodic,
        None,
        AddFactOptions {
            t_valid: Some(days_ago(20)),
            t_invalid: Some(days_ago(5)),
            ..Default::default()
        },
    )
    .await;

    // Fact valid from 3 days ago, still valid (no t_invalid).
    let id_current = add_fact_with_opts(
        &engine,
        "current sprint review started",
        FactType::Episodic,
        None,
        AddFactOptions {
            t_valid: Some(days_ago(3)),
            ..Default::default()
        },
    )
    .await;

    // Store-path period query: 10 days ago to now — should overlap with both facts.
    let response = engine
        .execute_query(&MemoryQuery::new().period(days_ago(10), Utc::now()))
        .await
        .expect("query failed");

    let ids: Vec<i64> = response.results.iter().map(|r| r.fact.id).collect();
    assert!(
        ids.contains(&id_past),
        "past fact overlapping period should be included"
    );
    assert!(
        ids.contains(&id_current),
        "current fact overlapping period should be included"
    );
}

#[tokio::test]
async fn period_excludes_non_overlapping_facts() {
    let engine = eval_engine();

    // Fact valid from 30 days ago to 20 days ago.
    let _old_id = add_fact_with_opts(
        &engine,
        "ancient sprint planning archived",
        FactType::Episodic,
        None,
        AddFactOptions {
            t_valid: Some(days_ago(30)),
            t_invalid: Some(days_ago(20)),
            ..Default::default()
        },
    )
    .await;

    // Store-path period query: last 5 days — should NOT overlap with the old fact.
    let response = engine
        .execute_query(&MemoryQuery::new().period(days_ago(5), Utc::now()))
        .await
        .expect("query failed");

    assert!(
        response.results.is_empty(),
        "non-overlapping fact should be excluded by period filter"
    );
}

#[tokio::test]
async fn event_sequence_ids_increase_monotonically() {
    let engine = eval_engine();

    let mut event_ids = Vec::new();
    for i in 0..3 {
        let event = NewEvent {
            timestamp: Utc::now(),
            event_type: EventType::Interaction,
            payload: serde_json::json!({"step": i}),
            source: "test".to_string(),
            session_id: Some("session-1".to_string()),
            scope_id: 1,
            origin_node_id: "node-1".to_string(),
            sequence_id: i64::from(i),
            created_at: None,
        };
        let id = engine.ingest(&event).await.expect("ingest failed");
        event_ids.push(id);
    }

    // Verify strict monotonic increase.
    for w in event_ids.windows(2) {
        assert!(
            w[1] > w[0],
            "event IDs must be strictly increasing: {} should be > {}",
            w[1],
            w[0]
        );
    }
}

#[tokio::test]
async fn system_time_independent_of_real_world_time() {
    let engine = eval_engine();

    // Create a fact with system t_created = now, but real-world t_valid = 10 days ago.
    let real_world_past = days_ago(10);
    let id = add_fact_with_opts(
        &engine,
        "historical observation recorded today",
        FactType::Semantic,
        None,
        AddFactOptions {
            t_valid: Some(real_world_past),
            ..Default::default()
        },
    )
    .await;

    let fact = engine.get_fact(id).await.expect("get_fact failed");

    // System time (t_created) should be recent (within last minute).
    let age = Utc::now() - fact.t_created;
    assert!(
        age < Duration::minutes(1),
        "t_created should be near now, not backdated to t_valid"
    );

    // Real-world time (t_valid) should be the value we set.
    assert_eq!(
        fact.t_valid,
        Some(real_world_past),
        "t_valid should reflect the real-world time we set"
    );
}
