use memory_engine::EmbeddingFingerprint;
use memory_engine::error::MemoryError;
use memory_engine::traits::EmbeddingProvider;

/// HTTP-based embedding provider calling an OpenAI-compatible `/v1/embeddings` endpoint.
///
/// Auto-detects response format: `OpenAI` (`data[].embedding`), Ollama (`embeddings[]`),
/// or direct (`embedding`). Supports both single and batch embedding calls.
///
/// Uses `reqwest::blocking::Client` because the engine's `EmbeddingProvider` trait is sync.
///
/// # Examples
///
/// ```no_run
/// use memory_engine_embed::HttpEmbeddingProvider;
///
/// let provider = HttpEmbeddingProvider::new(
///     "http://localhost:11434/v1/embeddings".to_string(),
///     "nomic-embed-text".to_string(),
///     "ollama".to_string(),
///     None,
///     768,
///     30,
/// )
/// .expect("failed to build HTTP client");
/// ```
pub struct HttpEmbeddingProvider {
    client: reqwest::blocking::Client,
    endpoint: String,
    model: String,
    provider: String,
    api_key: Option<String>,
    expected_dim: usize,
}

impl HttpEmbeddingProvider {
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be constructed (e.g., TLS init failure).
    /// `provider` is the operator-declared serving backend (e.g. `"ollama"`, `"tei"`,
    /// `"openai"`). It **cannot** be sniffed from the endpoint — Ollama and TEI both
    /// speak `/v1/embeddings` — so it is an explicit argument: it feeds
    /// [`EmbeddingProvider::fingerprint`] and must reflect the real backend.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be constructed (e.g., TLS init failure).
    pub fn new(
        endpoint: String,
        model: String,
        provider: String,
        api_key: Option<String>,
        expected_dim: usize,
        timeout_secs: u64,
    ) -> Result<Self, MemoryError> {
        let client = reqwest::blocking::ClientBuilder::new()
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .build()
            .map_err(|e| MemoryError::Internal(format!("failed to build HTTP client: {e}")))?;
        Ok(Self {
            client,
            endpoint,
            model,
            provider,
            api_key,
            expected_dim,
        })
    }

    /// Parse `OpenAI` batch response: `data` array with `index` + `embedding` fields.
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
            let idx = item
                .get("index")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| {
                    MemoryError::Internal("batch embedding: missing 'index' in data item".into())
                })
                .and_then(|n| {
                    usize::try_from(n).map_err(|_| {
                        MemoryError::Internal(format!("batch embedding: index {n} overflows usize"))
                    })
                })?;

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

    /// Validate that an embedding has the expected dimension and contains no NaN/Inf values.
    ///
    /// `idx` is included in the error message when validating a batch element; pass `None`
    /// for single-embedding validation.
    fn validate_embedding(&self, emb: &[f32], idx: Option<usize>) -> Result<(), MemoryError> {
        if emb.len() != self.expected_dim {
            return Err(MemoryError::EmbeddingDimension {
                expected: self.expected_dim,
                actual: emb.len(),
            });
        }
        if emb.iter().any(|v| v.is_nan() || v.is_infinite()) {
            return Err(MemoryError::Internal(idx.map_or_else(
                || "embedding contains NaN or Inf".into(),
                |i| format!("embedding at index {i} contains NaN or Inf"),
            )));
        }
        Ok(())
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
        // Multi-branch if-let chain is clearer than a nested map_or_else here.
        #[allow(clippy::option_if_let_else)]
        let embedding = if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
            data.first()
                .and_then(|d| d.get("embedding"))
                .and_then(|e| serde_json::from_value::<Vec<f32>>(e.clone()).ok())
        } else if let Some(embeddings) = body.get("embeddings").and_then(|e| e.as_array()) {
            embeddings
                .first()
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

        self.validate_embedding(&embedding, None)?;

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
        // Single: { "embedding": [...] } (only when one text was requested)
        let embeddings = if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
            Self::parse_openai_batch(data, texts.len())?
        } else if let Some(arr) = body.get("embeddings").and_then(|e| e.as_array()) {
            Self::parse_ollama_batch(arr, texts.len())?
        } else if let Some(embedding) = body.get("embedding") {
            // Single embedding format (e.g. Ollama's legacy `/api/embeddings`, which
            // accepts only one prompt). Only valid when exactly one text was requested.
            if texts.len() != 1 {
                return Err(MemoryError::Internal(format!(
                    "batch embedding: server returned a single 'embedding' but {} texts were requested",
                    texts.len()
                )));
            }
            let emb = serde_json::from_value::<Vec<f32>>(embedding.clone()).map_err(|_| {
                MemoryError::Internal("batch embedding: cannot parse single 'embedding'".into())
            })?;
            vec![emb]
        } else {
            let body_str = serde_json::to_string(&body).unwrap_or_default();
            let truncated = truncate_on_char_boundary(&body_str, 1000);
            return Err(MemoryError::Internal(format!(
                "cannot extract embeddings from batch response: {truncated}"
            )));
        };

        // Validate all dimensions and NaN/Inf
        for (i, emb) in embeddings.iter().enumerate() {
            self.validate_embedding(emb, Some(i))?;
        }

        Ok(embeddings)
    }

    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint::new(self.model.clone(), self.provider.clone(), self.expected_dim)
    }
}

/// Truncate `s` to at most `max` bytes for an error message, snapping the cut
/// down to the nearest UTF-8 char boundary.
///
/// `serde_json::to_string` emits non-ASCII as raw UTF-8, so a serialized
/// response body can contain multibyte codepoints. Slicing at a raw byte index
/// that lands inside one panics; this walks the index down to a boundary first.
/// Byte 0 is always a boundary, so the loop terminates.
fn truncate_on_char_boundary(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}... (truncated)", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_snaps_down_to_char_boundary() {
        // A 4-byte emoji at bytes 998..1002 straddles the byte-1000 cut.
        // A raw `&body_str[..1000]` slice would panic; the helper snaps to 998.
        let mut s = "a".repeat(998);
        s.push('\u{1F600}'); // emoji, 4 bytes: indices 998, 999, 1000, 1001
        s.push_str(&"b".repeat(50));
        assert!(s.len() > 1000);
        let out = truncate_on_char_boundary(&s, 1000);
        assert!(out.ends_with("... (truncated)"));
        assert!(out.starts_with(&"a".repeat(998)));
        assert!(!out.contains('\u{1F600}'));
    }

    #[test]
    fn truncate_leaves_short_strings_unchanged() {
        assert_eq!(truncate_on_char_boundary("short body", 1000), "short body");
    }

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
    fn parse_openai_batch_missing_index() {
        // Item lacks the "index" field -> the `ok_or_else` on line 60-62 fires.
        let data = vec![
            serde_json::json!({"index": 0, "embedding": [0.1]}),
            serde_json::json!({"embedding": [0.2]}), // no "index"
        ];
        let result = HttpEmbeddingProvider::parse_openai_batch(&data, 2);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("missing 'index' in data item")
        );
    }

    #[test]
    fn parse_openai_batch_index_wrong_type() {
        // "index" present but not a u64 -> as_u64() returns None -> error.
        let data = vec![serde_json::json!({"index": "zero", "embedding": [0.1]})];
        let result = HttpEmbeddingProvider::parse_openai_batch(&data, 1);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("missing 'index' in data item")
        );
    }

    #[test]
    fn parse_openai_batch_missing_embedding() {
        // Item lacks "embedding" -> the `ok_or_else` on line 67-71 fires,
        // reporting the index from the item.
        let data = vec![serde_json::json!({"index": 0})];
        let result = HttpEmbeddingProvider::parse_openai_batch(&data, 1);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("cannot parse embedding at index 0")
        );
    }

    #[test]
    fn parse_openai_batch_unparseable_embedding() {
        // "embedding" present but not an array of numbers -> from_value fails.
        let data = vec![serde_json::json!({"index": 0, "embedding": "not-a-vec"})];
        let result = HttpEmbeddingProvider::parse_openai_batch(&data, 1);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("cannot parse embedding at index 0")
        );
    }

    #[test]
    fn parse_openai_batch_duplicate_index() {
        // Two items share index 0, none is index 1 -> continuity check on
        // line 78-84 fires with the "gap or duplicate" message.
        let data = vec![
            serde_json::json!({"index": 0, "embedding": [0.1]}),
            serde_json::json!({"index": 0, "embedding": [0.2]}),
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

    #[test]
    fn embed_batch_empty_input_returns_empty_vec() {
        // embed_batch short-circuits on empty input without making any HTTP call.
        let provider = HttpEmbeddingProvider::new(
            "http://127.0.0.1:0/v1/embeddings".to_string(),
            "test-model".to_string(),
            "test-provider".to_string(),
            None,
            768,
            5,
        )
        .expect("client build should not fail");

        let result = provider
            .embed_batch(&[])
            .expect("empty batch should succeed");
        assert!(result.is_empty());
    }

    #[test]
    fn validate_embedding_dimension_mismatch() {
        let provider = HttpEmbeddingProvider::new(
            "http://127.0.0.1:0/v1/embeddings".to_string(),
            "test-model".to_string(),
            "test-provider".to_string(),
            None,
            3,
            5,
        )
        .expect("client build should not fail");

        let err = provider.validate_embedding(&[0.1, 0.2], None).unwrap_err();
        assert!(matches!(
            err,
            MemoryError::EmbeddingDimension {
                expected: 3,
                actual: 2
            }
        ));
    }

    #[test]
    fn validate_embedding_nan_rejected() {
        let provider = HttpEmbeddingProvider::new(
            "http://127.0.0.1:0/v1/embeddings".to_string(),
            "test-model".to_string(),
            "test-provider".to_string(),
            None,
            2,
            5,
        )
        .expect("client build should not fail");

        let err = provider
            .validate_embedding(&[0.1, f32::NAN], None)
            .unwrap_err();
        assert!(err.to_string().contains("NaN"));
    }

    #[test]
    fn validate_embedding_inf_rejected() {
        let provider = HttpEmbeddingProvider::new(
            "http://127.0.0.1:0/v1/embeddings".to_string(),
            "test-model".to_string(),
            "test-provider".to_string(),
            None,
            2,
            5,
        )
        .expect("client build should not fail");

        let err = provider
            .validate_embedding(&[0.1, f32::INFINITY], None)
            .unwrap_err();
        assert!(err.to_string().contains("Inf"));
    }

    #[test]
    fn fingerprint_reports_configured_identity() {
        let provider = HttpEmbeddingProvider::new(
            "http://127.0.0.1:0/v1/embeddings".to_string(),
            "qwen3-0.6b".to_string(),
            "tei".to_string(),
            None,
            1024,
            5,
        )
        .expect("client build should not fail");
        assert_eq!(
            provider.fingerprint(),
            EmbeddingFingerprint::new("qwen3-0.6b", "tei", 1024)
        );
    }
}
