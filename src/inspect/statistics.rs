use std::collections::BTreeMap;
use std::path::Path;

use chrono::Utc;
use rusqlite::Connection;

use super::types::{
    EdgeStats, EngineStatistics, EventStats, FactStats, ScopeStats, StorageStats, SummaryStats,
};
use crate::error::Result;
use crate::types::ConsolidationLevel;

/// Compute aggregate statistics from the database.
///
/// # Errors
///
/// Returns [`MemoryError::Database`] on SQL failure.
pub fn compute_statistics(conn: &Connection, db_path: Option<&Path>) -> Result<EngineStatistics> {
    let now = Utc::now();
    let now_str = now.to_rfc3339();

    // Fact counts
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM facts", [], |r| r.get(0))?;
    let active: i64 = conn.query_row(
        "SELECT COUNT(*) FROM facts WHERE t_expired IS NULL",
        [],
        |r| r.get(0),
    )?;
    let expired: i64 = conn.query_row(
        "SELECT COUNT(*) FROM facts WHERE t_expired IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    let pinned: i64 = conn.query_row(
        "SELECT COUNT(*) FROM facts WHERE is_pinned = 1 AND t_expired IS NULL",
        [],
        |r| r.get(0),
    )?;
    // Due: t_valid <= now, not expired, not invalidated
    let due: i64 = conn.query_row(
        "SELECT COUNT(*) FROM facts WHERE t_expired IS NULL AND t_valid IS NOT NULL AND t_valid <= ?1 AND (t_invalid IS NULL OR t_invalid > ?1)",
        [&now_str],
        |r| r.get(0),
    )?;

    // Edge counts
    let edge_total: i64 = conn.query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))?;
    let edge_active: i64 = conn.query_row(
        "SELECT COUNT(*) FROM edges WHERE t_expired IS NULL",
        [],
        |r| r.get(0),
    )?;
    let edge_expired: i64 = conn.query_row(
        "SELECT COUNT(*) FROM edges WHERE t_expired IS NOT NULL",
        [],
        |r| r.get(0),
    )?;

    // Summary counts by level
    let summary_total: i64 = conn.query_row("SELECT COUNT(*) FROM summaries", [], |r| r.get(0))?;
    let mut by_level = BTreeMap::new();
    {
        let mut stmt = conn.prepare("SELECT level, COUNT(*) FROM summaries GROUP BY level")?;
        let rows = stmt.query_map([], |row| {
            let level_str: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            Ok((level_str, count))
        })?;
        for row in rows {
            let (level_str, count) = row?;
            let level = match level_str.as_str() {
                "local" => ConsolidationLevel::Local,
                "cluster" => ConsolidationLevel::Cluster,
                "global" => ConsolidationLevel::Global,
                _ => continue,
            };
            by_level.insert(level, count);
        }
    }

    // Scope counts
    let scope_total: i64 = conn.query_row("SELECT COUNT(*) FROM scopes", [], |r| r.get(0))?;
    let scope_max_depth: i64 =
        conn.query_row("SELECT COALESCE(MAX(depth), 0) FROM scopes", [], |r| {
            r.get(0)
        })?;

    // Event count
    let event_total: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;

    // Storage stats
    let page_count: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
    let page_size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    let main_db_bytes = page_count.checked_mul(page_size).ok_or_else(|| {
        crate::error::MemoryError::Internal(format!(
            "storage size overflow: page_count {page_count} * page_size {page_size}"
        ))
    })?;

    Ok(EngineStatistics {
        facts: FactStats {
            total,
            active,
            expired,
            pinned,
            due,
        },
        edges: EdgeStats {
            total: edge_total,
            active: edge_active,
            expired: edge_expired,
        },
        summaries: SummaryStats {
            total: summary_total,
            by_level,
        },
        scopes: ScopeStats {
            total: scope_total,
            max_depth: scope_max_depth,
        },
        events: EventStats { total: event_total },
        storage: StorageStats {
            page_count,
            page_size,
            main_db_bytes,
            file_path: db_path.map(|p| p.to_string_lossy().into_owned()),
        },
    })
}

#[cfg(test)]
mod tests {
    use crate::engine::MemoryEngine;
    use crate::traits::EmbeddingProvider;
    use crate::types::{AddFactRequest, FactType};

    const DIM: usize = 4;

    struct FakeEmbed;
    impl EmbeddingProvider for FakeEmbed {
        fn embed(&self, _text: &str) -> crate::error::Result<Vec<f32>> {
            Ok(vec![0.1, 0.2, 0.3, 0.4])
        }
    }

    #[test]
    fn empty_engine_statistics() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let stats = engine.statistics().unwrap();
        assert_eq!(stats.facts.total, 0);
        assert_eq!(stats.facts.active, 0);
        assert_eq!(stats.facts.expired, 0);
        assert_eq!(stats.facts.pinned, 0);
        assert_eq!(stats.facts.due, 0);
        assert_eq!(stats.edges.total, 0);
        assert_eq!(stats.summaries.total, 0);
        // Root scope always exists
        assert!(stats.scopes.total >= 1);
        assert_eq!(stats.events.total, 0);
        assert!(stats.storage.page_count > 0);
        assert!(stats.storage.page_size > 0);
        assert!(stats.storage.main_db_bytes > 0);
        assert!(stats.storage.file_path.is_none());
    }

    #[test]
    fn statistics_with_facts() {
        use crate::types::AddFactOptions;

        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "fact one".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                &FakeEmbed,
                None,
            )
            .unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "fact two".into(),
                    fact_type: FactType::Episodic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                &FakeEmbed,
                None,
            )
            .unwrap();
        // Add a pinned fact
        let pin_opts = AddFactOptions {
            pinned: Some(true),
            ..Default::default()
        };
        engine
            .add_fact(
                &AddFactRequest {
                    content: "pinned fact".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: Some(pin_opts),
                },
                &FakeEmbed,
                None,
            )
            .unwrap();

        let stats = engine.statistics().unwrap();
        assert_eq!(stats.facts.total, 3);
        assert_eq!(stats.facts.active, 3);
        assert_eq!(stats.facts.expired, 0);
        assert_eq!(stats.facts.pinned, 1);
    }

    #[test]
    fn snapshot_empty_engine_statistics() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let stats = engine.statistics().unwrap();
        insta::assert_yaml_snapshot!(stats, {
            ".storage.page_count" => "[page_count]",
            ".storage.page_size" => "[page_size]",
            ".storage.main_db_bytes" => "[db_bytes]",
        });
    }

    #[test]
    fn snapshot_populated_statistics() {
        use crate::types::AddFactOptions;

        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "fact one".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                &FakeEmbed,
                None,
            )
            .unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "fact two".into(),
                    fact_type: FactType::Episodic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                &FakeEmbed,
                None,
            )
            .unwrap();
        let pin_opts = AddFactOptions {
            pinned: Some(true),
            ..Default::default()
        };
        engine
            .add_fact(
                &AddFactRequest {
                    content: "pinned fact".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: Some(pin_opts),
                },
                &FakeEmbed,
                None,
            )
            .unwrap();

        let stats = engine.statistics().unwrap();
        insta::assert_yaml_snapshot!(stats, {
            ".storage.page_count" => "[page_count]",
            ".storage.page_size" => "[page_size]",
            ".storage.main_db_bytes" => "[db_bytes]",
        });
    }
}
