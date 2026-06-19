use crate::error::Result;
use crate::store::lineage::LineageStore;
use crate::types::{LineageRecord, NewLineageRecord, PromotionProvenance};

use super::MemoryEngine;

impl MemoryEngine {
    /// Record provenance for a promoted wisdom fact.
    ///
    /// Writes to the `lineage` sidecar table via the writer connection.
    /// Returns the auto-assigned `lineage_id`.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine is read-only.
    /// Returns `MemoryError::Database` on insert failure.
    pub fn record_lineage(
        &self,
        record: &NewLineageRecord,
        provenance: &PromotionProvenance,
    ) -> Result<i64> {
        let conn = self.write_conn()?;
        LineageStore::new(&conn).insert(record, provenance)
    }

    /// Retrieve the provenance envelope and lineage record for a wisdom fact.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Lineage` if no lineage exists.
    pub fn get_provenance(
        &self,
        wisdom_fact_id: i64,
    ) -> Result<(LineageRecord, PromotionProvenance)> {
        self.with_read(|conn| {
            let store = LineageStore::new(conn);
            store.get_by_wisdom_fact(wisdom_fact_id)
        })
    }

    /// Retrieve just the full source-fact ID chain for a wisdom fact.
    ///
    /// Lighter than `get_provenance` — use when only the source chain is needed
    /// (e.g., "Why?" button, debugging bad promotions).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Lineage` if no lineage exists.
    pub fn get_full_lineage(&self, wisdom_fact_id: i64) -> Result<Vec<i64>> {
        self.with_read(|conn| {
            let store = LineageStore::new(conn);
            store.get_source_fact_ids(wisdom_fact_id)
        })
    }

    /// Delete the lineage record for a wisdom fact (e.g., when reversing a promotion).
    ///
    /// Returns `true` if a record was deleted, `false` if none existed.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine is read-only.
    pub fn delete_lineage(&self, wisdom_fact_id: i64) -> Result<bool> {
        let conn = self.write_conn()?;
        LineageStore::new(&conn).delete(wisdom_fact_id)
    }
}

#[cfg(test)]
mod tests {
    use crate::engine::MemoryEngine;
    use crate::types::{AddFactRequest, FactType, NewLineageRecord, PromotionProvenance};
    use chrono::Utc;

    fn test_provenance() -> PromotionProvenance {
        PromotionProvenance {
            source_count: 2,
            session_count: 1,
            date_range_start: Utc::now(),
            date_range_end: Utc::now(),
            confidence: 0.85,
            method_version: "dreamcycle-v1".into(),
            representative_ids: vec![],
            lineage_id: 0,
        }
    }

    /// Minimal embedder for testing — returns a fixed-dimension vector.
    struct FixedEmbedder;
    impl crate::traits::EmbeddingProvider for FixedEmbedder {
        fn embed(&self, _text: &str) -> crate::error::Result<Vec<f32>> {
            Ok(vec![0.1, 0.2, 0.3, 0.4])
        }

        fn fingerprint(&self) -> crate::types::EmbeddingFingerprint {
            crate::types::EmbeddingFingerprint::new("mock", "test", 4)
        }
    }

    fn engine_with_facts() -> MemoryEngine {
        let engine = MemoryEngine::builder(4).build().unwrap();
        let embedder = FixedEmbedder;

        // Insert wisdom fact (id=1)
        engine
            .add_fact(
                &AddFactRequest {
                    content: "synthesized wisdom".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                &embedder,
                None,
            )
            .unwrap();

        // Insert source facts (id=2, id=3)
        for label in &["source A", "source B"] {
            engine
                .add_fact(
                    &AddFactRequest {
                        content: label.to_string(),
                        fact_type: FactType::Episodic,
                        source_event_id: None,
                        scope: None,
                        opts: None,
                    },
                    &embedder,
                    None,
                )
                .unwrap();
        }
        engine
    }

    #[test]
    fn record_and_get_provenance() {
        let engine = engine_with_facts();
        let new_rec = NewLineageRecord {
            wisdom_fact_id: 1,
            source_fact_ids: vec![2, 3],
        };
        let prov = test_provenance();
        let lineage_id = engine.record_lineage(&new_rec, &prov).unwrap();
        assert!(lineage_id > 0);

        let (record, got_prov) = engine.get_provenance(1).unwrap();
        assert_eq!(record.wisdom_fact_id, 1);
        assert_eq!(record.source_fact_ids, vec![2, 3]);
        assert_eq!(got_prov.source_count, 2);
        assert_eq!(got_prov.lineage_id, lineage_id);
    }

    #[test]
    fn get_full_lineage_returns_ids() {
        let engine = engine_with_facts();
        let new_rec = NewLineageRecord {
            wisdom_fact_id: 1,
            source_fact_ids: vec![2, 3],
        };
        engine.record_lineage(&new_rec, &test_provenance()).unwrap();

        let ids = engine.get_full_lineage(1).unwrap();
        assert_eq!(ids, vec![2, 3]);
    }

    #[test]
    fn delete_lineage_removes_record() {
        let engine = engine_with_facts();
        let new_rec = NewLineageRecord {
            wisdom_fact_id: 1,
            source_fact_ids: vec![2, 3],
        };
        engine.record_lineage(&new_rec, &test_provenance()).unwrap();

        let deleted = engine.delete_lineage(1).unwrap();
        assert!(deleted);

        let err = engine.get_provenance(1).unwrap_err();
        assert!(matches!(err, crate::error::MemoryError::Lineage(_)));
    }

    #[test]
    fn get_provenance_not_found() {
        let engine = engine_with_facts();
        let err = engine.get_provenance(999).unwrap_err();
        assert!(matches!(err, crate::error::MemoryError::Lineage(_)));
    }
}
