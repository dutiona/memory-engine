//! Configuration type for the forget/prune operation.
//!
//! [`ForgetPolicy`] is the consumer-tunable policy controlling Ebbinghaus decay
//! and multi-signal importance scoring. Lives here (rather than in `traits.rs`)
//! because it isn't a trait — `traits.rs` declares the consumer behaviour
//! contracts, while this is the concrete forgetting-layer type it configures.
//! The output type `PruneStats` (pure data) has moved to `me-types` (Wave 2
//! #816 E.4b Phase B); re-exported from this crate's root (`me-forget`, Wave
//! 2 #816 / S3) as `me_forget::PruneStats`, and from the facade as
//! `crate::forgetting::PruneStats`.

use me_types::error::Result;

/// Policy for forgetting/pruning stale facts.
///
/// Importance is computed by the forgetting layer's `compute_importance` as a
/// weighted sum of 4 signals, **each normalized to `[0, 1]`** before weighting:
///
/// ```text
/// score = recency_weight          × 2^(-age/half_life)
///       + frequency_weight        × clamp(ln(access+1) / ln(101), 0, 1)
///       + graph_degree_weight     × clamp(ln(degree+1) / ln(51),  0, 1)
///       + base_importance_weight  × fact.base_importance
/// ```
///
/// Because every signal is pre-normalized to `[0, 1]`, each weight contributes at
/// most its own magnitude: the maximum possible score equals the sum of the four
/// weights (not unbounded). `compute_importance` is the authoritative source for
/// the normalization constants (`ln(101)` for frequency, `ln(51)` for graph
/// degree); keep this doc in sync with it.
///
/// The displayed recency term `2^(-age/half_life)` is the general (non-exempt)
/// case: for decay-exempt fact types (see [`decay_exempt_types`](Self::decay_exempt_types) /
/// [`is_decay_exempt`](Self::is_decay_exempt)) the recency signal is pinned to `1.0`.
///
/// Facts with computed importance below `min_importance` get soft-deleted (`t_expired` set).
#[derive(Debug, Clone)]
pub struct ForgetPolicy {
    /// Base Ebbinghaus half-life in days (default: 69.0).
    pub half_life_days: f64,
    /// Per-`FactType` half-life overrides. E.g., Episodic=30, Procedural=365.
    /// An explicit entry here wins over `decay_exempt_types`.
    pub half_life_overrides: std::collections::HashMap<me_types::types::FactType, f64>,
    /// Fact types exempt from decay-driven forgetting altogether.
    ///
    /// Content predicate: a type belongs here iff its facts' truth is
    /// independent of time-since-encoding — declarative assertions
    /// (`Semantic`) and validated procedures (`Procedural`). Such
    /// knowledge-shaped facts are governed by supersession and conflict
    /// resolution, never by Ebbinghaus decay; applying decay to them is the
    /// category error the four-layer model exists to prevent. Episodic facts
    /// are time-indexed experience records and decay by design.
    ///
    /// Default: `{Semantic, Procedural}`.
    pub decay_exempt_types: std::collections::HashSet<me_types::types::FactType>,
    /// Threshold below which facts are expired (default: 0.1).
    pub min_importance: f64,
    /// Weight for recency signal (Ebbinghaus decay). Default: 0.3.
    pub recency_weight: f64,
    /// Weight for access frequency signal. Default: 0.2.
    pub frequency_weight: f64,
    /// Weight for graph connectivity signal. Default: 0.3.
    pub graph_degree_weight: f64,
    /// Weight for base importance (`fact.base_importance`). Default: 0.2.
    pub base_importance_weight: f64,
}

impl Default for ForgetPolicy {
    fn default() -> Self {
        Self {
            half_life_days: 69.0,
            half_life_overrides: std::collections::HashMap::new(),
            decay_exempt_types: [
                me_types::types::FactType::Semantic,
                me_types::types::FactType::Procedural,
            ]
            .into_iter()
            .collect(),
            min_importance: 0.1,
            recency_weight: 0.3,
            frequency_weight: 0.2,
            graph_degree_weight: 0.3,
            base_importance_weight: 0.2,
        }
    }
}

impl ForgetPolicy {
    /// Whether `fact_type` is exempt from decay-driven forgetting.
    ///
    /// True iff the type is in `decay_exempt_types` and has no explicit
    /// `half_life_overrides` entry — an explicit override wins, re-enabling
    /// finite-half-life decay for that type.
    #[must_use]
    pub fn is_decay_exempt(&self, fact_type: &me_types::types::FactType) -> bool {
        self.decay_exempt_types.contains(fact_type)
            && !self.half_life_overrides.contains_key(fact_type)
    }

    /// Validate policy parameters.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Conflict` if any parameter is out of range.
    pub fn validate(&self) -> Result<()> {
        use me_types::error::{ConflictError, MemoryError};

        if !self.half_life_days.is_finite() || self.half_life_days <= 0.0 {
            return Err(MemoryError::Conflict(ConflictError::PolicyParameter(
                "half_life_days must be > 0".into(),
            )));
        }
        for (ft, &hl) in &self.half_life_overrides {
            if !hl.is_finite() || hl <= 0.0 {
                return Err(MemoryError::Conflict(ConflictError::PolicyParameter(
                    format!("half_life for {ft:?} must be > 0"),
                )));
            }
        }
        if !(0.0..=1.0).contains(&self.min_importance) {
            return Err(MemoryError::Conflict(ConflictError::PolicyParameter(
                "min_importance must be in [0, 1]".into(),
            )));
        }
        if !self.recency_weight.is_finite()
            || !self.frequency_weight.is_finite()
            || !self.graph_degree_weight.is_finite()
            || !self.base_importance_weight.is_finite()
            || self.recency_weight < 0.0
            || self.frequency_weight < 0.0
            || self.graph_degree_weight < 0.0
            || self.base_importance_weight < 0.0
        {
            return Err(MemoryError::Conflict(ConflictError::PolicyParameter(
                "weights must be >= 0".into(),
            )));
        }
        // Note: weights intentionally do NOT need to sum to 1.0 (ADR-0006).
        // Each signal is independently normalized to [0,1] via ln_1p + ceilings,
        // so the weighted sum naturally falls in [0, sum_of_weights].
        // The result is compared against min_importance, not against 1.0.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use me_types::types::FactType;

    // --- ForgetPolicy::default() ---

    #[test]
    fn default_has_expected_field_values() {
        let p = ForgetPolicy::default();
        assert!((p.half_life_days - 69.0).abs() < f64::EPSILON);
        assert!(p.half_life_overrides.is_empty());
        assert!((p.min_importance - 0.1).abs() < f64::EPSILON);
        assert!((p.recency_weight - 0.3).abs() < f64::EPSILON);
        assert!((p.frequency_weight - 0.2).abs() < f64::EPSILON);
        assert!((p.graph_degree_weight - 0.3).abs() < f64::EPSILON);
        assert!((p.base_importance_weight - 0.2).abs() < f64::EPSILON);
    }

    #[test]
    fn default_validates_ok() {
        ForgetPolicy::default().validate().unwrap();
    }

    // --- ForgetPolicy::validate() error paths ---

    #[test]
    fn validate_rejects_zero_half_life() {
        let p = ForgetPolicy {
            half_life_days: 0.0,
            ..Default::default()
        };
        let err = p.validate().unwrap_err().to_string();
        assert!(err.contains("half_life_days"), "error: {err}");
    }

    #[test]
    fn validate_rejects_negative_half_life() {
        let p = ForgetPolicy {
            half_life_days: -1.0,
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn validate_rejects_zero_override_half_life() {
        let mut overrides = std::collections::HashMap::new();
        overrides.insert(FactType::Episodic, 0.0);
        let p = ForgetPolicy {
            half_life_overrides: overrides,
            ..Default::default()
        };
        let err = p.validate().unwrap_err().to_string();
        assert!(err.contains("Episodic"), "error: {err}");
    }

    #[test]
    fn validate_rejects_negative_override_half_life() {
        let mut overrides = std::collections::HashMap::new();
        overrides.insert(FactType::Procedural, -5.0);
        let p = ForgetPolicy {
            half_life_overrides: overrides,
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn validate_accepts_valid_override() {
        let mut overrides = std::collections::HashMap::new();
        overrides.insert(FactType::Episodic, 30.0);
        overrides.insert(FactType::Procedural, 365.0);
        let p = ForgetPolicy {
            half_life_overrides: overrides,
            ..Default::default()
        };
        p.validate().unwrap();
    }

    #[test]
    fn validate_rejects_min_importance_above_one() {
        let p = ForgetPolicy {
            min_importance: 1.01,
            ..Default::default()
        };
        let err = p.validate().unwrap_err().to_string();
        assert!(err.contains("min_importance"), "error: {err}");
    }

    #[test]
    fn validate_rejects_negative_min_importance() {
        let p = ForgetPolicy {
            min_importance: -0.01,
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn validate_accepts_boundary_min_importance() {
        for val in [0.0, 1.0] {
            let p = ForgetPolicy {
                min_importance: val,
                ..Default::default()
            };
            p.validate().unwrap();
        }
    }

    #[test]
    fn validate_rejects_negative_weight() {
        let cases = [
            (
                "recency",
                ForgetPolicy {
                    recency_weight: -0.1,
                    ..Default::default()
                },
            ),
            (
                "frequency",
                ForgetPolicy {
                    frequency_weight: -0.1,
                    ..Default::default()
                },
            ),
            (
                "graph_degree",
                ForgetPolicy {
                    graph_degree_weight: -0.1,
                    ..Default::default()
                },
            ),
            (
                "base_importance",
                ForgetPolicy {
                    base_importance_weight: -0.1,
                    ..Default::default()
                },
            ),
        ];
        for (name, p) in &cases {
            assert!(
                p.validate().is_err(),
                "{name} weight should reject negative"
            );
        }
    }

    #[test]
    fn validate_accepts_zero_weights() {
        let p = ForgetPolicy {
            recency_weight: 0.0,
            frequency_weight: 0.0,
            graph_degree_weight: 0.0,
            base_importance_weight: 0.0,
            ..Default::default()
        };
        p.validate().unwrap();
    }

    #[test]
    fn validate_accepts_weights_summing_above_one() {
        // ADR-0006: weights don't need to sum to 1.0
        let p = ForgetPolicy {
            recency_weight: 1.0,
            frequency_weight: 1.0,
            graph_degree_weight: 1.0,
            base_importance_weight: 1.0,
            ..Default::default()
        };
        p.validate().unwrap();
    }

    // --- NaN rejection (IEEE 754: NaN comparisons always return false) ---

    #[test]
    fn validate_rejects_nan_half_life() {
        let p = ForgetPolicy {
            half_life_days: f64::NAN,
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn validate_rejects_infinity_half_life() {
        let p = ForgetPolicy {
            half_life_days: f64::INFINITY,
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn validate_rejects_nan_override_half_life() {
        let mut overrides = std::collections::HashMap::new();
        overrides.insert(FactType::Semantic, f64::NAN);
        let p = ForgetPolicy {
            half_life_overrides: overrides,
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn validate_rejects_nan_weight() {
        let cases = [
            ForgetPolicy {
                recency_weight: f64::NAN,
                ..Default::default()
            },
            ForgetPolicy {
                frequency_weight: f64::NAN,
                ..Default::default()
            },
            ForgetPolicy {
                graph_degree_weight: f64::NAN,
                ..Default::default()
            },
            ForgetPolicy {
                base_importance_weight: f64::NAN,
                ..Default::default()
            },
        ];
        for (i, p) in cases.iter().enumerate() {
            assert!(p.validate().is_err(), "NaN weight case {i} should reject");
        }
    }

    #[test]
    fn validate_rejects_nan_min_importance() {
        let p = ForgetPolicy {
            min_importance: f64::NAN,
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }
}
