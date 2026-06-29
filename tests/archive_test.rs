#![cfg(feature = "archive")]
#![allow(clippy::unwrap_used)] // test/bench code: panic-on-unwrap is the intended failure signal (#725)

use chrono::{Duration, Utc};
use memory_engine::ArchivePolicy;
use memory_engine::EmbeddingFingerprint;
use memory_engine::ForgetPolicy;
use memory_engine::MemoryQuery;
use memory_engine::engine::MemoryEngine;
use memory_engine::error::Result;
use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::{AddFactRequest, FactType};

const DIM: usize = 8;

/// Deterministic embedder for testing — uses blake3 hash to produce varied vectors.
struct TestEmbedder;

impl EmbeddingProvider for TestEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let hash = blake3::hash(text.as_bytes());
        let bytes = hash.as_bytes();
        let mut embedding = vec![0.0_f32; DIM];
        for (i, val) in embedding.iter_mut().enumerate() {
            let byte = bytes[i % 32];
            *val = (f32::from(byte) - 128.0) / 128.0;
        }
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for val in &mut embedding {
                *val /= norm;
            }
        }
        Ok(embedding)
    }
    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint::new("mock", "test", DIM)
    }
}

/// Open a file-backed engine in a temp directory.
fn open_file_engine(dir: &std::path::Path) -> MemoryEngine {
    let db_path = dir.join("test.db");
    MemoryEngine::builder(DIM).path(db_path).build().unwrap()
}

/// Add `n` facts with unique content.
async fn add_facts(engine: &MemoryEngine, n: usize) -> Vec<i64> {
    let embedder: std::sync::Arc<dyn EmbeddingProvider> = std::sync::Arc::new(TestEmbedder);
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let id = engine
            .add_fact(
                &AddFactRequest {
                    content: format!("archival test fact number {i}"),
                    fact_type: FactType::Episodic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                embedder.clone(),
                None,
            )
            .await
            .unwrap();
        ids.push(id);
    }
    ids
}

#[tokio::test]
async fn archive_lifecycle_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open_file_engine(dir.path());

    // Add 150 facts
    let _ids = add_facts(&engine, 150).await;

    // Forget them all — set a very aggressive policy
    let forget_policy = ForgetPolicy {
        half_life_days: 0.0001,
        min_importance: 1.0, // expire everything
        ..ForgetPolicy::default()
    };
    let prune_stats = engine.forget(&forget_policy).await.unwrap();
    assert!(
        prune_stats.facts_expired >= 150,
        "expected >=150 expired, got {}",
        prune_stats.facts_expired,
    );

    // Archive with cutoff in the future (all expired facts qualify)
    let archive_policy = ArchivePolicy {
        expired_before: Utc::now() + Duration::hours(1),
        min_facts: 10,
    };
    let stats = engine
        .archive(&archive_policy)
        .await
        .unwrap()
        .expect("should archive facts");

    assert_eq!(stats.facts_archived, 150);
    assert!(stats.pak_size_bytes > 0);
    assert!(stats.pak_path.exists());
    assert!(!stats.blake3_hash.is_empty());

    // Manifest has exactly one entry
    let manifest = engine.list_archives().await.unwrap();
    assert_eq!(manifest.len(), 1);
    assert_eq!(manifest[0].fact_count, 150);

    // Verify integrity
    let verify = engine.verify_archives().await.unwrap();
    assert_eq!(verify.len(), 1);
    assert!(
        verify[0].ok,
        "archive verification failed: {:?}",
        verify[0].error
    );

    // Facts are gone from the live database
    let active = engine.list_active_facts(None).await.unwrap();
    assert!(
        active.is_empty(),
        "expected 0 active facts, got {}",
        active.len()
    );
}

#[tokio::test]
async fn archive_skips_pinned_facts() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open_file_engine(dir.path());
    let embedder = TestEmbedder;

    // Add 1 pinned fact
    let pinned_id = engine
        .add_fact(
            &AddFactRequest {
                content: "pinned fact that must survive".to_string(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            std::sync::Arc::new(embedder) as std::sync::Arc<dyn EmbeddingProvider>,
            None,
        )
        .await
        .unwrap();
    engine.pin_fact(pinned_id).await.unwrap();

    // Add 50 non-pinned facts
    let _non_pinned = add_facts(&engine, 50).await;

    // Forget aggressively
    let forget_policy = ForgetPolicy {
        half_life_days: 0.0001,
        min_importance: 1.0,
        ..ForgetPolicy::default()
    };
    engine.forget(&forget_policy).await.unwrap();

    // Archive
    let archive_policy = ArchivePolicy {
        expired_before: Utc::now() + Duration::hours(1),
        min_facts: 1,
    };
    let stats = engine
        .archive(&archive_policy)
        .await
        .unwrap()
        .expect("should archive non-pinned facts");

    // Only 50 non-pinned facts archived (pinned fact is never expired by forget)
    assert_eq!(stats.facts_archived, 50);

    // Pinned fact still exists
    let pinned = engine.get_fact(pinned_id).await.unwrap();
    assert!(pinned.is_pinned);
}

#[tokio::test]
async fn archive_returns_none_below_min_facts() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open_file_engine(dir.path());

    // Empty engine — no expired facts
    let archive_policy = ArchivePolicy {
        expired_before: Utc::now() + Duration::hours(1),
        min_facts: 100,
    };
    let result = engine.archive(&archive_policy).await.unwrap();
    assert!(result.is_none(), "expected None for empty engine");
}

/// Archive ≥`min` facts into a single `.pak` and return `(engine, pak_path)`.
///
/// Adds `n` facts, forgets them all with an aggressive policy, then archives
/// with a future cutoff so every expired fact qualifies. The returned engine
/// holds exactly one manifest entry; `pak_path` is the absolute on-disk `.pak`.
async fn archive_one_pak(engine: &MemoryEngine, n: usize) -> std::path::PathBuf {
    let _ids = add_facts(engine, n).await;

    let forget_policy = ForgetPolicy {
        half_life_days: 0.0001,
        min_importance: 1.0, // expire everything
        ..ForgetPolicy::default()
    };
    let prune_stats = engine.forget(&forget_policy).await.unwrap();
    assert!(
        prune_stats.facts_expired >= n,
        "expected >={n} expired, got {}",
        prune_stats.facts_expired,
    );

    let archive_policy = ArchivePolicy {
        expired_before: Utc::now() + Duration::hours(1),
        min_facts: 1,
    };
    let stats = engine
        .archive(&archive_policy)
        .await
        .unwrap()
        .expect("should archive facts");
    assert_eq!(stats.facts_archived, n);
    assert!(stats.pak_path.exists(), "archived .pak must exist on disk");

    // Baseline: an intact archive verifies OK. This pins the asymmetry the
    // failure tests rely on — if `verify_archives` returned `ok=false`
    // unconditionally, this assert (not just the negative ones) would fail.
    let verify = engine.verify_archives().await.unwrap();
    assert_eq!(verify.len(), 1);
    assert!(
        verify[0].ok,
        "intact archive must verify OK, got error {:?}",
        verify[0].error
    );

    stats.pak_path
}

/// #422 (file-not-found branch, `engine/archive.rs` `verify_archives`): a
/// manifest row whose `.pak` no longer exists on disk must verify `ok == false`
/// with a "file not found" error — never silently pass. Mutation check: if the
/// `pak_path.exists()` guard were inverted (or the branch dropped), the removed
/// file would fall into the I/O path and the `ok == false` / "file not found"
/// assertions below would fail.
#[tokio::test]
async fn verify_archives_detects_missing_pak() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open_file_engine(dir.path());

    let pak_path = archive_one_pak(&engine, 150).await;

    // Remove the `.pak` out from under the manifest row.
    std::fs::remove_file(&pak_path).unwrap();
    assert!(!pak_path.exists(), "precondition: .pak was removed");

    let verify = engine.verify_archives().await.unwrap();
    assert_eq!(verify.len(), 1, "manifest still has the one entry");
    assert!(
        !verify[0].ok,
        "a missing .pak must verify as NOT ok, got ok=true"
    );
    let err = verify[0]
        .error
        .as_deref()
        .expect("a failed verify must carry an error message");
    assert!(
        err.contains("file not found"),
        "missing .pak must report 'file not found', got: {err:?}"
    );
}

/// #422 (hash-mismatch branch, `engine/archive.rs` `verify_single_archive`): a
/// `.pak` whose bytes were tampered with after archival must verify
/// `ok == false` with a "hash mismatch" error. Flips a single byte in place so
/// the file size is unchanged — this proves the integrity check is a content
/// hash, not a length/existence check. Mutation check: if `verify_single_archive`
/// ignored `verify_pak`'s `Ok(false)` arm (or compared lengths instead of
/// hashes), the same-length corrupted file would pass and these asserts fail.
#[tokio::test]
async fn verify_archives_detects_tampered_pak() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open_file_engine(dir.path());

    let pak_path = archive_one_pak(&engine, 150).await;

    // Corrupt one byte in place (size preserved → only the content hash moves).
    let mut bytes = std::fs::read(&pak_path).unwrap();
    assert!(!bytes.is_empty(), "precondition: .pak is non-empty");
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF;
    std::fs::write(&pak_path, &bytes).unwrap();
    assert_eq!(
        std::fs::metadata(&pak_path).unwrap().len(),
        bytes.len() as u64,
        "tamper must preserve file length (proves hash, not length, is checked)"
    );

    let verify = engine.verify_archives().await.unwrap();
    assert_eq!(verify.len(), 1, "manifest still has the one entry");
    assert!(
        !verify[0].ok,
        "a tampered .pak must verify as NOT ok, got ok=true"
    );
    let err = verify[0]
        .error
        .as_deref()
        .expect("a failed verify must carry an error message");
    assert!(
        err.contains("hash mismatch"),
        "tampered .pak must report 'hash mismatch', got: {err:?}"
    );
}

/// #422 / #292 (path-traversal guard, `engine/archive.rs` `verify_archives`):
/// a manifest `pak_path` that escapes the archive directory via `..` must be
/// rejected with "path traversal detected" BEFORE any filesystem access — the
/// guard must fire ahead of the file-not-found branch. The legitimate write path
/// only ever stores a separator-free `archive-<ts>.pak`, so a traversal path can
/// only arrive from a tampered/restored DB; here we plant exactly such a row by
/// writing the `SQLite` manifest table directly (the integration-tier analogue of
/// the inline unit test's `raw_exec` injection, which is `pub(crate)`-only).
///
/// Mutation check: were the guard removed, `../outside/escape.pak` would resolve
/// outside the archive dir, miss on disk, and surface "file not found" instead —
/// so the asymmetric `error == "path traversal detected"` assert (distinct from
/// the missing-file string) catches a dropped or reordered guard.
#[tokio::test]
async fn verify_archives_rejects_path_traversal_manifest_row() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");

    // Build the engine once so migrations create the schema (incl.
    // `archive_manifest`), then drop it to release the file lock before we open
    // a second connection to plant the malicious row.
    {
        let engine = open_file_engine(dir.path());
        let _ids = add_facts(&engine, 1).await;
    }

    // Plant a `..` traversal manifest row directly via SQLite — the public API
    // never produces such a path, and the crate-internal `raw_exec`/`storage()`
    // seam is not reachable from an external integration test.
    let evil_path = "../outside/escape.pak";
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO archive_manifest \
             (pak_path, created_at, fact_count, edge_count, fact_id_min, \
              fact_id_max, t_created_min, t_created_max, size_bytes, blake3_hash) \
             VALUES (?1, '2026-01-01T00:00:00Z', 0, 0, 0, 0, \
              '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 0, 'deadbeef')",
            [evil_path],
        )
        .unwrap();
    }

    // Reopen the engine on the same DB and verify.
    let engine = MemoryEngine::builder(DIM).path(&db_path).build().unwrap();

    let verify = engine.verify_archives().await.unwrap();
    assert_eq!(verify.len(), 1, "exactly one (malicious) manifest entry");
    assert!(
        !verify[0].ok,
        "a `..` traversal manifest path must be flagged, not verified"
    );
    assert_eq!(
        verify[0].error.as_deref(),
        Some("path traversal detected"),
        "the traversal must be caught by the containment guard, not fall \
         through to the I/O (file-not-found) path"
    );
    assert_eq!(verify[0].pak_path, evil_path);
}

#[tokio::test]
async fn archive_search_finds_archived_facts() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open_file_engine(dir.path());
    let embedder: std::sync::Arc<dyn EmbeddingProvider> = std::sync::Arc::new(TestEmbedder);

    // Add 20 facts about deployment issues
    for i in 0..20 {
        engine
            .add_fact(
                &AddFactRequest {
                    content: format!("deployment issue on server {i}"),
                    fact_type: FactType::Episodic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                embedder.clone(),
                None,
            )
            .await
            .unwrap();
    }

    // Expire all facts aggressively
    let forget_policy = ForgetPolicy {
        half_life_days: 0.0001,
        min_importance: 1.0, // expire everything
        ..ForgetPolicy::default()
    };
    let prune_stats = engine.forget(&forget_policy).await.unwrap();
    assert!(
        prune_stats.facts_expired >= 20,
        "expected >=20 expired facts, got {}",
        prune_stats.facts_expired
    );

    // Archive all expired facts
    let archive_policy = ArchivePolicy {
        expired_before: Utc::now() + Duration::hours(1),
        min_facts: 1,
    };
    let stats = engine
        .archive(&archive_policy)
        .await
        .unwrap()
        .expect("should produce archive stats");
    assert_eq!(stats.facts_archived, 20);

    // Normal search finds nothing (facts are gone from live DB)
    let query = MemoryQuery::new().text("deployment");
    let response = engine.execute_query(&query).await.unwrap();
    assert_eq!(
        response.results.len(),
        0,
        "normal search should find nothing after archive"
    );

    // Archive search finds them
    let query = MemoryQuery::new().text("deployment").include_archives();
    let response = engine.execute_query(&query).await.unwrap();
    assert!(
        !response.results.is_empty(),
        "archive search should find archived facts"
    );
    assert!(
        response.diagnostics.archive_paks_scanned > 0,
        "expected archive_paks_scanned > 0"
    );
    // All returned results should have MatchType::Archive
    for r in &response.results {
        assert_eq!(
            r.match_type,
            memory_engine::MatchType::Archive,
            "expected MatchType::Archive for all results"
        );
    }
}
