//! Output type for the forget/prune operation (relocated from the monolith's
//! `forgetting/types.rs`, Wave 2 #816 E.4b Phase B).
//!
//! `PruneStats` is the plain output struct returned by a prune pass. The
//! consumer-tunable `ForgetPolicy` stays in the monolith's `forgetting/types.rs`
//! (an L3 type; L0 me-types cannot link up to it) — it is not pure data (it's
//! paired with the forgetting layer's scoring logic).

/// Statistics returned by the forget/prune operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneStats {
    pub facts_expired: usize,
    pub facts_evaluated: usize,
}
