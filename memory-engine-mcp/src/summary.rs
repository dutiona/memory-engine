use std::sync::Arc;

use memory_engine::error::MemoryError;
use memory_engine::traits::SummaryGenerator;
use memory_engine::types::Fact;

use crate::embedding::HttpEmbeddingProvider;

/// Default system prompt for memory consolidation summarization.
///
/// Guides the LLM to produce concise, semantically rich summaries that
/// preserve all key information from a cluster of related facts.
const SUMMARIZE_SYSTEM_PROMPT: &str = "\
You are a memory consolidation assistant. \
Summarize the following set of related facts into a single, concise, \
and semantically rich statement that preserves all key information. \
Output only the summary text, no preamble or explanation.";

/// HTTP-based summary generator calling an OpenAI-compatible chat-completions endpoint.
///
/// Uses `reqwest::blocking::Client` because the engine's `SummaryGenerator` trait is sync.
/// This runs inside `tokio::task::spawn_blocking` via the server's dispatch layer.
///
/// Delegates `embed()` to the configured [`HttpEmbeddingProvider`] to ensure
/// dimensional consistency between summaries and facts in the vector space.
pub struct HttpSummaryGenerator {
    client: reqwest::blocking::Client,
    endpoint: String,
    model: String,
    api_key: Option<String>,
    embedder: Arc<HttpEmbeddingProvider>,
}

impl HttpSummaryGenerator {
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be constructed.
    pub fn new(
        endpoint: String,
        model: String,
        api_key: Option<String>,
        embedder: Arc<HttpEmbeddingProvider>,
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
            embedder,
        })
    }
}

impl SummaryGenerator for HttpSummaryGenerator {
    fn summarize(&self, facts: &[Fact]) -> Result<String, MemoryError> {
        let user_content = facts
            .iter()
            .enumerate()
            .map(|(i, f)| format!("{}. {}", i + 1, f.content))
            .collect::<Vec<_>>()
            .join("\n");

        let mut req = self.client.post(&self.endpoint).json(&serde_json::json!({
            "model": &self.model,
            "messages": [
                { "role": "system", "content": SUMMARIZE_SYSTEM_PROMPT },
                { "role": "user", "content": user_content },
            ],
        }));

        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        let resp = req
            .send()
            .map_err(|e| MemoryError::Internal(format!("summary HTTP request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(MemoryError::Internal(format!(
                "summary endpoint returned {status}: {body}"
            )));
        }

        let body: serde_json::Value = resp
            .json()
            .map_err(|e| MemoryError::Internal(format!("summary response parse error: {e}")))?;

        // Auto-detect response format:
        // OpenAI: { "choices": [{ "message": { "content": "..." } }] }
        // Ollama: { "message": { "content": "..." } }
        let content = if let Some(choices) = body.get("choices") {
            choices
                .get(0)
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
                .map(String::from)
        } else if let Some(message) = body.get("message") {
            message
                .get("content")
                .and_then(|c| c.as_str())
                .map(String::from)
        } else {
            None
        };

        content.ok_or_else(|| {
            MemoryError::Internal(format!(
                "cannot extract summary from response: {}",
                serde_json::to_string(&body).unwrap_or_default()
            ))
        })
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, MemoryError> {
        use memory_engine::traits::EmbeddingProvider;
        self.embedder.embed(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_is_non_empty() {
        assert!(!SUMMARIZE_SYSTEM_PROMPT.is_empty());
        assert!(SUMMARIZE_SYSTEM_PROMPT.contains("consolidation"));
    }
}
