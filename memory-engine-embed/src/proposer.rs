//! HTTP [`DeltaProposer`] for the pluggable consolidation backend (#554).
//!
//! Drives an LLM over HTTP to decide *what to merge*, returning a
//! [`ConsolidationProposal`] (ids + summary text) that an
//! [`LlmDreamCycle`](memory_engine::LlmDreamCycle) turns into `Synthesize` deltas.
//!
//! v1 targets Ollama's `/api/generate` endpoint in JSON mode at temperature 0 (so a
//! fixed corpus yields a deterministic proposal — the benchmark in Track B depends on
//! this). Uses `reqwest::blocking::Client` because the `DeltaProposer` trait is sync.
//!
//! The parser is deliberately split into pure helpers ([`request_body`](HttpDeltaProposer::request_body),
//! [`parse_response`](HttpDeltaProposer::parse_response)) so the wire contract is unit-
//! testable without a network, and so a future `OpenAI` `usage`-style endpoint is a
//! drop-in parser swap. Token usage (`eval_count` / `prompt_eval_count`) and the call
//! count are captured into interior atomics and read back via [`HttpDeltaProposer::stats`]
//! — the efficiency signal the benchmark records.

use std::sync::atomic::{AtomicU64, Ordering};

use memory_engine::error::MemoryError;
use memory_engine::traits::DeltaProposer;
use memory_engine::types::{ConsolidationProposal, Fact};

/// Cumulative LLM usage captured across a proposer's lifetime.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProposerStats {
    /// Number of successful `/api/generate` calls.
    pub llm_calls: u64,
    /// Sum of Ollama `eval_count` (generated tokens) across calls.
    pub eval_count: u64,
    /// Sum of Ollama `prompt_eval_count` (prompt tokens) across calls.
    pub prompt_eval_count: u64,
}

/// HTTP `DeltaProposer` calling an Ollama `/api/generate` endpoint in JSON mode.
///
/// See the [module docs](self) for the wire contract and determinism guarantees.
pub struct HttpDeltaProposer {
    client: reqwest::blocking::Client,
    endpoint: String,
    model: String,
    api_key: Option<String>,
    llm_calls: AtomicU64,
    eval_count: AtomicU64,
    prompt_eval_count: AtomicU64,
}

impl HttpDeltaProposer {
    /// Build a proposer targeting `endpoint` (the full Ollama `/api/generate` URL).
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be constructed (e.g. TLS init).
    pub fn new(
        endpoint: String,
        model: String,
        api_key: Option<String>,
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
            api_key,
            llm_calls: AtomicU64::new(0),
            eval_count: AtomicU64::new(0),
            prompt_eval_count: AtomicU64::new(0),
        })
    }

    /// Snapshot the cumulative LLM usage captured so far.
    #[must_use]
    pub fn stats(&self) -> ProposerStats {
        ProposerStats {
            llm_calls: self.llm_calls.load(Ordering::Relaxed),
            eval_count: self.eval_count.load(Ordering::Relaxed),
            prompt_eval_count: self.prompt_eval_count.load(Ordering::Relaxed),
        }
    }

    /// Build the consolidation prompt from the window + prior wisdom.
    ///
    /// States the exact JSON contract the model must return, lists the prior wisdom
    /// (retrieve-before-reflect — so the model does not re-derive known facts), then
    /// the window facts as `[id] content` lines. The `LlmDreamCycle` clamps the
    /// returned ids to this window, so a hallucinated id is harmless.
    fn build_prompt(window: &[Fact], prior_wisdom: &[Fact]) -> String {
        use std::fmt::Write;

        let mut p = String::from(
            "You are a memory consolidation engine. Group related facts from the FACTS \
             list and merge each group into one concise summary that preserves their \
             meaning.\n\n\
             Return ONLY a JSON object of this exact shape:\n\
             {\"merges\":[{\"source_ids\":[<fact ids>],\"summary\":\"<merged text>\"}]}\n\
             Rules: use only ids that appear in FACTS; put a fact in at most one group; \
             if nothing should be merged, return {\"merges\":[]}.\n",
        );
        if !prior_wisdom.is_empty() {
            p.push_str("\nPRIOR WISDOM (already known — do not re-derive):\n");
            for f in prior_wisdom {
                let _ = writeln!(p, "- [{}] {}", f.id, f.content);
            }
        }
        p.push_str("\nFACTS (candidates to consolidate):\n");
        for f in window {
            let _ = writeln!(p, "- [{}] {}", f.id, f.content);
        }
        p
    }

    /// Build the Ollama `/api/generate` request body. `stream:false` for a single
    /// response, `format:"json"` to constrain the output, and `temperature:0` so a
    /// fixed corpus yields a deterministic proposal (the Track B benchmark relies on it).
    fn request_body(model: &str, prompt: &str) -> serde_json::Value {
        serde_json::json!({
            "model": model,
            "prompt": prompt,
            "stream": false,
            "format": "json",
            "options": { "temperature": 0 },
        })
    }

    /// Extract `(eval_count, prompt_eval_count)` from the envelope, defaulting to 0
    /// when absent (a server that omits them degrades gracefully — no fake non-zero).
    ///
    /// Separate from [`parse_proposal`](Self::parse_proposal) so the caller can record
    /// the tokens a call actually burned **even when the inner merge JSON is malformed**
    /// — otherwise the Track B benchmark would undercount exactly the failure paths
    /// that matter. The envelope shape is endpoint-specific; an `OpenAI` `usage`-style
    /// endpoint is a drop-in swap of this function.
    fn extract_usage(body: &serde_json::Value) -> (u64, u64) {
        let eval = body
            .get("eval_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let prompt_eval = body
            .get("prompt_eval_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        (eval, prompt_eval)
    }

    /// Parse the proposal out of the envelope's top-level `response` string (itself
    /// JSON, because we requested `format: "json"`). Some models wrap their JSON in a
    /// markdown code fence despite JSON mode, so the fence is stripped before parsing.
    fn parse_proposal(body: &serde_json::Value) -> Result<ConsolidationProposal, MemoryError> {
        let response = body
            .get("response")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                MemoryError::Internal(
                    "proposer response missing 'response' string field".to_owned(),
                )
            })?;
        let cleaned = strip_code_fence(response);
        serde_json::from_str(cleaned).map_err(|e| {
            let preview: String = cleaned.chars().take(500).collect();
            MemoryError::Internal(format!(
                "proposer returned malformed merge JSON ({e}): {preview}"
            ))
        })
    }
}

/// Strip a wrapping markdown code fence (```` ```json `` … `` ``` ````) if present,
/// returning the inner text trimmed; otherwise return the input trimmed. Defensive
/// against models that fence their output despite Ollama's `format: "json"`.
fn strip_code_fence(s: &str) -> &str {
    let t = s.trim();
    let Some(after_open) = t.strip_prefix("```") else {
        return t;
    };
    // Drop an optional language tag on the opening fence's line (e.g. ```json).
    let body = after_open
        .split_once('\n')
        .map_or(after_open, |(_lang, rest)| rest);
    body.strip_suffix("```").map_or(t, str::trim)
}

impl DeltaProposer for HttpDeltaProposer {
    fn propose(
        &self,
        window: &[Fact],
        prior_wisdom: &[Fact],
    ) -> Result<ConsolidationProposal, MemoryError> {
        let prompt = Self::build_prompt(window, prior_wisdom);
        let body = Self::request_body(&self.model, &prompt);

        let mut req = self.client.post(&self.endpoint).json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req
            .send()
            .map_err(|e| MemoryError::Internal(format!("proposer HTTP request failed: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(MemoryError::Internal(format!(
                "proposer endpoint returned {status}: {body}"
            )));
        }
        let envelope: serde_json::Value = resp
            .json()
            .map_err(|e| MemoryError::Internal(format!("proposer response parse error: {e}")))?;

        // Record the call + tokens it burned BEFORE parsing the merge JSON: a 200 with
        // malformed merge JSON still cost a real LLM call, and the benchmark must see
        // it. `parse_proposal` may then fail without losing that accounting.
        let (eval, prompt_eval) = Self::extract_usage(&envelope);
        self.llm_calls.fetch_add(1, Ordering::Relaxed);
        self.eval_count.fetch_add(eval, Ordering::Relaxed);
        self.prompt_eval_count
            .fetch_add(prompt_eval, Ordering::Relaxed);

        Self::parse_proposal(&envelope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a persisted-looking `Fact` (id + content) for prompt tests. Constructed
    /// from JSON so the embed crate needs no `chrono` dependency; the trailing
    /// `#[serde(default)]` fields (`scope_id`, `importance_score`, …) are omitted.
    fn fact(id: i64, content: &str) -> Fact {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "content": content,
            "content_hash": "",
            "embedding": [0.0, 0.0, 0.0, 0.0],
            "fact_type": "Semantic",
            "t_created": "2026-06-16T00:00:00Z",
            "t_expired": null,
            "t_valid": null,
            "t_invalid": null,
            "source_event_id": null,
            "importance": 0.5,
            "access_count": 0,
            "last_accessed": "2026-06-16T00:00:00Z",
            "metadata": {}
        }))
        .unwrap()
    }

    /// An Ollama `/api/generate` envelope whose `response` is the model's JSON answer.
    fn envelope(response_json: &str, eval: u64, prompt_eval: u64) -> serde_json::Value {
        serde_json::json!({
            "model": "gemma4:26b",
            "response": response_json,
            "done": true,
            "eval_count": eval,
            "prompt_eval_count": prompt_eval,
        })
    }

    #[test]
    fn request_body_pins_temperature_zero_json_mode_no_stream() {
        let body = HttpDeltaProposer::request_body("gemma4:26b", "PROMPT TEXT");
        assert_eq!(body["model"], "gemma4:26b");
        assert_eq!(body["prompt"], "PROMPT TEXT");
        assert_eq!(body["stream"], serde_json::json!(false));
        assert_eq!(body["format"], "json");
        assert_eq!(
            body["options"]["temperature"],
            serde_json::json!(0),
            "temperature MUST be pinned to 0 for a deterministic benchmark"
        );
    }

    #[test]
    fn parse_proposal_extracts_merges() {
        let env = envelope(
            r#"{"merges":[{"source_ids":[1,2],"summary":"merged a+b"}]}"#,
            42,
            17,
        );
        let proposal = HttpDeltaProposer::parse_proposal(&env).unwrap();
        assert_eq!(proposal.merges.len(), 1);
        assert_eq!(proposal.merges[0].source_ids, vec![1, 2]);
        assert_eq!(proposal.merges[0].summary, "merged a+b");
    }

    #[test]
    fn extract_usage_reads_token_counts() {
        let env = envelope(r#"{"merges":[]}"#, 42, 17);
        assert_eq!(HttpDeltaProposer::extract_usage(&env), (42, 17));
    }

    #[test]
    fn parse_proposal_empty_merges_is_ok() {
        let env = envelope(r#"{"merges":[]}"#, 5, 9);
        let proposal = HttpDeltaProposer::parse_proposal(&env).unwrap();
        assert!(proposal.merges.is_empty());
    }

    #[test]
    fn parse_proposal_strips_markdown_code_fence() {
        // Some models fence their JSON despite `format: "json"`.
        let env = envelope(
            "```json\n{\"merges\":[{\"source_ids\":[3],\"summary\":\"s\"}]}\n```",
            1,
            1,
        );
        let proposal = HttpDeltaProposer::parse_proposal(&env).unwrap();
        assert_eq!(proposal.merges[0].source_ids, vec![3]);
    }

    #[test]
    fn parse_proposal_malformed_inner_json_errors() {
        let env = envelope("not valid json {{{", 1, 1);
        let err = HttpDeltaProposer::parse_proposal(&env).unwrap_err();
        assert!(err.to_string().contains("malformed"), "got: {err}");
    }

    #[test]
    fn parse_proposal_missing_response_field_errors() {
        let env = serde_json::json!({ "done": true, "eval_count": 1 });
        let err = HttpDeltaProposer::parse_proposal(&env).unwrap_err();
        assert!(err.to_string().contains("response"), "got: {err}");
    }

    #[test]
    fn extract_usage_missing_counts_default_to_zero() {
        let env = serde_json::json!({ "response": r#"{"merges":[]}"#, "done": true });
        assert_eq!(HttpDeltaProposer::extract_usage(&env), (0, 0));
    }

    #[test]
    fn build_prompt_lists_window_facts_and_demands_json_merges() {
        let window = [
            fact(1, "user prefers terse replies"),
            fact(2, "user codes in Rust"),
        ];
        let wisdom = [fact(99, "user is a researcher")];
        let prompt = HttpDeltaProposer::build_prompt(&window, &wisdom);
        assert!(prompt.contains("user prefers terse replies"));
        assert!(prompt.contains("user codes in Rust"));
        assert!(prompt.contains("user is a researcher"));
        assert!(prompt.contains("merges"));
        assert!(prompt.contains("source_ids"));
    }
}
