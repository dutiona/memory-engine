/// A node in the hierarchical scope tree.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScopeNode {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub label: String,
    pub depth: i64,
}

/// How to resolve scopes for a search query.
/// Paths are consumer-facing strings (e.g., "user:michael/project:demo").
/// The engine resolves them to internal integer IDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeQuery {
    /// Facts at exactly this scope path.
    Exact(String),
    /// Facts at this scope path and all descendants.
    Subtree(String),
    /// Facts at this scope path and all ancestors up to root.
    Ancestors(String),
    /// Facts at ancestors + at this scope path's subtree (full inherited context).
    Inherited(String),
}

/// Maximum byte length of a single scope-path segment (label).
pub const MAX_SEGMENT_LEN: usize = 256;

/// Validate the *structural* rules a single scope-path segment must satisfy:
/// non-empty, no embedded `/`, and at most [`MAX_SEGMENT_LEN`] bytes.
///
/// This is the single source of truth shared by the two scope APIs that must
/// agree on what a valid segment is: the **write** path (`ScopeStore::ensure_path`,
/// which wraps the failure reason in a `ConflictError::ScopeLabel`) and the **read**
/// path (`ScopeTree::resolve_path`, which maps any failure to `None`). It is homed in
/// `me-types` (L0) so both the `SQLite` backend and the in-memory index depend on it
/// *downward* — this is what keeps the graph/scope ↔ store carve acyclic (Wave 2
/// #816 / S2).
///
/// The "no surrounding whitespace" rule is intentionally *not* enforced here: it is a
/// write-path-only constraint applied by `ScopeStore::validate_label`, so the read
/// path can keep its defensive trim (a segment with incidental whitespace can never
/// match a stored — already-trimmed — label anyway).
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
