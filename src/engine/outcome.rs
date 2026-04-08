use chrono::Utc;

use crate::error::{MemoryError, Result};
use crate::store::events::{EventFilter, EventStore};
use crate::store::facts::FactStore;
use crate::types::{EventType, NewEvent, Outcome, OutcomeCounts};

use super::MemoryEngine;

impl MemoryEngine {
    // --- Public API: Outcome tracking ---

    /// Record an outcome signal for a fact.
    ///
    /// Appends an [`EventType::OutcomeSignal`] event to the event log with
    /// payload `{"fact_id": <id>, "outcome": "<variant>"}`. The fact must
    /// exist (active or expired); recording on a nonexistent fact returns
    /// [`MemoryError::NotFound`].
    ///
    /// # Errors
    ///
    /// - [`MemoryError::NotFound`] if `fact_id` does not exist.
    /// - [`MemoryError::ReadOnly`] if the engine is read-only.
    /// - [`MemoryError::Database`] on insert failure.
    pub fn record_outcome(&self, fact_id: i64, outcome: Outcome) -> Result<i64> {
        // Validate fact exists via read pool — no write lock needed for check.
        self.with_read(|conn| {
            FactStore::new(conn, self.embed_dim)
                .get(fact_id)
                .map_err(|_| MemoryError::NotFound(format!("fact {fact_id}")))
        })?;

        let event = NewEvent {
            timestamp: Utc::now(),
            event_type: EventType::OutcomeSignal,
            payload: serde_json::json!({
                "fact_id": fact_id,
                "outcome": outcome,
            }),
            source: "outcome_tracking".into(),
            session_id: None,
            scope_id: 1, // root scope — outcome signals are cross-scope
            origin_node_id: "local".into(),
            sequence_id: 0,
            created_at: None,
        };

        let conn = self.write_conn()?;
        EventStore::new(&conn, &self.upcaster_registry).insert(&event)
    }

    /// Return aggregated outcome counts for a fact.
    ///
    /// Scans all [`EventType::OutcomeSignal`] events whose payload references
    /// `fact_id` and tallies positive, negative, and neutral outcomes.
    ///
    /// Returns [`OutcomeCounts::default()`] (all zeros) if no outcomes have
    /// been recorded — this is not an error.
    ///
    /// # Errors
    ///
    /// - [`MemoryError::Database`] on query failure.
    pub fn get_outcome_counts(&self, fact_id: i64) -> Result<OutcomeCounts> {
        self.with_read(|conn| {
            let store = EventStore::new(conn, &self.upcaster_registry);

            let filter = EventFilter {
                event_type: Some(EventType::OutcomeSignal),
                ..EventFilter::default()
            };

            let events = store.list(&filter)?;

            let mut counts = OutcomeCounts::default();
            for event in &events {
                let payload_fact_id = event.payload.get("fact_id").and_then(|v| v.as_i64());
                if payload_fact_id != Some(fact_id) {
                    continue;
                }

                let outcome_str = event
                    .payload
                    .get("outcome")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                match outcome_str {
                    "Positive" => counts.positive += 1,
                    "Negative" => counts.negative += 1,
                    "Neutral" => counts.neutral += 1,
                    _ => {} // skip malformed payloads
                }
            }

            Ok(counts)
        })
    }
}
