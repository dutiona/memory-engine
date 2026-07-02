//! Hierarchical scope tree for multi-context memory isolation.

pub mod tree;

pub use tree::ScopeTree;

// `MAX_SEGMENT_LEN` + `validate_segment` live in `me-types` (Wave 2 #816 / S2): the
// shared scope-segment SSOT must sit *below* both the in-memory index (read path,
// this crate) and the SQLite backend (write path, the facade's `store`) to keep the
// graph/scope ↔ store carve acyclic. Re-exported here so `me_index::scope::
// {MAX_SEGMENT_LEN, validate_segment}` — and, transitively, the facade's
// `crate::scope::{MAX_SEGMENT_LEN, validate_segment}` re-export — keep resolving.
pub use me_types::types::{MAX_SEGMENT_LEN, validate_segment};
