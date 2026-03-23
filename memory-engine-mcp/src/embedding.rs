use memory_engine::error::MemoryError;
use memory_engine::traits::EmbeddingProvider;

/// HTTP-based embedding provider calling an OpenAI-compatible `/v1/embeddings` endpoint.
///
/// Uses `reqwest::blocking::Client` because the engine's `EmbeddingProvider` trait is sync.
/// This runs inside `tokio::task::spawn_blocking` via the engine's connection pool.
pub struct HttpEmbeddingProvider {
    client: reqwest::blocking::Client,
    endpoint: String,
    model: String,
    api_key: Option<String>,
    expected_dim: usize,
}

impl HttpEmbeddingProvider {
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be constructed (e.g., TLS init failure).
    pub fn new(
        endpoint: String,
        model: String,
        api_key: Option<String>,
        expected_dim: usize,
        timeout_secs: u64,
    ) -> Result<Self, String> {
        let client = reqwest::blocking::ClientBuilder::new()
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;
        Ok(Self {
            client,
            endpoint,
            model,
            api_key,
            expected_dim,
        })
    }
}

impl EmbeddingProvider for HttpEmbeddingProvider {
    fn embed(&self, text: &str) -> Result<Vec<f32>, MemoryError> {
        let mut req = self.client.post(&self.endpoint).json(&serde_json::json!({
            "model": &self.model,
            "input": text,
        }));

        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        let resp = req
            .send()
            .map_err(|e| MemoryError::Internal(format!("embedding HTTP request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(MemoryError::Internal(format!(
                "embedding endpoint returned {status}: {body}"
            )));
        }

        let body: serde_json::Value = resp
            .json()
            .map_err(|e| MemoryError::Internal(format!("embedding response parse error: {e}")))?;

        // Auto-detect response format:
        // OpenAI: { "data": [{ "embedding": [...] }] }
        // Ollama: { "embeddings": [[...]] }
        let embedding = if let Some(data) = body.get("data") {
            data.get(0)
                .and_then(|d| d.get("embedding"))
                .and_then(|e| serde_json::from_value::<Vec<f32>>(e.clone()).ok())
        } else if let Some(embeddings) = body.get("embeddings") {
            embeddings
                .get(0)
                .and_then(|e| serde_json::from_value::<Vec<f32>>(e.clone()).ok())
        } else if let Some(embedding) = body.get("embedding") {
            // Single embedding format
            serde_json::from_value::<Vec<f32>>(embedding.clone()).ok()
        } else {
            None
        };

        let embedding = embedding.ok_or_else(|| {
            MemoryError::Internal(format!(
                "cannot extract embedding from response: {}",
                serde_json::to_string(&body).unwrap_or_default()
            ))
        })?;

        if embedding.len() != self.expected_dim {
            return Err(MemoryError::EmbeddingDimension {
                expected: self.expected_dim,
                actual: embedding.len(),
            });
        }

        Ok(embedding)
    }
}

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
}
