use memory_engine::error::MemoryError;
use memory_engine::traits::{SummarizableContent, SummaryGenerator};

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
/// Produces summary text only. Embedding is performed by the
/// [`HttpEmbeddingProvider`](crate::embedding::HttpEmbeddingProvider) injected
/// separately into consolidation (issue #116 — embedding is no longer duplicated
/// on the `SummaryGenerator` trait), so summaries share the fact vector space.
pub struct HttpSummaryGenerator {
    client: reqwest::blocking::Client,
    endpoint: String,
    model: String,
    api_key: Option<String>,
}

impl HttpSummaryGenerator {
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be constructed.
    pub fn new(
        endpoint: String,
        model: String,
        api_key: Option<String>,
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
        })
    }
}

impl SummaryGenerator for HttpSummaryGenerator {
    fn summarize(&self, items: &[SummarizableContent<'_>]) -> Result<String, MemoryError> {
        let user_content = items
            .iter()
            .enumerate()
            .map(|(i, item)| format!("{}. {}", i + 1, item.text))
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

        // Check for API-level error in a 200 response (some providers do this)
        if let Some(error) = body.get("error") {
            return Err(MemoryError::Internal(format!(
                "summary API returned error: {}",
                serde_json::to_string(error).unwrap_or_default()
            )));
        }

        // Auto-detect response format:
        // OpenAI: { "choices": [{ "message": { "content": "..." } }] }
        // Ollama: { "message": { "content": "..." } }
        // Multi-branch if-let chain is clearer than a nested map_or_else here.
        #[allow(clippy::option_if_let_else)]
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
