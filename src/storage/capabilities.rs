//! Backend capability signals (P1 stub — the real `LexicalRanker` +
//! `BackendCapabilities` fields land in P3).

/// Capability probe a backend answers at `open`. **P1 placeholder** — replaced by
/// the dialect-free tier signal (`lexical_ranker` / `server_side_vector` /
/// `true_idf`) in P3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendCapabilities;
