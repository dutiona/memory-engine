//! The lifecycle route's store (#1025, ADR-0020 §3, §5): record one delivery with its effects in
//! one transaction, and end the segments of sessions that went quiet.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use me_types::error::Result;
use me_types::types::lifecycle::{Applied, Batch, BatchMode, DeliveryWrite, Keyed};

/// Idempotent delivery of harness events.
///
/// # Errors
/// Every method returns [`MemoryError::Storage`](me_types::error::MemoryError::Storage) on a
/// backend failure: [`StorageError::Unavailable`](me_types::error::StorageError::Unavailable)
/// when the store cannot take a write now and the same call can work later, `Backend` otherwise.
/// A write on a read-only backend returns
/// [`MemoryError::ReadOnly`](me_types::error::MemoryError::ReadOnly).
#[async_trait]
pub trait Lifecycle: Send + Sync {
    /// Record the delivery `(machine, event_id)` with the SHA-256 of its body, and what
    /// [`DeliveryWrite`] carries, in one transaction.
    ///
    /// The key is read first, under the write lock: a held key answers
    /// [`Applied::Duplicate`] (same digest) or [`Applied::Collision`] (another) and writes
    /// nothing, whatever `write` holds. Otherwise `write` is applied: [`DeliveryWrite::Check`]
    /// answers [`Applied::Absent`] and writes nothing; a quarantine stores its metadata under
    /// the key; a capture stores the event, its activity, its memory (queued for its vector) and
    /// the receipt that links it to the call record it names, and steps its session's segments
    /// by ADR-0020 §5. Every time written is `received_at` or `now`, never the backend's clock.
    ///
    /// # Contract
    ///
    /// `Ok ⟹ committed whole; Err ⟹ store unchanged`.
    async fn record_delivery(
        &self,
        machine: &str,
        event_id: &str,
        body_sha256: &[u8; 32],
        received_at: DateTime<Utc>,
        write: DeliveryWrite,
    ) -> Result<Applied>;

    /// Record an upload's `items` for `machine`, received at `received_at`, in one transaction
    /// (#1026).
    ///
    /// The transaction takes the write lock first, then reads, once per session of the batch's
    /// captures, the first live event of that session and `machine` (an event whose `source` is
    /// `lifecycle`, by order key). Each item is then applied as [`record_delivery`] applies one,
    /// in item order: its key first, a key an earlier item of the batch wrote included
    /// ([`Applied::Duplicate`] or [`Applied::Collision`]); then a capture at or after its
    /// session's first live event is [`Applied::LiveOverlap`] and writes nothing; otherwise the
    /// item is stored. A memory is filed under its home with its event, blocked by a tombstone
    /// of its text, reinforcing an active twin in its home, or queued for its vector.
    ///
    /// The batch's preview is [`preview_digest`](me_types::types::lifecycle::preview_digest) of
    /// `context` and the outcomes. A [`BatchMode::DryRun`] rolls back; a [`BatchMode::Write`]
    /// commits only when no item collided and its preview is the batch's, and rolls back
    /// otherwise. Every time written is `received_at`, never the backend's clock.
    ///
    /// [`record_delivery`]: Lifecycle::record_delivery
    ///
    /// # Contract
    ///
    /// `Ok` with `committed` ⟹ committed whole; any other answer ⟹ store unchanged. Every item
    /// has its outcome, a collision included.
    async fn record_deliveries(
        &self,
        machine: &str,
        received_at: DateTime<Utc>,
        items: Vec<Keyed>,
        context: [u8; 32],
        mode: BatchMode,
    ) -> Result<Batch>;

    /// End, as idle, every open segment whose latest event was received before `idle_before`,
    /// in one transaction; returns how many. A second call ends none of them again.
    async fn end_idle_segments(
        &self,
        idle_before: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<usize>;
}
