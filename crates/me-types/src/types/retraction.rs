//! The forget retraction (#1023, ADR-0020 §8): what a forget and an unforget answer, and the
//! tombstone a forget leaves.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What `forget` did to the fact it named.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForgetOutcome {
    /// The fact, and every active identical copy in the same Homes, is retracted now.
    Forgotten,
    /// A forget already retracted it; nothing changed.
    AlreadyForgotten,
    /// No such fact in the Homes the caller may act on: a missing id and a fact elsewhere
    /// answer the same.
    NotFound,
}

/// What `unforget` did to the fact it named.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnforgetOutcome {
    /// The forget that named the fact is reversed: each fact it retracted, with whether
    /// recall shows that fact again (it was neither expired nor invalid before the forget).
    Restored { facts: Vec<(i64, bool)> },
    /// The fact exists and no forget names it.
    NotForgotten,
    /// No fact has this id.
    NotFound,
}

/// The block a forget leaves on one fact's content.
///
/// It keeps the content's hash, never the text, so it outlives an erasure of its fact. The
/// reason and the prior timestamps are cleared when the fact is erased.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tombstone {
    /// The forgotten fact; `None` once it was erased.
    pub fact_id: Option<i64>,
    /// The hash of the forgotten content, as the fact store computes it.
    pub content_hash: String,
    /// Why it was forgotten, capped by the caller; `None` once the fact was erased.
    pub reason: Option<String>,
    /// When it was forgotten.
    pub forgotten_at: DateTime<Utc>,
    /// The call record of the forget; `None` once that event was erased.
    pub record_event_id: Option<i64>,
    /// The fact's `t_expired` before the forget, which an unforget writes back.
    pub prior_t_expired: Option<DateTime<Utc>>,
    /// The fact's `t_invalid` before the forget, which an unforget writes back.
    pub prior_t_invalid: Option<DateTime<Utc>>,
}
