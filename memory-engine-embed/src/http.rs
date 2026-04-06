use memory_engine::error::MemoryError;
use memory_engine::traits::EmbeddingProvider;

/// HTTP-based embedding provider calling an OpenAI-compatible `/v1/embeddings` endpoint.
///
/// Auto-detects response format: OpenAI (`data[].embedding`), Ollama (`embeddings[]`),
/// or direct (`embedding`). Supports both single and batch embedding calls.
///
/// Uses `reqwest::blocking::Client` because the engine's `EmbeddingProvider` trait is sync.
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

impl HttpEmbeddingProvider {
    /// Parse OpenAI batch response: `data` array with `index` + `embedding` fields.
    /// Sorts by `index` to handle out-of-order responses.
    fn parse_openai_batch(
        data: &[serde_json::Value],
        expected_count: usize,
    ) -> Result<Vec<Vec<f32>>, MemoryError> {
        if data.len() != expected_count {
            return Err(MemoryError::Internal(format!(
                "batch embedding: expected {expected_count} results, got {}",
                data.len()
            )));
        }

        // Extract (index, embedding) pairs
        let mut pairs: Vec<(usize, Vec<f32>)> = Vec::with_capacity(expected_count);
        for item in data {
            let idx = item.get("index").and_then(|v| v.as_u64()).ok_or_else(|| {
                MemoryError::Internal("batch embedding: missing 'index' in data item".into())
            })? as usize;

            let embedding = item
                .get("embedding")
                .and_then(|e| serde_json::from_value::<Vec<f32>>(e.clone()).ok())
                .ok_or_else(|| {
                    MemoryError::Internal(format!(
                        "batch embedding: cannot parse embedding at index {idx}"
                    ))
                })?;

            pairs.push((idx, embedding));
        }

        // Sort by index and validate continuity (0..N-1, no duplicates/gaps)
        pairs.sort_by_key(|(idx, _)| *idx);
        for (i, (idx, _)) in pairs.iter().enumerate() {
            if *idx != i {
                return Err(MemoryError::Internal(format!(
                    "batch embedding: expected index {i}, got {idx} (gap or duplicate)"
                )));
            }
        }

        Ok(pairs.into_iter().map(|(_, emb)| emb).collect())
    }

    /// Parse Ollama batch response: `embeddings` is an array of arrays.
    fn parse_ollama_batch(
        embeddings: &[serde_json::Value],
        expected_count: usize,
    ) -> Result<Vec<Vec<f32>>, MemoryError> {
        if embeddings.len() != expected_count {
            return Err(MemoryError::Internal(format!(
                "batch embedding: expected {expected_count} results, got {}",
                embeddings.len()
            )));
        }

        embeddings
            .iter()
            .enumerate()
            .map(|(i, v)| {
                serde_json::from_value::<Vec<f32>>(v.clone()).map_err(|_| {
                    MemoryError::Internal(format!(
                        "batch embedding: cannot parse embedding at index {i}"
                    ))
                })
            })
            .collect()
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

    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, MemoryError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let mut req = self.client.post(&self.endpoint).json(&serde_json::json!({
            "model": &self.model,
            "input": texts,
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

        // Auto-detect response format and extract all embeddings:
        // OpenAI: { "data": [{ "index": 0, "embedding": [...] }, ...] }
        // Ollama: { "embeddings": [[...], [...]] }
        let embeddings = if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
            Self::parse_openai_batch(data, texts.len())?
        } else if let Some(arr) = body.get("embeddings").and_then(|e| e.as_array()) {
            Self::parse_ollama_batch(arr, texts.len())?
        } else {
            let body_str = serde_json::to_string(&body).unwrap_or_default();
            let truncated = if body_str.len() > 1000 {
                format!("{}... (truncated)", &body_str[..1000])
            } else {
                body_str
            };
            return Err(MemoryError::Internal(format!(
                "cannot extract embeddings from batch response: {truncated}"
            )));
        };

        // Validate all dimensions
        for (i, emb) in embeddings.iter().enumerate() {
            if emb.len() != self.expected_dim {
                return Err(MemoryError::EmbeddingDimension {
                    expected: self.expected_dim,
                    actual: emb.len(),
                });
            }
            if emb.iter().any(|v| v.is_nan() || v.is_infinite()) {
                return Err(MemoryError::Internal(format!(
                    "embedding at index {i} contains NaN or Inf"
                )));
            }
        }

        Ok(embeddings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_openai_batch_valid() {
        let data = vec![
            serde_json::json!({"index": 1, "embedding": [0.2, 0.3]}),
            serde_json::json!({"index": 0, "embedding": [0.1, 0.4]}),
        ];
        let result = HttpEmbeddingProvider::parse_openai_batch(&data, 2).unwrap();
        // Should be sorted by index: index 0 first, then index 1
        assert_eq!(result[0], vec![0.1, 0.4]);
        assert_eq!(result[1], vec![0.2, 0.3]);
    }

    #[test]
    fn parse_openai_batch_count_mismatch() {
        let data = vec![serde_json::json!({"index": 0, "embedding": [0.1]})];
        let result = HttpEmbeddingProvider::parse_openai_batch(&data, 2);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("expected 2 results, got 1")
        );
    }

    #[test]
    fn parse_openai_batch_gap_in_indices() {
        let data = vec![
            serde_json::json!({"index": 0, "embedding": [0.1]}),
            serde_json::json!({"index": 2, "embedding": [0.2]}), // gap: missing index 1
        ];
        let result = HttpEmbeddingProvider::parse_openai_batch(&data, 2);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("gap or duplicate"));
    }

    #[test]
    fn parse_ollama_batch_valid() {
        let data = vec![serde_json::json!([0.1, 0.2]), serde_json::json!([0.3, 0.4])];
        let result = HttpEmbeddingProvider::parse_ollama_batch(&data, 2).unwrap();
        assert_eq!(result[0], vec![0.1f32, 0.2]);
        assert_eq!(result[1], vec![0.3f32, 0.4]);
    }

    #[test]
    fn parse_ollama_batch_count_mismatch() {
        let data = vec![serde_json::json!([0.1])];
        let result = HttpEmbeddingProvider::parse_ollama_batch(&data, 3);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("expected 3 results, got 1")
        );
    }
}
