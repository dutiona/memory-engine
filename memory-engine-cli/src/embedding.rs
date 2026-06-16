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
}
