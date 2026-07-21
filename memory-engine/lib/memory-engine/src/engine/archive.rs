//! Archival compression delegates for [`MemoryEngine`].
//!
//! Extracted into the [`me_archive`] crate (Wave 2 #816 / S4, sub-PR 3b; re-exported
//! here as `crate::archive`, matching the `pool`/`store`/`graph`/`scope`/`forgetting`
//! carve convention). Each public method below resolves this engine's `MemoryCtx` +
//! cold-storage handle + in-memory graph + archive directory, then delegates to the
//! corresponding `me_archive` free function. The `MemoryCtx`-reachable pre-flight (the
//! `ensure_open` fence + the read-only fail-fast) now self-gates inside
//! `me_archive::archive` (#972); this delegate keeps its own copy as defence-in-depth and
//! additionally owns the **file-backed** check, which must run *before* `archive_dir` can
//! be resolved and which `MemoryCtx` carries no path state for (see `me_archive::manage`'s
//! module docs for the mirror of this note).

use std::path::PathBuf;

use crate::archive::types::{
    ArchiveManifestEntry, ArchivePolicy, ArchiveStats, ArchiveVerifyResult,
};
use crate::error::{ArchiveError, Result};

use super::MemoryEngine;

impl MemoryEngine {
    /// Archive expired, non-pinned facts into a `.pak` file.
    ///
    /// Returns `None` if fewer than `policy.min_facts` candidates exist.
    /// Otherwise writes the `.pak`, inserts a manifest row, hard-deletes
    /// facts and edges from `SQLite` (single transaction), then prunes the
    /// archived facts' nodes from the in-memory graph cache (#332).
    ///
    /// The in-memory graph is a *derived cache* of the active edge set: the DB
    /// is the source of truth and the cache is rebuilt from it on every `open`
    /// (`MemoryGraph::load_from_db`). After the atomic commit
    /// succeeds, the archived facts' nodes are removed in place under a single
    /// graph write guard — an O(N) prune held across no `.await`, so it is
    /// atomic with respect to any other graph mutator.
    /// `MemoryGraph::remove_node` is
    /// loop-safe: petgraph's swap-remove relocates the former last node into the
    /// freed slot, and `remove_node` re-indexes `node_map` for that displaced
    /// node, so surviving nodes keep resolving to their correct indices across
    /// the whole loop (#833). If the process is killed mid-prune, the cache
    /// self-heals: the next `open` rebuilds it wholesale from the committed DB,
    /// which already reflects the hard-delete.
    ///
    /// # Panics
    ///
    /// Panics if the constructed `.pak` path has no filename component.
    /// This cannot happen in practice because the path is always built as
    /// `archive_dir.join("archive-<timestamp>.pak")`.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Archive` on I/O failure.
    /// Returns `MemoryError::Storage` on SQL failure.
    /// Returns `MemoryError::ReadOnly` if the engine is read-only.
    pub async fn archive(&self, policy: &ArchivePolicy) -> Result<Option<ArchiveStats>> {
        self.ensure_open()?;
        // Fail fast on read-only engines before any filesystem I/O — the atomic
        // commit below the seam checks this too, but we want to avoid writing an
        // orphan .pak file that would never be committed. Uses the shared
        // `ensure_writable()` helper (#972) for consistency with every other
        // facade write method, rather than an ad-hoc `read_only` check.
        self.ensure_writable()?;

        if !self.is_file_backed() {
            return Err(ArchiveError::NotFileBacked(
                "archival requires a file-backed engine".to_string(),
            )
            .into());
        }

        let archive_dir = self.archive_dir()?;

        crate::archive::archive(
            self.mem_ctx(),
            self.cold.as_ref(),
            &self.graph,
            &archive_dir,
            policy,
        )
        .await
    }

    /// List all archive manifest entries.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on SQL failure.
    pub async fn list_archives(&self) -> Result<Vec<ArchiveManifestEntry>> {
        crate::archive::list_archives(self.cold.as_ref()).await
    }

    /// Verify integrity of all archived `.pak` files.
    ///
    /// Checks each manifest entry's blake3 hash against the actual file.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on SQL failure.
    /// I/O errors for individual `.pak` files are reported per-entry, not propagated.
    ///
    pub async fn verify_archives(&self) -> Result<Vec<ArchiveVerifyResult>> {
        // Read the manifest BEFORE resolving the archive dir, preserving base's error
        // precedence: on an in-memory engine whose manifest read also fails, the storage
        // error surfaces (as pre-carve), not `NotFileBacked`.
        let entries = self.list_archives().await?;
        let archive_dir = self.archive_dir()?;
        Ok(crate::archive::verify_archives(&entries, &archive_dir))
    }

    /// Search all archived `.pak` files for facts matching `query`.
    ///
    /// Returns `Ok(None)` when there is no file-backed engine, no archive
    /// directory, or no manifest entries — not an error, just nothing to search.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on manifest read failure.
    /// Returns `MemoryError::Archive` on `.pak` I/O or decompression failure.
    pub(crate) async fn search_archives_fallback(
        &self,
        query: &crate::search::MemoryQuery,
        limit: usize,
    ) -> Result<Option<crate::archive::search::ArchiveSearchResult>> {
        let Ok(archive_dir) = self.archive_dir() else {
            return Ok(None);
        };
        crate::archive::search_archives_fallback(self.cold.as_ref(), &archive_dir, query, limit)
            .await
    }

    /// Resolve the archive directory (sibling of DB file + `/archives/`).
    fn archive_dir(&self) -> Result<PathBuf> {
        let db_path = self.db_path.as_deref().ok_or_else(|| {
            ArchiveError::NotFileBacked(
                "cannot resolve archive dir for in-memory database".to_string(),
            )
        })?;
        let parent = db_path.parent().ok_or_else(|| {
            ArchiveError::Io(format!(
                "database path has no parent: {}",
                db_path.display()
            ))
        })?;
        Ok(parent.join("archives"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use chrono::Utc;

    use crate::graph::MemoryGraph;
    use crate::types::{FactType, NewEdge, NewFact};

    const DIM: usize = 8;

    fn make_expired_fact(content: &str, expired_at: chrono::DateTime<Utc>) -> NewFact {
        NewFact {
            content: content.into(),
            content_hash: String::new(),
            embedding: vec![0.1_f32; DIM],
            fact_type: FactType::Episodic,
            t_created: Utc::now() - Duration::days(2),
            t_expired: Some(expired_at),
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            base_importance: 0.5,
            access_count: 0,
            last_accessed: Utc::now(),
            metadata: serde_json::json!({}),
            scope_id: 1,
            is_pinned: false,
        }
    }

    /// An active (non-expired), non-pinned fact — a *survivor* that archival
    /// must leave in both the DB and the in-memory graph.
    fn make_active_fact(content: &str) -> NewFact {
        NewFact {
            t_expired: None,
            ..make_expired_fact(content, Utc::now())
        }
    }

    fn make_edge(source: i64, target: i64) -> NewEdge {
        NewEdge {
            source_fact_id: source,
            target_fact_id: target,
            relation_type: "related".into(),
            weight: 1.0,
            t_created: Utc::now(),
            t_expired: None,
            scope_id: 1,
        }
    }

    /// #265: a failure inside `commit_archive` (here: the manifest INSERT fails
    /// because the table was dropped) must NOT leave an orphan `.pak` file behind.
    /// The `.pak` is written before the commit transaction; without on-error
    /// cleanup it would be a permanent disk leak with no manifest row (CWE-459).
    // Uses the `raw_exec` failure-injection seam, which since #816 A1 is gated on
    // `test-util` (a cross-crate trait method can't ride `cfg(test)`); without this gate
    // the test breaks `cargo test --features archive` (E0599). Runs under --all-features.
    #[cfg(feature = "test-util")]
    #[tokio::test]
    async fn archive_cleans_up_pak_when_commit_fails() {
        let dir = tempfile::tempdir().unwrap();
        let engine = MemoryEngine::builder(DIM)
            .path(dir.path().join("orphan.db"))
            .build()
            .unwrap();

        // Insert expired, non-pinned facts directly via the store so they qualify
        // as archive candidates.
        let expired_at = Utc::now() - Duration::hours(1);
        for i in 0..20 {
            engine
                .storage()
                .insert_fact(&make_expired_fact(&format!("orphan fact {i}"), expired_at))
                .await
                .unwrap();
        }
        // Force `commit_archive` to fail: drop the manifest table so its INSERT
        // errors out *after* the `.pak` has already been written to disk. The
        // test-only `raw_exec` seam (#727) injects the failure below the port now
        // that `engine.write_conn()` is gone post-#631.
        engine
            .storage()
            .raw_exec("DROP TABLE archive_manifest")
            .await
            .unwrap();

        let policy = ArchivePolicy {
            expired_before: Utc::now() + Duration::hours(1),
            min_facts: 1,
        };

        let result = engine.archive(&policy).await;
        assert!(
            result.is_err(),
            "archive must propagate the commit failure, got {result:?}"
        );

        // The archive directory must contain no orphan `.pak` file (CWE-459).
        let archive_dir = dir.path().join("archives");
        let orphans: Vec<_> = std::fs::read_dir(&archive_dir)
            .map(|rd| {
                rd.filter_map(std::result::Result::ok)
                    .filter(|e| {
                        e.path()
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("pak"))
                    })
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            orphans.is_empty(),
            "commit_archive failure left orphan .pak file(s): {orphans:?}"
        );
    }

    /// #332: after a *successful* archive, the in-memory graph cache must match
    /// the committed DB — archived facts' nodes (and their incident edges) are
    /// gone, while survivor nodes and their edges remain intact. This proves the
    /// post-commit in-place node-by-node prune (under one graph write guard)
    /// leaves no stale nodes and no dangling references to archived ids.
    ///
    /// The prune is loop-safe because `MemoryGraph::remove_node` re-indexes
    /// `node_map` for the node petgraph's swap-remove relocates into the freed
    /// slot (#833); without that re-indexing a survivor's cached `NodeIndex`
    /// would silently alias a removed slot and assertion 2 would fail. The
    /// unit test `remove_node_in_loop_keeps_map_consistent` covers the
    /// `MemoryGraph` contract directly; this test exercises it through the full
    /// archive path.
    #[tokio::test]
    async fn archive_keeps_in_memory_graph_consistent() {
        let dir = tempfile::tempdir().unwrap();
        let engine = MemoryEngine::builder(DIM)
            .path(dir.path().join("graph.db"))
            .build()
            .unwrap();

        // Two survivors (active) that must outlive the archive.
        let survivor_a = engine
            .storage()
            .insert_fact(&make_active_fact("survivor a"))
            .await
            .unwrap();
        let survivor_b = engine
            .storage()
            .insert_fact(&make_active_fact("survivor b"))
            .await
            .unwrap();

        // Expired, non-pinned facts — the archive candidates.
        let expired_at = Utc::now() - Duration::hours(1);
        let mut archived_ids = Vec::new();
        for i in 0..20 {
            archived_ids.push(
                engine
                    .storage()
                    .insert_fact(&make_expired_fact(&format!("doomed {i}"), expired_at))
                    .await
                    .unwrap(),
            );
        }

        // Edges the prune must handle on the success path:
        //  - internal: archived -> archived  (both endpoints archived; archive
        //    hard-deletes them from the DB and prunes the nodes from the graph)
        //  - survivor: survivor -> survivor  (must be left untouched)
        //
        // Boundary edges (exactly one archived endpoint) are covered by the
        // dedicated #847 regression below
        // (`archive_removes_boundary_edge_without_fk_violation`); this test keeps
        // its focus on #332's in-memory two-phase update with internal/survivor
        // edges only.
        engine
            .storage()
            .insert_edge(&make_edge(archived_ids[0], archived_ids[1]))
            .await
            .unwrap();
        engine
            .storage()
            .insert_edge(&make_edge(survivor_a, survivor_b))
            .await
            .unwrap();

        // Seed the in-memory graph from the DB exactly as `open` does, so the
        // cache starts in sync before we exercise the prune.
        {
            let active_edges = engine.storage().list_active_edges().await.unwrap();
            *engine.graph.write() = MemoryGraph::from_active_edges(&active_edges);
        }
        // Sanity: every node above is present before the archive.
        assert!(engine.graph_has_node(archived_ids[0]));
        assert!(engine.graph_has_node(survivor_a));
        assert!(engine.graph_has_node(survivor_b));

        let policy = ArchivePolicy {
            expired_before: Utc::now() + Duration::hours(1),
            min_facts: 1,
        };
        let stats = engine
            .archive(&policy)
            .await
            .unwrap()
            .expect("archive should run with candidates present");
        assert_eq!(stats.facts_archived, archived_ids.len());

        // 1. No archived fact retains a graph node.
        for &id in &archived_ids {
            assert!(
                !engine.graph_has_node(id),
                "archived fact {id} still has a stale node after archive"
            );
        }

        // 2. Survivors keep their nodes and their survivor<->survivor edge.
        assert!(engine.graph_has_node(survivor_a), "survivor_a node lost");
        assert!(engine.graph_has_node(survivor_b), "survivor_b node lost");
        assert_eq!(
            engine.graph_neighbors(survivor_a),
            vec![survivor_b],
            "survivor_a should still point at survivor_b only"
        );

        // 3. No survivor query returns a reference to an archived id — the
        //    archived nodes must be fully gone, not just detached.
        let component = engine.graph_component(survivor_a);
        for &id in &archived_ids {
            assert!(
                !component.contains(&id),
                "survivor's component still references archived fact {id}"
            );
        }

        // 4. The graph is internally consistent: node_count equals the number
        //    of live (survivor) nodes, with no orphaned index left behind.
        let (node_count, _edge_count) = engine.graph_stats();
        assert_eq!(
            node_count, 2,
            "graph should hold exactly the two survivor nodes, found {node_count}"
        );
    }

    /// #847 regression (end-to-end): `archive()` must succeed when a *boundary*
    /// edge — one with exactly ONE endpoint in the archive set — crosses the cut.
    ///
    /// Before the fix, the DB commit deleted only edges whose *both* endpoints
    /// were archived, so a boundary edge (`archived → survivor`, or the reverse)
    /// survived the edge-delete and then referenced an about-to-be-hard-deleted
    /// archived fact → `FOREIGN KEY constraint failed`, and the whole `archive()`
    /// returned `Err`. The fix hard-deletes every edge incident to an archived
    /// fact inside the same transaction.
    ///
    /// This test also proves the in-memory graph stays consistent: the survivor
    /// keeps its node but loses the dangling boundary edge, mirroring the DB.
    #[tokio::test]
    async fn archive_removes_boundary_edge_without_fk_violation() {
        let dir = tempfile::tempdir().unwrap();
        let engine = MemoryEngine::builder(DIM)
            .path(dir.path().join("boundary.db"))
            .build()
            .unwrap();

        // One survivor (active) — it must outlive the archive.
        let survivor = engine
            .storage()
            .insert_fact(&make_active_fact("survivor"))
            .await
            .unwrap();

        // Enough expired, non-pinned facts to clear `min_facts`.
        let expired_at = Utc::now() - Duration::hours(1);
        let mut archived_ids = Vec::new();
        for i in 0..5 {
            archived_ids.push(
                engine
                    .storage()
                    .insert_fact(&make_expired_fact(&format!("doomed {i}"), expired_at))
                    .await
                    .unwrap(),
            );
        }

        // Boundary edge #1: archived → survivor.
        engine
            .storage()
            .insert_edge(&make_edge(archived_ids[0], survivor))
            .await
            .unwrap();
        // Boundary edge #2: survivor → archived (the other direction).
        engine
            .storage()
            .insert_edge(&make_edge(survivor, archived_ids[1]))
            .await
            .unwrap();
        // An internal archived↔archived edge for good measure.
        engine
            .storage()
            .insert_edge(&make_edge(archived_ids[0], archived_ids[1]))
            .await
            .unwrap();

        // Seed the in-memory graph from the DB exactly as `open` does.
        {
            let active_edges = engine.storage().list_active_edges().await.unwrap();
            *engine.graph.write() = MemoryGraph::from_active_edges(&active_edges);
        }
        assert!(engine.graph_has_node(survivor));
        // `graph_degree` counts incident edges in both directions, so it sees
        // both boundary edges (survivor→archived and archived→survivor).
        assert_eq!(
            engine.graph_degree(survivor),
            2,
            "survivor starts linked to two archived facts via boundary edges"
        );

        let policy = ArchivePolicy {
            expired_before: Utc::now() + Duration::hours(1),
            min_facts: 1,
        };

        // The crux: this must NOT FK-violate. Pre-fix it returns an FK error.
        let stats = engine
            .archive(&policy)
            .await
            .expect("archive with a boundary edge must not trip the facts FK")
            .expect("archive should run with candidates present");
        assert_eq!(stats.facts_archived, archived_ids.len());

        // The survivor remains in the live DB.
        assert!(
            engine.storage().get_fact(survivor).await.is_ok(),
            "survivor fact must remain in the live DB"
        );
        // No archived fact remains (hard-deleted → `get_fact` errors NotFound).
        for &id in &archived_ids {
            assert!(
                engine.storage().get_fact(id).await.is_err(),
                "archived fact {id} must be hard-deleted from the live DB"
            );
        }
        // No active edge survives — the survivor's only links were to archived
        // facts, so the boundary edges are gone with them.
        assert!(
            engine
                .storage()
                .list_active_edges()
                .await
                .unwrap()
                .is_empty(),
            "all boundary edges to archived facts must be hard-deleted"
        );

        // In-memory graph: survivor node kept, but now isolated (no neighbors).
        assert!(
            engine.graph_has_node(survivor),
            "survivor node must remain in the graph cache"
        );
        assert_eq!(
            engine.graph_degree(survivor),
            0,
            "survivor must lose its dangling boundary edges in the graph cache too"
        );
        for &id in &archived_ids {
            assert!(
                !engine.graph_has_node(id),
                "archived fact {id} must be pruned from the graph cache"
            );
        }
    }

    /// #292: `verify_archives` must reject a manifest row whose `pak_path`
    /// escapes the archive directory via `..`. The legitimate write path only
    /// ever inserts a separator-free `archive-<ts>-<nanos>.pak` filename, so a
    /// traversal path can only arrive from a tampered/restored DB — exactly the
    /// untrusted-blob surface this guard defends. The old lexical
    /// `pak_path.starts_with(&archive_dir)` check let `..` through (it does not
    /// resolve `..`), so the row was handed to the I/O path instead of being
    /// flagged. me-archive's `is_within_archive_dir` containment check (crate-internal;
    /// its own unit tests cover the traversal cases) rejects it before any filesystem access.
    // Uses the `raw_exec` failure-injection seam (test-util-gated since #816 A1), so this
    // test compiles only with `test-util` on — matching the sibling `commit_fails` test.
    #[cfg(feature = "test-util")]
    #[tokio::test]
    async fn verify_archives_rejects_path_traversal_manifest_entry() {
        let dir = tempfile::tempdir().unwrap();
        let engine = MemoryEngine::builder(DIM)
            .path(dir.path().join("traversal.db"))
            .build()
            .unwrap();

        // A traversal path the legitimate write path can never produce. It is
        // crafted relative to `<db_parent>/archives`, so `archive_dir.join(..)`
        // would resolve outside the archive directory entirely. (me-archive's own
        // `is_within_archive_dir` unit tests cover that this shape is rejected.)
        let evil_path = "../outside/escape.pak";

        // Inject the malicious manifest row directly — `commit_archive_atomic`
        // always generates a safe filename, so the only way to exercise the
        // guard is to plant the row below the port via the test-only `raw_exec`
        // seam (#727), simulating a tampered/restored DB.
        engine
            .storage()
            .raw_exec(&format!(
                "INSERT INTO archive_manifest \
                 (pak_path, created_at, fact_count, edge_count, fact_id_min, \
                  fact_id_max, t_created_min, t_created_max, size_bytes, blake3_hash) \
                 VALUES ('{evil_path}', '2026-01-01T00:00:00Z', 0, 0, 0, 0, \
                  '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 0, 'deadbeef')"
            ))
            .await
            .unwrap();

        let results = engine.verify_archives().await.unwrap();
        assert_eq!(results.len(), 1, "exactly one manifest entry was planted");
        let result = &results[0];
        assert!(
            !result.ok,
            "a `..` traversal manifest path must be flagged, not verified"
        );
        assert_eq!(
            result.error.as_deref(),
            Some("path traversal detected"),
            "the traversal must be caught by the containment guard, not fall \
             through to the I/O (file-not-found) path"
        );
        assert_eq!(result.pak_path, evil_path);
    }

    /// #117 regression (Wave 2 #816 / S4 sub-PR 2): a *provided-but-missing* scope must
    /// yield no results, and the unscoped archive fallback MUST be suppressed on that
    /// scope-miss — otherwise it leaks archived facts from *all* scopes. Before the
    /// `me-query` carve this was guaranteed structurally (the scope-miss early-return
    /// preceded `execute_search_path`, where the archive block lived); the carve split
    /// those two facts across the crate boundary, and this test pins the
    /// `QueryExecution::ScopeMissing` guard that restores the behaviour.
    ///
    /// The positive control (an *unscoped* query returns the archived fact) proves the
    /// archive fallback is live and text-matchable, so the scope-miss assertion below
    /// cannot pass vacuously (the "verification theater" trap).
    #[tokio::test]
    async fn scope_miss_suppresses_unscoped_archive_fallback() {
        use crate::search::MemoryQuery;

        let dir = tempfile::tempdir().unwrap();
        let engine = MemoryEngine::builder(DIM)
            .path(dir.path().join("scope_archive.db"))
            .build()
            .unwrap();

        // Insert expired, non-pinned facts (scope_id = 1, the root) carrying a distinctive
        // token, then archive them so they live only in the cold `.pak` + manifest.
        let expired_at = Utc::now() - Duration::hours(1);
        for i in 0..5 {
            engine
                .storage()
                .insert_fact(&make_expired_fact(
                    &format!("archivable-token fact {i}"),
                    expired_at,
                ))
                .await
                .unwrap();
        }
        let stats = engine
            .archive(&ArchivePolicy {
                expired_before: Utc::now() + Duration::hours(1),
                min_facts: 1,
            })
            .await
            .unwrap();
        assert!(
            stats.is_some(),
            "archival must have written a pak + manifest"
        );
        assert!(
            !engine.list_archives().await.unwrap().is_empty(),
            "manifest must be populated for the archive fallback to have anything to scan"
        );

        // Positive control: an UNSCOPED query (scope resolves to `Some(None)`) with the
        // archive fallback opted in returns the archived facts — proving the fallback is
        // live and the token is matchable, so the scope-miss assertion is not vacuous.
        let unscoped = MemoryQuery::new()
            .text("archivable-token")
            .include_archives();
        let hit = engine.execute_query(&unscoped).await.unwrap();
        assert!(
            !hit.results.is_empty(),
            "unscoped archive fallback must surface the archived facts (control)"
        );

        // Regression: the SAME query against a provided-but-missing scope must return
        // empty — the #117 scope-miss guard suppresses the unscoped archive fallback
        // (which itself never filters on `query.scope`). Before the fix this leaked the
        // cross-scope archived facts.
        let scoped_miss = MemoryQuery::new()
            .text("archivable-token")
            .scope_exact("does:not:exist")
            .include_archives();
        let missed = engine.execute_query(&scoped_miss).await.unwrap();
        assert!(
            missed.results.is_empty(),
            "a provided-but-missing scope must yield no results — the unscoped archive \
             fallback must be suppressed on a scope-miss (#117), got {} result(s)",
            missed.results.len()
        );
    }
}
