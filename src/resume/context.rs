use std::collections::HashSet;

use rusqlite::Connection;

use crate::error::Result;
use crate::store::facts::FactStore;
use crate::types::Fact;

/// Configuration for [`resume_context`].
#[derive(Debug, Clone)]
pub struct ResumeConfig {
    /// Scope path to resume from. None = root only.
    pub scope_path: Option<String>,
    /// Max facts in identity tier (root scope, highest importance). Default: 5.
    pub identity_cap: usize,
    /// Max facts in core tier (importance >= threshold). Default: 20.
    pub core_cap: usize,
    /// Importance threshold for core tier. Default: 0.7.
    pub core_min_importance: f64,
    /// Max facts in recent tier (newest by t_created). Default: 10.
    pub recent_cap: usize,
}

impl Default for ResumeConfig {
    fn default() -> Self {
        Self {
            scope_path: None,
            identity_cap: 5,
            core_cap: 20,
            core_min_importance: 0.7,
            recent_cap: 10,
        }
    }
}

/// Result of [`resume_context`].
#[derive(Debug, Clone)]
pub struct ResumeContext {
    /// High-importance facts from root scope (user/agent identity).
    pub identity: Vec<Fact>,
    /// Important facts from the resolved scope and ancestors.
    pub core: Vec<Fact>,
    /// Most recent facts from the resolved scope and ancestors.
    pub recent: Vec<Fact>,
}

/// Retrieve tiered context for resuming a session.
///
/// Three tiers, mutually exclusive (no fact appears in multiple tiers):
///
/// 1. **Identity** — root scope, highest importance, capped at `config.identity_cap`
/// 2. **Core** — importance >= threshold, from resolved scope ancestors, excluding identity
/// 3. **Recent** — most recent by `t_created`, from scope ancestors, excluding identity + core
///
/// Takes pre-resolved scope IDs to avoid holding cache locks across DB access.
pub fn resume_context(
    conn: &Connection,
    root_id: i64,
    scope_ids: &[i64],
    embed_dim: usize,
    config: &ResumeConfig,
) -> Result<ResumeContext> {
    let fact_store = FactStore::new(conn, embed_dim);

    // Tier 1: Identity — root scope, highest importance
    let identity = fact_store.list_by_scope_importance(root_id, config.identity_cap)?;
    let identity_ids: HashSet<i64> = identity.iter().map(|f| f.id).collect();

    // Tier 2: Core — importance >= threshold, from scope ancestors, excluding identity
    let core = fact_store.list_by_scopes_importance(
        scope_ids,
        config.core_min_importance,
        config.core_cap,
        &identity_ids,
    )?;
    let core_ids: HashSet<i64> = core.iter().map(|f| f.id).collect();

    // Tier 3: Recent — newest first, excluding identity + core
    let exclude: HashSet<i64> = identity_ids.union(&core_ids).copied().collect();
    let recent = fact_store.list_by_scopes_recent(scope_ids, config.recent_cap, &exclude)?;

    Ok(ResumeContext {
        identity,
        core,
        recent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::facts::FactStore;
    use crate::store::schema::{init_schema, migrate, open_memory};
    use crate::types::{FactType, NewFact};
    use chrono::Utc;

    const DIM: usize = 4;

    fn setup() -> Connection {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn).unwrap();
        conn
    }

    fn make_fact(content: &str, importance: f64, scope_id: i64) -> NewFact {
        let now = Utc::now();
        NewFact {
            content: content.into(),
            content_hash: String::new(),
            embedding: vec![0.1; DIM],
            fact_type: FactType::Semantic,
            t_created: now,
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            scope_id,
            importance,
            access_count: 0,
            last_accessed: now,
            metadata: serde_json::json!({}),
            is_pinned: false,
        }
    }

    #[test]
    fn resume_empty_db() {
        let conn = setup();
        let ctx = resume_context(&conn, 1, &[1], DIM, &ResumeConfig::default()).unwrap();
        assert!(ctx.identity.is_empty());
        assert!(ctx.core.is_empty());
        assert!(ctx.recent.is_empty());
    }

    #[test]
    fn resume_identity_from_root() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        fs.insert(&make_fact("identity fact", 0.9, 1)).unwrap();

        let ctx = resume_context(&conn, 1, &[1], DIM, &ResumeConfig::default()).unwrap();
        assert_eq!(ctx.identity.len(), 1);
        assert!(ctx.identity[0].content.contains("identity"));
    }

    #[test]
    fn resume_core_filters_by_importance() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        // Insert scope for non-root facts
        conn.execute(
            "INSERT OR IGNORE INTO scopes (id, parent_id, label, depth) VALUES (2, 1, 'proj', 1)",
            [],
        )
        .unwrap();
        fs.insert(&make_fact("low importance", 0.3, 2)).unwrap();
        fs.insert(&make_fact("high importance", 0.9, 2)).unwrap();

        let config = ResumeConfig {
            scope_path: None,
            core_min_importance: 0.5,
            ..ResumeConfig::default()
        };
        let ctx = resume_context(&conn, 1, &[1, 2], DIM, &config).unwrap();
        // Only the 0.9 fact should be in core
        assert_eq!(ctx.core.len(), 1);
        assert!(ctx.core[0].importance >= 0.5);
    }

    #[test]
    fn resume_recent_chronological() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        // Insert 8 facts with low importance so they don't appear in core.
        // identity_cap=2 will consume 2, leaving 6 for recent (we cap at 3).
        for i in 0..8 {
            let mut fact = make_fact(&format!("recent fact {i}"), 0.1, 1);
            fact.t_created = Utc::now() + chrono::Duration::milliseconds(i * 100);
            fs.insert(&fact).unwrap();
        }

        let config = ResumeConfig {
            identity_cap: 2,
            core_min_importance: 0.5, // none qualify for core
            recent_cap: 3,
            ..ResumeConfig::default()
        };
        let ctx = resume_context(&conn, 1, &[1], DIM, &config).unwrap();
        assert_eq!(ctx.identity.len(), 2);
        assert_eq!(ctx.recent.len(), 3);
        // Most recent first
        assert!(ctx.recent[0].t_created >= ctx.recent[1].t_created);
        assert!(ctx.recent[1].t_created >= ctx.recent[2].t_created);
    }

    #[test]
    fn resume_tiers_mutually_exclusive() {
        let conn = setup();
        let fs = FactStore::new(&conn, DIM);
        // One very important root fact (should go to identity)
        fs.insert(&make_fact("identity", 0.95, 1)).unwrap();
        // One important non-root fact (should go to core)
        conn.execute(
            "INSERT OR IGNORE INTO scopes (id, parent_id, label, depth) VALUES (2, 1, 'proj', 1)",
            [],
        )
        .unwrap();
        fs.insert(&make_fact("core fact", 0.8, 2)).unwrap();
        // One low-importance fact (should go to recent)
        fs.insert(&make_fact("recent only", 0.1, 2)).unwrap();

        let config = ResumeConfig {
            scope_path: None,
            identity_cap: 5,
            core_min_importance: 0.7,
            core_cap: 20,
            recent_cap: 10,
        };
        let ctx = resume_context(&conn, 1, &[1, 2], DIM, &config).unwrap();

        let all_ids: Vec<i64> = ctx
            .identity
            .iter()
            .chain(ctx.core.iter())
            .chain(ctx.recent.iter())
            .map(|f| f.id)
            .collect();
        let unique: HashSet<i64> = all_ids.iter().copied().collect();
        assert_eq!(all_ids.len(), unique.len(), "no duplicates across tiers");
    }
}
