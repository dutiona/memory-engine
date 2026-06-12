use chrono::{DateTime, Utc};

use crate::error::Result;
use crate::store::facts::FactStore;
use crate::types::Fact;

use super::{MemoryEngine, apply_surfaced_stamps};

impl MemoryEngine {
    // --- Public API: Scheduling ---

    /// Returns active facts where `t_valid <= now` and `t_valid IS NOT NULL`.
    ///
    /// On first return, stamps `surfaced_at` for facts that have not yet been
    /// surfaced. Subsequent calls return the original timestamp. The returned
    /// facts always carry the DB-authoritative `surfaced_at` value.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` if scope resolution, the query, or
    /// surfaced-at stamping fails.
    pub fn list_due(&self, now: DateTime<Utc>, scope: Option<&str>) -> Result<Vec<Fact>> {
        let scope_ids = self.resolve_scope_ids(scope)?;
        let mut facts =
            self.with_read(|conn| FactStore::new(conn, self.embed_dim).list_due(now, &scope_ids))?;

        // Stamp surfaced_at for newly-surfaced facts
        let unsurfaced_ids: Vec<i64> = facts
            .iter()
            .filter(|f| f.surfaced_at.is_none())
            .map(|f| f.id)
            .collect();

        if !unsurfaced_ids.is_empty() {
            let stamped = self.stamp_surfaced_facts(&unsurfaced_ids, now)?;
            apply_surfaced_stamps(facts.iter_mut(), &stamped);
        }

        Ok(facts)
    }

    /// Scheduling hint: when should the consumer next call `list_due()`?
    /// Returns the earliest `t_valid` among active future-dated facts.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` if scope resolution or the query fails.
    pub fn next_due_time(&self, scope: Option<&str>) -> Result<Option<DateTime<Utc>>> {
        let scope_ids = self.resolve_scope_ids(scope)?;
        self.with_read(|conn| {
            FactStore::new(conn, self.embed_dim).next_due_time(Utc::now(), &scope_ids)
        })
    }
}
