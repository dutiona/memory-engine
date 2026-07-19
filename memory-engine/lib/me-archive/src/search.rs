//! Brute-force archive search: scan `.pak` files for matching facts.
//!
//! This is a slow fallback path. All `.pak` files are decompressed in sequence
//! and searched with text substring + cosine similarity. Only invoked when the
//! consumer explicitly opts in via [`MemoryQuery::include_archives`].

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::path::{Component, Path};
use std::time::Instant;

use crate::pak::read_pak;
use crate::types::ArchiveManifestEntry;
use me_types::error::Result;
use me_types::math::cosine_similarity;
use me_types::types::search::{MatchType, MemoryQuery, SearchResult};

/// Summary result from scanning archives.
///
/// `pub` (not `pub(crate)`): the facade (`memory-engine`) consumes this from a
/// separate crate now (Wave 2 #816 / S4, sub-PR 3b) — its `execute_query`
/// archive-fallback post-step reads `results`/`paks_scanned`/`search_ms` directly.
pub struct ArchiveSearchResult {
    pub results: Vec<SearchResult>,
    pub paks_scanned: usize,
    pub search_ms: u64,
}

/// A [`SearchResult`] ordered so a [`BinaryHeap`] behaves as a **min-heap** on
/// `score` (Rust's `BinaryHeap` is a max-heap by default).
///
/// With the comparison reversed, `heap.peek()`/`heap.pop()` yield the *lowest*
/// score, which is exactly the element to evict when the heap is full and a new
/// candidate scores higher. This bounds retained results to `limit` instead of
/// accumulating every match across all paks (#342).
///
/// Ordering uses [`f64::total_cmp`], a robust total order defined for every
/// `f64` including NaN (cosine can never produce NaN here — `cosine_similarity`
/// guards zero-magnitude — but the wrapper must still be a total order to
/// satisfy `Ord`, and `total_cmp` provides that unconditionally).
struct MinScored(SearchResult);

impl PartialEq for MinScored {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for MinScored {}

impl PartialOrd for MinScored {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MinScored {
    fn cmp(&self, other: &Self) -> Ordering {
        // `total_cmp` is a robust total order over all `f64` (including NaN), so
        // `Ord` holds unconditionally without a `partial_cmp` fallback. Reverse
        // so the *smallest* score is the heap's max (popped first).
        self.0.score.total_cmp(&other.0.score).reverse()
    }
}

/// Whether `pak_path` (the manifest's per-entry relative path) is safe to join
/// onto the archive directory without escaping it.
///
/// The manifest is consumer-controlled state on disk, so a malicious or
/// corrupted entry must not be able to read arbitrary files. A bare
/// `joined.starts_with(archive_dir)` is a **lexical prefix test that `..`
/// defeats**: `<dir>/../outside/x.pak` lexically starts with `<dir>` yet resolves
/// outside it. We therefore reject any component that can escape — `..`
/// ([`Component::ParentDir`]) or an absolute anchor ([`Component::RootDir`] /
/// [`Component::Prefix`], which would also make `join` discard `archive_dir`
/// entirely). Only plain path segments (`Normal`) and `.` (`CurDir`) are allowed.
///
/// This is checked *before* `read_pak`, needs no filesystem access (so it works
/// for not-yet-existing files), and is purely lexical — no `canonicalize`, no
/// TOCTOU window.
///
/// `pub` (not `pub(crate)`): the **shared archive path-containment guard** — the
/// sibling `verify_archives` (`crate::manage`) reuses this helper rather than
/// re-deriving the check (#292), and the facade's retained regression test
/// (`engine/archive.rs`) exercises it directly across the crate boundary (Wave 2
/// #816 / S4, sub-PR 3b).
#[must_use]
pub(crate) fn is_within_archive_dir(pak_path: &str) -> bool {
    Path::new(pak_path)
        .components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

/// Whether the pak described by `entry` can be **soundly** skipped for `query`
/// without reading it — i.e. it provably contains no fact the query could match.
///
/// CAUTION — the only manifest bounds available are `fact_id_{min,max}` and
/// `t_created_{min,max}` (transaction/system time). [`MemoryQuery`] currently
/// exposes only *valid-time* temporal filters (`period_start`/`period_end`/
/// `valid_at` map to `t_valid`/`t_invalid`). Pruning a `t_created` range against
/// a valid-time window is **unsound**: a fact created early can be valid late, so
/// such a prune would silently drop real matches (#387). We therefore prune
/// **only** on dimensions where manifest metadata and query semantics genuinely
/// align — today that is the fact-id range, and `MemoryQuery` has no fact-id
/// filter, so no prune currently fires. The check is structured so a future
/// id-range (or a `t_created`-based query window) slots in here without
/// reintroducing the unsound cross-axis prune.
const fn entry_is_prunable(_entry: &ArchiveManifestEntry, _query: &MemoryQuery) -> bool {
    // No sound prune is currently expressible: see the doc comment. Returning
    // `false` means every (in-bounds, present) pak is still scanned — correctness
    // over speed, exactly as #387's triage requires.
    false
}

/// Brute-force search through all listed `.pak` files.
///
/// Applies text substring matching and cosine similarity as available from the
/// query. Fact-type filtering is applied when `query.fact_type` is set.
///
/// Paks are decompressed one at a time and only the top-`limit` matches (by
/// score) are retained in a bounded min-heap, so peak memory is `O(limit)`
/// matching facts rather than `O(total matches)` (#342). Manifest entries are
/// pruned via `entry_is_prunable` before reading where the prune is provably
/// sound (#387). Results are returned sorted descending by score.
///
/// # Errors
///
/// Returns `MemoryError::Archive` on I/O or decompression failure.
/// Returns `MemoryError::Serialization` if a `.pak` file's JSON is corrupt or
/// truncated (surfaced by `read_pak` during decompression and deserialization).
///
/// Each `.pak` is opened via [`read_pak`], whose schema gate checks `me-types`'
/// backend-independent `ARCHIVE_SCHEMA_VERSION` — the same constant the write side
/// stamps. No backend schema version is involved (Wave 2 #816 / S4, sub-PR 3a).
pub(crate) fn search_archives(
    archive_dir: &Path,
    manifest_entries: &[ArchiveManifestEntry],
    query: &MemoryQuery,
    limit: usize,
) -> Result<ArchiveSearchResult> {
    let start = Instant::now();
    // Bounded min-heap: never holds more than `limit` matches at once. The
    // lowest-scored element sits at the top so it is the first evicted when a
    // higher-scored candidate arrives.
    let mut heap: BinaryHeap<MinScored> = BinaryHeap::new();
    let mut paks_scanned = 0usize;

    // Pre-compute the lowercased query once; it is invariant across all facts.
    let query_text_lower = query.text.as_ref().map(|text| text.to_lowercase());

    for entry in manifest_entries {
        // Path-traversal guard: reject any entry whose relative path could escape
        // `archive_dir` (`..` or an absolute anchor). Checked before any I/O.
        if !is_within_archive_dir(&entry.pak_path) {
            continue;
        }
        // Manifest-metadata prune: skip paks that provably hold no match (#387).
        if entry_is_prunable(entry, query) {
            continue;
        }
        let pak_path = archive_dir.join(&entry.pak_path);
        // `is_file` (not `exists`): a path occupied by a directory is treated as
        // an absent pak and skipped, rather than handed to `read_pak` only to
        // fail with a generic open error.
        if !pak_path.is_file() {
            continue;
        }

        let pak = read_pak(&pak_path)?;
        paks_scanned += 1;

        for fact in &pak.facts {
            // Fact-type filter
            if let Some(ref ft) = query.fact_type
                && &fact.fact_type != ft
            {
                continue;
            }

            let mut score = 0.0_f64;
            // `matched` is set from three independent conditions below (text, vector,
            // match-all), with `score` accumulated alongside. The let-if-seq lint's
            // single-`if` rewrite would drop the other two branches, so suppress it.
            #[allow(clippy::useless_let_if_seq)]
            let mut matched = false;

            // Text matching — simple case-insensitive substring
            if let Some(ref text_lower) = query_text_lower
                && fact.content.to_lowercase().contains(text_lower)
            {
                score += 1.0;
                matched = true;
            }

            // Vector similarity — reuse existing cosine_similarity
            if let Some(ref query_emb) = query.embedding
                && query_emb.len() == fact.embedding.len()
                && !fact.embedding.is_empty()
            {
                let cos = cosine_similarity(query_emb, &fact.embedding);
                if cos > 0.0 {
                    score += f64::from(cos);
                    matched = true;
                }
            }

            // If neither text nor embedding is set, match all facts
            if query.text.is_none() && query.embedding.is_none() {
                matched = true;
                score = 1.0;
            }

            if matched {
                push_bounded(&mut heap, fact, score, limit);
            }
        }
    }

    // Drain the heap into a descending-by-score vector. `into_sorted_vec` orders
    // ascending by `MinScored::Ord`, which is *reversed* score — so the resulting
    // vector is already highest-score-first, matching the previous
    // `sort_by(b.cmp(a))` contract.
    let results: Vec<SearchResult> = heap.into_sorted_vec().into_iter().map(|m| m.0).collect();

    let search_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

    Ok(ArchiveSearchResult {
        results,
        paks_scanned,
        search_ms,
    })
}

/// Consider `fact` at `score` for retention in the bounded min-heap, keeping at
/// most `limit` elements.
///
/// The [`Fact`](me_types::types::Fact) is **cloned only when the candidate is
/// actually retained** — a candidate below the current minimum (with a full heap)
/// is rejected before any clone. This is the memory bound #342 asks for: at most
/// `limit` cloned facts ever co-exist, versus the old `O(total matches)` clones
/// held until the final sort. When the heap is full, the candidate replaces the
/// current minimum only if it scores **strictly higher**. A `limit` of `0`
/// retains nothing. Tie-break among equal scores at the eviction boundary is
/// heap-order (unspecified) — the documented semantic change from the old stable
/// sort-then-truncate (#342).
fn push_bounded(
    heap: &mut BinaryHeap<MinScored>,
    fact: &me_types::types::Fact,
    score: f64,
    limit: usize,
) {
    if limit == 0 {
        return;
    }
    if heap.len() < limit {
        heap.push(MinScored(make_result(fact, score)));
    } else if let Some(min) = heap.peek()
        && score > min.0.score
    {
        heap.pop();
        heap.push(MinScored(make_result(fact, score)));
    }
}

/// Build a retained [`SearchResult`] (clones the fact — see [`push_bounded`]).
fn make_result(fact: &me_types::types::Fact, score: f64) -> SearchResult {
    SearchResult {
        fact: fact.clone(),
        score,
        match_type: MatchType::Archive,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pak::write_pak_and_hash;
    use crate::types::{ArchivePak, CURRENT_PAK_VERSION};
    use chrono::Utc;
    use me_types::types::archive::ARCHIVE_SCHEMA_VERSION;
    use me_types::types::{Fact, FactType};

    /// Build a minimal `Fact` for archive-search fixtures.
    ///
    /// Only the fields the search path reads (`content`, `fact_type`, `embedding`,
    /// and `id` for identification) carry meaningful values; everything else is a
    /// neutral default so tests stay focused on the search logic.
    fn fact(id: i64, content: &str, fact_type: FactType, embedding: Vec<f32>) -> Fact {
        let now = Utc::now();
        Fact {
            id,
            content: content.to_owned(),
            content_hash: format!("hash-{id}"),
            embedding,
            fact_type,
            t_created: now,
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            base_importance: 0.5,
            access_count: 0,
            last_accessed: now,
            metadata: serde_json::Value::Null,
            scope_id: 1,
            is_pinned: false,
            importance_score: 0.0,
            surfaced_at: None,
        }
    }

    /// Wrap facts into an `ArchivePak` with current versions.
    fn pak_with(facts: Vec<Fact>) -> ArchivePak {
        ArchivePak {
            pak_version: CURRENT_PAK_VERSION,
            engine_schema_version: ARCHIVE_SCHEMA_VERSION,
            embed_dim: facts.first().map_or(0, |f| f.embedding.len()),
            created_at: Utc::now(),
            facts,
            edges: vec![],
        }
    }

    /// Write a pak under `dir` with file name `name`, returning a manifest entry
    /// pointing at it. The manifest bounds (`fact_id_*`, `t_created_*`) are derived
    /// from the facts so pruning tests can rely on accurate metadata.
    fn write_entry(dir: &Path, name: &str, pak: &ArchivePak) -> ArchiveManifestEntry {
        let pak_path = dir.join(name);
        write_pak_and_hash(pak, &pak_path).expect("write pak fixture");
        let fact_id_min = pak.facts.iter().map(|f| f.id).min().unwrap_or(0);
        let fact_id_max = pak.facts.iter().map(|f| f.id).max().unwrap_or(0);
        let t_created_min = pak
            .facts
            .iter()
            .map(|f| f.t_created)
            .min()
            .unwrap_or_else(Utc::now);
        let t_created_max = pak
            .facts
            .iter()
            .map(|f| f.t_created)
            .max()
            .unwrap_or_else(Utc::now);
        ArchiveManifestEntry {
            id: 1,
            pak_path: name.to_owned(),
            created_at: Utc::now(),
            fact_count: i64::try_from(pak.facts.len()).expect("fixture fact count fits i64"),
            edge_count: 0,
            fact_id_min,
            fact_id_max,
            t_created_min,
            t_created_max,
            size_bytes: 0,
            blake3_hash: String::new(),
        }
    }

    // --- Text matching ---

    #[test]
    fn text_query_matches_substring_case_insensitively() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pak = pak_with(vec![
            fact(1, "The Quick Brown Fox", FactType::Episodic, vec![]),
            fact(2, "lazy dog", FactType::Episodic, vec![]),
        ]);
        let entry = write_entry(dir.path(), "a.pak", &pak);

        let query = MemoryQuery::new().text("brown");
        let out = search_archives(dir.path(), &[entry], &query, 10).expect("search");

        assert_eq!(out.paks_scanned, 1);
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.results[0].fact.id, 1);
        assert_eq!(out.results[0].match_type, MatchType::Archive);
    }

    // --- Vector / cosine branch (#298 named this untested) ---

    #[test]
    fn embedding_only_query_scores_by_cosine() {
        let dir = tempfile::tempdir().expect("tempdir");
        // f1 is collinear with the query (cos = 1); f2 is orthogonal (cos = 0 -> skipped).
        let pak = pak_with(vec![
            fact(1, "aligned", FactType::Semantic, vec![1.0, 0.0]),
            fact(2, "orthogonal", FactType::Semantic, vec![0.0, 1.0]),
        ]);
        let entry = write_entry(dir.path(), "vec.pak", &pak);

        let query = MemoryQuery::new().embedding(vec![1.0, 0.0]);
        let out = search_archives(dir.path(), &[entry], &query, 10).expect("search");

        // Orthogonal fact has cos == 0.0 which is not > 0.0, so it does not match.
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.results[0].fact.id, 1);
        assert!((out.results[0].score - 1.0).abs() < 1e-6);
    }

    #[test]
    fn embedding_with_mismatched_dim_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pak = pak_with(vec![fact(1, "x", FactType::Semantic, vec![1.0, 0.0])]);
        let entry = write_entry(dir.path(), "dim.pak", &pak);

        // Query embedding has a different length than the fact embedding, so the
        // vector branch is skipped; with no text either, nothing matches.
        let query = MemoryQuery::new().embedding(vec![1.0, 0.0, 0.0]);
        let out = search_archives(dir.path(), &[entry], &query, 10).expect("search");

        assert!(out.results.is_empty());
    }

    #[test]
    fn text_and_embedding_scores_combine() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pak = pak_with(vec![fact(
            1,
            "matching text",
            FactType::Semantic,
            vec![1.0, 0.0],
        )]);
        let entry = write_entry(dir.path(), "both.pak", &pak);

        let query = MemoryQuery::new()
            .text("matching")
            .embedding(vec![1.0, 0.0]);
        let out = search_archives(dir.path(), &[entry], &query, 10).expect("search");

        assert_eq!(out.results.len(), 1);
        // 1.0 (text) + 1.0 (cosine) == 2.0.
        assert!((out.results[0].score - 2.0).abs() < 1e-6);
    }

    // --- Match-all fallback (no text, no embedding) ---

    #[test]
    fn no_query_matches_all_facts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pak = pak_with(vec![
            fact(1, "a", FactType::Episodic, vec![]),
            fact(2, "b", FactType::Semantic, vec![]),
        ]);
        let entry = write_entry(dir.path(), "all.pak", &pak);

        let query = MemoryQuery::new();
        let out = search_archives(dir.path(), &[entry], &query, 10).expect("search");

        assert_eq!(out.results.len(), 2);
        assert!(out.results.iter().all(|r| (r.score - 1.0).abs() < 1e-6));
    }

    // --- Fact-type filter ---

    #[test]
    fn fact_type_filter_excludes_other_types() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pak = pak_with(vec![
            fact(1, "ep", FactType::Episodic, vec![]),
            fact(2, "se", FactType::Semantic, vec![]),
            fact(3, "pr", FactType::Procedural, vec![]),
        ]);
        let entry = write_entry(dir.path(), "types.pak", &pak);

        let query = MemoryQuery::new().fact_type(FactType::Semantic);
        let out = search_archives(dir.path(), &[entry], &query, 10).expect("search");

        assert_eq!(out.results.len(), 1);
        assert_eq!(out.results[0].fact.id, 2);
        assert_eq!(out.results[0].fact.fact_type, FactType::Semantic);
    }

    // --- Path-traversal guard ---

    #[test]
    fn path_traversal_entry_is_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Write a real pak OUTSIDE the archive dir to prove the guard, not file
        // absence, is what skips it.
        let outside = tempfile::tempdir().expect("outside tempdir");
        let escape_pak = pak_with(vec![fact(1, "secret", FactType::Episodic, vec![])]);
        write_pak_and_hash(&escape_pak, &outside.path().join("escape.pak")).expect("write escape");

        // A manifest entry whose join() escapes the archive dir via "..".
        let rel = format!(
            "..{sep}{name}{sep}escape.pak",
            sep = std::path::MAIN_SEPARATOR,
            name = outside
                .path()
                .file_name()
                .expect("outside name")
                .to_string_lossy(),
        );
        let entry = ArchiveManifestEntry {
            id: 1,
            pak_path: rel,
            created_at: Utc::now(),
            fact_count: 1,
            edge_count: 0,
            fact_id_min: 1,
            fact_id_max: 1,
            t_created_min: Utc::now(),
            t_created_max: Utc::now(),
            size_bytes: 0,
            blake3_hash: String::new(),
        };

        let query = MemoryQuery::new();
        let out = search_archives(dir.path(), &[entry], &query, 10).expect("search");

        assert_eq!(out.paks_scanned, 0, "traversal entry must not be read");
        assert!(out.results.is_empty());
    }

    // --- Missing pak file is silently skipped ---

    #[test]
    fn missing_pak_file_is_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Real pak present.
        let present = write_entry(
            dir.path(),
            "present.pak",
            &pak_with(vec![fact(1, "here", FactType::Episodic, vec![])]),
        );
        // Manifest references a file that was never written.
        let absent = ArchiveManifestEntry {
            id: 2,
            pak_path: "absent.pak".to_owned(),
            created_at: Utc::now(),
            fact_count: 1,
            edge_count: 0,
            fact_id_min: 99,
            fact_id_max: 99,
            t_created_min: Utc::now(),
            t_created_max: Utc::now(),
            size_bytes: 0,
            blake3_hash: String::new(),
        };

        let query = MemoryQuery::new();
        let out = search_archives(dir.path(), &[present, absent], &query, 10).expect("search");

        assert_eq!(out.paks_scanned, 1, "only the present pak is read");
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.results[0].fact.id, 1);
    }

    // --- Multi-pak iteration ---

    #[test]
    fn results_span_multiple_paks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let e1 = write_entry(
            dir.path(),
            "p1.pak",
            &pak_with(vec![fact(1, "apple pie", FactType::Episodic, vec![])]),
        );
        let e2 = write_entry(
            dir.path(),
            "p2.pak",
            &pak_with(vec![fact(2, "apple cider", FactType::Episodic, vec![])]),
        );

        let query = MemoryQuery::new().text("apple");
        let out = search_archives(dir.path(), &[e1, e2], &query, 10).expect("search");

        assert_eq!(out.paks_scanned, 2);
        assert_eq!(out.results.len(), 2);
        let ids: Vec<i64> = out.results.iter().map(|r| r.fact.id).collect();
        assert!(ids.contains(&1) && ids.contains(&2));
    }

    // --- >limit truncation: pins ordering + truncation before any refactor (#342) ---

    #[test]
    fn results_are_sorted_descending_and_truncated_to_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Three facts with distinct cosine scores against the query [1, 0]:
        //   f1: [1,0]    -> cos 1.0   (highest)
        //   f2: [1,1]    -> cos ~0.707
        //   f3: [3,1]    -> cos ~0.949
        // Expected descending order: f1 (1.0), f3 (~0.949), f2 (~0.707).
        let pak = pak_with(vec![
            fact(1, "f1", FactType::Semantic, vec![1.0, 0.0]),
            fact(2, "f2", FactType::Semantic, vec![1.0, 1.0]),
            fact(3, "f3", FactType::Semantic, vec![3.0, 1.0]),
        ]);
        let entry = write_entry(dir.path(), "scores.pak", &pak);

        let query = MemoryQuery::new().embedding(vec![1.0, 0.0]);

        // Full set: descending by score.
        let full =
            search_archives(dir.path(), std::slice::from_ref(&entry), &query, 10).expect("search");
        assert_eq!(full.results.len(), 3);
        let order: Vec<i64> = full.results.iter().map(|r| r.fact.id).collect();
        assert_eq!(order, vec![1, 3, 2], "descending by cosine score");

        // limit < candidates: keep the top-2 highest-scored only.
        let capped = search_archives(dir.path(), &[entry], &query, 2).expect("search");
        assert_eq!(capped.results.len(), 2);
        let capped_ids: Vec<i64> = capped.results.iter().map(|r| r.fact.id).collect();
        assert_eq!(capped_ids, vec![1, 3], "top-2 by score retained");
    }

    #[test]
    fn limit_zero_returns_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = write_entry(
            dir.path(),
            "z.pak",
            &pak_with(vec![fact(1, "x", FactType::Episodic, vec![])]),
        );

        let query = MemoryQuery::new();
        let out = search_archives(dir.path(), &[entry], &query, 0).expect("search");

        assert!(out.results.is_empty());
        // The pak is still scanned even when limit == 0.
        assert_eq!(out.paks_scanned, 1);
    }

    // --- Bounded retain across paks (#342): only `limit` survive at once ---

    #[test]
    fn limit_bounds_results_across_multiple_paks() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Two paks, three facts each, all matching the text query — six matches.
        let e1 = write_entry(
            dir.path(),
            "m1.pak",
            &pak_with(vec![
                fact(1, "apple a", FactType::Episodic, vec![]),
                fact(2, "apple b", FactType::Episodic, vec![]),
                fact(3, "apple c", FactType::Episodic, vec![]),
            ]),
        );
        let e2 = write_entry(
            dir.path(),
            "m2.pak",
            &pak_with(vec![
                fact(4, "apple d", FactType::Episodic, vec![]),
                fact(5, "apple e", FactType::Episodic, vec![]),
                fact(6, "apple f", FactType::Episodic, vec![]),
            ]),
        );

        let query = MemoryQuery::new().text("apple");
        let out = search_archives(dir.path(), &[e1, e2], &query, 2).expect("search");

        assert_eq!(out.paks_scanned, 2);
        // Six candidates, but only `limit` retained.
        assert_eq!(out.results.len(), 2);
    }

    // --- is_within_archive_dir (path-traversal guard) ---

    #[test]
    fn guard_accepts_plain_and_curdir_segments() {
        assert!(is_within_archive_dir("a.pak"));
        assert!(is_within_archive_dir("sub/dir/a.pak"));
        assert!(is_within_archive_dir("./a.pak"));
    }

    #[test]
    fn guard_rejects_parent_dir_traversal() {
        assert!(!is_within_archive_dir("../escape.pak"));
        assert!(!is_within_archive_dir("sub/../../escape.pak"));
        assert!(!is_within_archive_dir("a/../../b.pak"));
    }

    #[test]
    fn guard_rejects_absolute_paths() {
        // An absolute path makes `join` discard the archive dir entirely.
        #[cfg(unix)]
        assert!(!is_within_archive_dir("/etc/passwd"));
        #[cfg(windows)]
        assert!(!is_within_archive_dir(r"C:\Windows\system32"));
    }

    // --- entry_is_prunable: must NOT prune (no sound prune today, #387) ---

    #[test]
    fn no_entry_is_pruned_without_a_sound_filter() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pak = pak_with(vec![fact(42, "needle", FactType::Episodic, vec![])]);
        let entry = write_entry(dir.path(), "prune.pak", &pak);

        // A valid-time period query must NOT prune by t_created (unsound): the
        // matching fact must still be found.
        let now = Utc::now();
        let query = MemoryQuery::new().text("needle").period(
            now - chrono::Duration::days(365),
            now + chrono::Duration::days(365),
        );
        assert!(!entry_is_prunable(&entry, &query));

        let out = search_archives(dir.path(), &[entry], &query, 10).expect("search");
        assert_eq!(out.paks_scanned, 1);
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.results[0].fact.id, 42);
    }

    #[test]
    fn t_created_outside_valid_time_window_is_not_pruned() {
        // The cross-axis trap (#387): a fact whose *system* time (`t_created`,
        // the only temporal axis the manifest carries) lies WELL OUTSIDE the
        // query period, but whose *valid* time falls INSIDE it. Pruning a
        // `t_created` range against a valid-time window would silently drop this
        // real match. The fixture in `no_entry_is_pruned_without_a_sound_filter`
        // keeps `t_created = now` (inside the period), so it cannot expose such a
        // prune — this case can, and asserts the fact is still found.
        let dir = tempfile::tempdir().expect("tempdir");
        let now = Utc::now();

        // Created long ago (800 days back), but valid *now* (inside the window).
        let mut early_fact = fact(7, "needle", FactType::Episodic, vec![]);
        early_fact.t_created = now - chrono::Duration::days(800);
        early_fact.t_valid = Some(now);

        let pak = pak_with(vec![early_fact]);
        let entry = write_entry(dir.path(), "cross-axis.pak", &pak);

        // The manifest's `t_created_{min,max}` now sit entirely before the query
        // period, so any `t_created`-vs-period prune would skip this pak.
        assert!(entry.t_created_max < now - chrono::Duration::days(365));

        let query = MemoryQuery::new().text("needle").period(
            now - chrono::Duration::days(365),
            now + chrono::Duration::days(365),
        );

        // No sound prune is expressible (#387), and pruning on the system-time
        // axis against a valid-time window is unsound — the fact must survive.
        assert!(!entry_is_prunable(&entry, &query));

        let out = search_archives(dir.path(), &[entry], &query, 10).expect("search");
        assert_eq!(
            out.paks_scanned, 1,
            "the pak must still be read, not pruned"
        );
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.results[0].fact.id, 7);
    }

    // --- MinScored ordering: BinaryHeap behaves as a min-heap on score ---

    #[test]
    fn min_scored_orders_lowest_score_as_heap_max() {
        let low = MinScored(make_result(&fact(1, "lo", FactType::Episodic, vec![]), 0.1));
        let high = MinScored(make_result(&fact(2, "hi", FactType::Episodic, vec![]), 0.9));
        // Reversed ordering: the lower score is the "greater" element, so a
        // max-heap's peek would surface it for eviction.
        assert!(low > high);
    }
}
