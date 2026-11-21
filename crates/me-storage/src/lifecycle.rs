//! The lifecycle route's store (#1025, ADR-0020 §3, §5): record one delivery with its effects in
//! one transaction, and end the segments of sessions that went quiet.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use me_types::error::Result;
use me_types::types::lifecycle::{Applied, DeliveryWrite};

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

    /// End, as idle, every open segment whose latest event was received before `idle_before`,
    /// in one transaction; returns how many. A second call ends none of them again.
    async fn end_idle_segments(
        &self,
        idle_before: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<usize>;
}
