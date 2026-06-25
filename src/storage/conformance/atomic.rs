//! Cross-trait **atomic all-or-nothing** contracts: `Ok ⇒ committed; Err ⇒ store
//! byte-identical (transaction rolled back)`.
//!
//! Generalizes the per-store oracle rollback tests (`sqlite/graph.rs`,
//! `sqlite/consolidation.rs`) backend-agnostically. Two injection styles:
//! - **Typed faults** (wrong-dim embedding / mismatched fingerprint) fault mid-tx
//!   WITHOUT dropping a table, so the full-state [`snapshot`] proves no leak ANYWHERE.
//! - **Drop-a-late-table** (`raw_exec`) is used where the typed port can't express the
//!   fault (prune's edge cascade, apply-cycle's config watermark); those read the
//!   specific non-dropped tables (the dropped one is unreadable afterward).

use chrono::Utc;

use super::factory::ConformanceBackend;
use super::fixtures::{DIM, fingerprint, new_fact, seed_facts, snapshot};
use crate::error::MemoryError;
use crate::types::{EmbeddingFingerprint, FactType, NewFact};

/// A fact whose embedding length deliberately disagrees with `DIM` (the dim injector).
fn wrong_dim_fact(content: &str) -> NewFact {
    NewFact::builder(content, vec![0.1_f32; DIM + 1], FactType::Episodic)
        .scope_id(1)
        .build()
}

/// A fingerprint that disagrees with the established identity (the model injector).
fn mismatched_fingerprint() -> EmbeddingFingerprint {
    EmbeddingFingerprint::new("other-model", "test", DIM)
}

/// `insert_fact_atomic` with a wrong-dim embedding ⇒ `Err`, store byte-identical (#614).
///
/// A non-conforming backend fails this by committing an orphan vector (the full
/// snapshot — facts + fingerprint + every other table — would then differ).
pub async fn insert_fact_atomic_rollback_on_dim_mismatch<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    seed_facts(&be, &[new_fact("keeper")]).await;
    let before = snapshot(&be).await;
    let err = be
        .insert_fact_atomic(&wrong_dim_fact("orphan"), &fingerprint(), DIM)
        .await
        .expect_err("wrong-dim embedding must be rejected");
    assert!(
        matches!(err, MemoryError::EmbeddingDimension { .. }),
        "[{}] expected EmbeddingDimension, got {err:?}",
        f.name()
    );
    let after = snapshot(&be).await;
    assert_eq!(
        before,
        after,
        "[{}] insert_fact_atomic Err must leave the store byte-identical (no orphan vector)",
        f.name()
    );
}

/// `insert_facts_batch_atomic` with mismatched `facts`/`scope_paths` lengths ⇒ a
/// precondition `Internal("…length mismatch…")` (no write attempted).
///
/// A non-conforming backend fails this by silently truncating or panicking.
pub async fn insert_facts_batch_atomic_length_mismatch<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let facts = [new_fact("x"), new_fact("y")];
    let scope_paths = [None]; // length 1 ≠ 2 facts
    let err = be
        .insert_facts_batch_atomic(&facts, &scope_paths, &fingerprint(), DIM)
        .await
        .expect_err("facts/scope_paths length mismatch must be rejected");
    assert!(
        matches!(err, MemoryError::Internal(ref m) if m.contains("length mismatch")),
        "[{}] expected Internal(length mismatch), got {err:?}",
        f.name()
    );
}

/// `insert_facts_batch_atomic` with a mismatched fingerprint ⇒ `Err`, store
/// byte-identical (the whole savepoint rolls back — no partial batch).
pub async fn insert_facts_batch_atomic_rollback<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    seed_facts(&be, &[new_fact("seed")]).await; // establishes fingerprint() + 1 fact
    let before = snapshot(&be).await;
    let facts = [new_fact("x"), new_fact("y")];
    let scope_paths = [None, None];
    let err = be
        .insert_facts_batch_atomic(&facts, &scope_paths, &mismatched_fingerprint(), DIM)
        .await
        .expect_err("mismatched fingerprint must be rejected");
    assert!(
        matches!(err, MemoryError::EmbeddingModelMismatch { .. }),
        "[{}] expected EmbeddingModelMismatch, got {err:?}",
        f.name()
    );
    let after = snapshot(&be).await;
    assert_eq!(
        before,
        after,
        "[{}] insert_facts_batch_atomic Err must leave the store byte-identical (no partial batch)",
        f.name()
    );
}

/// THE F5 data-loss proof (#631): `resolve_conflict_atomic(Update, …)` with a
/// wrong-dim successor ⇒ `Err`, and the OLD fact is still **active**.
///
/// Inside the transaction `expire_and_invalidate(old)` runs FIRST, then the successor
/// insert faults — so a backend that decomposed this into separate transactions would
/// leave `old` expired with no successor (the exact data-loss class the cutover hit).
/// A non-conforming backend fails this by leaving `old.t_expired = Some(_)`.
pub async fn resolve_conflict_atomic_rollback_leaves_old_active<F: ConformanceBackend>(f: &F) {
    use crate::traits::CrudDecision;
    let be = f.make().await;
    let old_id = seed_facts(&be, &[new_fact("old fact")]).await[0];
    let before = snapshot(&be).await;
    let err = be
        .resolve_conflict_atomic(
            CrudDecision::Update,
            old_id,
            &wrong_dim_fact("successor"),
            "contradicts",
            1.0,
            Utc::now(),
        )
        .await
        .expect_err("wrong-dim successor must fault the transaction");
    let after = snapshot(&be).await;
    // The named F5 predicate FIRST (targeted message for the data-loss class), then the
    // broad full-state snapshot as the catch-all for any other leak.
    let old = be.get_fact(old_id).await.expect("old fact still present");
    assert!(
        old.t_expired.is_none() && old.t_invalid.is_none(),
        "[{}] F5: rollback must leave the OLD fact active (not expired with no successor), got {err:?}",
        f.name()
    );
    assert_eq!(
        before,
        after,
        "[{}] resolve_conflict_atomic Err must leave the store byte-identical",
        f.name()
    );
}

/// `prune_atomic` whose edge cascade faults mid-tx ⇒ `Err`, the to-expire fact is
/// still active (rollback). Injected by dropping `edges` so the cascade `UPDATE`
/// faults AFTER the fact-expiry step.
///
/// A non-conforming backend fails this by committing the fact expiry before the
/// cascade fault.
pub async fn prune_atomic_rollback<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let ids = seed_facts(&be, &[new_fact("a"), new_fact("b")]).await;
    // Inject: drop `edges` so prune's cascade UPDATE faults after the fact expiry.
    be.raw_exec("DROP TABLE edges")
        .await
        .expect("drop edges (crash injection)");
    let scored: Vec<(i64, f64)> = ids.iter().map(|&id| (id, 0.0)).collect();
    let err = be
        .prune_atomic(&scored, &[ids[0]], Utc::now())
        .await
        .expect_err("prune must fault on the dropped edge cascade");
    // `facts` is intact: the victim must still be active (the tx rolled back).
    let facts = be.list_all_facts().await.expect("list_all_facts");
    let victim = facts
        .iter()
        .find(|x| x.id == ids[0])
        .expect("victim fact still present");
    assert!(
        victim.t_expired.is_none(),
        "[{}] prune_atomic Err must leave the fact active (rollback), got {err:?}",
        f.name()
    );
}

/// `apply_cycle_deltas_atomic` whose final config write faults mid-tx ⇒ `Err`, and NO
/// delta committed across facts, events, AND lineage. The report intentionally writes
/// all three (`Quarantine` ⇒ facts, `TagOutcome` ⇒ an `OutcomeSignal` event, `Promote`
/// ⇒ a lineage row) so EVERY rollback assertion is LOAD-BEARING — a non-atomic backend
/// that committed any delta before the config write faults would leak a fact-expiry, an
/// event, or a lineage row (the review BLOCKER class a partial snapshot would miss). A
/// `Quarantine`-only report would make the event/lineage assertions vacuously true.
///
/// Injected by dropping `config` so the final `set_config(last_dream_cycle_at)` at the
/// END of the transaction faults, AFTER all deltas have run.
pub async fn apply_cycle_deltas_atomic_rollback<F: ConformanceBackend>(f: &F) {
    use crate::engine::cycle::{
        CycleDelta, CycleMetadata, CycleReport, IdentityOutput, TimeWindow,
    };
    use crate::types::{Outcome, PromotionProvenance};
    let be = f.make().await;
    let ids = seed_facts(&be, &[new_fact("victim"), new_fact("promotable")]).await;
    let (fact_id, promotable) = (ids[0], ids[1]);
    // Inject: drop `config` so the final watermark write at tx-end faults.
    be.raw_exec("DROP TABLE config")
        .await
        .expect("drop config (crash injection)");
    let start: chrono::DateTime<Utc> = "2026-06-16T00:00:00Z".parse().expect("parse start");
    let provenance = PromotionProvenance {
        source_count: 1,
        session_count: 1,
        date_range_start: start,
        date_range_end: start,
        confidence: 0.9,
        method_version: "conformance".into(),
        representative_ids: vec![promotable],
        lineage_id: 0,
    };
    let report = CycleReport {
        deltas: vec![
            // facts write
            CycleDelta::Quarantine {
                fact_id,
                reason: "conformance".into(),
            },
            // event write (OutcomeSignal)
            CycleDelta::TagOutcome {
                fact_id,
                outcome: Outcome::Negative,
            },
            // lineage write
            CycleDelta::Promote {
                fact_id: promotable,
                provenance,
            },
        ],
        identity: IdentityOutput::empty(),
        metadata: CycleMetadata {
            cycle_id: 1,
            ran_at: start,
            time_window: TimeWindow {
                start,
                end: "2026-06-16T01:00:00Z".parse().expect("parse end"),
            },
            facts_selected: 2,
            method_version: "conformance".into(),
            processed_ids: vec![fact_id, promotable],
        },
    };
    let registry = crate::store::upcaster::UpcasterRegistry::new();
    let err = be
        .apply_cycle_deltas_atomic(&report, DIM, &registry)
        .await
        .expect_err("apply must fault on the dropped config write");
    // Targeted reads of the non-dropped tables (config is gone): the Quarantine must
    // have rolled back, and nothing may have leaked into lineage or events.
    let facts = be.list_all_facts().await.expect("list_all_facts");
    let victim = facts
        .iter()
        .find(|x| x.id == fact_id)
        .expect("victim fact still present");
    assert!(
        victim.t_expired.is_none(),
        "[{}] apply_cycle Err must leave the fact un-quarantined (rollback), got {err:?}",
        f.name()
    );
    let mut lineage = Vec::new();
    be.for_each_lineage(&mut |e| {
        lineage.push(e);
        Ok(())
    })
    .await
    .expect("for_each_lineage");
    assert!(
        lineage.is_empty(),
        "[{}] apply_cycle rollback must leak no lineage row",
        f.name()
    );
    let events = be
        .list_events(&crate::types::EventFilter::default())
        .await
        .expect("list_events");
    assert!(
        events.is_empty(),
        "[{}] apply_cycle rollback must leak no event row",
        f.name()
    );
}
