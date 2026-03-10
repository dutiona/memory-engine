//! HNSW-based approximate nearest neighbor search.
//!
//! Gated behind the `ann` feature flag.
//! Uses the `hnsw` crate (rust-cv) which owns inserted vectors,
//! avoiding lifetime issues with the HNSW graph.

use space::Metric;

use crate::search::cosine_similarity;

/// Cosine distance metric for the `hnsw` crate.
///
/// Converts cosine distance (1 - similarity) to `u32` via `f32::to_bits()`.
/// For non-negative f32 values, bit representation preserves total order,
/// satisfying `space::Metric`'s `Unit: Ord` requirement.
///
/// Edge cases:
/// - Zero-norm vectors return distance 1.0 (maximum cosine distance for
///   degenerate input, placing them far from all real vectors).
/// - Result clamped to \[0, 2\] to avoid NaN/negative from floating-point noise.
#[derive(Copy, Clone)]
pub struct CosineMetric;

#[allow(clippy::ptr_arg)] // space::Metric trait requires &P where P=Vec<f32>
impl Metric<Vec<f32>> for CosineMetric {
    type Unit = u32;

    fn distance(&self, a: &Vec<f32>, b: &Vec<f32>) -> u32 {
        let sim = cosine_similarity(a, b);
        // cosine_similarity returns 0.0 for zero-norm vectors.
        // Clamp to [0, 2] to handle floating-point edge cases and NaN.
        let dist = (1.0 - sim).clamp(0.0, 2.0);
        dist.to_bits()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_metric_identical_vectors() {
        let m = CosineMetric;
        let a = vec![1.0_f32, 0.0, 0.0];
        let b = vec![1.0_f32, 0.0, 0.0];
        assert_eq!(m.distance(&a, &b), 0.0_f32.to_bits());
    }

    #[test]
    fn cosine_metric_orthogonal_vectors() {
        let m = CosineMetric;
        let a = vec![1.0_f32, 0.0, 0.0];
        let b = vec![0.0_f32, 1.0, 0.0];
        assert_eq!(m.distance(&a, &b), 1.0_f32.to_bits());
    }

    #[test]
    fn cosine_metric_preserves_distance_ordering() {
        let m = CosineMetric;
        let query = vec![1.0_f32, 0.0, 0.0];
        let close = vec![0.9_f32, 0.1, 0.0];
        let far = vec![0.0_f32, 1.0, 0.0];
        assert!(m.distance(&query, &close) < m.distance(&query, &far));
    }
}
