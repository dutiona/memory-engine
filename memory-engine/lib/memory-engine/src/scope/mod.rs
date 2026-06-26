//! Hierarchical scope tree for multi-context memory isolation.

pub mod tree;

pub use tree::ScopeTree;

/// Maximum byte length of a single scope-path segment (label).
pub const MAX_SEGMENT_LEN: usize = 256;

/// Validate the *structural* rules a single scope-path segment must satisfy:
/// non-empty, no embedded `/`, and at most [`MAX_SEGMENT_LEN`] bytes.
///
/// This is the single source of truth shared by the two scope APIs that must
/// agree on what a valid segment is:
/// - [`crate::store::ScopeStore::ensure_path`] (write path) — wraps the failure
///   reason in [`crate::error::ConflictError::ScopeLabel`].
/// - [`ScopeTree::resolve_path`] (read path) — maps any failure to `None`.
///
/// The "no surrounding whitespace" rule is intentionally *not* enforced here: it
/// is a write-path-only constraint applied by `ScopeStore::validate_label`, so
/// that the read path can keep its defensive trim (a segment with incidental
/// whitespace can never match a stored — already-trimmed — label anyway). See
/// [`ScopeTree::resolve_path`] for the trim semantics.
///
/// # Errors
///
/// Returns a static reason string when `segment` violates a structural rule.
pub fn validate_segment(segment: &str) -> std::result::Result<(), &'static str> {
    if segment.is_empty() {
        return Err("scope label must not be empty");
    }
    if segment.contains('/') {
        return Err("scope label must not contain '/'");
    }
    if segment.len() > MAX_SEGMENT_LEN {
        return Err("scope label must be at most 256 bytes");
    }
    Ok(())
}
