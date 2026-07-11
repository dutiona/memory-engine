//! `ConsolidationStore` (summaries + lineage) contract bodies.

use me_types::error::MemoryError;
use me_types::types::{ConsolidationLevel, NewLineageRecord, PromotionProvenance};

use super::factory::ConformanceBackend;
use super::fixtures::{new_fact, new_summary, seed_facts};

/// Summary insert → get → list-by-level → delete-by-level.
pub async fn summary_insert_list_get_delete_by_level<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let id = be
        .insert_summary(&new_summary("cluster summary"))
        .await
        .expect("insert_summary");
    let got = be.get_summary(id).await.expect("get_summary");
    assert_eq!(
        got.content,
        "cluster summary",
        "[{}] summary content",
        f.name()
    );
    assert_eq!(
        got.level,
        ConsolidationLevel::Cluster,
        "[{}] summary level",
        f.name()
    );
    assert!(
        be.list_summaries_by_level(&ConsolidationLevel::Cluster)
            .await
            .expect("list cluster")
            .iter()
            .any(|s| s.id == id),
        "[{}] list_summaries_by_level must include the summary",
        f.name()
    );
    assert!(
        be.list_summaries_by_level(&ConsolidationLevel::Global)
            .await
            .expect("list global")
            .is_empty(),
        "[{}] another level must be empty",
        f.name()
    );
    let deleted = be
        .delete_summaries_by_level(&ConsolidationLevel::Cluster)
        .await
        .expect("delete by level");
    assert_eq!(deleted, 1, "[{}] delete_summaries_by_level count", f.name());
    assert!(
        be.list_summaries_by_level(&ConsolidationLevel::Cluster)
            .await
            .expect("list after delete")
            .is_empty(),
        "[{}] the level must be empty after delete",
        f.name()
    );
}

/// `for_each_summary` delivers all rows (parity with `list_all_summaries`) and a
/// callback `Err` stops early.
pub async fn for_each_summary_parity_and_early_exit<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    for i in 0..3 {
        be.insert_summary(&new_summary(&format!("s{i}")))
            .await
            .expect("insert");
    }
    let mut seen = Vec::new();
    be.for_each_summary(&mut |s| {
        seen.push(s.id);
        Ok(())
    })
    .await
    .expect("for_each_summary");
    assert_eq!(
        seen.len(),
        be.list_all_summaries().await.expect("list_all").len(),
        "[{}] for_each_summary must reach parity with list_all_summaries",
        f.name()
    );

    let mut count = 0;
    let err = be
        .for_each_summary(&mut |_s| {
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
    assert_eq!(count, 2, "[{}] for_each_summary must stop early", f.name());
}

/// Lineage insert → has → get → source-ids → delete (the wisdom-provenance contract).
pub async fn lineage_insert_get_has_delete_sources<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let ids = seed_facts(
        &be,
        &[new_fact("wisdom"), new_fact("src-a"), new_fact("src-b")],
    )
    .await;
    let (wisdom, src_a, src_b) = (ids[0], ids[1], ids[2]);
    let record = NewLineageRecord {
        wisdom_fact_id: wisdom,
        source_fact_ids: vec![src_a, src_b],
    };
    let prov = PromotionProvenance {
        source_count: 2,
        session_count: 1,
        date_range_start: chrono::Utc::now(),
        date_range_end: chrono::Utc::now(),
        confidence: 0.9,
        method_version: "conformance".into(),
        representative_ids: vec![src_a, src_b],
    };
    be.insert_lineage(&record, &prov)
        .await
        .expect("insert_lineage");
    assert!(
        be.has_lineage(wisdom).await.expect("has_lineage"),
        "[{}] has_lineage must be true after insert",
        f.name()
    );
    let (rec, _prov) = be
        .get_lineage_by_wisdom_fact(wisdom)
        .await
        .expect("get_lineage_by_wisdom_fact");
    assert_eq!(
        rec.wisdom_fact_id,
        wisdom,
        "[{}] lineage wisdom_fact_id",
        f.name()
    );
    let mut sources = be
        .get_lineage_source_fact_ids(wisdom)
        .await
        .expect("source ids");
    sources.sort_unstable();
    assert_eq!(
        sources,
        vec![src_a, src_b],
        "[{}] lineage source fact ids",
        f.name()
    );
    assert!(
        be.delete_lineage(wisdom).await.expect("delete_lineage"),
        "[{}] delete_lineage must return true",
        f.name()
    );
    assert!(
        !be.has_lineage(wisdom).await.expect("has after delete"),
        "[{}] has_lineage must be false after delete",
        f.name()
    );
}
