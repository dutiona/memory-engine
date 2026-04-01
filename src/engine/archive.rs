//! Archival compression orchestration for [`MemoryEngine`].
//!
//! Moves expired, non-pinned facts into `.pak` files (zstd + blake3),
//! records a manifest row, hard-deletes from `SQLite`, and prunes the
//! in-memory graph — all in a crash-safe sequence.

use std::collections::HashSet;
use std::path::PathBuf;

use chrono::Utc;

use crate::archive::pak::{hash_file, verify_pak, write_pak_and_hash};
use crate::archive::types::{
    ArchiveManifestEntry, ArchivePak, ArchivePolicy, ArchiveStats, ArchiveVerifyResult,
};
use crate::error::{MemoryError, Result};
use crate::store::archive_manifest::ArchiveManifestStore;
use crate::store::edges::EdgeStore;
use crate::store::facts::FactStore;
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
    /// Cannot panic in practice — the `.pak` path is always constructed with a filename.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Archive` on I/O failure.
    /// Returns `MemoryError::Database` on SQL failure.
    /// Returns `MemoryError::ReadOnly` if the engine is read-only.
    pub fn archive(&self, policy: &ArchivePolicy) -> Result<Option<ArchiveStats>> {
        if !self.is_file_backed() {
            return Err(MemoryError::Archive(
                "archival requires a file-backed engine".to_string(),
            ));
        }

        let archive_dir = self.archive_dir()?;

        let (candidate_facts, candidate_edges) = self.select_archive_candidates(policy)?;

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

        self.commit_archive(
            &pak_filename,
            &candidate_facts,
            &candidate_edges,
            &fact_ids,
            pak_size_bytes,
            &blake3_hash,
        )?;

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
    pub fn list_archives(&self) -> Result<Vec<ArchiveManifestEntry>> {
        self.with_read(|conn| ArchiveManifestStore::new(conn).list())
    }

    /// Verify integrity of all archived `.pak` files.
    ///
    /// Checks each manifest entry's blake3 hash against the actual file.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    /// I/O errors for individual `.pak` files are reported per-entry, not propagated.
    pub fn verify_archives(&self) -> Result<Vec<ArchiveVerifyResult>> {
        let entries = self.list_archives()?;
        let archive_dir = self.archive_dir()?;

        let mut results = Vec::with_capacity(entries.len());
        for entry in &entries {
            let pak_path = archive_dir.join(&entry.pak_path);
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

    /// Select expired, non-pinned facts and their internal edges.
    fn select_archive_candidates(&self, policy: &ArchivePolicy) -> Result<(Vec<Fact>, Vec<Edge>)> {
        let conn = self.pool.read();
        let all_facts = FactStore::new(&conn, self.embed_dim).list_all()?;
        let candidate_facts: Vec<_> = all_facts
            .into_iter()
            .filter(|f| !f.is_pinned && f.t_expired.is_some_and(|te| te < policy.expired_before))
            .collect();

        let candidate_ids: HashSet<i64> = candidate_facts.iter().map(|f| f.id).collect();

        let all_edges = EdgeStore::new(&conn).list_all()?;
        let candidate_edges: Vec<_> = all_edges
            .into_iter()
            .filter(|e| {
                candidate_ids.contains(&e.source_fact_id)
                    && candidate_ids.contains(&e.target_fact_id)
            })
            .collect();

        drop(conn);
        Ok((candidate_facts, candidate_edges))
    }

    /// Build the `.pak` payload.
    fn build_pak(&self, facts: &[Fact], edges: &[Edge]) -> ArchivePak {
        ArchivePak {
            pak_version: 1,
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
            MemoryError::Archive(format!(
                "failed to create archive dir {}: {e}",
                archive_dir.display()
            ))
        })?;

        let now = Utc::now();
        let nanos = now.timestamp_subsec_nanos();
        let pak_filename = format!("archive-{}-{nanos:08x}.pak", now.format("%Y%m%d%H%M%S"));
        let pak_path = archive_dir.join(&pak_filename);

        let blake3_hash = write_pak_and_hash(pak, &pak_path)?;
        let pak_size_bytes = std::fs::metadata(&pak_path)
            .map_err(|e| {
                MemoryError::Archive(format!(
                    "failed to stat pak file {}: {e}",
                    pak_path.display()
                ))
            })?
            .len();

        Ok((pak_path, pak_size_bytes, blake3_hash))
    }

    /// Single write transaction: manifest insert + hard-delete edges + hard-delete facts.
    // conn must outlive tx (tx borrows from conn) — clippy::significant_drop_tightening is a false positive.
    #[allow(clippy::cast_possible_wrap, clippy::significant_drop_tightening)]
    fn commit_archive(
        &self,
        pak_filename: &str,
        facts: &[Fact],
        edges: &[Edge],
        fact_ids: &[i64],
        pak_size_bytes: u64,
        blake3_hash: &str,
    ) -> Result<()> {
        let now = Utc::now();
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

        let conn = self.write_conn()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| MemoryError::Archive(format!("failed to begin transaction: {e}")))?;

        ArchiveManifestStore::new(&tx).insert(
            pak_filename,
            now,
            facts.len() as i64,
            edges.len() as i64,
            fact_id_min,
            fact_id_max,
            t_created_min,
            t_created_max,
            pak_size_bytes as i64,
            blake3_hash,
        )?;

        // Delete edges first (FK safety), then facts
        EdgeStore::new(&tx).hard_delete_by_facts(fact_ids)?;
        FactStore::new(&tx, self.embed_dim).hard_delete_ids(fact_ids)?;

        tx.commit().map_err(|e| {
            MemoryError::Archive(format!("failed to commit archive transaction: {e}"))
        })?;

        Ok(())
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
    pub(crate) fn search_archives_fallback(
        &self,
        query: &crate::search::query::MemoryQuery,
        limit: usize,
    ) -> Result<Option<crate::archive::search::ArchiveSearchResult>> {
        let Ok(archive_dir) = self.archive_dir() else {
            return Ok(None);
        };
        let entries = self.with_read(|conn| ArchiveManifestStore::new(conn).list())?;
        if entries.is_empty() {
            return Ok(None);
        }
        let result = crate::archive::search::search_archives(&archive_dir, &entries, query, limit)?;
        Ok(Some(result))
    }

    /// Resolve the archive directory (sibling of DB file + `/archives/`).
    fn archive_dir(&self) -> Result<PathBuf> {
        let db_path = self.pool.path().ok_or_else(|| {
            MemoryError::Archive("cannot resolve archive dir for in-memory database".to_string())
        })?;
        let parent = db_path.parent().ok_or_else(|| {
            MemoryError::Archive(format!(
                "database path has no parent: {}",
                db_path.display()
            ))
        })?;
        Ok(parent.join("archives"))
    }
}
