//! The forget retraction (#1023, ADR-0020 §8): retract a fact behind a tombstone, reverse it,
//! and read the block.
//!
//! A bounded context of its own: a forget writes a tombstone, ends the fact's validity and
//! appends the call's record in one transaction; the tombstone then keeps that content out
//! of every gated insert (the import chokepoint, batches, promotion), which each backend
//! checks inside its own write.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use me_types::error::Result;
use me_types::types::{
    EraseReport, EraseTarget, ForgetOutcome, NewEvent, Tombstone, UnforgetOutcome,
};

use crate::ScopedWrite;

/// Retraction and its reversal.
///
/// # Errors
/// Every method returns [`MemoryError::Storage`](me_types::error::MemoryError::Storage) on
/// a backend failure; a write on a read-only backend returns
/// [`MemoryError::ReadOnly`](me_types::error::MemoryError::ReadOnly).
#[async_trait]
pub trait Retraction: Send + Sync {
    /// Retract fact `id` when its scope is one of `homes`, and append `record` under the
    /// scope path `record_at` (its `scope_id` is replaced by that path's leaf), in one
    /// transaction that creates the path.
    ///
    /// - The fact is not in `homes` (or `homes` is empty, or no such fact):
    ///   [`ForgetOutcome::NotFound`]; only the record is written.
    /// - A tombstone already names it: [`ForgetOutcome::AlreadyForgotten`]; only the record.
    /// - Otherwise the fact and every active fact with the same content in `homes` each get
    ///   a tombstone naming the record, holding `reason` and the fact's prior `t_expired`
    ///   and `t_invalid`; `t_expired` becomes `coalesce(t_expired, now)` and `t_invalid`
    ///   `min(coalesce(t_invalid, now), now)`: [`ForgetOutcome::Forgotten`].
    ///
    /// # Contract
    ///
    /// `Ok ⟹ the record and every change committed together; Err ⟹ store unchanged`.
    async fn forget_fact(
        &self,
        id: i64,
        homes: &[i64],
        reason: &str,
        now: DateTime<Utc>,
        record_at: &str,
        record: &NewEvent,
    ) -> Result<(ForgetOutcome, ScopedWrite)>;

    /// Reverse the forget that named fact `id`: every fact whose tombstone names the same
    /// record and still exists gets its prior `t_expired` and `t_invalid` back and loses its
    /// tombstone (a tombstone with no record reverses its own fact only). An `unforget`
    /// maintenance record is appended at the root in the same transaction.
    ///
    /// # Contract
    ///
    /// `Ok ⟹ committed; Err ⟹ store unchanged`, except a post-commit
    /// [`IndexInconsistent`](me_types::error::MemoryError) from a vector-index notify: the
    /// write succeeded, rebuild the index rather than retry.
    async fn unforget_fact(&self, id: i64, now: DateTime<Utc>) -> Result<UnforgetOutcome>;

    /// The tombstone that names fact `id`, if any.
    async fn tombstone(&self, fact_id: i64) -> Result<Option<Tombstone>>;

    /// Whether a tombstone blocks `content`, in any scope.
    async fn is_forgotten(&self, content: &str) -> Result<bool>;

    /// Erase `target`: the store's one hard delete (#1023).
    ///
    /// - [`EraseTarget::Fact`]: the fact. Its events stay, so a `remember`'s call record
    ///   stays linkable through the erasure record.
    /// - [`EraseTarget::Machine`]: the facts whose `metadata.machine` names it or that sit
    ///   in its bucket subtree, the events whose top-level `payload.machine` names it or that
    ///   sit there, the facts built on those events, the bucket's activities and session
    ///   checkpoints, the activities it captured (wherever they sit), and its lifecycle
    ///   receipts, quarantine records and segments.
    ///
    /// Both add every wisdom fact whose lineage names a taken fact (transitively), the
    /// summaries naming one, the activities that promoted one, and every incident edge. A
    /// tombstone of a taken fact keeps its hash and loses its reason and priors, so the
    /// block survives. One `erase` maintenance record is appended when anything was deleted.
    /// A missing fact or a machine that wrote nothing deletes nothing.
    ///
    /// # Contract
    ///
    /// `Ok ⟹ committed; Err ⟹ store unchanged`. The one carve-out is the post-commit purge
    /// (`VACUUM`, WAL truncation), which reports its failure in
    /// [`EraseReport::purged`] instead of failing the committed erasure. It runs even when
    /// nothing was deleted, so the same call retries a failed purge.
    ///
    /// # Errors
    ///
    /// `Conflict(PolicyParameter)` for a machine name that is not a lowercase DNS label;
    /// `Conflict(PayloadTooLarge)` when the erasure record would exceed the event limit.
    async fn erase(&self, target: &EraseTarget, now: DateTime<Utc>) -> Result<EraseReport>;
}
