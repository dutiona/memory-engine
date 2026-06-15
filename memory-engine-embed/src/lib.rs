//! HTTP embedding provider for `memory-engine`.
//!
//! Implements [`memory_engine::traits::EmbeddingProvider`] by calling an
//! OpenAI-compatible `/v1/embeddings` endpoint (OpenAI, Ollama, and any
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
//!     None,   // no API key needed for local Ollama
//!     768,    // expected embedding dimension
//!     30,     // HTTP timeout in seconds
//! )
//! .expect("failed to build HTTP client");
//! ```

mod http;

pub use http::HttpEmbeddingProvider;
