//! The deferred-embedding queue's vocabulary (#1021): what the drain takes, and how a
//! failed attempt changes the queue.

use chrono::{DateTime, Utc};

/// A fact waiting for its vector, as the drain takes it from the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingEmbedding {
    /// The fact to embed.
    pub fact_id: i64,
    /// Its content, the embedder's input.
    pub content: String,
    /// How many attempts already ended in a retry or a failure.
    pub attempts: u32,
}

/// What the queue holds next for the drain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextEmbedding {
    /// This item is due now.
    Due(PendingEmbedding),
    /// Items wait, and the earliest is due at this time.
    NotBefore(DateTime<Utc>),
    /// No item waits (failed items never come back on their own).
    Idle,
}

/// How a failed embedding attempt changes the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingFailure {
    /// Back off only this item: it is due again at `at`.
    Retry {
        /// When the item is due again.
        at: DateTime<Utc>,
    },
    /// This input will never embed as sent: the item stops being taken.
    Fail,
    /// No item was at fault: no queue row changes. `paused` marks the whole queue paused
    /// (a rejected key) until the next answered request.
    Hold {
        /// Whether the attempt pauses the queue.
        paused: bool,
    },
}
