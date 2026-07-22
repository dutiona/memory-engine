use crate::error::Result;
use crate::types::{LineageRecord, PromotionProvenance};

use super::MemoryEngine;

impl MemoryEngine {
    /// Record provenance for a promoted wisdom fact.
    ///
    /// **Crate-internal.** This is a bare lineage-row insert with no surrounding
    /// transaction; exposing it publicly let a consumer write lineage in a
    /// separate transaction from the wisdom-fact insert (interruptible between
    /// the two) — undermining the atomicity guarantee that
    /// [`MemoryEngine::promote_with_lineage`](crate::engine::MemoryEngine::promote_with_lineage)
    /// documents (the body `DreamCtx::promote` delegates to, Wave 2 #816 / S5). The
    /// only sanctioned way to create a wisdom fact + lineage is
    /// that atomic path (fact insert + lineage insert in one savepoint); this
    /// primitive stays `pub(crate)` for it and for tests. The insert still
    /// rejects a missing or expired `wisdom_fact_id` (see [`LineageStore::insert`]).
    ///
    /// Writes to the `lineage` sidecar table via the writer connection.
    /// Returns the auto-assigned `lineage_id`.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine is read-only.
    /// Returns `MemoryError::Lineage` if the wisdom fact is missing or expired,
    /// or a source fact ID does not exist.
    /// Returns `MemoryError::Storage` on insert failure.
    ///
    /// `#[cfg(test)]`: production promotion writes lineage atomically inside
    /// [`promote_in_conn`](crate::engine::MemoryEngine::promote_in_conn) (one
    /// savepoint), never through this bare wrapper. It is retained only so the
    /// engine-level lineage tests can seed records directly.
    #[cfg(test)]
    pub(crate) async fn record_lineage(
        &self,
        record: &crate::types::NewLineageRecord,
        provenance: &PromotionProvenance,
    ) -> Result<i64> {
        self.storage.insert_lineage(record, provenance).await
    }

    /// Retrieve the provenance envelope and lineage record for a wisdom fact.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Lineage` if no lineage exists.
    pub async fn get_provenance(
        &self,
        wisdom_fact_id: i64,
    ) -> Result<(LineageRecord, PromotionProvenance)> {
        self.storage
            .get_lineage_by_wisdom_fact(wisdom_fact_id)
            .await
    }

    /// Retrieve just the full source-fact ID chain for a wisdom fact.
    ///
    /// Lighter than `get_provenance` — use when only the source chain is needed
    /// (e.g., "Why?" button, debugging bad promotions).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Lineage` if no lineage exists.
    pub async fn get_full_lineage(&self, wisdom_fact_id: i64) -> Result<Vec<i64>> {
        self.storage
            .get_lineage_source_fact_ids(wisdom_fact_id)
            .await
    }

    /// Delete the lineage record for a wisdom fact (e.g., when reversing a promotion).
    ///
    /// Returns `true` if a record was deleted, `false` if none existed.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine is read-only.
    pub async fn delete_lineage(&self, wisdom_fact_id: i64) -> Result<bool> {
        self.ensure_writable()?;
        self.storage.delete_lineage(wisdom_fact_id).await
    }
}

#[cfg(test)]
mod tests {
    use crate::engine::MemoryEngine;
    use crate::traits::EmbeddingProvider;
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
        }
    }

    async fn engine_with_facts() -> MemoryEngine {
        let engine = MemoryEngine::builder(4).build().unwrap();
        let embedder: std::sync::Arc<dyn EmbeddingProvider> =
            std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4());

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
                embedder.clone(),
                None,
            )
            .await
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
                    embedder.clone(),
                    None,
                )
                .await
                .unwrap();
        }
        engine
    }

    #[tokio::test]
    async fn record_and_get_provenance() {
        let engine = engine_with_facts().await;
        let new_rec = NewLineageRecord {
            wisdom_fact_id: 1,
            source_fact_ids: vec![2, 3],
        };
        let prov = test_provenance();
        let lineage_id = engine.record_lineage(&new_rec, &prov).await.unwrap();
        assert!(lineage_id > 0);

        let (record, got_prov) = engine.get_provenance(1).await.unwrap();
        assert_eq!(record.wisdom_fact_id, 1);
        assert_eq!(record.source_fact_ids, vec![2, 3]);
        assert_eq!(got_prov.source_count, 2);
        // The lineage_id is returned on the companion record, not the envelope.
        assert_eq!(record.lineage_id, lineage_id);
    }

    #[tokio::test]
    async fn get_full_lineage_returns_ids() {
        let engine = engine_with_facts().await;
        let new_rec = NewLineageRecord {
            wisdom_fact_id: 1,
            source_fact_ids: vec![2, 3],
        };
        engine
            .record_lineage(&new_rec, &test_provenance())
            .await
            .unwrap();

        let ids = engine.get_full_lineage(1).await.unwrap();
        assert_eq!(ids, vec![2, 3]);
    }

    #[tokio::test]
    async fn delete_lineage_removes_record() {
        let engine = engine_with_facts().await;
        let new_rec = NewLineageRecord {
            wisdom_fact_id: 1,
            source_fact_ids: vec![2, 3],
        };
        engine
            .record_lineage(&new_rec, &test_provenance())
            .await
            .unwrap();

        let deleted = engine.delete_lineage(1).await.unwrap();
        assert!(deleted);

        let err = engine.get_provenance(1).await.unwrap_err();
        assert!(matches!(err, crate::error::MemoryError::Lineage(_)));
    }

    #[tokio::test]
    async fn get_provenance_not_found() {
        let engine = engine_with_facts().await;
        let err = engine.get_provenance(999).await.unwrap_err();
        assert!(matches!(err, crate::error::MemoryError::Lineage(_)));
    }
}
