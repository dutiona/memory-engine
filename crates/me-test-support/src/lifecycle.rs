//! The lifecycle delivery contract (#1025, #1026): `Lifecycle::{record_delivery,
//! record_deliveries, end_idle_segments}`, as far as the port shows it. Segment rows and call
//! links are a backend's own tables, asserted in that backend's tests.

use std::sync::Arc;

use chrono::{DateTime, Duration, TimeZone, Utc};
use me_storage::StorageBackend;
use me_types::error::{MemoryError, Result, StorageError};
use me_types::types::lifecycle::{
    Applied, Batch, BatchMode, CaptureWrite, DeliveryWrite, Keyed, Kind, MemoryWrite, OrderKey,
};
use me_types::types::{EventFilter, FactType, NewFact, NextEmbedding};

use super::factory::ConformanceBackend;
use super::fixtures::{new_activity, new_event, new_fact, seed_facts};

pub fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 21, 9, 0, 0).unwrap()
}

pub fn key(secs: i64, seq: u32, id: &str) -> OrderKey {
    OrderKey {
        time: t0() + Duration::seconds(secs),
        seq,
        event_id: id.to_owned(),
    }
}

/// A main-agent prompt of `session` as the engine would hand it over, filed under `p`.
pub fn prompt(session: &str, key: OrderKey) -> CaptureWrite {
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
pub fn vectorless(content: &str) -> NewFact {
    NewFact::builder(content, Vec::new(), FactType::Episodic)
        .scope_id(1)
        .build()
}

pub fn capture(session: &str, id: &str) -> DeliveryWrite {
    DeliveryWrite::Capture(Box::new(prompt(session, key(0, 1, id))))
}

pub async fn deliver_at(
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

pub async fn deliver(
    be: &Arc<dyn StorageBackend>,
    machine: &str,
    id: &str,
    body: u8,
    write: DeliveryWrite,
) -> Result<Applied> {
    deliver_at(be, machine, id, body, Utc::now(), write).await
}

pub async fn events(be: &Arc<dyn StorageBackend>) -> i64 {
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

// ── An upload's deliveries in one transaction (#1026) ───────────────────────────

/// The digest the engine computes over an upload's effects; any fixed value serves the store.
const CONTEXT: [u8; 32] = [9; 32];

pub fn keyed(id: &str, body: u8, write: DeliveryWrite) -> Keyed {
    Keyed {
        event_id: id.to_owned(),
        body_sha256: [body; 32],
        write,
    }
}

/// An imported prompt of `session` at `secs`, labelled as the engine labels one.
pub fn imported(session: &str, secs: i64, seq: u32, id: &str) -> CaptureWrite {
    let mut write = prompt(session, key(secs, seq, id));
    "backfill".clone_into(&mut write.event.source);
    "backfill".clone_into(&mut write.event.origin_node_id);
    write
}

pub fn import(session: &str, secs: i64, id: &str) -> DeliveryWrite {
    DeliveryWrite::Capture(Box::new(imported(session, secs, 1, id)))
}

/// An imported final message of `session` at `secs` that becomes the memory `text`.
pub fn import_final(session: &str, secs: i64, id: &str, text: &str) -> DeliveryWrite {
    let mut write = imported(session, secs, 1, id);
    write.kind = Kind::Stop;
    write.final_message = true;
    write.fact = Some(vectorless(text));
    DeliveryWrite::Capture(Box::new(write))
}

/// A live capture of `machine`, as the lifecycle route stores one: its payload names the
/// machine and the order key's seq and event ID.
pub fn live(machine: &str, session: &str, key: &OrderKey) -> DeliveryWrite {
    let mut write = prompt(session, key.clone());
    "lifecycle".clone_into(&mut write.event.source);
    write.event.payload = serde_json::json!({
        "machine": machine, "event_id": key.event_id, "seq": key.seq,
    });
    DeliveryWrite::Capture(Box::new(write))
}

/// An imported memory `text` filed under `home`, with its event.
pub fn memory(home: &str, text: &str) -> DeliveryWrite {
    let mut event = new_event("");
    event.session_id = None;
    "backfill".clone_into(&mut event.source);
    DeliveryWrite::Memory(Box::new(MemoryWrite {
        home: home.to_owned(),
        event,
        fact: vectorless(text),
    }))
}

pub async fn batch(be: &Arc<dyn StorageBackend>, items: Vec<Keyed>, mode: BatchMode) -> Batch {
    be.record_deliveries("laptop", t0(), items, CONTEXT, mode)
        .await
        .unwrap()
}

/// Each item's outcome, without its row ids.
pub fn outcomes(applied: &[Applied]) -> Vec<String> {
    applied
        .iter()
        .map(|a| match a {
            Applied::Stored {
                fact, reinforced, ..
            } => format!("stored fact={} reinforced={reinforced}", fact.is_some()),
            other => format!("{other:?}"),
        })
        .collect()
}

/// What the port shows of a store: its events, its active facts, its queue, its activities and
/// whether each of `keys` is held.
pub async fn state(be: &Arc<dyn StorageBackend>, keys: &[&str]) -> String {
    let facts: Vec<(i64, String, i64)> = be
        .list_active_facts(None)
        .await
        .unwrap()
        .into_iter()
        .map(|f| (f.id, f.content, f.access_count))
        .collect();
    let mut held = Vec::new();
    for key in keys {
        held.push(
            deliver(be, "laptop", key, 0, DeliveryWrite::Check)
                .await
                .unwrap(),
        );
    }
    format!(
        "events={} facts={facts:?} queue={:?} activities={} held={held:?}",
        events(be).await,
        be.next_embedding(t0() + Duration::days(1)).await.unwrap(),
        be.count_activities_by_session("s").await.unwrap(),
    )
}

fn an_upload() -> Vec<Keyed> {
    let mut tool = imported("s", 1, 1, "b");
    tool.kind = Kind::ToolUse;
    tool.activity = Some((new_activity("s", "Bash"), 300));
    vec![
        keyed("a", 1, import("s", 0, "a")),
        keyed("b", 1, DeliveryWrite::Capture(Box::new(tool))),
        keyed(
            "c",
            1,
            import_final("s", 2, "c", "an imported final message"),
        ),
        keyed("d", 1, memory("h", "an imported memory")),
    ]
}

const UPLOAD_KEYS: [&str; 4] = ["a", "b", "c", "d"];

/// A batch that faults at its third item, after that item's event, segment step, memory and
/// queue row, leaves nothing of any item; the same batch on a sound store commits.
pub async fn a_batch_commits_whole_or_not_at_all<F: ConformanceBackend>(f: &F) {
    let (be, twin) = (f.make().await, f.make().await);
    let name = f.name();
    let preview = batch(&twin, an_upload(), BatchMode::DryRun).await.preview;
    f.refuse_receipts_after(&be, 2)
        .await
        .expect("inject the fault");
    let before = state(&be, &UPLOAD_KEYS).await;

    let err = be
        .record_deliveries(
            "laptop",
            t0(),
            an_upload(),
            CONTEXT,
            BatchMode::Write { preview },
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, MemoryError::Storage(StorageError::Backend(_))),
        "[{name}] {err:?}"
    );
    assert_eq!(state(&be, &UPLOAD_KEYS).await, before, "[{name}]");
    let sound = batch(&twin, an_upload(), BatchMode::Write { preview }).await;
    assert!(
        sound.committed,
        "[{name}] the same batch commits on a sound store"
    );
}

/// A dry run writes nothing, and its outcomes and preview are those of the write that commits
/// the same batch, on its own store and on a twin.
pub async fn a_dry_run_batch_changes_nothing<F: ConformanceBackend>(f: &F) {
    let (be, twin) = (f.make().await, f.make().await);
    let name = f.name();
    let before = state(&be, &UPLOAD_KEYS).await;

    let dry = batch(&be, an_upload(), BatchMode::DryRun).await;

    assert!(!dry.committed, "[{name}]");
    assert_eq!(
        state(&be, &UPLOAD_KEYS).await,
        before,
        "[{name}] nothing written"
    );
    assert_eq!(
        outcomes(&dry.applied),
        [
            "stored fact=false reinforced=false",
            "stored fact=false reinforced=false",
            "stored fact=true reinforced=false",
            "stored fact=true reinforced=false",
        ],
        "[{name}]"
    );
    let preview = dry.preview;
    for store in [&twin, &be] {
        let write = batch(store, an_upload(), BatchMode::Write { preview }).await;
        assert!(write.committed, "[{name}]");
        assert_eq!(write.preview, preview, "[{name}]");
        assert_eq!(outcomes(&write.applied), outcomes(&dry.applied), "[{name}]");
    }
    assert_ne!(
        state(&be, &UPLOAD_KEYS).await,
        before,
        "[{name}] the write wrote"
    );
}

/// A key held with another body is a collision: the dry run lists it, and the write that
/// carries the dry run's own preview still commits nothing.
pub async fn a_collision_rolls_the_batch_back<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let name = f.name();
    deliver(&be, "laptop", "b", 7, capture("s", "b"))
        .await
        .unwrap();
    let items = || {
        vec![
            keyed("a", 1, import("s", 0, "a")),
            keyed("b", 1, import("s", 1, "b")),
        ]
    };
    let before = state(&be, &["a"]).await;

    let dry = batch(&be, items(), BatchMode::DryRun).await;
    assert_eq!(dry.applied[1], Applied::Collision, "[{name}]");
    let write = batch(
        &be,
        items(),
        BatchMode::Write {
            preview: dry.preview,
        },
    )
    .await;

    assert!(!write.committed, "[{name}]");
    assert_eq!(write.applied[1], Applied::Collision, "[{name}]");
    assert_eq!(state(&be, &["a"]).await, before, "[{name}] nothing written");
}

/// A write lands only the effects its dry run showed: another preview, or a live capture of the
/// session stored between the dry run and the write, rolls it back whole.
pub async fn a_write_commits_only_on_its_preview<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let name = f.name();
    let items = || {
        vec![
            keyed("a", 1, import("s", 0, "a")),
            keyed("c", 1, import("s", 2, "c")),
        ]
    };
    let dry = batch(&be, items(), BatchMode::DryRun).await;
    let before = state(&be, &["a", "c"]).await;

    let other = batch(&be, items(), BatchMode::Write { preview: [0; 32] }).await;
    assert!(!other.committed, "[{name}] another preview");
    assert_eq!(
        other.preview, dry.preview,
        "[{name}] the store did not change"
    );
    assert_eq!(state(&be, &["a", "c"]).await, before, "[{name}]");

    deliver(
        &be,
        "laptop",
        "live",
        1,
        live("laptop", "s", &key(1, 1, "live")),
    )
    .await
    .unwrap();
    let before = state(&be, &["a", "c"]).await;
    let stale = batch(
        &be,
        items(),
        BatchMode::Write {
            preview: dry.preview,
        },
    )
    .await;
    assert!(!stale.committed, "[{name}] a live capture came in between");
    assert_ne!(stale.preview, dry.preview, "[{name}]");
    assert_eq!(
        state(&be, &["a", "c"]).await,
        before,
        "[{name}] nothing written"
    );
}

/// Two items of one batch with one key: the first is stored, the second meets it as a
/// duplicate (the same body) or a collision (another), inside the same transaction.
pub async fn a_key_written_earlier_in_the_batch_is_a_duplicate<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let name = f.name();
    let items = vec![
        keyed("a", 1, import("s", 0, "a")),
        keyed("a", 1, import("forked", 0, "a")),
        keyed("a", 2, import("s", 0, "a")),
    ];
    let dry = batch(&be, items.clone(), BatchMode::DryRun).await;
    assert_eq!(
        outcomes(&dry.applied),
        [
            "stored fact=false reinforced=false",
            "Duplicate",
            "Collision"
        ],
        "[{name}]"
    );
    let write = batch(
        &be,
        items,
        BatchMode::Write {
            preview: dry.preview,
        },
    )
    .await;
    assert!(!write.committed, "[{name}] a collision is never written");
    let items = vec![
        keyed("a", 1, import("s", 0, "a")),
        keyed("a", 1, import("forked", 0, "a")),
    ];
    let dry = batch(&be, items.clone(), BatchMode::DryRun).await;
    let write = batch(
        &be,
        items,
        BatchMode::Write {
            preview: dry.preview,
        },
    )
    .await;
    assert!(write.committed, "[{name}]");
    assert_eq!(events(&be).await, 1, "[{name}] one row for one key");
}

/// A session captured live is authoritative from its first live event, of this machine only:
/// an imported capture at or after it is dropped, an earlier one and another session's kept.
pub async fn a_live_event_drops_the_items_at_or_after_it<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let name = f.name();
    let first_live = key(10, 5, "live-1");
    for (machine, id, at) in [
        ("laptop", "live-2", key(30, 1, "live-2")),
        ("laptop", "live-1", first_live.clone()),
        ("desktop", "elsewhere", key(0, 1, "elsewhere")),
    ] {
        deliver(&be, machine, id, 1, live(machine, "s", &at))
            .await
            .unwrap();
    }
    let at = |session: &str, secs, seq, id: &str| {
        keyed(
            id,
            1,
            DeliveryWrite::Capture(Box::new(imported(session, secs, seq, id))),
        )
    };
    let items = vec![
        at("s", 5, 1, "before"),
        at("s", 10, 4, "just-before"),
        at("s", 10, 6, "just-after"),
        at("s", 20, 1, "after"),
        at("other", 20, 1, "other-session"),
    ];

    let dry = batch(&be, items.clone(), BatchMode::DryRun).await;

    assert_eq!(
        outcomes(&dry.applied),
        [
            "stored fact=false reinforced=false",
            "stored fact=false reinforced=false",
            "LiveOverlap",
            "LiveOverlap",
            "stored fact=false reinforced=false",
        ],
        "[{name}]"
    );
    assert_eq!(
        dry.live_since.get("s"),
        Some(&first_live),
        "[{name}] the earliest live event of this machine"
    );
    assert_eq!(dry.live_since.get("other"), None, "[{name}]");
    let before = events(&be).await;
    let write = batch(
        &be,
        items,
        BatchMode::Write {
            preview: dry.preview,
        },
    )
    .await;
    assert!(write.committed, "[{name}]");
    assert_eq!(
        events(&be).await,
        before + 3,
        "[{name}] the overlap wrote nothing"
    );
}

/// A memory whose text a forget's tombstone blocks writes no fact and reinforces nothing, even
/// when an active twin of it lives in its home; its event and key are still stored.
pub async fn a_tombstone_blocks_even_with_an_active_twin<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let name = f.name();
    let home = be.ensure_scope_path("h").await.unwrap();
    let other = be.ensure_scope_path("o").await.unwrap();
    let mut twin = new_fact("forgotten elsewhere");
    twin.scope_id = home;
    let mut copy = twin.clone();
    copy.scope_id = other;
    let ids = seed_facts(&be, &[twin, copy]).await;
    be.forget_fact(
        ids[1],
        &[other],
        "wrong",
        Utc::now(),
        "rec",
        &new_event("s"),
    )
    .await
    .unwrap();

    let items = vec![keyed("m", 1, memory("h", "forgotten elsewhere"))];
    let dry = batch(&be, items.clone(), BatchMode::DryRun).await;
    let write = batch(
        &be,
        items,
        BatchMode::Write {
            preview: dry.preview,
        },
    )
    .await;

    assert!(write.committed, "[{name}]");
    assert_eq!(
        outcomes(&write.applied),
        ["stored fact=false reinforced=false"],
        "[{name}] blocked"
    );
    assert_eq!(
        be.get_fact(ids[0]).await.unwrap().access_count,
        0,
        "[{name}] the twin is not reinforced"
    );
}

/// An imported memory with an active twin in its home reinforces it; otherwise it is stored
/// without a vector, queued, and linked to its event, in its home only.
pub async fn an_imported_memory_reinforces_or_queues<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let name = f.name();
    let write = |items: Vec<Keyed>| {
        let be = Arc::clone(&be);
        async move {
            let dry = batch(&be, items.clone(), BatchMode::DryRun).await;
            let write = batch(
                &be,
                items,
                BatchMode::Write {
                    preview: dry.preview,
                },
            )
            .await;
            assert!(write.committed);
            write.applied
        }
    };

    let first = write(vec![keyed("m1", 1, memory("h", "a memory"))]).await;
    let Applied::Stored {
        event,
        fact: Some(made),
        reinforced: false,
        ..
    } = first[0]
    else {
        panic!("[{name}] {first:?}");
    };
    let fact = be.get_fact(made).await.unwrap();
    assert!(
        fact.embedding.is_empty(),
        "[{name}] queued without a vector"
    );
    assert_eq!(fact.source_event_id, Some(event), "[{name}]");
    assert_eq!(
        fact.scope_id,
        be.ensure_scope_path("h").await.unwrap(),
        "[{name}] filed in its home"
    );
    let queue = be.next_embedding(t0() + Duration::days(1)).await.unwrap();
    assert!(
        matches!(queue, NextEmbedding::Due(p) if p.fact_id == made),
        "[{name}]"
    );

    let twin = write(vec![keyed("m2", 1, memory("h", "a memory"))]).await;
    assert!(
        matches!(twin[0], Applied::Stored { fact: Some(id), reinforced: true, .. } if id == made),
        "[{name}] {twin:?}"
    );
    assert_eq!(be.get_fact(made).await.unwrap().access_count, 1, "[{name}]");

    let elsewhere = write(vec![keyed("m3", 1, memory("o", "a memory"))]).await;
    assert!(
        matches!(elsewhere[0], Applied::Stored { fact: Some(id), reinforced: false, .. } if id != made),
        "[{name}] another home is another memory"
    );
}
