//! Fuzz target for the HTTP embedding-response batch parsers (#449).
//!
//! `HttpEmbeddingProvider::{parse_openai_batch, parse_ollama_batch}` parse
//! untrusted network JSON: each runs `serde_json::from_value::<Vec<f32>>` over an
//! attacker-controlled `data` / `embeddings` array, then validates index
//! continuity (OpenAI) or per-element shape (Ollama). A compromised or
//! misconfigured endpoint could return adversarial bodies; the contract is that
//! both parsers return `Ok`/`Err` and never panic.
//!
//! The parsers are private associated fns of `HttpEmbeddingProvider`; they are
//! reached through the `#[cfg(fuzzing)]` `memory_engine_embed::fuzz_seam`
//! re-export, so the shipped crate's API is unchanged.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(data) else {
        return;
    };

    // OpenAI shape: { "data": [ { "index": .., "embedding": [..] }, .. ] }.
    if let Some(items) = body.get("data").and_then(|v| v.as_array()) {
        // Drive both the matching-count branch and a deliberately mismatched
        // count so the count-guard and the continuity check are both exercised.
        let _ = memory_engine_embed::fuzz_seam::parse_openai_batch(items, items.len());
        let _ = memory_engine_embed::fuzz_seam::parse_openai_batch(items, items.len() + 1);
    }

    // Ollama shape: { "embeddings": [ [..], [..] ] }.
    if let Some(rows) = body.get("embeddings").and_then(|v| v.as_array()) {
        let _ = memory_engine_embed::fuzz_seam::parse_ollama_batch(rows, rows.len());
        let _ = memory_engine_embed::fuzz_seam::parse_ollama_batch(rows, rows.len() + 1);
    }
});
