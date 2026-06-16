use crate::error::Result;
use crate::store::events::EventStore;
use crate::store::facts::FactStore;
use crate::types::{EventType, NewEvent, Outcome, OutcomeCounts};
use chrono::Utc;
use rusqlite::params;

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
    /// [`MemoryError::NotFound`].
    ///
    /// # Errors
    ///
    /// - [`MemoryError::NotFound`] if `fact_id` does not exist.
    /// - [`MemoryError::ReadOnly`] if the engine is read-only.
    /// - [`MemoryError::Database`] on insert failure.
    pub fn record_outcome(&self, fact_id: i64, outcome: Outcome) -> Result<i64> {
        // Validate fact exists via read pool — propagate DB errors as-is,
        // only remap NotFound for a clearer message.
        self.with_read(|conn| {
            FactStore::new(conn, self.embed_dim)
                .get(fact_id)
                .map(|_| ())
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
            scope_id: ROOT_SCOPE_ID,
            origin_node_id: "local".into(),
            sequence_id: 0,
            created_at: None,
        };

        let conn = self.write_conn()?;
        EventStore::new(&conn, &self.upcaster_registry).insert(&event)
    }

    /// Return aggregated outcome counts for a fact.
    ///
    /// Pushes `fact_id` filtering into SQL via `json_extract` so only matching
    /// events are scanned. Returns [`OutcomeCounts::default()`] (all zeros) if
    /// the fact exists but has no outcomes recorded.
    ///
    /// # Errors
    ///
    /// - [`MemoryError::NotFound`] if `fact_id` does not exist.
    /// - [`MemoryError::Database`] on query failure.
    pub fn get_outcome_counts(&self, fact_id: i64) -> Result<OutcomeCounts> {
        self.with_read(|conn| {
            // Validate fact exists (consistent with record_outcome).
            FactStore::new(conn, self.embed_dim)
                .get(fact_id)
                .map(|_| ())?;

            // Aggregate in SQL — avoids loading all OutcomeSignal events into memory.
            let mut stmt = conn.prepare(
                "SELECT json_extract(payload, '$.outcome') AS outcome, COUNT(*) AS cnt
                 FROM events
                 WHERE event_type = 'OutcomeSignal'
                   AND json_extract(payload, '$.fact_id') = ?1
                 GROUP BY outcome",
            )?;

            let mut counts = OutcomeCounts::default();
            let rows = stmt.query_map(params![fact_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
            })?;

            for row in rows {
                let (outcome_str, cnt) = row?;
                match outcome_str.as_str() {
                    "Positive" => counts.positive = cnt,
                    "Negative" => counts.negative = cnt,
                    "Neutral" => counts.neutral = cnt,
                    _ => {} // skip unknown variants (forward-compat)
                }
            }

            Ok(counts)
        })
    }

    /// Return aggregated outcome counts for many facts in a **single** query.
    ///
    /// The batch equivalent of [`Self::get_outcome_counts`], for callers like the
    /// dream cycle that need counts for every fact in a window (avoids the N+1
    /// pattern of one query per fact). Aggregates with one `GROUP BY fact_id, outcome`
    /// scan filtered to the requested ids.
    ///
    /// Unlike the single-fact variant, this does **not** validate that each id
    /// exists (that would reintroduce the per-fact round-trips it exists to avoid).
    /// Facts with no recorded outcomes — including nonexistent ids — are simply
    /// absent from the map; callers treat a missing key as [`OutcomeCounts::default`].
    /// An empty `fact_ids` returns an empty map without querying.
    ///
    /// # Errors
    ///
    /// - [`MemoryError::Database`] on query failure.
    pub fn get_outcome_counts_batch(
        &self,
        fact_ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, OutcomeCounts>> {
        if fact_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        self.with_read(|conn| {
            let ids_json = serde_json::to_string(fact_ids)?;
            let mut stmt = conn.prepare(
                "SELECT json_extract(payload, '$.fact_id') AS fid,
                        json_extract(payload, '$.outcome') AS outcome,
                        COUNT(*) AS cnt
                 FROM events
                 WHERE event_type = 'OutcomeSignal'
                   AND json_extract(payload, '$.fact_id') IN (SELECT value FROM json_each(?1))
                 GROUP BY fid, outcome",
            )?;

            let mut by_fact: std::collections::HashMap<i64, OutcomeCounts> =
                std::collections::HashMap::new();
            let rows = stmt.query_map(params![ids_json], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u32>(2)?,
                ))
            })?;

            for row in rows {
                let (fid, outcome_str, cnt) = row?;
                let entry = by_fact.entry(fid).or_default();
                match outcome_str.as_str() {
                    "Positive" => entry.positive = cnt,
                    "Negative" => entry.negative = cnt,
                    "Neutral" => entry.neutral = cnt,
                    _ => {} // skip unknown variants (forward-compat)
                }
            }

            Ok(by_fact)
        })
    }
}
