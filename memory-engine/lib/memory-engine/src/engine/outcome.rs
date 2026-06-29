use crate::error::Result;
use crate::types::{EventType, NewEvent, Outcome, OutcomeCounts};
use chrono::Utc;

use super::MemoryEngine;

/// Root scope ID as inserted by `init_schema`. Outcome signals are cross-scope
/// and always written to the root.
const ROOT_SCOPE_ID: i64 = 1;

impl MemoryEngine {
    // --- Public API: Outcome tracking ---

    /// Record an outcome signal for a fact.
    ///
    /// Appends an [`EventType::OutcomeSignal`] event to the event log with
    /// payload `{"fact_id": <id>, "outcome": "<variant>"}`. The fact must
    /// exist (active or expired); recording on a nonexistent fact returns
    /// [`MemoryError::NotFound`](crate::MemoryError::NotFound).
    ///
    /// # Errors
    ///
    /// - [`MemoryError::NotFound`](crate::MemoryError::NotFound) if `fact_id` does not exist.
    /// - [`MemoryError::ReadOnly`](crate::MemoryError::ReadOnly) if the engine is read-only.
    /// - [`MemoryError::Database`](crate::MemoryError::Database) on insert failure.
    pub async fn record_outcome(&self, fact_id: i64, outcome: Outcome) -> Result<i64> {
        // Validate fact exists via the port — propagate DB errors as-is,
        // only remap NotFound for a clearer message.
        self.storage.get_fact(fact_id).await.map(|_| ())?;

        let event = NewEvent {
            timestamp: Utc::now(),
            event_type: EventType::OutcomeSignal,
            payload: serde_json::json!({
                "fact_id": fact_id,
                "outcome": outcome,
            }),
            source: "outcome_tracking".into(),
            session_id: None,
            scope_id: ROOT_SCOPE_ID,
            origin_node_id: "local".into(),
            sequence_id: 0,
            created_at: None,
        };

        self.storage.insert_event(&event).await
    }

    /// Return aggregated outcome counts for a fact.
    ///
    /// Returns [`OutcomeCounts::default()`] (all zeros) if the fact exists but has
    /// no outcomes recorded. The `fact_id` filter and `GROUP BY` are pushed into SQL
    /// below the seam ([`EventLog::count_outcome_signals`](crate::storage::EventLog::count_outcome_signals)),
    /// so only the aggregated counts cross the port — not the full `OutcomeSignal`
    /// window.
    ///
    /// # Errors
    ///
    /// - [`MemoryError::NotFound`](crate::MemoryError::NotFound) if `fact_id` does not exist.
    /// - [`MemoryError::Database`](crate::MemoryError::Database) on query failure.
    pub async fn get_outcome_counts(&self, fact_id: i64) -> Result<OutcomeCounts> {
        // Validate fact exists (consistent with record_outcome) — propagate NotFound.
        self.storage.get_fact(fact_id).await.map(|_| ())?;
        self.storage.count_outcome_signals(fact_id).await
    }

    /// Return aggregated outcome counts for many facts in a **single** query.
    ///
    /// The batch equivalent of [`Self::get_outcome_counts`], for callers like the
    /// dream cycle that need counts for every fact in a window (avoids the N+1
    /// pattern of one query per fact). The id-set filter and `GROUP BY fact_id,
    /// outcome` are pushed into SQL below the seam.
    ///
    /// Unlike the single-fact variant, this does **not** validate that each id
    /// exists (that would reintroduce the per-fact round-trips it exists to avoid).
    /// Facts with no recorded outcomes — including nonexistent ids — are simply
    /// absent from the map; callers treat a missing key as [`OutcomeCounts::default`].
    /// An empty `fact_ids` returns an empty map without querying.
    ///
    /// # Errors
    ///
    /// - [`MemoryError::Database`](crate::MemoryError::Database) on query failure.
    pub async fn get_outcome_counts_batch(
        &self,
        fact_ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, OutcomeCounts>> {
        self.storage.count_outcome_signals_batch(fact_ids).await
    }
}
