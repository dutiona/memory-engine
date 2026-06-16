//! Pure, deterministic DBSCAN clustering over embedding space.
//!
//! No `SQLite`, no engine state — a free function over `(FactId, embedding)` pairs,
//! so it is trivially unit-testable. Distance is `1 - cosine_similarity`; two points
//! are neighbours when their cosine similarity is `>= 1 - eps`. `cosine_similarity`
//! returns `0.0` for a zero vector, so degenerate embeddings become isolated noise
//! rather than producing `NaN`. Determinism: points are visited in slice order and a
//! cluster's seed set is expanded in discovery order, so the same input always yields
//! the same clusters.

use crate::search::vector::cosine_similarity;
use crate::types::FactId;

/// Upper bound on points fed to DBSCAN in one call (mirrors the consolidation
/// clustering cap). Above this the O(N²) neighbour scan is skipped with a warning,
/// since the cycle operates on a bounded time window anyway.
const MAX_DBSCAN_POINTS: usize = 50_000;

const UNVISITED: i32 = -2;
const NOISE: i32 = -1;

/// Cluster `points` with the given `eps` (cosine *distance*) and `min_pts`.
///
/// Returns clusters (each a list of [`FactId`]s) in cluster-discovery order, with
/// members in the input's slice order; noise points are omitted. A core point has
/// at least `min_pts` points (including itself) within cosine distance `eps`.
///
/// Returns an empty vec for an empty input, `min_pts == 0`, or an input larger than
/// [`MAX_DBSCAN_POINTS`].
pub(super) fn dbscan(points: &[(FactId, &[f32])], eps: f32, min_pts: usize) -> Vec<Vec<FactId>> {
    let n = points.len();
    if n == 0 || min_pts == 0 {
        return Vec::new();
    }
    if n > MAX_DBSCAN_POINTS {
        tracing::warn!(
            points = n,
            max = MAX_DBSCAN_POINTS,
            "dbscan: point set exceeds cap; skipping clustering"
        );
        return Vec::new();
    }

    // Neighbour iff cosine similarity >= (1 - eps); excludes the point itself.
    let sim_threshold = 1.0 - eps;
    let neighbours = |i: usize| -> Vec<usize> {
        (0..n)
            .filter(|&j| j != i && cosine_similarity(points[i].1, points[j].1) >= sim_threshold)
            .collect()
    };

    let mut labels = vec![UNVISITED; n];
    let mut cluster_id = 0i32;

    for p in 0..n {
        if labels[p] != UNVISITED {
            continue;
        }
        let mut seeds = neighbours(p);
        if seeds.len() + 1 < min_pts {
            labels[p] = NOISE; // not a core point (may be reclassified as a border later)
            continue;
        }
        labels[p] = cluster_id;

        // Track membership in the seed set to avoid O(n) `contains` during expansion.
        let mut queued = vec![false; n];
        queued[p] = true;
        for &s in &seeds {
            queued[s] = true;
        }

        let mut i = 0;
        while i < seeds.len() {
            let q = seeds[i];
            i += 1;
            if labels[q] == NOISE {
                labels[q] = cluster_id; // border point joins this cluster
            }
            if labels[q] != UNVISITED {
                continue;
            }
            labels[q] = cluster_id;
            let q_neighbours = neighbours(q);
            if q_neighbours.len() + 1 >= min_pts {
                // q is itself a core point — extend the frontier.
                for &r in &q_neighbours {
                    if !queued[r] {
                        queued[r] = true;
                        seeds.push(r);
                    }
                }
            }
        }
        cluster_id += 1;
    }

    // `cluster_id` only ever increments from 0, and `label >= 0` is guarded — both
    // casts are non-negative by construction.
    #[allow(clippy::cast_sign_loss)]
    let mut clusters: Vec<Vec<FactId>> = vec![Vec::new(); cluster_id as usize];
    for (idx, &(fact_id, _)) in points.iter().enumerate() {
        let label = labels[idx];
        if label >= 0 {
            #[allow(clippy::cast_sign_loss)]
            clusters[label as usize].push(fact_id);
        }
    }
    clusters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_separated_blobs_yield_two_clusters() {
        // Two tight groups far apart in cosine space.
        let a1 = [1.0_f32, 0.0, 0.0];
        let a2 = [0.99, 0.01, 0.0];
        let a3 = [0.98, 0.02, 0.0];
        let b1 = [0.0_f32, 0.0, 1.0];
        let b2 = [0.0, 0.01, 0.99];
        let b3 = [0.0, 0.02, 0.98];
        let points: Vec<(FactId, &[f32])> =
            vec![(1, &a1), (2, &a2), (3, &a3), (4, &b1), (5, &b2), (6, &b3)];
        let clusters = dbscan(&points, 0.15, 3);
        assert_eq!(clusters.len(), 2, "expected two clusters, got {clusters:?}");
        // Each cluster holds one of the blobs.
        assert!(
            clusters
                .iter()
                .any(|c| c.contains(&1) && c.contains(&2) && c.contains(&3))
        );
        assert!(
            clusters
                .iter()
                .any(|c| c.contains(&4) && c.contains(&5) && c.contains(&6))
        );
    }

    #[test]
    fn empty_input_no_clusters() {
        assert!(dbscan(&[], 0.15, 3).is_empty());
    }

    #[test]
    fn below_min_pts_all_noise() {
        let a = [1.0_f32, 0.0];
        let b = [0.99, 0.01];
        let points: Vec<(FactId, &[f32])> = vec![(1, &a), (2, &b)];
        // min_pts = 3 but only 2 near points → no core → no clusters.
        assert!(dbscan(&points, 0.15, 3).is_empty());
    }

    #[test]
    fn all_identical_one_cluster() {
        let v = [0.5_f32, 0.5, 0.5];
        let points: Vec<(FactId, &[f32])> = vec![(1, &v), (2, &v), (3, &v), (4, &v)];
        let clusters = dbscan(&points, 0.15, 3);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].len(), 4);
    }

    #[test]
    fn zero_vectors_are_noise_not_nan() {
        let z1 = [0.0_f32, 0.0, 0.0];
        let z2 = [0.0_f32, 0.0, 0.0];
        let z3 = [0.0_f32, 0.0, 0.0];
        let points: Vec<(FactId, &[f32])> = vec![(1, &z1), (2, &z2), (3, &z3)];
        // cosine of a zero vector is 0.0 → distance 1.0 → never a neighbour.
        assert!(dbscan(&points, 0.15, 3).is_empty());
    }

    #[test]
    fn deterministic_across_runs() {
        let a1 = [1.0_f32, 0.0];
        let a2 = [0.99, 0.01];
        let a3 = [0.98, 0.02];
        let points: Vec<(FactId, &[f32])> = vec![(7, &a1), (8, &a2), (9, &a3)];
        let first = dbscan(&points, 0.15, 2);
        let second = dbscan(&points, 0.15, 2);
        assert_eq!(first, second);
    }
}
