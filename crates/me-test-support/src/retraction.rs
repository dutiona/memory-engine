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
