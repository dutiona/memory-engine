use memory_engine::EmbeddingFingerprint;
use memory_engine::error::MemoryError;
use memory_engine::traits::EmbeddingProvider;

/// Pass-through embedder for pre-computed embeddings supplied via `--embedding`.
pub struct PassthroughEmbedder {
    embedding: Vec<f32>,
}

impl PassthroughEmbedder {
    pub(crate) const fn new(embedding: Vec<f32>) -> Self {
        Self { embedding }
    }
}

impl EmbeddingProvider for PassthroughEmbedder {
    fn embed(&self, _text: &str) -> Result<Vec<f32>, MemoryError> {
        Ok(self.embedding.clone())
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, MemoryError> {
        if texts.len() != 1 {
            return Err(MemoryError::Internal(format!(
                "PassthroughEmbedder holds a single embedding and cannot batch-embed {} texts",
                texts.len()
            )));
        }
        Ok(vec![self.embedding.clone()])
    }

    fn fingerprint(&self) -> EmbeddingFingerprint {
        // TODO(#615): pre-computed embedding has no declared model identity; sentinel
        // names the hole #615 will enforce against.
        EmbeddingFingerprint::new("precomputed", "passthrough", self.embedding.len())
    }
}

#[cfg(test)]
mod tests {
    use memory_engine::traits::EmbeddingProvider;

    use super::PassthroughEmbedder;

    fn make_embedder() -> PassthroughEmbedder {
        PassthroughEmbedder::new(vec![1.0, 2.0, 3.0, 4.0])
    }

    // --- embed ---

    #[test]
    fn embed_returns_stored_embedding() {
        let emb = make_embedder();
        let result = emb.embed("any text").unwrap();
        assert_eq!(result, vec![1.0_f32, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn embed_ignores_input_text() {
        let emb = make_embedder();
        assert_eq!(emb.embed("hello").unwrap(), emb.embed("world").unwrap());
    }

    // --- embed_batch ---

    #[test]
    fn embed_batch_with_one_text_succeeds() {
        let emb = make_embedder();
        let result = emb.embed_batch(&["hello"]).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], vec![1.0_f32, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn embed_batch_with_zero_texts_returns_error() {
        let emb = make_embedder();
        let err = emb.embed_batch(&[]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains('0'),
            "error message should mention count 0, got: {msg}"
        );
    }

    #[test]
    fn embed_batch_with_two_texts_returns_error() {
        let emb = make_embedder();
        let err = emb.embed_batch(&["a", "b"]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains('2'),
            "error message should mention count 2, got: {msg}"
        );
    }

    // --- fingerprint ---

    #[test]
    fn fingerprint_dim_matches_embedding_length() {
        let embedding = vec![0.1_f32, 0.2, 0.3];
        let emb = PassthroughEmbedder::new(embedding.clone());
        let fp = emb.fingerprint();
        assert_eq!(fp.dim, embedding.len());
    }

    #[test]
    fn fingerprint_reports_precomputed_model_and_passthrough_provider() {
        let emb = make_embedder();
        let fp = emb.fingerprint();
        // new("precomputed", "passthrough", dim): first arg is model, second is provider.
        assert_eq!(fp.model, "precomputed");
        assert_eq!(fp.provider, "passthrough");
    }
}
