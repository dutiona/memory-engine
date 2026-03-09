use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};

use crate::error::{MemoryError, Result};
use crate::graph::{EdgeData, MemoryGraph};
use crate::store::edges::EdgeStore;
use crate::store::facts::FactStore;
use crate::traits::{ConflictArbiter, ConflictResolution, CrudDecision};
use crate::types::{NewEdge, NewFact};

/// Relation type for edges created when facts supplement each other.
const RELATION_SUPPLEMENTS: &str = "supplements";
/// Relation type for edges created when facts contradict each other.
const RELATION_CONTRADICTS: &str = "contradicts";
/// Default weight for conflict resolution edges.
const DEFAULT_EDGE_WEIGHT: f64 = 1.0;

/// Resolve a conflict between an existing fact and a candidate new fact.
///
/// Delegates the decision to the consumer-provided [`ConflictArbiter`] trait.
/// Based on the decision:
/// - **Add**: Both coexist. Creates a "supplements" edge.
/// - **Update**: Old fact expired + invalidated. New fact inserted. Creates "contradicts" edge.
///   All active edges involving the old fact are also expired (cascade).
/// - **Delete**: Old fact expired + invalidated. New fact NOT inserted.
///   All active edges involving the old fact are also expired (cascade).
/// - **Noop**: No changes.
///
/// All mutations happen in a single transaction. Graph is updated only after commit.
///
/// # Errors
///
/// Returns `MemoryError::NotFound` if the old fact doesn't exist.
/// Propagates errors from the arbiter or database operations.
pub fn resolve_conflict(
    conn: &Connection,
    graph: &mut MemoryGraph,
    arbiter: &dyn ConflictArbiter,
    old_fact_id: i64,
    new_fact: &NewFact,
    embed_dim: usize,
    now: DateTime<Utc>,
) -> Result<ConflictResolution> {
    let fact_store = FactStore::new(conn, embed_dim);
    let old_fact = fact_store.get(old_fact_id)?;

    // Build a temporary Fact from NewFact for the arbiter (it needs both as Fact)
    let new_as_fact = crate::types::Fact {
        id: 0, // placeholder, not yet inserted
        content: new_fact.content.clone(),
        content_hash: new_fact.content_hash.clone(),
        embedding: new_fact.embedding.clone(),
        fact_type: new_fact.fact_type.clone(),
        t_created: new_fact.t_created,
        t_expired: new_fact.t_expired,
        t_valid: new_fact.t_valid,
        t_invalid: new_fact.t_invalid,
        source_event_id: new_fact.source_event_id,
        scope_id: new_fact.scope_id,
        importance: new_fact.importance,
        access_count: new_fact.access_count,
        last_accessed: new_fact.last_accessed,
        metadata: new_fact.metadata.clone(),
    };

    let decision = arbiter.arbitrate(&old_fact, &new_as_fact)?;

    match decision {
        CrudDecision::Noop => Ok(ConflictResolution {
            decision: CrudDecision::Noop,
            old_fact_id,
            new_fact_id: None,
        }),

        CrudDecision::Add => {
            let tx = conn.unchecked_transaction()?;

            let new_id = FactStore::new(&tx, embed_dim).insert(new_fact)?;

            // Create "supplements" edge: new → old
            let edge_store = EdgeStore::new(&tx);
            let edge_id = edge_store.insert(&NewEdge {
                source_fact_id: new_id,
                target_fact_id: old_fact_id,
                relation_type: RELATION_SUPPLEMENTS.to_string(),
                weight: DEFAULT_EDGE_WEIGHT,
                scope_id: new_fact.scope_id,
                t_created: now,
                t_expired: None,
            })?;

            tx.commit()?;

            // Update in-memory graph after successful commit
            graph.add_edge(
                new_id,
                old_fact_id,
                EdgeData {
                    edge_id,
                    relation_type: RELATION_SUPPLEMENTS.to_string(),
                    weight: DEFAULT_EDGE_WEIGHT,
                },
            );

            Ok(ConflictResolution {
                decision: CrudDecision::Add,
                old_fact_id,
                new_fact_id: Some(new_id),
            })
        }

        CrudDecision::Update => {
            let tx = conn.unchecked_transaction()?;

            // Expire + invalidate old fact (bi-temporal)
            expire_and_invalidate(&tx, old_fact_id, now)?;

            // Cascade: expire all edges involving the old fact
            let edge_store = EdgeStore::new(&tx);
            edge_store.expire_by_fact(old_fact_id, now)?;

            // Insert new fact
            let new_id = FactStore::new(&tx, embed_dim).insert(new_fact)?;

            // Create "contradicts" edge: new → old
            let edge_id = edge_store.insert(&NewEdge {
                source_fact_id: new_id,
                target_fact_id: old_fact_id,
                relation_type: RELATION_CONTRADICTS.to_string(),
                weight: DEFAULT_EDGE_WEIGHT,
                scope_id: new_fact.scope_id,
                t_created: now,
                t_expired: None,
            })?;

            tx.commit()?;

            // Update in-memory graph: remove expired edges, add new one
            rebuild_graph_for_fact(graph, old_fact_id, new_id, edge_id);

            Ok(ConflictResolution {
                decision: CrudDecision::Update,
                old_fact_id,
                new_fact_id: Some(new_id),
            })
        }

        CrudDecision::Delete => {
            let tx = conn.unchecked_transaction()?;

            // Expire + invalidate old fact
            expire_and_invalidate(&tx, old_fact_id, now)?;

            // Cascade: expire all edges involving the old fact
            let edge_store = EdgeStore::new(&tx);
            edge_store.expire_by_fact(old_fact_id, now)?;

            tx.commit()?;

            // Remove edges from in-memory graph
            remove_edges_for_fact(graph, old_fact_id);

            Ok(ConflictResolution {
                decision: CrudDecision::Delete,
                old_fact_id,
                new_fact_id: None,
            })
        }
    }
}

/// Set both `t_expired` and `t_invalid` on a fact (bi-temporal expiry).
fn expire_and_invalidate(conn: &Connection, fact_id: i64, now: DateTime<Utc>) -> Result<()> {
    let now_str = now.to_rfc3339();
    let changed = conn.execute(
        "UPDATE facts SET t_expired = ?1, t_invalid = ?1 WHERE id = ?2 AND t_expired IS NULL",
        params![now_str, fact_id],
    )?;
    if changed == 0 {
        return Err(MemoryError::NotFound(format!("fact {fact_id}")));
    }
    Ok(())
}

/// Rebuild the in-memory graph after an Update conflict resolution.
///
/// Removes all edges involving the old fact, then adds the new "contradicts" edge.
fn rebuild_graph_for_fact(graph: &mut MemoryGraph, old_fact_id: i64, new_id: i64, edge_id: i64) {
    remove_edges_for_fact(graph, old_fact_id);
    graph.add_edge(
        new_id,
        old_fact_id,
        EdgeData {
            edge_id,
            relation_type: RELATION_CONTRADICTS.to_string(),
            weight: DEFAULT_EDGE_WEIGHT,
        },
    );
}

/// Remove all edges from the in-memory graph that involve a given fact id.
fn remove_edges_for_fact(graph: &mut MemoryGraph, fact_id: i64) {
    graph.remove_edges_by_fact(fact_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    use crate::store::schema::{init_schema, open_memory};
    use crate::types::{Fact, FactType, NewFact};

    /// Mock arbiter with a fixed decision.
    struct FixedArbiter {
        decision: CrudDecision,
    }

    impl ConflictArbiter for FixedArbiter {
        fn arbitrate(&self, _old: &Fact, _new: &Fact) -> Result<CrudDecision> {
            Ok(self.decision.clone())
        }
    }

    /// Mock arbiter that returns an error.
    struct FailingArbiter;

    impl ConflictArbiter for FailingArbiter {
        fn arbitrate(&self, _old: &Fact, _new: &Fact) -> Result<CrudDecision> {
            Err(MemoryError::Conflict("arbiter failed".into()))
        }
    }

    fn setup() -> (Connection, usize) {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        (conn, 4)
    }

    fn make_new_fact(content: &str) -> NewFact {
        let now = Utc::now();
        NewFact {
            content: content.into(),
            content_hash: blake3::hash(content.as_bytes()).to_hex().as_str()[..32].to_string(),
            embedding: vec![0.1; 4],
            fact_type: FactType::Semantic,
            t_created: now,
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            scope_id: 1,
            importance: 0.5,
            access_count: 0,
            last_accessed: now,
            metadata: serde_json::json!({}),
        }
    }

    fn insert_fact(conn: &Connection, dim: usize, content: &str) -> i64 {
        FactStore::new(conn, dim)
            .insert(&make_new_fact(content))
            .unwrap()
    }

    #[test]
    fn noop_no_changes() {
        let (conn, dim) = setup();
        let old_id = insert_fact(&conn, dim, "existing fact");
        let mut graph = MemoryGraph::new();

        let result = resolve_conflict(
            &conn,
            &mut graph,
            &FixedArbiter {
                decision: CrudDecision::Noop,
            },
            old_id,
            &make_new_fact("new fact"),
            dim,
            Utc::now(),
        )
        .unwrap();

        assert_eq!(result.decision, CrudDecision::Noop);
        assert_eq!(result.old_fact_id, old_id);
        assert!(result.new_fact_id.is_none());

        // Old fact should be unchanged
        let old = FactStore::new(&conn, dim).get(old_id).unwrap();
        assert!(old.t_expired.is_none());
    }

    #[test]
    fn add_keeps_both_creates_supplements_edge() {
        let (conn, dim) = setup();
        let old_id = insert_fact(&conn, dim, "existing");
        let mut graph = MemoryGraph::new();

        let result = resolve_conflict(
            &conn,
            &mut graph,
            &FixedArbiter {
                decision: CrudDecision::Add,
            },
            old_id,
            &make_new_fact("supplementary"),
            dim,
            Utc::now(),
        )
        .unwrap();

        assert_eq!(result.decision, CrudDecision::Add);
        assert!(result.new_fact_id.is_some());

        let new_id = result.new_fact_id.unwrap();

        // Both facts should be active
        let fact_store = FactStore::new(&conn, dim);
        assert!(fact_store.get(old_id).unwrap().t_expired.is_none());
        assert!(fact_store.get(new_id).unwrap().t_expired.is_none());

        // Edge should exist in SQLite
        let edge_store = EdgeStore::new(&conn);
        let edges = edge_store.list_active_by_source(new_id).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].relation_type, "supplements");
        assert_eq!(edges[0].target_fact_id, old_id);

        // Edge should exist in graph
        assert!(graph.has_node(new_id));
        assert!(graph.has_node(old_id));
        assert_eq!(graph.neighbors(new_id), vec![old_id]);
    }

    #[test]
    fn update_expires_old_creates_contradicts_edge() {
        let (conn, dim) = setup();
        let old_id = insert_fact(&conn, dim, "outdated");
        let mut graph = MemoryGraph::new();

        let result = resolve_conflict(
            &conn,
            &mut graph,
            &FixedArbiter {
                decision: CrudDecision::Update,
            },
            old_id,
            &make_new_fact("updated"),
            dim,
            Utc::now(),
        )
        .unwrap();

        assert_eq!(result.decision, CrudDecision::Update);
        let new_id = result.new_fact_id.unwrap();

        // Old fact should be expired AND invalidated
        let old = FactStore::new(&conn, dim).get(old_id).unwrap();
        assert!(old.t_expired.is_some());
        assert!(old.t_invalid.is_some());

        // New fact should be active
        let new = FactStore::new(&conn, dim).get(new_id).unwrap();
        assert!(new.t_expired.is_none());

        // "contradicts" edge should exist
        let edges = EdgeStore::new(&conn).list_active_by_source(new_id).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].relation_type, "contradicts");
    }

    #[test]
    fn delete_expires_old_no_new() {
        let (conn, dim) = setup();
        let old_id = insert_fact(&conn, dim, "to delete");
        let mut graph = MemoryGraph::new();

        let result = resolve_conflict(
            &conn,
            &mut graph,
            &FixedArbiter {
                decision: CrudDecision::Delete,
            },
            old_id,
            &make_new_fact("irrelevant"),
            dim,
            Utc::now(),
        )
        .unwrap();

        assert_eq!(result.decision, CrudDecision::Delete);
        assert!(result.new_fact_id.is_none());

        // Old fact should be expired AND invalidated
        let old = FactStore::new(&conn, dim).get(old_id).unwrap();
        assert!(old.t_expired.is_some());
        assert!(old.t_invalid.is_some());
    }

    #[test]
    fn update_cascades_edges_on_old_fact() {
        let (conn, dim) = setup();
        let fact_a = insert_fact(&conn, dim, "fact A");
        let fact_b = insert_fact(&conn, dim, "fact B");
        let fact_c = insert_fact(&conn, dim, "fact C");

        // Create edges: A → B and C → A
        let edge_store = EdgeStore::new(&conn);
        edge_store
            .insert(&NewEdge {
                source_fact_id: fact_a,
                target_fact_id: fact_b,
                relation_type: "relates".into(),
                weight: 1.0,
                scope_id: 1,
                t_created: Utc::now(),
                t_expired: None,
            })
            .unwrap();
        edge_store
            .insert(&NewEdge {
                source_fact_id: fact_c,
                target_fact_id: fact_a,
                relation_type: "depends".into(),
                weight: 1.0,
                scope_id: 1,
                t_created: Utc::now(),
                t_expired: None,
            })
            .unwrap();

        let mut graph = MemoryGraph::load_from_db(&conn).unwrap();
        assert_eq!(graph.edge_count(), 2);

        // Update fact A — should cascade-expire both edges
        resolve_conflict(
            &conn,
            &mut graph,
            &FixedArbiter {
                decision: CrudDecision::Update,
            },
            fact_a,
            &make_new_fact("updated A"),
            dim,
            Utc::now(),
        )
        .unwrap();

        // Only the new "contradicts" edge should be active
        let active_edges = edge_store.list_active().unwrap();
        assert_eq!(active_edges.len(), 1);
        assert_eq!(active_edges[0].relation_type, "contradicts");
    }

    #[test]
    fn arbiter_error_rolls_back_all_changes() {
        let (conn, dim) = setup();
        let old_id = insert_fact(&conn, dim, "should survive");
        let mut graph = MemoryGraph::new();

        let result = resolve_conflict(
            &conn,
            &mut graph,
            &FailingArbiter,
            old_id,
            &make_new_fact("should not appear"),
            dim,
            Utc::now(),
        );

        assert!(result.is_err());

        // Old fact should be unchanged
        let old = FactStore::new(&conn, dim).get(old_id).unwrap();
        assert!(old.t_expired.is_none());
        assert!(old.t_invalid.is_none());

        // No new facts should exist
        let active = FactStore::new(&conn, dim).list_active().unwrap();
        assert_eq!(active.len(), 1);
    }
}
