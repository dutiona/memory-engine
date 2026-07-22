//! Hierarchical scope tree for multi-context memory isolation.

pub mod tree;

use parking_lot::RwLock;

use me_storage::StorageBackend;
use me_types::error::Result;

pub use tree::ScopeTree;

/// Hydrate the in-memory [`ScopeTree`] from a leaf scope's persisted ancestry.
///
/// Walks `parent_id` links up from `leaf_id` through the persistence **port**'s
/// `get_scope` — stopping at the root or the first already-seen node
/// (cycle guard) — collects the chain, then inserts it under one brief write lock.
///
/// Single-sourced here (#973): the ingest primitive (`me-ingest`), the facade's
/// `MemoryEngine::cache_scope_chain`, its bootstrap path, and the dream-cycle scope
/// mirroring all route to this one definition — collapsing the copy the S2 "me-index is
/// storage-free" carve forced into `me-ingest` and the facade. me-index depends on the
/// port **trait**, not a concrete backend, so it stays backend-agnostic (DIP).
///
/// # Send-safety
///
/// The `get_scope` awaits run with **no** lock held; the `scope_tree` write guard is taken
/// only for the final synchronous insert loop (no `.await` inside), so the returned future
/// stays `Send`.
///
/// # Errors
///
/// Propagates any failure from the port's `get_scope`.
///
/// # Examples
///
/// The `storage` argument is the persistence port (`&dyn StorageBackend`); consumers pass
/// their `Arc<dyn StorageBackend>` by deref (`&*arc` / `&**arc_ref`). The example is
/// `ignore`d because it needs a live backend:
///
/// ```ignore
/// use parking_lot::RwLock;
/// use me_index::{ScopeTree, cache_scope_chain};
/// use me_storage::StorageBackend;
///
/// async fn hydrate(
///     storage: &dyn StorageBackend,
///     scope_tree: &RwLock<ScopeTree>,
///     leaf_id: i64,
/// ) -> me_types::error::Result<()> {
///     // Insert `leaf_id` and all its ancestors (up to, but excluding, the root).
///     cache_scope_chain(storage, scope_tree, leaf_id).await
/// }
/// ```
pub async fn cache_scope_chain(
    storage: &dyn StorageBackend,
    scope_tree: &RwLock<ScopeTree>,
    leaf_id: i64,
) -> Result<()> {
    let mut nodes = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut current = Some(leaf_id);
    while let Some(node_id) = current {
        if node_id == ScopeTree::root_id() || !seen.insert(node_id) {
            break;
        }
        let node = storage.get_scope(node_id).await?;
        current = node.parent_id;
        nodes.push(node);
    }
    {
        let mut tree = scope_tree.write();
        for node in nodes {
            tree.insert(node);
        }
    }
    Ok(())
}

// `MAX_SEGMENT_LEN` + `validate_segment` live in `me-types` (Wave 2 #816 / S2): the
// shared scope-segment SSOT must sit *below* both the in-memory index (read path,
// this crate) and the SQLite backend (write path, the facade's `store`) to keep the
// graph/scope ↔ store carve acyclic. Re-exported here so `me_index::scope::
// {MAX_SEGMENT_LEN, validate_segment}` — and, transitively, the facade's
// `crate::scope::{MAX_SEGMENT_LEN, validate_segment}` re-export — keep resolving.
pub use me_types::types::{MAX_SEGMENT_LEN, validate_segment};
