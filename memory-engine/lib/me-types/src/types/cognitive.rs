use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::types::facts::{FactId, FactType};
use crate::types::provenance::PromotionProvenance;

/// Semantic alias for lineage record identifiers.
pub type LineageId = i64;

/// A high-value observation captured by the intelligence layer.
///
/// Used as input to `crate::traits::InsightStream::record`.
/// The consumer creates `Insight` values during conversations to capture
/// reasoning, decisions, and connections that only the model can make.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Insight {
    /// The insight text to store as a fact.
    pub content: String,
    /// Categorization (typically `Semantic` for insights).
    pub fact_type: FactType,
    /// Optional importance hint in [0.0, 1.0]. Defaults to 0.7 if `None`.
    pub importance: Option<f64>,
    /// Arbitrary JSON metadata (e.g., `{"source": "pre_compaction_flush"}`).
    pub metadata: Option<serde_json::Value>,
    /// Scope path (e.g., `"project/memory-engine"`). `None` → root scope.
    pub scope: Option<String>,
}

// `CycleReport` (delta-based, R7) now lives in `crate::types::cycle_report` and is
// re-exported from the crate root. The old counts-based struct was removed in #49.

/// Per-`FactType` compression configuration for `DreamCycle`.
///
/// Controls what fraction of facts to retain per type and the percentile
/// threshold for promotion candidates.
#[derive(Debug, Clone)]
pub struct DreamCycleConfig {
    /// Fraction of facts to retain per `FactType` (0.0 = compress all, 1.0 = keep all).
    ///
    /// Defaults: Episodic=0.2, Semantic=0.8, Procedural=0.8
    pub compression_ratios: HashMap<FactType, f64>,
    /// Importance percentile threshold for promotion candidates.
    /// Facts above this percentile (within their type) are candidates.
    ///
    /// Default: 0.75 (P75).
    pub promotion_percentile: f64,
}

impl Default for DreamCycleConfig {
    fn default() -> Self {
        let mut ratios = HashMap::new();
        ratios.insert(FactType::Episodic, 0.2);
        ratios.insert(FactType::Semantic, 0.8);
        ratios.insert(FactType::Procedural, 0.8);
        Self {
            compression_ratios: ratios,
            promotion_percentile: 0.75,
        }
    }
}

impl DreamCycleConfig {
    /// Validate configuration parameters.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Conflict` if any ratio or percentile is out of [0, 1].
    pub fn validate(&self) -> crate::error::Result<()> {
        use crate::error::{ConflictError, MemoryError};

        for (ft, &ratio) in &self.compression_ratios {
            if !ratio.is_finite() || !(0.0..=1.0).contains(&ratio) {
                return Err(MemoryError::Conflict(ConflictError::PolicyParameter(
                    format!("compression ratio for {ft:?} must be in [0.0, 1.0], got {ratio}"),
                )));
            }
        }
        if !self.promotion_percentile.is_finite()
            || !(0.0..=1.0).contains(&self.promotion_percentile)
        {
            return Err(MemoryError::Conflict(ConflictError::PolicyParameter(
                format!(
                    "promotion_percentile must be in [0.0, 1.0], got {}",
                    self.promotion_percentile
                ),
            )));
        }
        Ok(())
    }
}

/// Request to promote a fact to wisdom with provenance tracking.
///
/// Carries a precomputed embedding so the engine does not need an
/// `crate::traits::EmbeddingProvider` at promotion time — the `DreamCycle`
/// consumer owns its embedder and computes the embedding before calling
/// `DreamContext::promote`.
#[derive(Debug, Clone)]
pub struct PromoteRequest {
    /// The promoted fact text.
    pub content: String,
    /// Fact type for the promoted wisdom (typically `Semantic`).
    pub fact_type: FactType,
    /// Precomputed embedding vector.
    pub embedding: Vec<f32>,
    /// Importance score for the promoted fact.
    pub importance: f64,
    /// Metadata JSON (will have `promotion_provenance` key injected).
    pub metadata: serde_json::Value,
    /// Scope path. `None` → root scope.
    pub scope: Option<String>,
    /// Source fact IDs for the lineage sidecar table.
    pub source_fact_ids: Vec<FactId>,
    /// Provenance envelope (serialized into metadata automatically).
    pub provenance: PromotionProvenance,
}

/// Result of a successful promotion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionResult {
    /// Database ID of the newly created promoted fact.
    pub fact_id: FactId,
    /// Database ID of the lineage record in the sidecar table.
    pub lineage_id: LineageId,
}

/// Outcome of an atomic background-reconstruction promote (#623 D6).
///
/// Returned by `SchemaManager::promote_space`
/// after the new (`populating`) space's vectors have been copy-swapped into the
/// active serving store (`facts.embedding`) and the registry status flipped — all
/// in one transaction. Internal-only this wave; the operator surface that renders
/// it is #689.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromoteOutcome {
    /// Facts whose active vector was copy-swapped to the new space (the homogeneous
    /// active space covers every fact, so this is the total fact count).
    pub promoted: usize,
    /// The previously-active space, now `deprecated` and retained in `fact_vectors`
    /// for an instant rollback (the inverse copy-swap, #689).
    pub deprecated_space: String,
    /// The now-active embedding identity. Carries the new `dim`, so the different-dim
    /// follow-up (#742) reads it to drive the engine effective-dim transition with no
    /// API change.
    pub new_fingerprint: EmbeddingFingerprint,
    /// Stragglers embedded by the engine's pre-tx catch-up pass (facts ingested after
    /// their backfill cursor passed). `0` from the storage port's perspective — the
    /// engine orchestration sets this; reserved for the live-write race work (#625).
    pub stragglers_caught: usize,
    /// The active vectors all changed, so a live in-process vector index (HNSW) must
    /// rebuild. Always `true` for a promote — even same-dim, the vectors are new. The
    /// engine acts on this by calling
    /// `SearchIndex::rebuild_vector_index`
    /// on the **same-dim** path (#624); a **different-dim** promote rebuilds on the
    /// required reopen (#742). This flag is the operator-facing signal (#689) that the
    /// active vectors changed — **not** an assertion that the index is currently stale.
    pub rebuild_index: bool,
}

/// Identity of the embedding model that produced a stored vector.
///
/// The canonical **identity tuple** shared across the Memory and Knowledge layers
/// (see ADR 0015, `docs/design/adr/0015-cross-layer-embedding-identity-policy.md`).
///
/// An embedding is only meaningful within the vector space of the exact model that
/// produced it. Vector *dimension* alone is insufficient identity — two different
/// models can share a dimension and silently corrupt retrieval. This tuple is the
/// full identity; mismatch detection (issue #614) compares two fingerprints with
/// [`PartialEq`]/[`Eq`].
///
/// # Cross-layer parity contract
///
/// The field **names** (`model`, `provider`, `dim`, `matryoshka_base_dim`,
/// `element_type`) are **normative** and shared verbatim with the `knowledge-base`
/// repository's `embed_spaces` registry. Do not rename without updating ADR 0015 in
/// both repos. `model` is an operator-declared slug, not a weight hash.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EmbeddingFingerprint {
    /// Model identity slug, e.g. `"Qwen/Qwen3-Embedding-0.6B"`. Operator-declared.
    pub model: String,
    /// Serving backend, e.g. `"tei"`, `"ollama"`, `"openai"`.
    pub provider: String,
    /// Stored vector dimension (post-truncation). Literal field name `dim` per ADR 0015.
    pub dim: usize,
    /// Native model dimension before Matryoshka (MRL) truncation; `None` if untruncated.
    pub matryoshka_base_dim: Option<usize>,
    /// Vector element storage type: `"float32"` today (reserved: `"int8"`).
    pub element_type: String,
}

impl EmbeddingFingerprint {
    /// The default vector element type (`"float32"`).
    pub const ELEMENT_F32: &'static str = "float32";

    /// Construct a fingerprint for an untruncated `float32` embedding space.
    ///
    /// Sets `matryoshka_base_dim` to `None` and `element_type` to
    /// [`ELEMENT_F32`](Self::ELEMENT_F32).
    #[must_use]
    pub fn new(model: impl Into<String>, provider: impl Into<String>, dim: usize) -> Self {
        Self {
            model: model.into(),
            provider: provider.into(),
            dim,
            matryoshka_base_dim: None,
            element_type: Self::ELEMENT_F32.to_string(),
        }
    }

    /// Construct a fingerprint for a Matryoshka-truncated `float32` embedding space,
    /// recording the native `base_dim` the model emits before truncation to `dim`.
    #[must_use]
    pub fn with_matryoshka(
        model: impl Into<String>,
        provider: impl Into<String>,
        dim: usize,
        base_dim: usize,
    ) -> Self {
        Self {
            matryoshka_base_dim: Some(base_dim),
            ..Self::new(model, provider, dim)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Phase 5a type tests ---

    #[test]
    fn dream_cycle_config_default_has_expected_ratios() {
        let cfg = DreamCycleConfig::default();
        assert!(
            (cfg.compression_ratios[&FactType::Episodic] - 0.2).abs() < f64::EPSILON,
            "Episodic should be 0.2"
        );
        assert!(
            (cfg.compression_ratios[&FactType::Semantic] - 0.8).abs() < f64::EPSILON,
            "Semantic should be 0.8"
        );
        assert!(
            (cfg.compression_ratios[&FactType::Procedural] - 0.8).abs() < f64::EPSILON,
            "Procedural should be 0.8"
        );
        assert!(
            (cfg.promotion_percentile - 0.75).abs() < f64::EPSILON,
            "promotion_percentile should be 0.75"
        );
    }

    #[test]
    fn dream_cycle_config_validate_ok() {
        DreamCycleConfig::default().validate().unwrap();
    }

    #[test]
    fn dream_cycle_config_validate_rejects_ratio_above_one() {
        let mut cfg = DreamCycleConfig::default();
        cfg.compression_ratios.insert(FactType::Episodic, 1.5);
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("Episodic"), "error: {err}");
    }

    #[test]
    fn dream_cycle_config_validate_rejects_negative_ratio() {
        let mut cfg = DreamCycleConfig::default();
        cfg.compression_ratios.insert(FactType::Semantic, -0.1);
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn dream_cycle_config_validate_rejects_nan_ratio() {
        let mut cfg = DreamCycleConfig::default();
        cfg.compression_ratios
            .insert(FactType::Procedural, f64::NAN);
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn dream_cycle_config_validate_rejects_bad_percentile() {
        let cfg = DreamCycleConfig {
            promotion_percentile: 1.5,
            ..DreamCycleConfig::default()
        };
        assert!(cfg.validate().is_err());

        let cfg = DreamCycleConfig {
            promotion_percentile: -0.1,
            ..DreamCycleConfig::default()
        };
        assert!(cfg.validate().is_err());

        let cfg = DreamCycleConfig {
            promotion_percentile: f64::NAN,
            ..DreamCycleConfig::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn dream_cycle_config_validate_accepts_boundaries() {
        let cfg = DreamCycleConfig {
            promotion_percentile: 0.0,
            ..DreamCycleConfig::default()
        };
        cfg.validate().unwrap();

        let cfg = DreamCycleConfig {
            promotion_percentile: 1.0,
            ..DreamCycleConfig::default()
        };
        cfg.validate().unwrap();
    }

    #[test]
    fn insight_serde_round_trip() {
        let insight = Insight {
            content: "User prefers terse responses".into(),
            fact_type: FactType::Semantic,
            importance: Some(0.8),
            metadata: Some(serde_json::json!({"source": "model_observation"})),
            scope: Some("project/demo".into()),
        };
        let json = serde_json::to_string(&insight).unwrap();
        let back: Insight = serde_json::from_str(&json).unwrap();
        assert_eq!(insight, back);
    }

    #[test]
    fn insight_serde_with_none_fields() {
        let insight = Insight {
            content: "test".into(),
            fact_type: FactType::Episodic,
            importance: None,
            metadata: None,
            scope: None,
        };
        let json = serde_json::to_string(&insight).unwrap();
        let back: Insight = serde_json::from_str(&json).unwrap();
        assert_eq!(insight, back);
    }

    // `CycleReport` serde round-trip moved to `engine::cycle::report` tests (#49).

    // --- EmbeddingFingerprint (#612) ---

    #[test]
    fn fingerprint_new_sets_float32_untruncated_defaults() {
        let fp = EmbeddingFingerprint::new("m", "p", 768);
        assert_eq!(fp.model, "m");
        assert_eq!(fp.provider, "p");
        assert_eq!(fp.dim, 768);
        assert_eq!(fp.matryoshka_base_dim, None);
        assert_eq!(fp.element_type, EmbeddingFingerprint::ELEMENT_F32);
        assert_eq!(fp.element_type, "float32");
    }

    #[test]
    fn fingerprint_with_matryoshka_records_base_dim() {
        let fp = EmbeddingFingerprint::with_matryoshka("qwen", "tei", 512, 1024);
        assert_eq!(fp.dim, 512);
        assert_eq!(fp.matryoshka_base_dim, Some(1024));
        assert_eq!(fp.element_type, "float32");
    }

    #[test]
    fn fingerprint_eq_requires_every_field() {
        // Equality is the #614 mismatch contract: any differing field => incompatible.
        let base = EmbeddingFingerprint::new("m", "p", 768);
        assert_eq!(base, EmbeddingFingerprint::new("m", "p", 768));
        assert_ne!(base, EmbeddingFingerprint::new("other", "p", 768));
        assert_ne!(base, EmbeddingFingerprint::new("m", "other", 768));
        assert_ne!(base, EmbeddingFingerprint::new("m", "p", 384));
        assert_ne!(
            base,
            EmbeddingFingerprint::with_matryoshka("m", "p", 768, 1024)
        );
        let mut int8 = EmbeddingFingerprint::new("m", "p", 768);
        int8.element_type = "int8".to_string();
        assert_ne!(base, int8);
    }

    #[test]
    fn fingerprint_hash_consistent_with_eq() {
        let mut set = std::collections::HashSet::new();
        set.insert(EmbeddingFingerprint::new("m", "p", 768));
        assert!(set.contains(&EmbeddingFingerprint::new("m", "p", 768)));
        assert!(!set.contains(&EmbeddingFingerprint::new("m", "p", 384)));
    }

    #[test]
    fn fingerprint_serde_pins_adr0015_key_set() {
        // The JSON key set is the normative ME<->KB parity contract (ADR 0015):
        // a field rename MUST break this test.
        let fp = EmbeddingFingerprint::with_matryoshka("qwen", "tei", 512, 1024);
        let v = serde_json::to_value(&fp).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "dim",
                "element_type",
                "matryoshka_base_dim",
                "model",
                "provider"
            ]
        );
        let back: EmbeddingFingerprint = serde_json::from_value(v).unwrap();
        assert_eq!(fp, back);
    }
}
