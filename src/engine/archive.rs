//! Archival compression orchestration for [`MemoryEngine`].
//!
//! Moves expired, non-pinned facts into `.pak` files (zstd + blake3),
//! records a manifest row, hard-deletes from `SQLite`, and prunes the
//! in-memory graph — all in a crash-safe sequence.

use std::path::PathBuf;

use chrono::Utc;

use crate::archive::pak::{hash_file, verify_pak, write_pak_and_hash};
use crate::archive::types::{
    ArchiveManifestEntry, ArchivePak, ArchivePolicy, ArchiveStats, ArchiveVerifyResult,
    CURRENT_PAK_VERSION,
};
use crate::error::{ArchiveError, MemoryError, Result};
use crate::store::schema::CURRENT_SCHEMA_VERSION;
use crate::types::{Edge, Fact};

use super::MemoryEngine;

impl MemoryEngine {
    /// Archive expired, non-pinned facts into a `.pak` file.
    ///
    /// Returns `None` if fewer than `policy.min_facts` candidates exist.
    /// Otherwise writes the `.pak`, inserts a manifest row, hard-deletes
    /// facts and edges from `SQLite` (single transaction), and updates the
    /// in-memory graph.
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
    /// Returns `MemoryError::Database` on SQL failure.
    /// Returns `MemoryError::ReadOnly` if the engine is read-only.
    pub async fn archive(&self, policy: &ArchivePolicy) -> Result<Option<ArchiveStats>> {
        self.ensure_open()?;
        // Fail fast on read-only engines before any filesystem I/O — the atomic
        // commit below the seam checks this too, but we want to avoid writing an
        // orphan .pak file that would never be committed.
        if self.read_only {
            return Err(MemoryError::ReadOnly);
        }

        if !self.is_file_backed() {
            return Err(ArchiveError::NotFileBacked(
                "archival requires a file-backed engine".to_string(),
            )
            .into());
        }

        let archive_dir = self.archive_dir()?;

        let (candidate_facts, candidate_edges) = self
            .storage
            .select_archive_candidates(policy.expired_before)
            .await?;

        if candidate_facts.len() < policy.min_facts {
            return Ok(None);
        }

        let fact_ids: Vec<i64> = candidate_facts.iter().map(|f| f.id).collect();

        let pak = self.build_pak(&candidate_facts, &candidate_edges);
        let (pak_path, pak_size_bytes, blake3_hash) = Self::write_pak_to_disk(&archive_dir, &pak)?;
        let pak_filename = pak_path
            .file_name()
            .expect("pak_path has a filename")
            .to_string_lossy()
            .to_string();

        // The `.pak` is already on disk. If the commit (manifest insert +
        // hard-delete tx) fails, the file would be an orphan with no manifest
        // row — a permanent disk leak that `verify_archives()` could never
        // reconcile (CWE-459). Remove it on error, mirroring the cleanup the
        // restore path uses for its half-written DB file.
        self.commit_archive(
            &pak_filename,
            &candidate_facts,
            &candidate_edges,
            &fact_ids,
            pak_size_bytes,
            &blake3_hash,
        )
        .await
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&pak_path);
        })?;

        // Update in-memory graph
        {
            let mut graph = self.graph.write();
            for &fid in &fact_ids {
                graph.remove_edges_by_fact(fid);
                graph.remove_node(fid);
            }
        }

        Ok(Some(ArchiveStats {
            facts_archived: candidate_facts.len(),
            edges_archived: candidate_edges.len(),
            pak_path,
            pak_size_bytes,
            blake3_hash,
        }))
    }

    /// List all archive manifest entries.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    pub async fn list_archives(&self) -> Result<Vec<ArchiveManifestEntry>> {
        self.cold.list_archive_manifest().await
    }

    /// Verify integrity of all archived `.pak` files.
    ///
    /// Checks each manifest entry's blake3 hash against the actual file.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    /// I/O errors for individual `.pak` files are reported per-entry, not propagated.
    pub async fn verify_archives(&self) -> Result<Vec<ArchiveVerifyResult>> {
        let entries = self.list_archives().await?;
        let archive_dir = self.archive_dir()?;

        let mut results = Vec::with_capacity(entries.len());
        for entry in &entries {
            let pak_path = archive_dir.join(&entry.pak_path);
            // Path traversal guard
            if !pak_path.starts_with(&archive_dir) {
                results.push(ArchiveVerifyResult {
                    manifest_id: entry.id,
                    pak_path: entry.pak_path.clone(),
                    ok: false,
                    error: Some("path traversal detected".to_string()),
                });
                continue;
            }
            let result = if pak_path.exists() {
                Self::verify_single_archive(entry, &pak_path)
            } else {
                ArchiveVerifyResult {
                    manifest_id: entry.id,
                    pak_path: entry.pak_path.clone(),
                    ok: false,
                    error: Some(format!("file not found: {}", pak_path.display())),
                }
            };
            results.push(result);
        }
        Ok(results)
    }

    // --- Private helpers ---

    /// Build the `.pak` payload.
    fn build_pak(&self, facts: &[Fact], edges: &[Edge]) -> ArchivePak {
        ArchivePak {
            pak_version: CURRENT_PAK_VERSION,
            engine_schema_version: CURRENT_SCHEMA_VERSION,
            embed_dim: self.embed_dim,
            created_at: Utc::now(),
            facts: facts.to_vec(),
            edges: edges.to_vec(),
        }
    }

    /// Write the `.pak` file and return (path, size, hash).
    fn write_pak_to_disk(
        archive_dir: &std::path::Path,
        pak: &ArchivePak,
    ) -> Result<(PathBuf, u64, String)> {
        std::fs::create_dir_all(archive_dir).map_err(|e| {
            ArchiveError::Io(format!(
                "failed to create archive dir {}: {e}",
                archive_dir.display()
            ))
        })?;

        let now = Utc::now();
        let nanos = now.timestamp_subsec_nanos();
        let pak_filename = format!("archive-{}-{nanos:08x}.pak", now.format("%Y%m%d%H%M%S"));
        let pak_path = archive_dir.join(&pak_filename);

        let blake3_hash = write_pak_and_hash(pak, &pak_path)?;
        // `write_pak_and_hash` has now renamed the `.pak` into place, so it
        // physically exists. Any failure from here on must remove it, or it
        // becomes an orphan with no manifest row (CWE-459) — the same guarantee
        // `commit_archive`'s cleanup gives for the downstream commit step. The
        // stat below is the only such fallible step before this fn returns.
        let pak_size_bytes = std::fs::metadata(&pak_path)
            .map_err(|e| {
                let _ = std::fs::remove_file(&pak_path);
                ArchiveError::Io(format!(
                    "failed to stat pak file {}: {e}",
                    pak_path.display()
                ))
            })?
            .len();

        Ok((pak_path, pak_size_bytes, blake3_hash))
    }

    /// Commit the database side of an archive operation below the seam: manifest
    /// insert + hard-delete edges + hard-delete facts, in one atomic transaction
    /// ([`ColdStorage::commit_archive_atomic`]). The `.pak` file I/O stays
    /// engine-side; on `Err` the caller removes the already-written `.pak`.
    #[allow(clippy::cast_possible_wrap)]
    async fn commit_archive(
        &self,
        pak_filename: &str,
        facts: &[Fact],
        edges: &[Edge],
        fact_ids: &[i64],
        pak_size_bytes: u64,
        blake3_hash: &str,
    ) -> Result<()> {
        let fact_id_min = facts.iter().map(|f| f.id).min().unwrap_or(0);
        let fact_id_max = facts.iter().map(|f| f.id).max().unwrap_or(0);
        let t_created_min = facts
            .iter()
            .map(|f| f.t_created)
            .min()
            .unwrap_or_else(Utc::now);
        let t_created_max = facts
            .iter()
            .map(|f| f.t_created)
            .max()
            .unwrap_or_else(Utc::now);

        self.cold
            .commit_archive_atomic(
                pak_filename,
                facts.len() as i64,
                edges.len() as i64,
                fact_id_min,
                fact_id_max,
                t_created_min,
                t_created_max,
                pak_size_bytes as i64,
                blake3_hash,
                fact_ids,
            )
            .await
    }

    /// Verify a single `.pak` file against its manifest hash.
    fn verify_single_archive(
        entry: &ArchiveManifestEntry,
        pak_path: &std::path::Path,
    ) -> ArchiveVerifyResult {
        match verify_pak(pak_path, &entry.blake3_hash) {
            Ok(true) => ArchiveVerifyResult {
                manifest_id: entry.id,
                pak_path: entry.pak_path.clone(),
                ok: true,
                error: None,
            },
            Ok(false) => {
                let actual = hash_file(pak_path).unwrap_or_default();
                ArchiveVerifyResult {
                    manifest_id: entry.id,
                    pak_path: entry.pak_path.clone(),
                    ok: false,
                    error: Some(format!(
                        "hash mismatch: expected {}, got {actual}",
                        entry.blake3_hash
                    )),
                }
            }
            Err(e) => ArchiveVerifyResult {
                manifest_id: entry.id,
                pak_path: entry.pak_path.clone(),
                ok: false,
                error: Some(format!("verification error: {e}")),
            },
        }
    }

    /// Search all archived `.pak` files for facts matching `query`.
    ///
    /// Returns `Ok(None)` when there is no file-backed engine, no archive
    /// directory, or no manifest entries — not an error, just nothing to search.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on manifest read failure.
    /// Returns `MemoryError::Archive` on `.pak` I/O or decompression failure.
    pub(crate) async fn search_archives_fallback(
        &self,
        query: &crate::search::query::MemoryQuery,
        limit: usize,
    ) -> Result<Option<crate::archive::search::ArchiveSearchResult>> {
        let Ok(archive_dir) = self.archive_dir() else {
            return Ok(None);
        };
        let entries = self.cold.list_archive_manifest().await?;
        if entries.is_empty() {
            return Ok(None);
        }
        let result = crate::archive::search::search_archives(&archive_dir, &entries, query, limit)?;
        Ok(Some(result))
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

    use crate::types::{FactType, NewFact};

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
            importance: 0.5,
            access_count: 0,
            last_accessed: Utc::now(),
            metadata: serde_json::json!({}),
            scope_id: 1,
            is_pinned: false,
        }
    }

    /// #265: a failure inside `commit_archive` (here: the manifest INSERT fails
    /// because the table was dropped) must NOT leave an orphan `.pak` file behind.
    /// The `.pak` is written before the commit transaction; without on-error
    /// cleanup it would be a permanent disk leak with no manifest row (CWE-459).
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
}
