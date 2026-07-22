//! Pure math primitives shared across the workspace.
//!
//! No SQL, no I/O, no `me-*` dependency of their own — homed at L0 (Wave 2 #816 /
//! S4, sub-PR 3a) so retrieval (`me-query`), the SQLite backend
//! (`me-backend-sqlite`), consolidation, and the forthcoming `me-archive` crate can
//! all share one definition without any of them depending on a concrete storage
//! backend. Relocated verbatim from `me-backend-sqlite/src/search/vector.rs`, where
//! leaving it would have forced `me-archive` (an L3 leaf) to depend on a specific
//! L2 backend crate just to reuse a scoring function.

/// Compute the cosine similarity between two vectors.
///
/// Returns 0.0 if either vector has zero magnitude (avoids NaN).
///
/// # Length mismatch
///
/// When `a.len() != b.len()` the similarity is computed over the first
/// `min(a.len(), b.len())` components — the `zip` silently truncates to the
/// shorter slice, so any trailing components of the longer slice are ignored.
/// This function performs no length check; the **caller is responsible for
/// passing equal-length vectors**. In-engine callers already enforce dimension
/// uniformity at the store/query layer (e.g. `vector_search_filtered` rejects
/// a wrongly-sized query, and stored embeddings are validated against
/// `embed_dim`), so the truncation never fires on the real read path.
#[must_use]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut norm_a, mut norm_b) = (0.0_f32, 0.0_f32, 0.0_f32);
    for (&x, &y) in a.iter().zip(b.iter()) {
        dot = x.mul_add(y, dot);
        norm_a = x.mul_add(x, norm_a);
        norm_b = y.mul_add(y, norm_b);
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 { 0.0 } else { dot / denom }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identical_vectors() {
        let a = [1.0_f32, 0.0];
        let b = [1.0_f32, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn cosine_orthogonal_vectors() {
        let a = [1.0_f32, 0.0];
        let b = [0.0_f32, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < f32::EPSILON);
    }

    #[test]
    fn cosine_opposite_vectors() {
        let a = [1.0_f32, 0.0];
        let b = [-1.0_f32, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - (-1.0)).abs() < f32::EPSILON);
    }

    #[test]
    fn cosine_mismatched_length_truncates_to_shorter() {
        // Documented behavior (#479): `zip` truncates to the shorter slice, so
        // the trailing component of `a` (9.9) is ignored and the result equals
        // the equal-length computation over the common prefix.
        let truncated = cosine_similarity(&[1.0_f32, 0.0, 9.9], &[1.0_f32, 0.0]);
        let control = cosine_similarity(&[1.0_f32, 0.0], &[1.0_f32, 0.0]);
        assert!(
            (truncated - control).abs() < f32::EPSILON,
            "trailing component of the longer slice must be ignored: \
             truncated={truncated}, control={control}"
        );
        // The common prefix [1,0] vs [1,0] is identical, so the score is 1.0.
        assert!((truncated - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn cosine_mismatched_length_symmetric_truncation() {
        // Truncation is independent of which argument is longer: swapping the
        // operands yields the same prefix-only score.
        let a_longer = cosine_similarity(&[1.0_f32, 2.0, 7.0], &[1.0_f32, 2.0]);
        let b_longer = cosine_similarity(&[1.0_f32, 2.0], &[1.0_f32, 2.0, 7.0]);
        let control = cosine_similarity(&[1.0_f32, 2.0], &[1.0_f32, 2.0]);
        assert!((a_longer - control).abs() < f32::EPSILON);
        assert!((b_longer - control).abs() < f32::EPSILON);
    }

    #[test]
    fn cosine_zero_vector_returns_zero() {
        let a = [0.0_f32, 0.0];
        let b = [1.0_f32, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < f32::EPSILON);
        // Also both zero
        let sim2 = cosine_similarity(&a, &a);
        assert!(sim2.abs() < f32::EPSILON);
    }

    #[test]
    fn cosine_empty_slices_return_zero() {
        // Empty zip -> norm 0 -> denom 0 -> returns 0.0 (NaN-avoidance promise).
        // Use abs-epsilon (not assert_eq!) to satisfy clippy::float_cmp under the
        // CI `-D warnings` gate, matching the sibling zero-vector test convention.
        assert!(cosine_similarity(&[], &[]).abs() < f32::EPSILON);
        assert!(cosine_similarity(&[], &[1.0]).abs() < f32::EPSILON);
    }

    mod proptest_cosine {
        use super::*;
        use proptest::prelude::*;

        // Bounded to avoid f32 overflow in squared-sum (x*x).
        // Real embeddings are typically in [-1, 1] or small magnitudes.
        fn bounded_f32() -> impl Strategy<Value = f32> {
            -1e18_f32..1e18_f32
        }

        fn nonzero_vec(max_len: usize) -> impl Strategy<Value = Vec<f32>> {
            proptest::collection::vec(bounded_f32(), 1..max_len)
                .prop_filter("at least one nonzero", |v| v.iter().any(|&x| x != 0.0))
        }

        proptest! {
            #[test]
            fn symmetry(
                a in proptest::collection::vec(bounded_f32(), 1..64usize),
                b in proptest::collection::vec(bounded_f32(), 1..64usize),
            ) {
                let len = a.len().min(b.len());
                let a = &a[..len];
                let b = &b[..len];
                let ab = cosine_similarity(a, b);
                let ba = cosine_similarity(b, a);
                prop_assert!((ab - ba).abs() < 1e-6,
                    "symmetry violated: cos(a,b)={ab} != cos(b,a)={ba}");
            }

            #[test]
            fn identical_vectors_equal_one(v in nonzero_vec(64)) {
                let sim = cosine_similarity(&v, &v);
                prop_assert!((sim - 1.0).abs() < 1e-5,
                    "identical vectors should give 1.0, got {sim}");
            }

            #[test]
            fn bounded(
                a in proptest::collection::vec(bounded_f32(), 1..64usize),
                b in proptest::collection::vec(bounded_f32(), 1..64usize),
            ) {
                let len = a.len().min(b.len());
                let sim = cosine_similarity(&a[..len], &b[..len]);
                prop_assert!((-1.0 - 1e-5..=1.0 + 1e-5).contains(&sim),
                    "cosine similarity {sim} out of [-1, 1] bounds");
            }

            /// Anti-parallel vectors (v and -v) must yield cosine similarity of -1.0.
            #[test]
            fn antiparallel_vectors_equal_minus_one(v in nonzero_vec(64)) {
                let neg: Vec<f32> = v.iter().map(|&x| -x).collect();
                let sim = cosine_similarity(&v, &neg);
                prop_assert!((sim - (-1.0)).abs() < 1e-5,
                    "anti-parallel vectors should give -1.0, got {sim}");
            }
        }
    }
}
