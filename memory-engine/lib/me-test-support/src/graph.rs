//! `FactGraph` / `FactFilter` contract bodies.
//!
//! Every body asserts the documented CONTRACT directly (a specific id / content /
//! row-set / error variant) — never `is_ok()`, and never parity against a `SQLite`
//! `FactStore` oracle (that would not transfer to another backend).

use std::collections::HashSet;

use chrono::{DateTime, Utc};

use me_storage::{FactFilter, MetadataPredicate, TemporalFilter};
use me_types::error::{ConflictError, MemoryError};
use me_types::types::{FactType, NewEdge, NewFact};

use super::factory::ConformanceBackend;
use super::fixtures::{DIM, new_event, new_fact, seed_facts};

/// Parse a fixed UTC instant for the bi-temporal bodies (deterministic, no `now()`).
fn instant(s: &str) -> DateTime<Utc> {
    s.parse().expect("parse instant")
}

/// insert → get round-trips the fact (id + content + type + scope preserved).
///
/// A non-conforming backend fails this by returning a different row or losing fields.
pub async fn insert_get_round_trip<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let id = seed_facts(&be, &[new_fact("round-trip me")]).await[0];
    let got = be.get_fact(id).await.expect("get_fact");
    assert_eq!(got.id, id, "[{}] id", f.name());
    assert_eq!(got.content, "round-trip me", "[{}] content", f.name());
    assert_eq!(
        got.fact_type,
        FactType::Episodic,
        "[{}] fact_type",
        f.name()
    );
    assert_eq!(got.scope_id, 1, "[{}] scope_id", f.name());
}

// -------------------------------------------------------------------------
// Bi-temporal (TemporalFilter) contracts
// -------------------------------------------------------------------------

/// `AsOf(t)` (via `list_active_facts_at`) excludes a soft-deleted row even at a
/// historical instant when it WAS valid — the `t_expired IS NULL` clause gate
/// (filter.rs:100-104). A backend that implements `AsOf` as valid-time-only (drops the
/// `t_expired` clause) returns the expired row and fails.
pub async fn as_of_returns_historical_excludes_expired<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let t0 = instant("2026-01-01T00:00:00Z");
    let t_mid = instant("2026-01-02T00:00:00Z");
    let t1 = instant("2026-01-03T00:00:00Z");
    let fact = NewFact::builder("temporal", vec![0.1_f32; DIM], FactType::Episodic)
        .scope_id(1)
        .t_valid(t0)
        .build();
    let id = seed_facts(&be, &[fact]).await[0];
    // Before expiry: valid at t_mid, not expired ⇒ present.
    let at_mid = be.list_active_facts_at(t_mid).await.expect("as_of mid");
    assert!(
        at_mid.iter().any(|x| x.id == id),
        "[{}] AsOf(mid) must include the live, valid fact",
        f.name()
    );
    // Soft-delete it at t1 (sets t_expired AND t_invalid).
    be.expire_and_invalidate_fact(id, t1)
        .await
        .expect("expire+invalidate");
    // After expiry: AsOf(mid) must EXCLUDE it even though it was valid at t_mid.
    let at_mid2 = be
        .list_active_facts_at(t_mid)
        .await
        .expect("as_of mid post-expiry");
    assert!(
        !at_mid2.iter().any(|x| x.id == id),
        "[{}] AsOf(mid) must exclude the soft-deleted fact (t_expired clause)",
        f.name()
    );
}

/// `expire_fact` then `list_active_facts` excludes it (the issue's named case).
/// A non-conforming backend leaks the system-time-expired row.
pub async fn active_excludes_expired<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let id = seed_facts(&be, &[new_fact("to expire")]).await[0];
    assert!(
        be.list_active_facts(None)
            .await
            .expect("active")
            .iter()
            .any(|x| x.id == id),
        "[{}] seeded fact must be active",
        f.name()
    );
    be.expire_fact(id, Utc::now()).await.expect("expire");
    assert!(
        !be.list_active_facts(None)
            .await
            .expect("active2")
            .iter()
            .any(|x| x.id == id),
        "[{}] expire_fact then list_active must exclude it",
        f.name()
    );
}

/// `IncludeExpired` is the ONLY temporal mode that surfaces a soft-deleted row.
/// Asserted through `vector_search`'s `FactFilter`: a backend that ignores the
/// temporal dimension would either leak the row under `Active` or hide it under
/// `IncludeExpired`.
pub async fn include_expired_surfaces_soft_deleted<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let id = seed_facts(&be, &[new_fact("soft delete me")]).await[0];
    let emb = vec![0.1_f32; DIM];
    be.expire_fact(id, Utc::now()).await.expect("expire");
    let active = be
        .vector_search(
            &emb,
            &FactFilter::default().temporal(TemporalFilter::Active),
            10,
        )
        .await
        .expect("vector active");
    assert!(
        !active.iter().any(|(fid, _)| *fid == id),
        "[{}] Active vector_search must exclude the expired fact",
        f.name()
    );
    let incl = be
        .vector_search(
            &emb,
            &FactFilter::default().temporal(TemporalFilter::IncludeExpired),
            10,
        )
        .await
        .expect("vector include-expired");
    assert!(
        incl.iter().any(|(fid, _)| *fid == id),
        "[{}] IncludeExpired vector_search must surface the soft-deleted fact",
        f.name()
    );
}

// -------------------------------------------------------------------------
// The three distinct empty-slice contracts (graph.rs:33-45)
// -------------------------------------------------------------------------

/// `FactGraph` `scope_ids: &[i64]` **empty = ALL scopes** (filter disabled) on ALL
/// SEVEN documented methods. A backend that treats empty as "no scopes" (NONE) would
/// return empty and fail (the doc says "the #632 conformance suite pins it").
pub async fn scope_ids_empty_slice_means_all<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let now = Utc::now();
    let past = instant("2020-01-01T00:00:00Z");
    let future = instant("2099-01-01T00:00:00Z");
    let empty_excl: HashSet<i64> = HashSet::new();
    let period_start = instant("2000-01-01T00:00:00Z");
    let period_end = instant("2100-01-01T00:00:00Z");

    // A fact satisfying pinned + due + importance + in-period + undreamt predicates.
    let fact = NewFact::builder("all-method fact", vec![0.1_f32; DIM], FactType::Episodic)
        .scope_id(1)
        .is_pinned(true)
        .t_valid(past)
        .base_importance(0.9)
        .build();
    let id = seed_facts(&be, &[fact]).await[0];
    // A future-due fact so `next_due_time` has a candidate (identity now established).
    let future_fact = NewFact::builder("future due", vec![0.1_f32; DIM], FactType::Episodic)
        .scope_id(1)
        .t_valid(future)
        .build();
    be.insert_fact(&future_fact)
        .await
        .expect("insert future fact");
    // A session-linked fact (event → fact.source_event_id).
    let event_id = be
        .insert_event(&new_event("sess-1"))
        .await
        .expect("insert_event");
    let sess_fact = NewFact::builder("session fact", vec![0.1_f32; DIM], FactType::Episodic)
        .scope_id(1)
        .source_event_id(event_id)
        .build();
    let sess_id = be
        .insert_fact(&sess_fact)
        .await
        .expect("insert session fact");

    macro_rules! has {
        ($got:expr, $want:expr, $label:literal) => {
            assert!(
                $got.iter().any(|x| x.id == $want),
                "[{}] {} empty scope_ids must mean ALL",
                f.name(),
                $label
            );
        };
    }
    has!(
        be.list_pinned_facts(&[], None).await.expect("pinned"),
        id,
        "list_pinned_facts"
    );
    has!(
        be.list_due_facts(now, &[], &[], None).await.expect("due"),
        id,
        "list_due_facts"
    );
    assert!(
        be.next_due_time(now, &[])
            .await
            .expect("next_due")
            .is_some(),
        "[{}] next_due_time empty scope_ids must mean ALL",
        f.name()
    );
    has!(
        be.list_facts_by_importance_score(&[], 0.0, 100, &empty_excl)
            .await
            .expect("by score"),
        id,
        "list_facts_by_importance_score"
    );
    assert!(
        be.list_active_facts_by_session("sess-1", &[])
            .await
            .expect("by session")
            .iter()
            .any(|x| x.id == sess_id),
        "[{}] list_active_facts_by_session empty scope_ids must mean ALL",
        f.name()
    );
    has!(
        be.list_active_facts_in_period(period_start, period_end, &[], None)
            .await
            .expect("in period"),
        id,
        "list_active_facts_in_period"
    );
    has!(
        be.list_undreamt_facts_in_period(period_start, period_end, &[], None)
            .await
            .expect("undreamt"),
        id,
        "list_undreamt_facts_in_period"
    );
}

/// `FactGraph` `scope_ids: &[i64]` **empty = NONE** (empty result) on ALL THREE
/// documented methods. A backend that treats empty as "no filter" (ALL) would return
/// the scoped fact and fail. A non-empty `&[scope]` control proves the predicate is
/// satisfiable, so the empty result is meaningful (not just "no matching facts").
pub async fn scope_ids_empty_slice_means_none<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let scope = be
        .ensure_scope_path("conformance/none")
        .await
        .expect("ensure scope");
    let empty_excl: HashSet<i64> = HashSet::new();
    let fact = NewFact::builder("none-test", vec![0.1_f32; DIM], FactType::Episodic)
        .scope_id(scope)
        .base_importance(0.9)
        .metadata(serde_json::json!({ "marker": 1 }))
        .build();
    let id = seed_facts(&be, &[fact]).await[0];

    // by_scopes_importance
    let ctl = be
        .list_facts_by_scopes_importance(&[scope], 0.0, 100, &empty_excl)
        .await
        .expect("ctl imp");
    assert!(
        ctl.iter().any(|x| x.id == id),
        "[{}] by_scopes_importance &[scope] control",
        f.name()
    );
    let empty = be
        .list_facts_by_scopes_importance(&[], 0.0, 100, &empty_excl)
        .await
        .expect("empty imp");
    assert!(
        empty.is_empty(),
        "[{}] by_scopes_importance empty scope_ids must mean NONE",
        f.name()
    );

    // by_scopes_recent
    let ctl = be
        .list_facts_by_scopes_recent(&[scope], 100, &empty_excl)
        .await
        .expect("ctl recent");
    assert!(
        ctl.iter().any(|x| x.id == id),
        "[{}] by_scopes_recent &[scope] control",
        f.name()
    );
    let empty = be
        .list_facts_by_scopes_recent(&[], 100, &empty_excl)
        .await
        .expect("empty recent");
    assert!(
        empty.is_empty(),
        "[{}] by_scopes_recent empty scope_ids must mean NONE",
        f.name()
    );

    // by_metadata_key_recent
    let ctl = be
        .list_active_facts_by_metadata_key_recent(&[scope], "marker", 100)
        .await
        .expect("ctl meta");
    assert!(
        ctl.iter().any(|x| x.id == id),
        "[{}] by_metadata_key_recent &[scope] control",
        f.name()
    );
    let empty = be
        .list_active_facts_by_metadata_key_recent(&[], "marker", 100)
        .await
        .expect("empty meta");
    assert!(
        empty.is_empty(),
        "[{}] by_metadata_key_recent empty scope_ids must mean NONE",
        f.name()
    );
}

/// `FactFilter.scope_ids: Some(empty)` = **matches NOTHING** (the backend MUST NOT
/// normalize it to "no filter"). Distinct from the `&[i64]` convention. A `None`
/// control proves the row is otherwise findable.
pub async fn filter_scope_ids_some_empty_matches_nothing<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let id = seed_facts(&be, &[new_fact("findme")]).await[0];
    let emb = vec![0.1_f32; DIM];
    let control = be
        .vector_search(&emb, &FactFilter::default(), 10)
        .await
        .expect("control");
    assert!(
        control.iter().any(|(fid, _)| *fid == id),
        "[{}] FactFilter scope_ids None must return the fact (control)",
        f.name()
    );
    let some_empty = be
        .vector_search(&emb, &FactFilter::default().scope_ids(vec![]), 10)
        .await
        .expect("some-empty");
    assert!(
        some_empty.is_empty(),
        "[{}] FactFilter scope_ids Some(empty) must match NOTHING (not be normalized to no-filter)",
        f.name()
    );
}

/// `FactFilter.ids: Some(empty)` = **matches NOTHING** (sibling of the `scope_ids` rule).
pub async fn filter_ids_some_empty_matches_nothing<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let id = seed_facts(&be, &[new_fact("findme2")]).await[0];
    let emb = vec![0.1_f32; DIM];
    let control = be
        .vector_search(&emb, &FactFilter::default(), 10)
        .await
        .expect("control");
    assert!(
        control.iter().any(|(fid, _)| *fid == id),
        "[{}] FactFilter ids None must return the fact (control)",
        f.name()
    );
    let some_empty = be
        .vector_search(&emb, &FactFilter::default().ids(vec![]), 10)
        .await
        .expect("some-empty");
    assert!(
        some_empty.is_empty(),
        "[{}] FactFilter ids Some(empty) must match NOTHING",
        f.name()
    );
}

// -------------------------------------------------------------------------
// Breadth: NotFound, injection guard, metadata predicates, pinned, edges, scopes
// -------------------------------------------------------------------------

/// `get_fact(missing)` yields the semantic `NotFound` (not an opaque backend error).
pub async fn get_missing_yields_not_found<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let err = be
        .get_fact(999_999)
        .await
        .expect_err("missing id must be NotFound");
    assert!(
        matches!(err, MemoryError::NotFound(_)),
        "[{}] expected NotFound, got {err:?}",
        f.name()
    );
}

/// `list_active_facts_by_metadata_key_recent` rejects a non-`[A-Za-z0-9_]+`
/// `marker_key` with the EXACT `Conflict(QueryValidation(_))` — the JSON-path
/// injection guard the seam must preserve (a weaker outer-only match would let a
/// backend slip a different error past the gate).
pub async fn metadata_key_recent_rejects_injection<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    for bad in ["", "in'sight", "$.x", "a b", "a;b"] {
        let err = be
            .list_active_facts_by_metadata_key_recent(&[1], bad, 10)
            .await
            .expect_err("invalid marker_key must be rejected");
        assert!(
            matches!(
                err,
                MemoryError::Conflict(ConflictError::QueryValidation(_))
            ),
            "[{}] marker_key {bad:?} must yield Conflict(QueryValidation), got {err:?}",
            f.name()
        );
    }
}

/// `MetadataPredicate::KeyPresent` / `KeyAbsent` partition facts by a top-level
/// metadata key (the port must HONOR the predicate; the exact JSON dialect is golden).
pub async fn metadata_predicate_present_and_absent<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let with_key = NewFact::builder("has marker", vec![0.1_f32; DIM], FactType::Episodic)
        .scope_id(1)
        .metadata(serde_json::json!({ "marker": 1 }))
        .build();
    let without_key = NewFact::builder("no marker", vec![0.1_f32; DIM], FactType::Episodic)
        .scope_id(1)
        .build();
    let ids = seed_facts(&be, &[with_key, without_key]).await;
    let (with_id, without_id) = (ids[0], ids[1]);
    let emb = vec![0.1_f32; DIM];

    let present = be
        .vector_search(
            &emb,
            &FactFilter::default().with_metadata(MetadataPredicate::KeyPresent("marker".into())),
            10,
        )
        .await
        .expect("present");
    assert!(
        present.iter().any(|(fid, _)| *fid == with_id)
            && !present.iter().any(|(fid, _)| *fid == without_id),
        "[{}] KeyPresent must return only the fact with the key",
        f.name()
    );

    let absent = be
        .vector_search(
            &emb,
            &FactFilter::default().with_metadata(MetadataPredicate::KeyAbsent("marker".into())),
            10,
        )
        .await
        .expect("absent");
    assert!(
        absent.iter().any(|(fid, _)| *fid == without_id)
            && !absent.iter().any(|(fid, _)| *fid == with_id),
        "[{}] KeyAbsent must return only the fact without the key",
        f.name()
    );
}

/// `FactFilter.pinned` partitions pinned vs unpinned facts.
pub async fn pinned_filter_partitions<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let pinned = NewFact::builder("pinned", vec![0.1_f32; DIM], FactType::Episodic)
        .scope_id(1)
        .is_pinned(true)
        .build();
    let unpinned = NewFact::builder("unpinned", vec![0.1_f32; DIM], FactType::Episodic)
        .scope_id(1)
        .is_pinned(false)
        .build();
    let ids = seed_facts(&be, &[pinned, unpinned]).await;
    let (p_id, u_id) = (ids[0], ids[1]);
    let emb = vec![0.1_f32; DIM];

    let only_pinned = be
        .vector_search(&emb, &FactFilter::default().pinned(true), 10)
        .await
        .expect("pinned");
    assert!(
        only_pinned.iter().any(|(fid, _)| *fid == p_id)
            && !only_pinned.iter().any(|(fid, _)| *fid == u_id),
        "[{}] pinned(true) must return only pinned facts",
        f.name()
    );
    let only_unpinned = be
        .vector_search(&emb, &FactFilter::default().pinned(false), 10)
        .await
        .expect("unpinned");
    assert!(
        only_unpinned.iter().any(|(fid, _)| *fid == u_id)
            && !only_unpinned.iter().any(|(fid, _)| *fid == p_id),
        "[{}] pinned(false) must return only unpinned facts",
        f.name()
    );
}

/// Edge insert → get → list-by-source → expire round-trip (the graph-edge contract).
pub async fn edge_insert_get_list_expire<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let ids = seed_facts(&be, &[new_fact("src"), new_fact("tgt")]).await;
    let (src, tgt) = (ids[0], ids[1]);
    let edge = NewEdge {
        source_fact_id: src,
        target_fact_id: tgt,
        relation_type: "relates".into(),
        weight: 0.7,
        t_created: Utc::now(),
        t_expired: None,
        scope_id: 1,
    };
    let edge_id = be.insert_edge(&edge).await.expect("insert_edge");
    let got = be.get_edge(edge_id).await.expect("get_edge");
    assert_eq!(got.source_fact_id, src, "[{}] edge src", f.name());
    assert_eq!(got.target_fact_id, tgt, "[{}] edge tgt", f.name());
    assert_eq!(got.relation_type, "relates", "[{}] edge relation", f.name());
    assert!(
        be.list_active_edges_by_source(src)
            .await
            .expect("by source")
            .iter()
            .any(|e| e.id == edge_id),
        "[{}] active edge must list by source",
        f.name()
    );
    assert!(
        be.edge_exists_active(src, tgt, "relates")
            .await
            .expect("exists"),
        "[{}] edge_exists_active must be true",
        f.name()
    );
    be.expire_edge(edge_id, Utc::now())
        .await
        .expect("expire_edge");
    assert!(
        !be.list_active_edges_by_source(src)
            .await
            .expect("by source 2")
            .iter()
            .any(|e| e.id == edge_id),
        "[{}] expired edge must be excluded from active",
        f.name()
    );
}

/// Scope `ensure_path` (idempotent) → `get_scope` → `find_scope_by_label`.
pub async fn scope_ensure_get_find<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let id = be
        .ensure_scope_path("conformance/child")
        .await
        .expect("ensure");
    let again = be
        .ensure_scope_path("conformance/child")
        .await
        .expect("ensure idempotent");
    assert_eq!(
        id,
        again,
        "[{}] ensure_scope_path must be idempotent",
        f.name()
    );
    let node = be.get_scope(id).await.expect("get_scope");
    assert_eq!(node.label, "child", "[{}] scope label", f.name());
    let parent = node.parent_id.expect("child scope has a parent");
    let found = be
        .find_scope_by_label(parent, "child")
        .await
        .expect("find_scope_by_label");
    assert_eq!(
        found.map(|n| n.id),
        Some(id),
        "[{}] find_scope_by_label must resolve the child",
        f.name()
    );
}
