use crate::error::{MemoryError, Result};
use crate::search::vector::cosine_similarity;
use crate::types::Fact;

use super::MemoryEngine;

/// Default importance score threshold for dormant facts.
/// Facts with `importance_score` below this value are considered dormant.
const DORMANT_THRESHOLD: f64 = 0.5;

impl MemoryEngine {
    /// Sample dormant (low-importance) facts semantically related to a context.
    ///
    /// "Resonance" — surfaces forgotten memories that may be relevant to
    /// the current conversation. Returns facts sorted by cosine similarity
    /// (descending) to the context embedding, filtered to low-importance,
    /// non-expired, non-pinned, temporally valid facts only.
    ///
    /// # Arguments
    ///
    /// * `n` — Maximum number of facts to return.
    /// * `context` — Context embedding to compute similarity against.
    /// * `scope_ids` — Optional scope filter. When `Some`, only facts in these
    ///   scopes are considered. When `None`, all scopes are searched.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::EmbeddingDimension` if `context.len() != embed_dim`.
    pub async fn sample_dormant(
        &self,
        n: usize,
        context: &[f32],
        scope_ids: Option<&[i64]>,
    ) -> Result<Vec<Fact>> {
        self.ensure_open()?;
        if context.len() != self.embed_dim {
            return Err(MemoryError::EmbeddingDimension {
                expected: self.embed_dim,
                actual: context.len(),
            });
        }

        if n == 0 {
            return Ok(Vec::new());
        }

        // Query dormant facts from the store, filtered by scope
        let scope_ids_owned = scope_ids.map(<[i64]>::to_vec);
        let candidates = self
            .storage
            .list_dormant_facts(DORMANT_THRESHOLD, scope_ids_owned.as_deref())
            .await?;

        // Compute cosine similarity and sort descending (resonance = most relevant dormant)
        let mut scored: Vec<(f32, Fact)> = candidates
            .into_iter()
            .map(|fact| {
                let sim = cosine_similarity(context, &fact.embedding);
                (sim, fact)
            })
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(n);

        Ok(scored.into_iter().map(|(_, fact)| fact).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::EmbeddingProvider;
    use crate::types::{AddFactRequest, FactType};

    struct FixedEmbedder(Vec<f32>);
    impl EmbeddingProvider for FixedEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(self.0.clone())
        }

        fn fingerprint(&self) -> crate::types::EmbeddingFingerprint {
            crate::types::EmbeddingFingerprint::new("mock", "test", self.0.len())
        }
    }

    async fn add_fact_with_importance(
        engine: &MemoryEngine,
        content: &str,
        importance: f64,
        embedding: Vec<f32>,
    ) -> i64 {
        let embedder = FixedEmbedder(embedding);
        let req = AddFactRequest {
            content: content.into(),
            fact_type: FactType::Episodic,
            source_event_id: None,
            scope: None,
            opts: Some(crate::types::AddFactOptions {
                importance: Some(importance),
                ..Default::default()
            }),
        };
        engine
            .add_fact(
                &req,
                std::sync::Arc::new(embedder) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn sample_dormant_empty_store() {
        let engine = MemoryEngine::builder(4).build().unwrap();
        let results = engine
            .sample_dormant(10, &[0.1, 0.2, 0.3, 0.4], None)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn sample_dormant_wrong_dimension() {
        let engine = MemoryEngine::builder(4).build().unwrap();
        let err = engine
            .sample_dormant(10, &[0.1, 0.2], None)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::EmbeddingDimension {
                expected: 4,
                actual: 2
            }
        ));
    }

    #[tokio::test]
    async fn sample_dormant_returns_low_importance_only() {
        let engine = MemoryEngine::builder(4).build().unwrap();

        // Low importance — should be returned
        add_fact_with_importance(&engine, "low importance", 0.1, vec![1.0, 0.0, 0.0, 0.0]).await;
        // High importance — should NOT be returned
        add_fact_with_importance(&engine, "high importance", 0.9, vec![1.0, 0.0, 0.0, 0.0]).await;

        let results = engine
            .sample_dormant(10, &[1.0, 0.0, 0.0, 0.0], None)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "low importance");
    }

    #[tokio::test]
    async fn sample_dormant_excludes_expired() {
        let engine = MemoryEngine::builder(4).build().unwrap();

        let id =
            add_fact_with_importance(&engine, "to expire", 0.1, vec![1.0, 0.0, 0.0, 0.0]).await;
        // Expire the fact via the storage port
        engine
            .storage
            .expire_fact(id, chrono::Utc::now())
            .await
            .unwrap();

        let results = engine
            .sample_dormant(10, &[1.0, 0.0, 0.0, 0.0], None)
            .await
            .unwrap();
        assert!(results.is_empty(), "expired facts should not appear");
    }

    #[tokio::test]
    async fn sample_dormant_excludes_pinned() {
        let engine = MemoryEngine::builder(4).build().unwrap();

        // Add a pinned low-importance fact
        let embedder = FixedEmbedder(vec![1.0, 0.0, 0.0, 0.0]);
        let req = AddFactRequest {
            content: "pinned fact".into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: Some(crate::types::AddFactOptions {
                importance: Some(0.1),
                pinned: Some(true),
                ..Default::default()
            }),
        };
        engine
            .add_fact(
                &req,
                std::sync::Arc::new(embedder) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();

        let results = engine
            .sample_dormant(10, &[1.0, 0.0, 0.0, 0.0], None)
            .await
            .unwrap();
        assert!(results.is_empty(), "pinned facts should not appear");
    }

    #[tokio::test]
    async fn sample_dormant_respects_n_limit() {
        let engine = MemoryEngine::builder(4).build().unwrap();

        for i in 0..5 {
            #[allow(clippy::cast_precision_loss)] // test data, precision irrelevant
            let emb = vec![1.0 / (i as f32 + 1.0), 0.0, 0.0, 0.0];
            add_fact_with_importance(&engine, &format!("fact {i}"), 0.1, emb).await;
        }

        let results = engine
            .sample_dormant(3, &[1.0, 0.0, 0.0, 0.0], None)
            .await
            .unwrap();
        assert_eq!(results.len(), 3);
    }

    #[tokio::test]
    async fn sample_dormant_sorts_by_similarity() {
        let engine = MemoryEngine::builder(4).build().unwrap();

        // Less similar
        add_fact_with_importance(&engine, "less similar", 0.1, vec![0.0, 1.0, 0.0, 0.0]).await;
        // More similar
        add_fact_with_importance(&engine, "more similar", 0.1, vec![1.0, 0.0, 0.0, 0.0]).await;

        let results = engine
            .sample_dormant(10, &[1.0, 0.0, 0.0, 0.0], None)
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].content, "more similar",
            "most similar should be first"
        );
    }

    #[tokio::test]
    async fn sample_dormant_zero_n() {
        let engine = MemoryEngine::builder(4).build().unwrap();
        add_fact_with_importance(&engine, "fact", 0.1, vec![1.0, 0.0, 0.0, 0.0]).await;
        let results = engine
            .sample_dormant(0, &[1.0, 0.0, 0.0, 0.0], None)
            .await
            .unwrap();
        assert!(results.is_empty());
    }
}
