//! B1: Scope isolation conformance tests.
//!
//! Verifies that scope-based queries enforce strict isolation:
//! facts in one scope never leak into queries targeting another scope.

use memory_engine::search::query::MemoryQuery;
use memory_engine::types::FactType;

use crate::helpers::{add_scoped_fact, eval_engine};

#[tokio::test]
async fn scope_exact_excludes_other_scopes() {
    let engine = eval_engine();

    let alice_id = add_scoped_fact(
        &engine,
        "Alice prefers dark mode",
        FactType::Semantic,
        "project:alpha",
    )
    .await;
    let _bob_id = add_scoped_fact(
        &engine,
        "Bob prefers light mode",
        FactType::Semantic,
        "project:beta",
    )
    .await;

    // Store-path query (no text/embedding) to isolate scope behavior.
    let response = engine
        .execute_query(&MemoryQuery::new().scope_exact("project:alpha"))
        .await
        .expect("query failed");

    let ids: Vec<i64> = response.results.iter().map(|r| r.fact.id).collect();
    assert!(
        ids.contains(&alice_id),
        "alice's fact should appear in exact scope query"
    );
    assert_eq!(ids.len(), 1, "only alice's fact should appear; got {ids:?}");
}

#[tokio::test]
async fn scope_exact_returns_empty_for_nonexistent_scope() {
    let engine = eval_engine();

    add_scoped_fact(&engine, "some fact", FactType::Semantic, "project:alpha").await;

    let response = engine
        .execute_query(&MemoryQuery::new().scope_exact("project:nonexistent"))
        .await
        .expect("query failed");

    assert!(
        response.results.is_empty(),
        "nonexistent scope should return zero results"
    );
}

#[tokio::test]
async fn scope_subtree_returns_only_descendant_facts() {
    let engine = eval_engine();

    // Pre-populate intermediate scopes so the in-memory tree has all nodes.
    engine
        .ensure_scope_path("user:alice")
        .await
        .expect("ensure alice scope");
    engine
        .ensure_scope_path("user:bob")
        .await
        .expect("ensure bob scope");

    let alice_alpha = add_scoped_fact(
        &engine,
        "Alice alpha deployment strategy",
        FactType::Semantic,
        "user:alice/project:alpha",
    )
    .await;
    let alice_beta = add_scoped_fact(
        &engine,
        "Alice beta testing plan",
        FactType::Semantic,
        "user:alice/project:beta",
    )
    .await;
    let _bob_gamma = add_scoped_fact(
        &engine,
        "Bob gamma release notes",
        FactType::Semantic,
        "user:bob/project:gamma",
    )
    .await;

    let response = engine
        .execute_query(&MemoryQuery::new().scope_subtree("user:alice"))
        .await
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
    let mut bob_present = false;
    for id in &ids {
        if engine.get_fact(*id).await.unwrap().content.contains("Bob") {
            bob_present = true;
            break;
        }
    }
    assert!(
        !bob_present,
        "bob's facts should not appear in alice's subtree"
    );
}

#[tokio::test]
async fn unscoped_query_returns_all_facts() {
    let engine = eval_engine();

    let id_alice = add_scoped_fact(
        &engine,
        "Alice config preference data",
        FactType::Semantic,
        "project:alpha",
    )
    .await;
    let id_bob = add_scoped_fact(
        &engine,
        "Bob config preference data",
        FactType::Semantic,
        "project:beta",
    )
    .await;

    // Unscoped store-path query returns all active facts.
    let response = engine
        .execute_query(&MemoryQuery::new())
        .await
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

#[tokio::test]
async fn scope_ancestors_returns_self_and_parent_facts_not_sibling() {
    // End-to-end coverage for MemoryQuery::scope_ancestors → ScopeQuery::Ancestors
    // → ScopeTree::resolve_query dispatch (#322). Ancestors of a leaf are the
    // leaf itself plus every parent up to root, so a fact at the parent scope
    // must surface while a sibling-subtree fact must not.
    let engine = eval_engine();

    engine
        .ensure_scope_path("user:alice")
        .await
        .expect("ensure alice scope");

    let parent_id = add_scoped_fact(
        &engine,
        "Alice parent-scope config",
        FactType::Semantic,
        "user:alice",
    )
    .await;
    let child_id = add_scoped_fact(
        &engine,
        "Alice child deployment plan",
        FactType::Semantic,
        "user:alice/project:x",
    )
    .await;
    let _sibling_id = add_scoped_fact(
        &engine,
        "Bob unrelated note",
        FactType::Semantic,
        "user:bob",
    )
    .await;

    let response = engine
        .execute_query(&MemoryQuery::new().scope_ancestors("user:alice/project:x"))
        .await
        .expect("query failed");

    let ids: Vec<i64> = response.results.iter().map(|r| r.fact.id).collect();
    assert!(
        ids.contains(&child_id),
        "the queried scope's own fact must appear in an ancestors query"
    );
    assert!(
        ids.contains(&parent_id),
        "the parent scope's fact must appear in an ancestors query"
    );
    let mut bob_present = false;
    for id in &ids {
        if engine.get_fact(*id).await.unwrap().content.contains("Bob") {
            bob_present = true;
            break;
        }
    }
    assert!(
        !bob_present,
        "a sibling-branch fact must NOT appear in an ancestors query; got {ids:?}"
    );
}

#[tokio::test]
async fn scope_inherited_includes_ancestors_and_descendants_not_unrelated() {
    // End-to-end coverage for MemoryQuery::scope_inherited → ScopeQuery::Inherited
    // → ScopeTree::resolve_query dispatch (#322). Inherited context is ancestors
    // PLUS the subtree of the queried scope, so both a parent fact and a child
    // fact surface while an unrelated branch does not.
    let engine = eval_engine();

    engine
        .ensure_scope_path("user:alice")
        .await
        .expect("ensure alice scope");

    let parent_id = add_scoped_fact(
        &engine,
        "Alice parent-scope config",
        FactType::Semantic,
        "user:alice",
    )
    .await;
    let child_id = add_scoped_fact(
        &engine,
        "Alice child env settings",
        FactType::Semantic,
        "user:alice/project:x",
    )
    .await;
    let _other_id = add_scoped_fact(
        &engine,
        "Bob unrelated fact",
        FactType::Semantic,
        "user:bob",
    )
    .await;

    let response = engine
        .execute_query(&MemoryQuery::new().scope_inherited("user:alice"))
        .await
        .expect("query failed");

    let ids: Vec<i64> = response.results.iter().map(|r| r.fact.id).collect();
    assert!(
        ids.contains(&parent_id),
        "the queried scope's own fact must appear in an inherited query"
    );
    assert!(
        ids.contains(&child_id),
        "a descendant fact must appear in an inherited query"
    );
    let mut bob_present = false;
    for id in &ids {
        if engine.get_fact(*id).await.unwrap().content.contains("Bob") {
            bob_present = true;
            break;
        }
    }
    assert!(
        !bob_present,
        "an unrelated-branch fact must NOT appear in an inherited query; got {ids:?}"
    );
}

#[tokio::test]
async fn empty_scope_string_returns_no_facts_not_everything() {
    // Caller-level regression for the empty-string fail-open (#360 follow-up).
    // An empty scope-query string must be treated as a non-existent scope ("no
    // results"), NOT silently resolved to root. If `resolve_path("")` returned
    // root, `scope_exact("")` would surface the root scope's facts and, worse,
    // `scope_subtree("")` would expand to subtree(root) = EVERY fact across
    // EVERY context — leaking all facts through a blank/defaulted scope field.
    // This guards the boundary that actually flips (engine/query.rs), which the
    // tree-level unit tests don't exercise end-to-end.
    let engine = eval_engine();

    add_scoped_fact(&engine, "Alice fact", FactType::Semantic, "project:alpha").await;
    add_scoped_fact(&engine, "Bob fact", FactType::Semantic, "project:beta").await;

    let exact = engine
        .execute_query(&MemoryQuery::new().scope_exact(""))
        .await
        .expect("query failed");
    assert!(
        exact.results.is_empty(),
        "empty scope_exact must return zero results, not root facts; got {:?}",
        exact.results.iter().map(|r| r.fact.id).collect::<Vec<_>>()
    );

    let subtree = engine
        .execute_query(&MemoryQuery::new().scope_subtree(""))
        .await
        .expect("query failed");
    assert!(
        subtree.results.is_empty(),
        "empty scope_subtree must NOT leak the whole store (subtree(root)); got {:?}",
        subtree
            .results
            .iter()
            .map(|r| r.fact.id)
            .collect::<Vec<_>>()
    );

    let inherited = engine
        .execute_query(&MemoryQuery::new().scope_inherited(""))
        .await
        .expect("query failed");
    assert!(
        inherited.results.is_empty(),
        "empty scope_inherited must NOT leak the whole store; got {:?}",
        inherited
            .results
            .iter()
            .map(|r| r.fact.id)
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn scope_exact_does_not_include_parent_facts() {
    let engine = eval_engine();

    // Pre-populate intermediate scope so the tree has all nodes.
    engine
        .ensure_scope_path("user:alice")
        .await
        .expect("ensure alice scope");

    let _parent_id = add_scoped_fact(
        &engine,
        "Parent scope fact about config",
        FactType::Semantic,
        "user:alice",
    )
    .await;
    let child_id = add_scoped_fact(
        &engine,
        "Child scope fact about config",
        FactType::Semantic,
        "user:alice/project:alpha",
    )
    .await;

    let response = engine
        .execute_query(&MemoryQuery::new().scope_exact("user:alice/project:alpha"))
        .await
        .expect("query failed");

    let ids: Vec<i64> = response.results.iter().map(|r| r.fact.id).collect();
    assert!(ids.contains(&child_id), "child fact should appear");
    assert_eq!(
        ids.len(),
        1,
        "exact scope should not include parent facts; got {ids:?}"
    );
}
