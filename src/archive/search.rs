//! Brute-force archive search: scan `.pak` files for matching facts.
//!
//! This is a slow fallback path. All `.pak` files are decompressed in sequence
//! and searched with text substring + cosine similarity. Only invoked when the
//! consumer explicitly opts in via [`MemoryQuery::include_archives`].

use std::path::Path;
use std::time::Instant;

use crate::archive::pak::read_pak;
use crate::archive::types::ArchiveManifestEntry;
use crate::error::Result;
use crate::search::hybrid::{MatchType, SearchResult};
use crate::search::query::MemoryQuery;
use crate::search::vector::cosine_similarity;

/// Summary result from scanning archives.
pub struct ArchiveSearchResult {
    pub(crate) results: Vec<SearchResult>,
    pub(crate) paks_scanned: usize,
    pub(crate) search_ms: u64,
}

/// Brute-force search through all listed `.pak` files.
///
/// Applies text substring matching and cosine similarity as available from the
/// query. Fact-type filtering is applied when `query.fact_type` is set.
///
/// Results are sorted descending by score and truncated to `limit`.
///
/// # Errors
///
/// Returns `MemoryError::Archive` on I/O or decompression failure.
pub fn search_archives(
    archive_dir: &Path,
    manifest_entries: &[ArchiveManifestEntry],
    query: &MemoryQuery,
    limit: usize,
) -> Result<ArchiveSearchResult> {
    let start = Instant::now();
    let mut all_results: Vec<SearchResult> = Vec::new();
    let mut paks_scanned = 0usize;

    // Pre-compute the lowercased query once; it is invariant across all facts.
    let query_text_lower = query.text.as_ref().map(|text| text.to_lowercase());

    for entry in manifest_entries {
        let pak_path = archive_dir.join(&entry.pak_path);
        // Path traversal guard: ensure resolved path stays inside archive_dir
        if !pak_path.starts_with(archive_dir) {
            continue;
        }
        if !pak_path.exists() {
            continue;
        }

        let pak = read_pak(&pak_path)?;
        paks_scanned += 1;

        for fact in &pak.facts {
            // Fact-type filter
            if let Some(ref ft) = query.fact_type {
                if &fact.fact_type != ft {
                    continue;
                }
            }

            let mut score = 0.0_f64;
            let mut matched = false;

            // Text matching — simple case-insensitive substring
            if let Some(ref text_lower) = query_text_lower {
                if fact.content.to_lowercase().contains(text_lower) {
                    score += 1.0;
                    matched = true;
                }
            }

            // Vector similarity — reuse existing cosine_similarity
            if let Some(ref query_emb) = query.embedding {
                if query_emb.len() == fact.embedding.len() && !fact.embedding.is_empty() {
                    let cos = cosine_similarity(query_emb, &fact.embedding);
                    if cos > 0.0 {
                        score += f64::from(cos);
                        matched = true;
                    }
                }
            }

            // If neither text nor embedding is set, match all facts
            if query.text.is_none() && query.embedding.is_none() {
                matched = true;
                score = 1.0;
            }

            if matched {
                all_results.push(SearchResult {
                    fact: fact.clone(),
                    score,
                    match_type: MatchType::Archive,
                });
            }
        }
    }

    all_results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all_results.truncate(limit);

    let search_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

    Ok(ArchiveSearchResult {
        results: all_results,
        paks_scanned,
        search_ms,
    })
}
