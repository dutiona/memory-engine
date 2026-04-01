//! B1: Scope isolation conformance tests.
//!
//! Verifies that scope-based queries enforce strict isolation:
//! facts in one scope never leak into queries targeting another scope.

use memory_engine::search::query::MemoryQuery;
use memory_engine::types::FactType;

use crate::helpers::{add_scoped_fact, eval_engine};

#[test]
fn scope_exact_excludes_other_scopes() {
    let engine = eval_engine();

    let alice_id = add_scoped_fact(
        &engine,
        "Alice prefers dark mode",
        FactType::Semantic,
        "project:alpha",
    );
    let _bob_id = add_scoped_fact(
        &engine,
        "Bob prefers light mode",
        FactType::Semantic,
        "project:beta",
    );

    // Store-path query (no text/embedding) to isolate scope behavior.
    let response = engine
        .execute_query(&MemoryQuery::new().scope_exact("project:alpha"))
        .expect("query failed");

    let ids: Vec<i64> = response.results.iter().map(|r| r.fact.id).collect();
    assert!(
        ids.contains(&alice_id),
        "alice's fact should appear in exact scope query"
    );
    assert_eq!(ids.len(), 1, "only alice's fact should appear; got {ids:?}");
}

#[test]
fn scope_exact_returns_empty_for_nonexistent_scope() {
    let engine = eval_engine();

    add_scoped_fact(&engine, "some fact", FactType::Semantic, "project:alpha");

    let response = engine
        .execute_query(&MemoryQuery::new().scope_exact("project:nonexistent"))
        .expect("query failed");

    assert!(
        response.results.is_empty(),
        "nonexistent scope should return zero results"
    );
}

#[test]
fn scope_subtree_returns_only_descendant_facts() {
    let engine = eval_engine();

    // Pre-populate intermediate scopes so the in-memory tree has all nodes.
    engine
        .ensure_scope_path("user:alice")
        .expect("ensure alice scope");
    engine
        .ensure_scope_path("user:bob")
        .expect("ensure bob scope");

    let alice_alpha = add_scoped_fact(
        &engine,
        "Alice alpha deployment strategy",
        FactType::Semantic,
        "user:alice/project:alpha",
    );
    let alice_beta = add_scoped_fact(
        &engine,
        "Alice beta testing plan",
        FactType::Semantic,
        "user:alice/project:beta",
    );
    let _bob_gamma = add_scoped_fact(
        &engine,
        "Bob gamma release notes",
        FactType::Semantic,
        "user:bob/project:gamma",
    );

    let response = engine
        .execute_query(&MemoryQuery::new().scope_subtree("user:alice"))
        .expect("query failed");

    let ids: Vec<i64> = response.results.iter().map(|r| r.fact.id).collect();
    assert!(
        ids.contains(&alice_alpha),
        "alice/alpha fact should be in subtree"
    );
    assert!(
        ids.contains(&alice_beta),
        "alice/beta fact should be in subtree"
    );
    assert!(
        !ids.iter().any(|id| {
            let f = engine.get_fact(*id).unwrap();
            f.content.contains("Bob")
        }),
        "bob's facts should not appear in alice's subtree"
    );
}

#[test]
fn unscoped_query_returns_all_facts() {
    let engine = eval_engine();

    let id_alice = add_scoped_fact(
        &engine,
        "Alice config preference data",
        FactType::Semantic,
        "project:alpha",
    );
    let id_bob = add_scoped_fact(
        &engine,
        "Bob config preference data",
        FactType::Semantic,
        "project:beta",
    );

    // Unscoped store-path query returns all active facts.
    let response = engine
        .execute_query(&MemoryQuery::new())
        .expect("query failed");

    let ids: Vec<i64> = response.results.iter().map(|r| r.fact.id).collect();
    assert!(
        ids.contains(&id_alice),
        "alice's fact should be in unscoped results"
    );
    assert!(
        ids.contains(&id_bob),
        "bob's fact should be in unscoped results"
    );
}

#[test]
fn scope_exact_does_not_include_parent_facts() {
    let engine = eval_engine();

    // Pre-populate intermediate scope so the tree has all nodes.
    engine
        .ensure_scope_path("user:alice")
        .expect("ensure alice scope");

    let _parent_id = add_scoped_fact(
        &engine,
        "Parent scope fact about config",
        FactType::Semantic,
        "user:alice",
    );
    let child_id = add_scoped_fact(
        &engine,
        "Child scope fact about config",
        FactType::Semantic,
        "user:alice/project:alpha",
    );

    let response = engine
        .execute_query(&MemoryQuery::new().scope_exact("user:alice/project:alpha"))
        .expect("query failed");

    let ids: Vec<i64> = response.results.iter().map(|r| r.fact.id).collect();
    assert!(ids.contains(&child_id), "child fact should appear");
    assert_eq!(
        ids.len(),
        1,
        "exact scope should not include parent facts; got {ids:?}"
    );
}
