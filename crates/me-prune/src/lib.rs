//! Ebbinghaus decay and multi-signal importance scoring for fact pruning.
//!
//! Facts with computed importance below `PrunePolicy::min_importance` get soft-deleted.
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod policy;

pub use me_types::types::pruning::{PrunePolicy, PruneStats};
pub use policy::prune;
