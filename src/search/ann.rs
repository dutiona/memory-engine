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

    #[test]
    fn hnsw_spike_basic_search() {
        use hnsw::{Hnsw, Searcher};
        use rand::rngs::SmallRng;
        use rand::SeedableRng;
        use space::Neighbor;

        // Build a small index: 5 vectors, dim=4, M=8, M0=16
        let mut index: Hnsw<CosineMetric, Vec<f32>, SmallRng, 8, 16> = Hnsw::new_params_and_prng(
            CosineMetric,
            hnsw::Params::new().ef_construction(100),
            SmallRng::seed_from_u64(42),
        );
        let mut searcher = Searcher::default();

        let vectors = vec![
            vec![1.0, 0.0, 0.0, 0.0], // id 0: "north"
            vec![0.9, 0.1, 0.0, 0.0], // id 1: close to north
            vec![0.0, 1.0, 0.0, 0.0], // id 2: "east"
            vec![0.0, 0.0, 1.0, 0.0], // id 3: "up"
            vec![0.1, 0.0, 0.0, 1.0], // id 4: mostly "w" axis
        ];

        for v in &vectors {
            index.insert(v.clone(), &mut searcher);
        }

        // Search for "north" -- should find id 0 first, id 1 second
        let query = vec![1.0_f32, 0.0, 0.0, 0.0];
        let mut dest = vec![
            Neighbor {
                index: !0,
                distance: !0,
            };
            3
        ];
        let results = index.nearest(&query, 24, &mut searcher, &mut dest);

        assert!(results.len() >= 2, "should find at least 2 neighbors");
        // Closest should be id 0 (exact match, distance 0)
        assert_eq!(results[0].index, 0);
        assert_eq!(results[0].distance, 0);
        // Second closest should be id 1
        assert_eq!(results[1].index, 1);
    }

    #[test]
    fn hnsw_is_send_sync() {
        use hnsw::Hnsw;
        use rand::rngs::SmallRng;

        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Hnsw<CosineMetric, Vec<f32>, SmallRng, 16, 32>>();
    }
}
