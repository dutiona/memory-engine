//! Cross-cutting read-only-rejection contract.

use super::factory::ConformanceBackend;
use super::fixtures::{checkpoint, new_event, new_fact, new_summary};
use crate::error::MemoryError;

/// A write of EACH trait family is rejected with `ReadOnly`; a representative read
/// still succeeds. A non-conforming backend fails this by accepting any write or
/// erroring the read.
pub async fn write_rejected_reads_succeed<F: ConformanceBackend>(f: &F) {
    let be = f.make_read_only().await;

    macro_rules! assert_ro {
        ($call:expr, $label:literal) => {{
            let err = $call
                .await
                .expect_err(concat!($label, " on a read-only backend must be rejected"));
            assert!(
                matches!(err, MemoryError::ReadOnly),
                "[{}] {} must be ReadOnly, got {err:?}",
                f.name(),
                $label
            );
        }};
    }

    assert_ro!(be.insert_fact(&new_fact("nope")), "FactGraph::insert_fact");
    assert_ro!(be.insert_event(&new_event("ro")), "EventLog::insert_event");
    assert_ro!(
        be.insert_summary(&new_summary("ro")),
        "ConsolidationStore::insert_summary"
    );
    assert_ro!(
        be.upsert_checkpoint(&checkpoint("ro")),
        "SessionStore::upsert_checkpoint"
    );
    assert_ro!(be.set_config("k", "v"), "SchemaManager::set_config");

    // A representative read still works on a read-only backend.
    be.list_active_facts(None)
        .await
        .expect("read must still work on a read-only backend");
}
