//! Archival compression orchestration (Wave 2 #816 / S4, sub-PR 3b).
//!
//! `MemoryEngine::{archive, list_archives, verify_archives, search_archives_fallback}`'s
//! bodies, extracted as free functions over [`MemoryCtx`].
//!
//! Moved from the facade's `engine/archive.rs` verbatim, with one adjustment: `MemoryCtx`
//! carries no path state (`self.db_path` / `self.is_file_backed` / the private
//! `archive_dir()` helper stay facade concerns — the facade resolves the archive
//! directory, a sibling of the DB file, and passes it in as `archive_dir: &Path`).
//!
//! [`archive`]'s pre-flight checks (`ensure_open`, the read-only fail-fast, and the
//! file-backed check) also stay in the facade's delegate rather than moving here: the
//! file-backed check must run *before* `archive_dir` can even be resolved (it is not
//! reachable through [`MemoryCtx`]), so preserving the original's exact check order and
//! error messages requires the facade to own the whole pre-flight sequence, not just the
//! path resolution. See the facade's `engine/archive.rs` for that sequence.

use std::path::{Path, PathBuf};

use chrono::Utc;
use parking_lot::RwLock;

use me_index::MemoryGraph;
use me_storage::{ColdStorage, MemoryCtx};
use me_types::error::{ArchiveError, Result};
use me_types::types::archive::ARCHIVE_SCHEMA_VERSION;
use me_types::types::search::MemoryQuery;
use me_types::types::{Edge, Fact};

use crate::pak::{hash_file, verify_pak, write_pak_and_hash};
use crate::search::{ArchiveSearchResult, is_within_archive_dir, search_archives};
use crate::types::{
    ArchiveManifestEntry, ArchivePak, ArchivePolicy, ArchiveStats, ArchiveVerifyResult,
    CURRENT_PAK_VERSION,
};

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
pub async fn archive(
    ctx: MemoryCtx<'_>,
    cold: &dyn ColdStorage,
    graph: &RwLock<MemoryGraph>,
    archive_dir: &Path,
    policy: &ArchivePolicy,
) -> Result<Option<ArchiveStats>> {
    let (candidate_facts, candidate_edges) = ctx
        .storage
        .select_archive_candidates(policy.expired_before)
        .await?;

    if candidate_facts.len() < policy.min_facts {
        return Ok(None);
    }

    let fact_ids: Vec<i64> = candidate_facts.iter().map(|f| f.id).collect();

    let pak = build_pak(ctx.embed_dim, &candidate_facts, &candidate_edges);
    let (pak_path, pak_size_bytes, blake3_hash) = write_pak_to_disk(archive_dir, &pak)?;
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
    commit_archive(
        cold,
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

    // Prune the archived facts from the in-memory graph cache (#332).
    //
    // The in-memory graph is a *derived cache* of the active edge set; the
    // DB (committed atomically above) is the source of truth. The whole
    // prune runs under one graph write guard with no `.await` inside, so it
    // is atomic with respect to any concurrent graph mutator — there is no
    // off-lock read and therefore no lost-update window. The removal is O(N)
    // in the number of archived facts rather than O(|E|) for a full reload.
    //
    // `MemoryGraph::remove_node` is loop-safe (#833): petgraph's swap-remove
    // relocates the former last node into the freed slot, and `remove_node`
    // re-indexes `node_map` for that displaced node, so surviving nodes keep
    // resolving correctly across every iteration. If the process is killed
    // mid-prune, the cache self-heals — the next `open` rebuilds it from the
    // committed DB via `MemoryGraph::load_from_db`.
    {
        let mut graph = graph.write();
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
/// Returns `MemoryError::Storage` on SQL failure.
pub async fn list_archives(cold: &dyn ColdStorage) -> Result<Vec<ArchiveManifestEntry>> {
    cold.list_archive_manifest().await
}

/// Verify integrity of all archived `.pak` files.
///
/// Checks each manifest entry's blake3 hash against the actual file.
///
/// # Errors
///
/// Returns `MemoryError::Storage` on SQL failure.
/// I/O errors for individual `.pak` files are reported per-entry, not propagated.
pub async fn verify_archives(
    cold: &dyn ColdStorage,
    archive_dir: &Path,
) -> Result<Vec<ArchiveVerifyResult>> {
    let entries = list_archives(cold).await?;

    let mut results = Vec::with_capacity(entries.len());
    for entry in &entries {
        // Path-traversal guard (#292): reject any manifest path that could
        // escape `archive_dir` (`..` or an absolute anchor) BEFORE any
        // filesystem access. The old `pak_path.starts_with(&archive_dir)`
        // check was purely lexical and did not resolve `..`, so a tampered
        // or restored DB row like `../outside/x.pak` slipped through. Reuse
        // the shared containment check that already guards the sibling
        // `search_archives` (single source of truth for the rule).
        if !is_within_archive_dir(&entry.pak_path) {
            results.push(ArchiveVerifyResult {
                manifest_id: entry.id,
                pak_path: entry.pak_path.clone(),
                ok: false,
                error: Some("path traversal detected".to_string()),
            });
            continue;
        }
        let pak_path = archive_dir.join(&entry.pak_path);
        let result = if pak_path.exists() {
            verify_single_archive(entry, &pak_path)
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

/// Search all archived `.pak` files for facts matching `query`.
///
/// Returns `Ok(None)` when there are no manifest entries — not an error, just
/// nothing to search. The facade's caller additionally treats a failure to resolve
/// `archive_dir` (an in-memory engine) as `Ok(None)` before ever calling this —
/// see `engine/archive.rs`'s `search_archives_fallback` delegate.
///
/// # Errors
///
/// Returns `MemoryError::Storage` on manifest read failure.
/// Returns `MemoryError::Archive` on `.pak` I/O or decompression failure.
pub async fn search_archives_fallback(
    cold: &dyn ColdStorage,
    archive_dir: &Path,
    query: &MemoryQuery,
    limit: usize,
) -> Result<Option<ArchiveSearchResult>> {
    let entries = cold.list_archive_manifest().await?;
    if entries.is_empty() {
        return Ok(None);
    }
    let result = search_archives(archive_dir, &entries, query, limit)?;
    Ok(Some(result))
}

// --- Private helpers ---

/// Build the `.pak` payload.
///
/// `pub` (not private): the facade's `pak_write_stamp_matches_read_gate` regression
/// test (`engine/archive.rs`) calls this directly across the crate boundary to pin the
/// write side of the `.pak` schema-version symmetry guard (Wave 2 #816 / S4, sub-PR 3b
/// — see the module docs' "structural prize").
#[must_use]
pub fn build_pak(embed_dim: usize, facts: &[Fact], edges: &[Edge]) -> ArchivePak {
    ArchivePak {
        pak_version: CURRENT_PAK_VERSION,
        engine_schema_version: ARCHIVE_SCHEMA_VERSION,
        embed_dim,
        created_at: Utc::now(),
        facts: facts.to_vec(),
        edges: edges.to_vec(),
    }
}

/// Write the `.pak` file and return (path, size, hash).
fn write_pak_to_disk(archive_dir: &Path, pak: &ArchivePak) -> Result<(PathBuf, u64, String)> {
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
/// caller-side; on `Err` the caller removes the already-written `.pak`.
#[allow(clippy::cast_possible_wrap)]
async fn commit_archive(
    cold: &dyn ColdStorage,
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

    cold.commit_archive_atomic(
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
fn verify_single_archive(entry: &ArchiveManifestEntry, pak_path: &Path) -> ArchiveVerifyResult {
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
