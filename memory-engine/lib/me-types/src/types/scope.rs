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
