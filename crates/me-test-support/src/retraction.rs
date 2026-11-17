//! The forget retraction contract (#1023): `Retraction::{forget_fact, unforget_fact,
//! tombstone, is_forgotten}`.

use std::sync::Arc;

use chrono::{Duration, Utc};
use me_storage::{FactFilter, StorageBackend};
use me_types::types::{EventFilter, ForgetOutcome, NewFact, UnforgetOutcome};

use super::factory::ConformanceBackend;
use super::fixtures::{DIM, new_event, new_fact, seed_facts};

/// Two Homes (`h`, `u`), another scope `o`, and the forget's record path.
struct Homes {
    h: i64,
    u: i64,
    o: i64,
}

async fn homes(be: &Arc<dyn StorageBackend>) -> Homes {
    Homes {
        h: be.ensure_scope_path("h").await.unwrap(),
        u: be.ensure_scope_path("u").await.unwrap(),
        o: be.ensure_scope_path("o").await.unwrap(),
    }
}

fn at(content: &str, scope: i64) -> NewFact {
    let mut fact = new_fact(content);
    fact.scope_id = scope;
    fact
}

async fn forget(be: &Arc<dyn StorageBackend>, id: i64, homes: &[i64]) -> (ForgetOutcome, i64) {
    let (outcome, written) = be
        .forget_fact(id, homes, "wrong", Utc::now(), "rec", &new_event("s"))
        .await
        .expect("forget_fact");
    (outcome, written.id)
}

/// A missing id, a fact outside `homes`, empty `homes`: `NotFound`, the fact untouched, no
/// tombstone, the record still written.
pub async fn forget_is_not_found_outside_its_homes<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let s = homes(&be).await;
    let id = seed_facts(&be, &[at("elsewhere", s.o)]).await[0];
    let events = be.count_events(&EventFilter::default()).await.unwrap();
    for (target, set) in [(id + 100, vec![s.h]), (id, vec![s.h, s.u]), (id, vec![])] {
        let (outcome, record) = forget(&be, target, &set).await;
        assert_eq!(outcome, ForgetOutcome::NotFound, "[{}] {set:?}", f.name());
        assert!(
            be.get_event(record).await.is_ok(),
            "[{}] recorded",
            f.name()
        );
    }
    assert_eq!(
        be.count_events(&EventFilter::default()).await.unwrap(),
        events + 3,
        "[{}] one record per call",
        f.name()
    );
    assert!(be.get_fact(id).await.unwrap().t_expired.is_none());
    assert_eq!(be.tombstone(id).await.unwrap(), None, "[{}]", f.name());
    // A forgotten fact outside the Homes answers the same.
    forget(&be, id, &[s.o]).await;
    assert_eq!(forget(&be, id, &[s.h]).await.0, ForgetOutcome::NotFound);
}

/// A forget ends validity, keeps the row and names its record in the tombstone.
pub async fn forget_ends_validity_and_writes_the_tombstone<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let s = homes(&be).await;
    let id = seed_facts(&be, &[at("the build is red", s.h)]).await[0];
    let (outcome, record) = forget(&be, id, &[s.h, s.u]).await;
    assert_eq!(outcome, ForgetOutcome::Forgotten, "[{}]", f.name());
    let fact = be.get_fact(id).await.expect("the row stays");
    assert!(fact.t_expired.is_some(), "[{}] expired", f.name());
    assert!(fact.t_invalid.is_some(), "[{}] invalid", f.name());
    assert_eq!(fact.content, "the build is red");
    let stone = be.tombstone(id).await.unwrap().expect("tombstone");
    assert_eq!(stone.record_event_id, Some(record), "[{}]", f.name());
    assert_eq!(stone.reason.as_deref(), Some("wrong"));
    assert_eq!(stone.prior_t_expired, None);
    assert_eq!(stone.prior_t_invalid, None);
    assert_eq!(stone.content_hash.len(), 32, "[{}] content hash", f.name());
}

/// The named fact's active identical copies in the Homes go with it; a copy elsewhere and an
/// already expired copy stay as they were.
pub async fn forget_retracts_active_copies_in_its_homes_only<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let s = homes(&be).await;
    let mut expired = at("twin", s.u);
    expired.t_expired = Some(Utc::now() - Duration::hours(1));
    let ids = seed_facts(
        &be,
        &[at("twin", s.h), at("twin", s.u), at("twin", s.o), expired],
    )
    .await;
    let (_, record) = forget(&be, ids[0], &[s.h, s.u]).await;
    for (id, forgotten) in [
        (ids[0], true),
        (ids[1], true),
        (ids[2], false),
        (ids[3], false),
    ] {
        let stone = be.tombstone(id).await.unwrap();
        assert_eq!(stone.is_some(), forgotten, "[{}] fact {id}", f.name());
        if let Some(stone) = stone {
            assert_eq!(stone.record_event_id, Some(record));
        }
    }
    assert!(be.get_fact(ids[2]).await.unwrap().t_expired.is_none());
}

/// A second forget changes nothing.
pub async fn forget_twice_is_already_forgotten<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let s = homes(&be).await;
    let id = seed_facts(&be, &[at("once", s.h)]).await[0];
    forget(&be, id, &[s.h]).await;
    let first = be.tombstone(id).await.unwrap();
    let (outcome, _) = be
        .forget_fact(id, &[s.h], "again", Utc::now(), "rec", &new_event("s"))
        .await
        .unwrap();
    assert_eq!(outcome, ForgetOutcome::AlreadyForgotten, "[{}]", f.name());
    assert_eq!(be.tombstone(id).await.unwrap(), first, "[{}]", f.name());
}

/// An expired fact keeps its expiry and an earlier invalidity; the tombstone keeps both, and
/// an unforget writes them back exactly, leaving it out of recall.
pub async fn forget_of_an_expired_fact_keeps_its_history<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let s = homes(&be).await;
    let expired_at = Utc::now() - Duration::hours(2);
    let invalid_at = Utc::now() - Duration::hours(3);
    let mut fact = at("pruned", s.h);
    fact.t_expired = Some(expired_at);
    fact.t_invalid = Some(invalid_at);
    let id = seed_facts(&be, &[fact]).await[0];
    let before = be.get_fact(id).await.unwrap();
    assert_eq!(forget(&be, id, &[s.h]).await.0, ForgetOutcome::Forgotten);
    let after = be.get_fact(id).await.unwrap();
    assert_eq!(after.t_expired, before.t_expired, "[{}]", f.name());
    assert_eq!(after.t_invalid, before.t_invalid, "[{}]", f.name());
    let outcome = be.unforget_fact(id, Utc::now()).await.unwrap();
    assert_eq!(
        outcome,
        UnforgetOutcome::Restored {
            facts: vec![(id, false)]
        },
        "[{}]",
        f.name()
    );
    assert_eq!(
        serde_json::to_value(be.get_fact(id).await.unwrap()).unwrap(),
        serde_json::to_value(before).unwrap(),
        "[{}] restored exactly",
        f.name()
    );
}

/// A forgotten fact leaves both search channels; an unforget brings it back.
pub async fn a_forgotten_fact_leaves_lexical_and_vector_search<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let s = homes(&be).await;
    let id = seed_facts(&be, &[at("kestrel nests", s.h)]).await[0];
    let lexical = |be: Arc<dyn StorageBackend>| async move {
        be.lexical_search("kestrel", &FactFilter::default(), 5)
            .await
            .unwrap()
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>()
    };
    let vector = |be: Arc<dyn StorageBackend>| async move {
        be.vector_search(&[0.1; DIM], &FactFilter::default(), 5)
            .await
            .unwrap()
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>()
    };
    assert_eq!(lexical(Arc::clone(&be)).await, vec![id], "[{}]", f.name());
    forget(&be, id, &[s.h]).await;
    assert!(lexical(Arc::clone(&be)).await.is_empty(), "[{}]", f.name());
    assert!(vector(Arc::clone(&be)).await.is_empty(), "[{}]", f.name());
    be.unforget_fact(id, Utc::now()).await.unwrap();
    assert_eq!(lexical(Arc::clone(&be)).await, vec![id], "[{}]", f.name());
    assert_eq!(vector(Arc::clone(&be)).await, vec![id], "[{}]", f.name());
}

/// An unforget restores the prior state, drops the tombstone and appends one record.
pub async fn unforget_restores_the_prior_state_and_records_it<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let s = homes(&be).await;
    let id = seed_facts(&be, &[at("restore me", s.h)]).await[0];
    let before = be.get_fact(id).await.unwrap();
    forget(&be, id, &[s.h]).await;
    let events = be.count_events(&EventFilter::default()).await.unwrap();
    let outcome = be.unforget_fact(id, Utc::now()).await.unwrap();
    assert_eq!(
        outcome,
        UnforgetOutcome::Restored {
            facts: vec![(id, true)]
        },
        "[{}]",
        f.name()
    );
    assert_eq!(
        serde_json::to_value(be.get_fact(id).await.unwrap()).unwrap(),
        serde_json::to_value(before).unwrap(),
        "[{}]",
        f.name()
    );
    assert_eq!(be.tombstone(id).await.unwrap(), None);
    assert!(!be.is_forgotten("restore me").await.unwrap());
    assert_eq!(
        be.count_events(&EventFilter::default()).await.unwrap(),
        events + 1,
        "[{}] one unforget record",
        f.name()
    );
}

/// Given any of its facts, an unforget reverses the whole forget, and only that one.
pub async fn unforget_reverses_the_whole_forget<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let s = homes(&be).await;
    let ids = seed_facts(&be, &[at("pair", s.h), at("pair", s.u), at("other", s.h)]).await;
    forget(&be, ids[0], &[s.h, s.u]).await;
    forget(&be, ids[2], &[s.h, s.u]).await;
    let outcome = be.unforget_fact(ids[1], Utc::now()).await.unwrap();
    assert_eq!(
        outcome,
        UnforgetOutcome::Restored {
            facts: vec![(ids[0], true), (ids[1], true)]
        },
        "[{}]",
        f.name()
    );
    assert!(
        be.tombstone(ids[2]).await.unwrap().is_some(),
        "[{}]",
        f.name()
    );
}

/// A live fact is `NotForgotten`; a missing id is `NotFound`.
pub async fn unforget_of_a_live_or_missing_fact<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let id = seed_facts(&be, &[new_fact("live")]).await[0];
    let events = be.count_events(&EventFilter::default()).await.unwrap();
    assert_eq!(
        be.unforget_fact(id, Utc::now()).await.unwrap(),
        UnforgetOutcome::NotForgotten,
        "[{}]",
        f.name()
    );
    assert_eq!(
        be.unforget_fact(id + 100, Utc::now()).await.unwrap(),
        UnforgetOutcome::NotFound,
        "[{}]",
        f.name()
    );
    assert_eq!(
        be.count_events(&EventFilter::default()).await.unwrap(),
        events,
        "[{}] no record",
        f.name()
    );
}

/// The block matches the content in any scope, and nothing else.
pub async fn is_forgotten_matches_content_in_any_scope<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let s = homes(&be).await;
    let id = seed_facts(&be, &[at("blocked text", s.h)]).await[0];
    assert!(!be.is_forgotten("blocked text").await.unwrap());
    forget(&be, id, &[s.h]).await;
    assert!(
        be.is_forgotten("blocked text").await.unwrap(),
        "[{}]",
        f.name()
    );
    assert!(
        !be.is_forgotten("blocked tex").await.unwrap(),
        "[{}]",
        f.name()
    );
}

// --- erase -------------------------------------------------------------------------------

use me_types::types::{
    ActivityStatus, EraseReport, EraseTarget, NewEdge, NewLineageRecord, PromotionProvenance,
    RelationType,
};

use super::fixtures::{checkpoint, new_activity, new_summary};

fn provenance() -> PromotionProvenance {
    PromotionProvenance {
        source_count: 1,
        session_count: 0,
        date_range_start: Utc::now(),
        date_range_end: Utc::now(),
        confidence: 1.0,
        method_version: "conformance".into(),
        representative_ids: Vec::new(),
    }
}

fn edge(source: i64, target: i64, relation: RelationType) -> NewEdge {
    NewEdge {
        source_fact_id: source,
        target_fact_id: target,
        relation_type: relation,
        weight: 1.0,
        t_created: Utc::now(),
        t_expired: None,
        scope_id: 1,
    }
}

async fn erase(be: &Arc<dyn StorageBackend>, target: EraseTarget) -> EraseReport {
    be.erase(&target, Utc::now()).await.expect("erase")
}

/// The erasure records, newest last.
async fn erase_records(be: &Arc<dyn StorageBackend>) -> Vec<serde_json::Value> {
    let filter = EventFilter {
        source: Some("maintenance".into()),
        ..EventFilter::default()
    };
    be.list_events(&filter)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.payload)
        .filter(|p| p["op"] == "erase")
        .collect()
}

/// An erasure by fact takes the fact, what was derived from it and what promoted it, and
/// leaves no reference behind; a neighbour, its own edges and the tombstone's hash stay.
pub async fn erase_fact_clears_every_reference<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let s = homes(&be).await;
    let ids = seed_facts(
        &be,
        &[at("alpha kestrel", s.h), at("beta", s.h), at("gamma", s.h)],
    )
    .await;
    let (alpha, beta, gamma) = (ids[0], ids[1], ids[2]);
    let wisdom = seed_facts(&be, &[new_fact("wisdom")]).await[0];
    for (source, target) in [(alpha, beta), (beta, alpha), (alpha, wisdom), (beta, gamma)] {
        be.insert_edge(&edge(source, target, RelationType::Supplements))
            .await
            .unwrap();
    }
    be.insert_lineage(
        &NewLineageRecord {
            wisdom_fact_id: wisdom,
            source_fact_ids: vec![alpha],
        },
        &provenance(),
    )
    .await
    .unwrap();
    let mut summary = new_summary("about alpha");
    summary.source_fact_ids = vec![alpha];
    let summary = be.insert_summary(&summary).await.unwrap();
    let (activity, _) = be
        .insert_or_dedup_activity(&new_activity("s", "Edit"), 0)
        .await
        .unwrap();
    be.update_activity_status(activity, ActivityStatus::Promoted, Some(alpha))
        .await
        .unwrap();
    forget(&be, alpha, &[s.h]).await;

    let report = erase(&be, EraseTarget::Fact(alpha)).await;
    let mut erased = vec![alpha, wisdom];
    erased.sort_unstable();
    assert_eq!(report.erased_facts, erased, "[{}]", f.name());
    assert_eq!(report.deleted["facts"], 2, "[{}]", f.name());
    assert_eq!(report.deleted["edges"], 3, "[{}]", f.name());
    assert_eq!(report.deleted["summaries"], 1, "[{}]", f.name());
    assert_eq!(report.deleted["activities"], 1, "[{}]", f.name());
    assert_eq!(
        report.deleted["events"],
        0,
        "[{}] --fact deletes no event",
        f.name()
    );
    for id in [alpha, wisdom] {
        assert!(be.get_fact(id).await.is_err(), "[{}] {id} gone", f.name());
    }
    assert!(be.get_fact(beta).await.is_ok() && be.get_fact(gamma).await.is_ok());
    assert_eq!(be.list_active_edges_by_source(beta).await.unwrap().len(), 1);
    assert!(be.get_summary(summary).await.is_err(), "[{}]", f.name());
    assert!(be.get_activity(activity).await.is_err(), "[{}]", f.name());
    assert!(
        be.lexical_search("kestrel", &FactFilter::default(), 5)
            .await
            .unwrap()
            .is_empty(),
        "[{}] no full-text hit",
        f.name()
    );
    assert!(
        be.is_forgotten("alpha kestrel").await.unwrap(),
        "[{}] block stays",
        f.name()
    );
    assert_eq!(
        be.tombstone(alpha).await.unwrap(),
        None,
        "[{}] the link goes",
        f.name()
    );
}

/// An erasure by fact keeps the fact's source event and names the link in its record.
pub async fn erase_fact_keeps_its_source_event_and_links_it<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let source = be.insert_event(&new_event("s")).await.unwrap();
    let mut fact = new_fact("remembered");
    fact.source_event_id = Some(source);
    let id = seed_facts(&be, &[fact]).await[0];
    erase(&be, EraseTarget::Fact(id)).await;
    assert!(
        be.get_event(source).await.is_ok(),
        "[{}] the event stays",
        f.name()
    );
    let records = erase_records(&be).await;
    assert_eq!(records.len(), 1, "[{}]", f.name());
    assert_eq!(
        records[0]["linked"],
        serde_json::json!([{ "fact": id, "source_event": source }]),
        "[{}]",
        f.name()
    );
    assert!(records[0].get("machine").is_none(), "[{}]", f.name());
}

/// Facts a deleted successor had replaced are reported as orphans; identical text left
/// behind is reported as copies.
pub async fn erase_reports_orphans_and_copies<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let mut old = new_fact("old version");
    old.t_expired = Some(Utc::now());
    let ids = seed_facts(
        &be,
        &[new_fact("new version"), old, new_fact("new version")],
    )
    .await;
    be.insert_edge(&edge(ids[0], ids[1], RelationType::Supersedes))
        .await
        .unwrap();
    let report = erase(&be, EraseTarget::Fact(ids[0])).await;
    assert_eq!(report.orphaned, vec![ids[1]], "[{}]", f.name());
    assert_eq!(report.copies, vec![ids[2]], "[{}]", f.name());
}

/// An erasure by machine takes what names the machine or sits in its bucket, and what was
/// built on its events; another machine's rows stay.
pub async fn erase_machine_takes_what_names_it_or_sits_in_its_bucket<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let bucket = be.ensure_scope_path("unscoped:laptop").await.unwrap();
    let child = be.ensure_scope_path("unscoped:laptop/x").await.unwrap();
    let user = be.ensure_scope_path("user:alice").await.unwrap();
    let stamped = |content: &str, machine: &str| {
        let mut fact = at(content, user);
        fact.metadata = serde_json::json!({ "machine": machine });
        fact
    };
    let mut laptop_event = new_event("s");
    laptop_event.payload = serde_json::json!({ "machine": "laptop" });
    let laptop_event = be.insert_event(&laptop_event).await.unwrap();
    let mut bucket_event = new_event("s");
    bucket_event.scope_id = bucket;
    let bucket_event = be.insert_event(&bucket_event).await.unwrap();
    let mut desktop_event = new_event("s");
    desktop_event.payload = serde_json::json!({ "machine": "desktop" });
    let desktop_event = be.insert_event(&desktop_event).await.unwrap();
    let mut nested = new_event("s");
    nested.payload = serde_json::json!({ "target": { "machine": "laptop" } });
    let nested = be.insert_event(&nested).await.unwrap();
    let mut built_on = stamped("desktop built on laptop", "desktop");
    built_on.source_event_id = Some(laptop_event);
    let ids = seed_facts(
        &be,
        &[
            at("in the bucket", bucket),
            at("in a child", child),
            stamped("laptop at user", "laptop"),
            stamped("desktop at user", "desktop"),
            built_on,
        ],
    )
    .await;
    let mut activity = new_activity("s", "Edit");
    activity.scope_id = bucket;
    let (activity, _) = be.insert_or_dedup_activity(&activity, 0).await.unwrap();
    let mut saved = checkpoint("laptop-session");
    saved.scope_path = Some("unscoped:laptop".into());
    be.upsert_checkpoint(&saved).await.unwrap();
    // A forget of desktop's fact recorded by laptop: the block stays, its record goes.
    let (_, record) = be
        .forget_fact(
            ids[3],
            &[user],
            "wrong",
            Utc::now(),
            "unscoped:laptop",
            &new_event("s"),
        )
        .await
        .unwrap();

    let report = erase(&be, EraseTarget::Machine("laptop".into())).await;
    let mut taken = vec![ids[0], ids[1], ids[2], ids[4]];
    taken.sort_unstable();
    assert_eq!(report.erased_facts, taken, "[{}]", f.name());
    assert!(
        be.get_fact(ids[3]).await.is_ok(),
        "[{}] desktop's fact stays",
        f.name()
    );
    for gone in [laptop_event, bucket_event, record.id] {
        assert!(
            be.get_event(gone).await.is_err(),
            "[{}] event {gone}",
            f.name()
        );
    }
    for kept in [desktop_event, nested] {
        assert!(
            be.get_event(kept).await.is_ok(),
            "[{}] event {kept}",
            f.name()
        );
    }
    assert!(be.get_activity(activity).await.is_err(), "[{}]", f.name());
    assert_eq!(
        be.get_checkpoint("laptop-session").await.unwrap(),
        None,
        "[{}]",
        f.name()
    );
    let stone = be.tombstone(ids[3]).await.unwrap().expect("block stays");
    assert_eq!(stone.record_event_id, None, "[{}]", f.name());
    assert!(
        erase_records(&be).await[0].get("machine").is_none(),
        "[{}] the record is attributed to no machine",
        f.name()
    );
}

/// A second erasure of the same machine deletes nothing and records nothing.
pub async fn a_second_machine_erasure_deletes_nothing<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let bucket = be.ensure_scope_path("unscoped:laptop").await.unwrap();
    seed_facts(&be, &[at("once", bucket)]).await;
    erase(&be, EraseTarget::Machine("laptop".into())).await;
    let report = erase(&be, EraseTarget::Machine("laptop".into())).await;
    assert!(report.deleted.values().all(|n| *n == 0), "[{}]", f.name());
    assert_eq!(erase_records(&be).await.len(), 1, "[{}]", f.name());
}

/// A missing fact deletes nothing and records nothing; an invalid machine name is refused.
pub async fn erase_of_a_missing_fact_deletes_nothing<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let report = erase(&be, EraseTarget::Fact(42)).await;
    assert!(report.erased_facts.is_empty(), "[{}]", f.name());
    assert!(erase_records(&be).await.is_empty(), "[{}]", f.name());
    let err = be
        .erase(&EraseTarget::Machine("a/b".into()), Utc::now())
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            me_types::error::MemoryError::Conflict(
                me_types::error::ConflictError::PolicyParameter(_)
            )
        ),
        "[{}] {err:?}",
        f.name()
    );
}
