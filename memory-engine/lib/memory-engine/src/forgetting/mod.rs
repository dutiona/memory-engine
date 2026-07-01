//! Ebbinghaus decay and multi-signal importance scoring for fact pruning.
//!
//! Facts with computed importance below `ForgetPolicy::min_importance` get soft-deleted.

mod policy;
mod types;

pub use me_types::types::forgetting::PruneStats;
pub use policy::prune;
pub use types::ForgetPolicy;
