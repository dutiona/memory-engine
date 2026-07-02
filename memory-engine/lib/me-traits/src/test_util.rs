//! Shared `EmbeddingProvider` test double (Wave 2 #816).
//!
//! Relocated from the monolith's `test_utils.rs` — `MockEmbedder` implements
//! [`crate::EmbeddingProvider`], a `me-traits` trait, so it lives here behind
//! the `test-util` feature rather than in `me-types` (which owns no traits).
use me_types::error::Result;
use me_types::types::EmbeddingFingerprint;

use crate::EmbeddingProvider;

/// Generic test double for [`EmbeddingProvider`] that produces a deterministic,
/// constant output vector of a given dimension.
///
/// Use [`MockEmbedder::new`] for the dominant pattern (`vec![0.5; dim]`) or
/// [`MockEmbedder::constant`] to choose a specific fill value. For tests that
/// need a fixed 4-element gradient vector `[0.1, 0.2, 0.3, 0.4]`, use
/// [`MockEmbedder::fixed4`].
///
/// **Not a replacement for purpose-built doubles.** If a test needs
/// dimension-mismatching, call-counting, error injection, or text-dependent
/// output, keep the local struct — those properties encode intent.
pub struct MockEmbedder {
    dim: usize,
    value: f32,
    /// When `Some`, overrides `value` and returns this exact vector.
    fixed: Option<Vec<f32>>,
}

impl MockEmbedder {
    /// Constant-vector embedder that returns `vec![0.5; dim]`.
    ///
    /// This is the most common pattern across store and consolidation tests.
    #[must_use]
    pub const fn new(dim: usize) -> Self {
        Self {
            dim,
            value: 0.5,
            fixed: None,
        }
    }

    /// Constant-vector embedder that returns `vec![value; dim]`.
    #[must_use]
    pub const fn constant(dim: usize, value: f32) -> Self {
        Self {
            dim,
            value,
            fixed: None,
        }
    }

    /// Fixed 4-element gradient vector `[0.1, 0.2, 0.3, 0.4]` with dim=4.
    ///
    /// Covers the family of `FakeEmbed` / `FixedEmbedder` / `FixedEmbed`
    /// structs that all produce this exact vector (inspect, ingest, lineage,
    /// apply, cognitive tests).
    #[must_use]
    pub fn fixed4() -> Self {
        Self {
            dim: 4,
            value: 0.0,
            fixed: Some(vec![0.1, 0.2, 0.3, 0.4]),
        }
    }
}

impl EmbeddingProvider for MockEmbedder {
    fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        self.fixed
            .as_ref()
            .map_or_else(|| Ok(vec![self.value; self.dim]), |v| Ok(v.clone()))
    }

    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint::new("mock", "test", self.dim)
    }
}
