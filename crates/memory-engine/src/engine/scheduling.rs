use chrono::{DateTime, Utc};

use crate::error::Result;
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
    /// Returns `MemoryError::Storage` if scope resolution, the query, or
    /// surfaced-at stamping fails.
    pub async fn list_due(&self, now: DateTime<Utc>, scope: Option<&str>) -> Result<Vec<Fact>> {
        let scope_ids = self.resolve_scope_ids(scope)?;
        // Scheduling contract: ALL due facts, uncapped and unfiltered (#396 made
        // the exclude/limit params optional precisely so this path stays uncapped).
        let mut facts = self
            .storage
            .list_due_facts(now, &scope_ids, &[], None)
            .await?;

        // Stamp surfaced_at for newly-surfaced facts
        let unsurfaced_ids: Vec<i64> = facts
            .iter()
            .filter(|f| f.surfaced_at.is_none())
            .map(|f| f.id)
            .collect();

        if !unsurfaced_ids.is_empty() {
            let stamped = self.stamp_surfaced_facts(&unsurfaced_ids, now).await?;
            apply_surfaced_stamps(facts.iter_mut(), &stamped);
        }

        Ok(facts)
    }

    /// Scheduling hint: when should the consumer next call `list_due()`?
    /// Returns the earliest `t_valid` among active future-dated facts.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` if scope resolution or the query fails.
    pub async fn next_due_time(&self, scope: Option<&str>) -> Result<Option<DateTime<Utc>>> {
        let scope_ids = self.resolve_scope_ids(scope)?;
        self.storage.next_due_time(Utc::now(), &scope_ids).await
    }
}
