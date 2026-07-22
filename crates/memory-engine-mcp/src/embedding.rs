pub use memory_engine_embed::HttpEmbeddingProvider;

// `PassthroughEmbedder` was removed in #615: pre-computed `memory_add_fact` /
// `memory_query` submissions now declare their model identity directly (parsed into an
// `EmbeddingFingerprint` and checked against the store), instead of wrapping the vector
// in a sentinel-fingerprint embedder. See `tools::parse::parse_declared_fingerprint` and
// `MemoryEngine::add_fact_precomputed` / `verify_embedding_fingerprint`.
