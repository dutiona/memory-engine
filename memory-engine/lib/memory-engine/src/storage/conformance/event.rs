//! `EventLog` contract bodies.

use super::factory::ConformanceBackend;
use super::fixtures::{new_event, new_fact, seed_facts};
use crate::error::MemoryError;
use crate::types::{EventFilter, EventType, NewEvent};

/// insert → get round-trips the event.
pub async fn insert_get_round_trip<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let id = be
        .insert_event(&new_event("sess"))
        .await
        .expect("insert_event");
    let got = be.get_event(id).await.expect("get_event");
    assert_eq!(got.id, id, "[{}] event id", f.name());
    assert_eq!(
        got.session_id.as_deref(),
        Some("sess"),
        "[{}] event session_id",
        f.name()
    );
    assert_eq!(
        got.event_type,
        EventType::Interaction,
        "[{}] event type",
        f.name()
    );
}

/// `get_event(missing)` yields `NotFound`.
pub async fn get_missing_yields_not_found<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let err = be
        .get_event(999_999)
        .await
        .expect_err("missing event must be NotFound");
    assert!(
        matches!(err, MemoryError::NotFound(_)),
        "[{}] expected NotFound, got {err:?}",
        f.name()
    );
}

/// `list_events(filter).len() == count_events(filter)` (the push-down parity).
pub async fn list_count_parity<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    for i in 0..3 {
        be.insert_event(&new_event(&format!("s{i}")))
            .await
            .expect("insert");
    }
    let filter = EventFilter::default();
    let listed = be.list_events(&filter).await.expect("list").len();
    let counted = be.count_events(&filter).await.expect("count");
    assert_eq!(
        i64::try_from(listed).expect("event count fits i64"),
        counted,
        "[{}] list/count parity",
        f.name()
    );
    assert_eq!(counted, 3, "[{}] count must be 3", f.name());
}

/// `for_each_event` delivers every row (asserted as a SET — the trait doc promises no
/// ordering, so pinning a delivery order would couple to a backend artifact and
/// false-fail a conforming `PgBackend`), and a callback `Err` at row k propagates that
/// exact error and stops early.
pub async fn for_each_order_and_callback_error<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let mut inserted = std::collections::HashSet::new();
    for i in 0..5 {
        inserted.insert(
            be.insert_event(&new_event(&format!("s{i}")))
                .await
                .expect("insert"),
        );
    }
    let mut seen = std::collections::HashSet::new();
    be.for_each_event(&mut |e| {
        seen.insert(e.id);
        Ok(())
    })
    .await
    .expect("for_each_event");
    assert_eq!(
        seen,
        inserted,
        "[{}] for_each_event must deliver exactly the inserted events (set membership, not a backend-specific order)",
        f.name()
    );

    let mut count = 0;
    let err = be
        .for_each_event(&mut |_e| {
            count += 1;
            if count == 2 {
                return Err(MemoryError::Internal("stop".into()));
            }
            Ok(())
        })
        .await
        .expect_err("callback error must propagate");
    assert!(
        matches!(err, MemoryError::Internal(ref m) if m == "stop"),
        "[{}] the callback error must win, got {err:?}",
        f.name()
    );
    assert_eq!(
        count,
        2,
        "[{}] for_each must stop early at the erroring row",
        f.name()
    );
}

/// The upcasted-read path is WIRED: for a current-revision event with the (empty)
/// registry, `get_upcasted_event` equals the raw `get_event` (proving the path is
/// present and non-corrupting). The registry-LIFT of a downgraded revision stays a
/// per-backend golden test (it needs a backend-constructed upcaster).
pub async fn upcasted_read_is_wired<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let id = be.insert_event(&new_event("sess")).await.expect("insert");
    let raw = be.get_event(id).await.expect("raw");
    let upcasted = be.get_upcasted_event(id).await.expect("upcasted");
    assert_eq!(
        raw,
        upcasted,
        "[{}] upcasted read of a current-revision event must equal the raw read",
        f.name()
    );
}

/// `count_outcome_signals[_batch]` aggregates `OutcomeSignal` events per fact; a fact
/// with no outcomes (incl. an empty batch) is absent / returns the empty map.
pub async fn count_outcome_signals_and_batch<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let fid = seed_facts(&be, &[new_fact("rated")]).await[0];
    for outcome in ["Positive", "Positive", "Negative"] {
        let ev = NewEvent {
            event_type: EventType::OutcomeSignal,
            payload: serde_json::json!({ "fact_id": fid, "outcome": outcome }),
            ..new_event("outcome-sess")
        };
        be.insert_event(&ev).await.expect("insert outcome");
    }
    let counts = be.count_outcome_signals(fid).await.expect("count outcomes");
    assert_eq!(counts.positive, 2, "[{}] positive outcomes", f.name());
    assert_eq!(counts.negative, 1, "[{}] negative outcomes", f.name());

    let batch = be
        .count_outcome_signals_batch(&[fid])
        .await
        .expect("batch counts");
    assert_eq!(
        batch.get(&fid).map(|c| c.positive),
        Some(2),
        "[{}] batch positive outcomes",
        f.name()
    );
    assert!(
        be.count_outcome_signals_batch(&[])
            .await
            .expect("empty batch")
            .is_empty(),
        "[{}] empty fact_ids must return an empty map (no query)",
        f.name()
    );
}
