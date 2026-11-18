//! The deferred-embedding queue's vocabulary (#1021): what the drain takes, how a failed
//! attempt changes the queue, and what `status` reports.

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
        /// Whether the embedder answered, which proves the key works and clears a pause.
        answered: bool,
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

/// The deferred-embedding queue as `status` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedQueueStatus {
    /// Items waiting for their vector (failed ones excluded).
    pub waiting: u64,
    /// When the oldest waiting item was queued.
    pub oldest_enqueued_at: Option<DateTime<Utc>>,
    /// Items whose input the embedder refused.
    pub failed: u64,
    /// When a rejected key first paused the queue; `None` when not paused.
    pub paused_since: Option<DateTime<Utc>>,
    /// The latest rejected-key error while paused.
    pub pause_error: Option<String>,
    /// When an embed last succeeded.
    pub last_success_at: Option<DateTime<Utc>>,
    /// When an embed last failed.
    pub last_error_at: Option<DateTime<Utc>>,
    /// The last embed failure's error.
    pub last_error: Option<String>,
}
