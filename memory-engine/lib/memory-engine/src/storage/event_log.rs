//! The append-only event log — the engine's source of truth.
//!
//! Maps to `store/events.rs`. **Upcasting** (event-payload schema evolution) is a
//! backend *construction detail*: the `UpcasterRegistry` is handed to the backend
//! at construction and never crosses a method boundary. The trait exposes both the
//! **raw** reads (`get_event`/`list_events`) and the **upcasted** reads
//! (`get_upcasted_event`/`list_upcasted_events`); the backend applies its registry
//! internally for the latter.

use async_trait::async_trait;

use crate::error::Result;
use crate::types::{Event, EventFilter, NewEvent};

/// The append-only event log.
///
/// # Errors
/// Every method returns [`MemoryError::Storage`](crate::error::MemoryError::Storage)
/// on a backend failure (or [`NotFound`](crate::error::MemoryError::NotFound) for a
/// missing id).
#[async_trait]
pub trait EventLog: Send + Sync {
    async fn insert_event(&self, event: &NewEvent) -> Result<i64>;
    async fn get_event(&self, id: i64) -> Result<Event>;
    async fn list_events(&self, filter: &EventFilter) -> Result<Vec<Event>>;
    async fn count_events(&self, filter: &EventFilter) -> Result<i64>;
    /// Stream every event to `f`, one row at a time — the O(1)-peak-memory dump
    /// primitive. `EventStore` has no `list_all` today, so this is the full-scan.
    async fn for_each_event(&self, f: &mut (dyn FnMut(Event) -> Result<()> + Send)) -> Result<()>;
    /// Current-revision view of one event (the backend applies its upcaster
    /// registry internally).
    async fn get_upcasted_event(&self, id: i64) -> Result<Event>;
    /// Current-revision view of a filtered window.
    async fn list_upcasted_events(&self, filter: &EventFilter) -> Result<Vec<Event>>;

    /// Aggregate outcome-signal counts for one fact via a SQL `GROUP BY` push-down
    /// over the `OutcomeSignal` events (restoring the push-down the cutover would
    /// otherwise lose by materializing the window in memory). Returns
    /// [`OutcomeCounts::default`](crate::types::OutcomeCounts) when the fact has no
    /// recorded outcomes. Does **not** validate fact existence — the caller does.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Storage`](crate::error::MemoryError::Storage) on a
    /// backend failure.
    async fn count_outcome_signals(&self, fact_id: i64) -> Result<crate::types::OutcomeCounts>;

    /// Batch variant of [`count_outcome_signals`](Self::count_outcome_signals):
    /// one `GROUP BY fact_id, outcome` scan filtered to `fact_ids`. Facts with no
    /// recorded outcomes (including nonexistent ids) are absent from the map; an
    /// empty `fact_ids` returns an empty map without querying.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Storage`](crate::error::MemoryError::Storage) on a
    /// backend failure.
    async fn count_outcome_signals_batch(
        &self,
        fact_ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, crate::types::OutcomeCounts>>;
}
