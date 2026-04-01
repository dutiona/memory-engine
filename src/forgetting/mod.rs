//! Ebbinghaus decay and multi-signal importance scoring for fact pruning.
//!
//! Facts with computed importance below `ForgetPolicy::min_importance` get soft-deleted.

mod policy;

pub use policy::prune;
