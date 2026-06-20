//! `impl ConsolidationStore for SqliteBackend` — delegates to [`SummaryStore`]
//! and [`LineageStore`], the concrete SQL owners of the `summaries` and `lineage`
//! tables.
//!
//! **Conn selection rule:**
//! - `insert_*` / `delete_*` → [`super::SqliteBackend::block_write`].
//! - `get_*` / `list_*` / `has_*` / `for_each_*` →
//!   [`super::SqliteBackend::block_read`] / [`super::SqliteBackend::for_each_streamed`].
//!
//! Summary methods that (de)serialize embeddings capture `self.embed_dim` as a
//! `let` binding outside the closure — the `'static` closures cannot borrow `self`.

use async_trait::async_trait;

use super::{SqliteBackend, stream_consumer_dropped};
use crate::error::Result;
use crate::storage::consolidation::ConsolidationStore;
use crate::store::lineage::LineageStore;
use crate::store::summaries::SummaryStore;
use crate::types::{
    ConsolidationLevel, LineageRecord, LineageSnapshotEntry, NewLineageRecord, NewSummary,
    PromotionProvenance, Summary,
};

#[async_trait]
impl ConsolidationStore for SqliteBackend {
    // -------------------------------------------------------------------------
    // summaries
    // -------------------------------------------------------------------------

    // WRITE
    async fn insert_summary(&self, summary: &NewSummary) -> Result<i64> {
        let summary = summary.clone();
        let dim = self.embed_dim;
        self.block_write(move |c| SummaryStore::new(c, dim).insert(&summary))
            .await
    }

    // READ
    async fn get_summary(&self, id: i64) -> Result<Summary> {
        let dim = self.embed_dim;
        self.block_read(move |c| SummaryStore::new(c, dim).get(id))
            .await
    }

    // READ
    async fn list_summaries_by_level(&self, level: &ConsolidationLevel) -> Result<Vec<Summary>> {
        let level = level.clone();
        let dim = self.embed_dim;
        self.block_read(move |c| SummaryStore::new(c, dim).list_by_level(&level))
            .await
    }

    // READ
    async fn list_all_summaries(&self) -> Result<Vec<Summary>> {
        let dim = self.embed_dim;
        self.block_read(move |c| SummaryStore::new(c, dim).list_all())
            .await
    }

    // READ (streaming)
    async fn for_each_summary(
        &self,
        f: &mut (dyn FnMut(Summary) -> Result<()> + Send),
    ) -> Result<()> {
        let dim = self.embed_dim;
        self.for_each_streamed(
            move |conn, tx| {
                SummaryStore::new(conn, dim).for_each(|summary| {
                    tx.blocking_send(summary)
                        .map_err(|_| stream_consumer_dropped())
                })
            },
            f,
        )
        .await
    }

    // WRITE
    async fn delete_summaries_by_level(&self, level: &ConsolidationLevel) -> Result<usize> {
        let level = level.clone();
        let dim = self.embed_dim;
        self.block_write(move |c| SummaryStore::new(c, dim).delete_by_level(&level))
            .await
    }

    // -------------------------------------------------------------------------
    // lineage
    // -------------------------------------------------------------------------

    // WRITE
    async fn insert_lineage(
        &self,
        record: &NewLineageRecord,
        provenance: &PromotionProvenance,
    ) -> Result<i64> {
        let record = record.clone();
        let provenance = provenance.clone();
        self.block_write(move |c| LineageStore::new(c).insert(&record, &provenance))
            .await
    }

    // WRITE
    async fn insert_lineage_raw(&self, entry: &LineageSnapshotEntry) -> Result<()> {
        let entry = entry.clone();
        self.block_write(move |c| LineageStore::new(c).insert_raw(&entry))
            .await
    }

    // READ
    async fn get_lineage_by_wisdom_fact(
        &self,
        wisdom_fact_id: i64,
    ) -> Result<(LineageRecord, PromotionProvenance)> {
        self.block_read(move |c| LineageStore::new(c).get_by_wisdom_fact(wisdom_fact_id))
            .await
    }

    // READ
    async fn get_lineage_source_fact_ids(&self, wisdom_fact_id: i64) -> Result<Vec<i64>> {
        self.block_read(move |c| LineageStore::new(c).get_source_fact_ids(wisdom_fact_id))
            .await
    }

    // WRITE
    async fn delete_lineage(&self, wisdom_fact_id: i64) -> Result<bool> {
        self.block_write(move |c| LineageStore::new(c).delete(wisdom_fact_id))
            .await
    }

    // READ
    async fn has_lineage(&self, wisdom_fact_id: i64) -> Result<bool> {
        self.block_read(move |c| LineageStore::new(c).has_lineage(wisdom_fact_id))
            .await
    }

    // READ (streaming)
    async fn for_each_lineage(
        &self,
        f: &mut (dyn FnMut(LineageSnapshotEntry) -> Result<()> + Send),
    ) -> Result<()> {
        self.for_each_streamed(
            move |conn, tx| {
                LineageStore::new(conn).for_each(|entry| {
                    tx.blocking_send(entry)
                        .map_err(|_| stream_consumer_dropped())
                })
            },
            f,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;

    use super::super::SqliteBackend;
    use crate::error::MemoryError;
    use crate::pool::ConnectionPool;
    use crate::storage::consolidation::ConsolidationStore;
    use crate::store::facts::FactStore;
    use crate::store::upcaster::UpcasterRegistry;
    use crate::types::{
        ConsolidationLevel, FactType, LineageSnapshotEntry, NewFact, NewLineageRecord, NewSummary,
        PromotionProvenance,
    };

    const DIM: usize = 4;

    fn backend() -> SqliteBackend {
        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());
        SqliteBackend::from_pool(pool, Arc::new(UpcasterRegistry::new()))
    }

    fn make_summary(level: ConsolidationLevel) -> NewSummary {
        NewSummary {
            content: "test summary".into(),
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            level,
            source_fact_ids: vec![1, 2],
            created_at: Utc::now(),
            scope_id: 1,
        }
    }

    fn test_provenance() -> PromotionProvenance {
        PromotionProvenance {
            source_count: 2,
            session_count: 1,
            date_range_start: chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            date_range_end: chrono::DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            confidence: 0.9,
            method_version: "dreamcycle-v1".into(),
            representative_ids: vec![1, 2],
            lineage_id: 0,
        }
    }

    /// Seed two facts so FK constraints on lineage are satisfied.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "write guard held across seed loop"
    )]
    fn seeded_with_facts() -> SqliteBackend {
        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());
        {
            let conn = pool.write();
            let store = FactStore::new(&conn, DIM);
            for i in 0..2 {
                store
                    .insert(&NewFact {
                        content: format!("source fact {i}"),
                        content_hash: format!("h{i}"),
                        embedding: vec![0.1, 0.2, 0.3, 0.4],
                        fact_type: FactType::Semantic,
                        t_created: Utc::now(),
                        t_expired: None,
                        t_valid: None,
                        t_invalid: None,
                        source_event_id: None,
                        scope_id: 1,
                        importance: 0.5,
                        access_count: 0,
                        last_accessed: Utc::now(),
                        metadata: serde_json::json!({}),
                        is_pinned: false,
                    })
                    .unwrap();
            }
        }
        SqliteBackend::from_pool(pool, Arc::new(UpcasterRegistry::new()))
    }

    // -------------------------------------------------------------------------
    // summaries
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn summary_insert_then_list_by_level() {
        let be = backend();
        let id = be
            .insert_summary(&make_summary(ConsolidationLevel::Cluster))
            .await
            .unwrap();
        assert!(id > 0);

        let list = be
            .list_summaries_by_level(&ConsolidationLevel::Cluster)
            .await
            .unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id);
        assert_eq!(list[0].level, ConsolidationLevel::Cluster);

        // Other level returns empty.
        let empty = be
            .list_summaries_by_level(&ConsolidationLevel::Global)
            .await
            .unwrap();
        assert!(empty.is_empty());
    }

    #[tokio::test]
    async fn summary_get_round_trip() {
        let be = backend();
        let s = make_summary(ConsolidationLevel::Local);
        let id = be.insert_summary(&s).await.unwrap();
        let got = be.get_summary(id).await.unwrap();
        assert_eq!(got.content, s.content);
        assert_eq!(got.embedding, s.embedding);
        assert_eq!(got.level, ConsolidationLevel::Local);
    }

    #[tokio::test]
    async fn summary_list_all_and_delete_by_level() {
        let be = backend();
        be.insert_summary(&make_summary(ConsolidationLevel::Local))
            .await
            .unwrap();
        be.insert_summary(&make_summary(ConsolidationLevel::Cluster))
            .await
            .unwrap();

        let all = be.list_all_summaries().await.unwrap();
        assert_eq!(all.len(), 2);

        let deleted = be
            .delete_summaries_by_level(&ConsolidationLevel::Local)
            .await
            .unwrap();
        assert_eq!(deleted, 1);

        let remaining = be.list_all_summaries().await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].level, ConsolidationLevel::Cluster);
    }

    #[tokio::test]
    async fn for_each_summary_parity() {
        let be = backend();
        be.insert_summary(&make_summary(ConsolidationLevel::Local))
            .await
            .unwrap();
        be.insert_summary(&make_summary(ConsolidationLevel::Cluster))
            .await
            .unwrap();

        let expected = be.list_all_summaries().await.unwrap();
        let mut streamed: Vec<i64> = Vec::new();
        be.for_each_summary(&mut |s| {
            streamed.push(s.id);
            Ok(())
        })
        .await
        .unwrap();

        let expected_ids: Vec<i64> = expected.iter().map(|s| s.id).collect();
        assert_eq!(streamed, expected_ids);
    }

    #[tokio::test]
    async fn for_each_summary_early_exit() {
        let be = backend();
        for _ in 0..5 {
            be.insert_summary(&make_summary(ConsolidationLevel::Local))
                .await
                .unwrap();
        }
        let mut count = 0usize;
        let err = be
            .for_each_summary(&mut |_| {
                count += 1;
                if count == 2 {
                    return Err(MemoryError::Internal("stop at 2".into()));
                }
                Ok(())
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::Internal(_)));
        assert_eq!(count, 2);
    }

    // -------------------------------------------------------------------------
    // lineage
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn lineage_insert_then_get() {
        let be = seeded_with_facts();
        // Get fact ids from the seeded backend
        let facts = {
            use crate::storage::graph::FactGraph as _;
            be.list_all_facts().await.unwrap()
        };
        let (wf_id, sf_id) = (facts[0].id, facts[1].id);

        let record = NewLineageRecord {
            wisdom_fact_id: wf_id,
            source_fact_ids: vec![sf_id],
        };
        let lineage_id = be
            .insert_lineage(&record, &test_provenance())
            .await
            .unwrap();
        assert!(lineage_id > 0);

        let (lr, prov) = be.get_lineage_by_wisdom_fact(wf_id).await.unwrap();
        assert_eq!(lr.wisdom_fact_id, wf_id);
        assert_eq!(lr.source_fact_ids, vec![sf_id]);
        assert!((prov.confidence - 0.9).abs() < f64::EPSILON);

        let ids = be.get_lineage_source_fact_ids(wf_id).await.unwrap();
        assert_eq!(ids, vec![sf_id]);
    }

    #[tokio::test]
    async fn lineage_missing_yields_lineage_error() {
        let be = backend();
        let err = be.get_lineage_by_wisdom_fact(9999).await.unwrap_err();
        assert!(
            matches!(err, MemoryError::Lineage(_)),
            "expected Lineage error, got {err:?}"
        );
    }

    #[tokio::test]
    async fn lineage_has_and_delete() {
        let be = seeded_with_facts();
        let facts = {
            use crate::storage::graph::FactGraph as _;
            be.list_all_facts().await.unwrap()
        };
        let (wf_id, sf_id) = (facts[0].id, facts[1].id);

        assert!(!be.has_lineage(wf_id).await.unwrap());
        be.insert_lineage(
            &NewLineageRecord {
                wisdom_fact_id: wf_id,
                source_fact_ids: vec![sf_id],
            },
            &test_provenance(),
        )
        .await
        .unwrap();
        assert!(be.has_lineage(wf_id).await.unwrap());

        let deleted = be.delete_lineage(wf_id).await.unwrap();
        assert!(deleted);
        assert!(!be.has_lineage(wf_id).await.unwrap());

        let not_deleted = be.delete_lineage(wf_id).await.unwrap();
        assert!(!not_deleted);
    }

    #[tokio::test]
    async fn lineage_insert_raw_and_for_each() {
        let be = seeded_with_facts();
        let facts = {
            use crate::storage::graph::FactGraph as _;
            be.list_all_facts().await.unwrap()
        };
        let (wf_id, sf_id) = (facts[0].id, facts[1].id);

        let prov = test_provenance();
        let entry = LineageSnapshotEntry {
            lineage_id: 42,
            wisdom_fact_id: wf_id,
            source_fact_ids: vec![sf_id],
            provenance: prov,
        };
        be.insert_lineage_raw(&entry).await.unwrap();

        let mut collected: Vec<i64> = Vec::new();
        be.for_each_lineage(&mut |e| {
            collected.push(e.wisdom_fact_id);
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(collected, vec![wf_id]);
    }
}
