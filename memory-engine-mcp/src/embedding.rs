pub use memory_engine_embed::HttpEmbeddingProvider;

use memory_engine::error::MemoryError;
use memory_engine::traits::EmbeddingProvider;

/// Pass-through embedder for pre-computed embeddings supplied by the caller.
///
/// Used when `memory_add_fact` receives an `embedding` parameter directly.
pub struct PassthroughEmbedder {
    embedding: Vec<f32>,
}

impl PassthroughEmbedder {
    pub fn new(embedding: Vec<f32>) -> Self {
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
                "PassthroughEmbedder holds a single pre-computed embedding \
                 and cannot batch-embed {} texts",
                texts.len()
            )));
        }
        Ok(vec![self.embedding.clone()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_returns_stored_embedding() {
        let emb = vec![0.1, 0.2, 0.3];
        let provider = PassthroughEmbedder::new(emb.clone());
        let result = provider.embed("anything").unwrap();
        assert_eq!(result, emb);
    }

    #[test]
    fn passthrough_batch_single_text_ok() {
        let emb = vec![0.1, 0.2, 0.3];
        let provider = PassthroughEmbedder::new(emb.clone());
        let result = provider.embed_batch(&["anything"]).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], emb);
    }

    #[test]
    fn passthrough_batch_multiple_texts_rejected() {
        let provider = PassthroughEmbedder::new(vec![0.1, 0.2, 0.3]);
        let result = provider.embed_batch(&["a", "b"]);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("cannot batch-embed 2 texts")
        );
    }
}
