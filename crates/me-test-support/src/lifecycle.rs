//! The lifecycle delivery contract (#1025): `Lifecycle::{record_delivery, end_idle_segments}`,
//! as far as the port shows it. Segment rows and call links are a backend's own tables, asserted
//! in that backend's tests.

use std::sync::Arc;

use chrono::{DateTime, Duration, TimeZone, Utc};
use me_storage::StorageBackend;
use me_types::error::{MemoryError, Result, StorageError};
use me_types::types::lifecycle::{Applied, CaptureWrite, DeliveryWrite, Kind, OrderKey};
use me_types::types::{EventFilter, FactType, NewFact, NextEmbedding};

use super::factory::ConformanceBackend;
use super::fixtures::{new_activity, new_event, new_fact, seed_facts};

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 21, 9, 0, 0).unwrap()
}

fn key(secs: i64, seq: u32, id: &str) -> OrderKey {
    OrderKey {
        time: t0() + Duration::seconds(secs),
        seq,
        event_id: id.to_owned(),
    }
}

/// A main-agent prompt of `session` as the engine would hand it over, filed under `p`.
fn prompt(session: &str, key: OrderKey) -> CaptureWrite {
    let mut event = new_event(session);
    event.timestamp = key.time;
    CaptureWrite {
        home: "p".to_owned(),
        event,
        session_id: session.to_owned(),
        key,
        kind: Kind::Prompt,
        main_turn: true,
        boundary: false,
        final_message: false,
        activity: None,
        fact: None,
        call: None,
    }
}

/// A fact as the engine queues it: no vector yet.
fn vectorless(content: &str) -> NewFact {
    NewFact::builder(content, Vec::new(), FactType::Episodic)
        .scope_id(1)
        .build()
}

fn capture(session: &str, id: &str) -> DeliveryWrite {
    DeliveryWrite::Capture(Box::new(prompt(session, key(0, 1, id))))
}

async fn deliver_at(
    be: &Arc<dyn StorageBackend>,
    machine: &str,
    id: &str,
    body: u8,
    received_at: DateTime<Utc>,
    write: DeliveryWrite,
) -> Result<Applied> {
    be.record_delivery(machine, id, &[body; 32], received_at, write)
        .await
}

async fn deliver(
    be: &Arc<dyn StorageBackend>,
    machine: &str,
    id: &str,
    body: u8,
    write: DeliveryWrite,
) -> Result<Applied> {
    deliver_at(be, machine, id, body, Utc::now(), write).await
}

async fn events(be: &Arc<dyn StorageBackend>) -> i64 {
    be.count_events(&EventFilter::default()).await.unwrap()
}

/// A key is held once, per machine, whatever the delivery carries: the same body is a
/// `Duplicate`, another a `Collision`, across stored and quarantined deliveries, and a held key
/// writes nothing. A `Check` of a free key leaves no key behind.
pub async fn a_key_is_held_once<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let name = f.name();

    let stored = deliver(&be, "laptop", "e1", 1, capture("s", "e1"))
        .await
        .unwrap();
    assert!(
        matches!(stored, Applied::Stored { fact: None, .. }),
        "[{name}] {stored:?}"
    );
    let again = deliver(&be, "laptop", "e1", 1, capture("s", "e1"))
        .await
        .unwrap();
    assert_eq!(again, Applied::Duplicate, "[{name}]");
    let other = deliver(&be, "laptop", "e1", 2, capture("s", "e1"))
        .await
        .unwrap();
    assert_eq!(other, Applied::Collision, "[{name}]");

    let quarantine = || DeliveryWrite::Quarantine(serde_json::json!({ "size": 3 }));
    let first = deliver(&be, "laptop", "q1", 1, quarantine()).await.unwrap();
    assert_eq!(first, Applied::Quarantined, "[{name}]");
    for (body, write, expected) in [
        (1, quarantine(), Applied::Duplicate),
        (1, capture("s", "q1"), Applied::Duplicate),
        (1, DeliveryWrite::Check, Applied::Duplicate),
        (2, quarantine(), Applied::Collision),
        (2, capture("s", "q1"), Applied::Collision),
    ] {
        let got = deliver(&be, "laptop", "q1", body, write).await.unwrap();
        assert_eq!(got, expected, "[{name}] body {body}");
    }

    let check = deliver(&be, "laptop", "free", 1, DeliveryWrite::Check).await;
    assert_eq!(check.unwrap(), Applied::Absent, "[{name}]");
    let later = deliver(&be, "laptop", "free", 1, capture("s", "free"))
        .await
        .unwrap();
    assert!(
        matches!(later, Applied::Stored { .. }),
        "[{name}] a check holds no key"
    );

    let desktop = deliver(&be, "desktop", "e1", 2, capture("s", "e1"))
        .await
        .unwrap();
    assert!(
        matches!(desktop, Applied::Stored { .. }),
        "[{name}] the key is per machine"
    );
    assert_eq!(
        events(&be).await,
        3,
        "[{name}] only the stored deliveries wrote"
    );
}

/// A delivery that faults at its last step leaves nothing of the earlier ones: no event, no
/// activity, no memory, no queue row and no key.
pub async fn a_delivery_commits_whole_or_not_at_all<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let name = f.name();
    f.refuse_segment_inserts(&be)
        .await
        .expect("inject the fault");
    let mut write = prompt("s", key(0, 1, "e1"));
    write.activity = Some((new_activity("s", "Bash"), 300));
    write.fact = Some(vectorless("committed with its event"));

    let err = deliver(
        &be,
        "laptop",
        "e1",
        1,
        DeliveryWrite::Capture(Box::new(write)),
    )
    .await
    .unwrap_err();

    assert!(
        matches!(err, MemoryError::Storage(StorageError::Backend(_))),
        "[{name}] {err:?}"
    );
    assert_eq!(events(&be).await, 0, "[{name}] event");
    assert_eq!(
        be.count_activities_by_session("s").await.unwrap(),
        0,
        "[{name}] activity"
    );
    assert!(
        be.list_active_facts(None).await.unwrap().is_empty(),
        "[{name}] fact"
    );
    let queue = be
        .next_embedding(Utc::now() + Duration::days(1))
        .await
        .unwrap();
    assert!(matches!(queue, NextEmbedding::Idle), "[{name}] queue row");
    let key = deliver(&be, "laptop", "e1", 1, DeliveryWrite::Check)
        .await
        .unwrap();
    assert_eq!(key, Applied::Absent, "[{name}] receipt");
}

/// A store another writer holds locked answers `Unavailable` and writes nothing; once the lock is
/// released the same delivery is stored.
pub async fn a_busy_store_is_unavailable<F: ConformanceBackend>(f: &F) {
    let (be, hold) = f.make_contended().await;
    let name = f.name();

    let err = deliver(&be, "laptop", "e1", 1, capture("s", "e1"))
        .await
        .unwrap_err();

    assert!(
        matches!(err, MemoryError::Storage(StorageError::Unavailable(_))),
        "[{name}] {err:?}"
    );
    assert_eq!(events(&be).await, 0, "[{name}]");
    drop(hold);
    let stored = deliver(&be, "laptop", "e1", 1, capture("s", "e1"))
        .await
        .unwrap();
    assert!(
        matches!(stored, Applied::Stored { .. }),
        "[{name}] {stored:?}"
    );
}

/// The sweep ends each quiet session's open segment once: a second call ends none of them again.
pub async fn idle_segments_end_once<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let name = f.name();
    let now = t0() + Duration::hours(2);
    let write = |session: &str, id: &str, secs| {
        DeliveryWrite::Capture(Box::new(prompt(session, key(secs, 1, id))))
    };
    deliver_at(&be, "laptop", "a", 1, t0(), write("s1", "a", 0))
        .await
        .unwrap();
    let ten = t0() + Duration::minutes(10);
    deliver_at(&be, "laptop", "b", 1, ten, write("s2", "b", 600))
        .await
        .unwrap();

    let sweep = |idle_before| {
        let be = Arc::clone(&be);
        async move { be.end_idle_segments(idle_before, now).await.unwrap() }
    };

    assert_eq!(
        sweep(t0() + Duration::minutes(5)).await,
        1,
        "[{name}] s1 only"
    );
    assert_eq!(sweep(t0() + Duration::minutes(5)).await, 0, "[{name}] once");
    assert_eq!(
        sweep(t0() + Duration::minutes(20)).await,
        1,
        "[{name}] s2 now"
    );
    assert_eq!(
        sweep(t0() + Duration::minutes(20)).await,
        0,
        "[{name}] once"
    );
}

/// A text a tombstone blocks stores its event and no memory; any other text still becomes one.
pub async fn a_tombstoned_text_makes_no_fact<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let name = f.name();
    let home = be.ensure_scope_path("h").await.unwrap();
    let mut seed = new_fact("never again");
    seed.scope_id = home;
    let id = seed_facts(&be, &[seed]).await[0];
    be.forget_fact(id, &[home], "wrong", Utc::now(), "rec", &new_event("s"))
        .await
        .unwrap();

    let mut blocked = prompt("s", key(0, 1, "e1"));
    blocked.fact = Some(vectorless("never again"));
    let blocked = deliver(
        &be,
        "laptop",
        "e1",
        1,
        DeliveryWrite::Capture(Box::new(blocked)),
    )
    .await
    .unwrap();
    let Applied::Stored { event, fact, .. } = blocked else {
        panic!("[{name}] {blocked:?}");
    };
    assert_eq!(fact, None, "[{name}] the blocked text makes no memory");
    assert!(
        be.get_event(event).await.is_ok(),
        "[{name}] its event is stored"
    );

    let mut free = prompt("s", key(1, 1, "e2"));
    free.fact = Some(vectorless("something else"));
    let free = deliver(
        &be,
        "laptop",
        "e2",
        1,
        DeliveryWrite::Capture(Box::new(free)),
    )
    .await
    .unwrap();
    let Applied::Stored {
        fact: Some(made), ..
    } = free
    else {
        panic!("[{name}] {free:?}");
    };
    let stored = be.get_fact(made).await.unwrap();
    assert_eq!(stored.content, "something else", "[{name}]");
    assert!(
        stored.embedding.is_empty(),
        "[{name}] queued without a vector"
    );
    let queue = be
        .next_embedding(Utc::now() + Duration::days(1))
        .await
        .unwrap();
    assert!(
        matches!(queue, NextEmbedding::Due(p) if p.fact_id == made),
        "[{name}]"
    );
}
