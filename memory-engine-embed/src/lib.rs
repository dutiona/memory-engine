//! HTTP embedding provider for `memory-engine`.
//!
//! Implements [`memory_engine::traits::EmbeddingProvider`] by calling an
//! OpenAI-compatible `/v1/embeddings` endpoint (`OpenAI`, Ollama, and any
//! compatible server).
//!
//! # Quick start
//!
//! ```no_run
//! use memory_engine_embed::HttpEmbeddingProvider;
//!
//! let provider = HttpEmbeddingProvider::new(
//!     "http://localhost:11434/v1/embeddings".to_string(),
//!     "nomic-embed-text".to_string(),
//!     "ollama".to_string(),   // serving backend (operator-declared)
//!     None,   // no API key needed for local Ollama
//!     768,    // expected embedding dimension
//!     30,     // HTTP timeout in seconds
//! )
//! .expect("failed to build HTTP client");
//! ```

//! This crate also provides [`HttpDeltaProposer`], an HTTP
//! [`memory_engine::traits::DeltaProposer`] for the pluggable consolidation backend
//! (#554), driving an Ollama `/api/generate` endpoint to propose fact merges.

mod http;
mod proposer;

pub use http::HttpEmbeddingProvider;
pub use proposer::{HttpDeltaProposer, ProposerStats};

/// Fuzz-only seam (`--cfg fuzzing`, set only by `cargo fuzz`).
///
/// Re-exports the otherwise-private batch response parsers so cargo-fuzz targets
/// can drive them directly. Compiles to nothing on a normal build, so it adds no
/// public API to the shipped crate. See [`http::fuzz_seam`].
#[cfg(fuzzing)]
#[doc(hidden)]
pub use http::fuzz_seam;
