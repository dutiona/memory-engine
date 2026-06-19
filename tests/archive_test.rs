#![cfg(feature = "archive")]

use chrono::{Duration, Utc};
use memory_engine::ArchivePolicy;
use memory_engine::EmbeddingFingerprint;
use memory_engine::MemoryQuery;
use memory_engine::engine::MemoryEngine;
use memory_engine::error::Result;
use memory_engine::traits::{EmbeddingProvider, ForgetPolicy};
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
fn add_facts(engine: &MemoryEngine, n: usize) -> Vec<i64> {
    let embedder = TestEmbedder;
    (0..n)
        .map(|i| {
            engine
                .add_fact(
                    &AddFactRequest {
                        content: format!("archival test fact number {i}"),
                        fact_type: FactType::Episodic,
                        source_event_id: None,
                        scope: None,
                        opts: None,
                    },
                    &embedder,
                    None,
                )
                .unwrap()
        })
        .collect()
}

#[test]
fn archive_lifecycle_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open_file_engine(dir.path());

    // Add 150 facts
    let _ids = add_facts(&engine, 150);

    // Forget them all — set a very aggressive policy
    let forget_policy = ForgetPolicy {
        half_life_days: 0.0001,
        min_importance: 1.0, // expire everything
        ..ForgetPolicy::default()
    };
    let prune_stats = engine.forget(&forget_policy).unwrap();
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
        .unwrap()
        .expect("should archive facts");

    assert_eq!(stats.facts_archived, 150);
    assert!(stats.pak_size_bytes > 0);
    assert!(stats.pak_path.exists());
    assert!(!stats.blake3_hash.is_empty());

    // Manifest has exactly one entry
    let manifest = engine.list_archives().unwrap();
    assert_eq!(manifest.len(), 1);
    assert_eq!(manifest[0].fact_count, 150);

    // Verify integrity
    let verify = engine.verify_archives().unwrap();
    assert_eq!(verify.len(), 1);
    assert!(
        verify[0].ok,
        "archive verification failed: {:?}",
        verify[0].error
    );

    // Facts are gone from the live database
    let active = engine.list_active_facts(None).unwrap();
    assert!(
        active.is_empty(),
        "expected 0 active facts, got {}",
        active.len()
    );
}

#[test]
fn archive_skips_pinned_facts() {
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
            &embedder,
            None,
        )
        .unwrap();
    engine.pin_fact(pinned_id).unwrap();

    // Add 50 non-pinned facts
    let _non_pinned = add_facts(&engine, 50);

    // Forget aggressively
    let forget_policy = ForgetPolicy {
        half_life_days: 0.0001,
        min_importance: 1.0,
        ..ForgetPolicy::default()
    };
    engine.forget(&forget_policy).unwrap();

    // Archive
    let archive_policy = ArchivePolicy {
        expired_before: Utc::now() + Duration::hours(1),
        min_facts: 1,
    };
    let stats = engine
        .archive(&archive_policy)
        .unwrap()
        .expect("should archive non-pinned facts");

    // Only 50 non-pinned facts archived (pinned fact is never expired by forget)
    assert_eq!(stats.facts_archived, 50);

    // Pinned fact still exists
    let pinned = engine.get_fact(pinned_id).unwrap();
    assert!(pinned.is_pinned);
}

#[test]
fn archive_returns_none_below_min_facts() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open_file_engine(dir.path());

    // Empty engine — no expired facts
    let archive_policy = ArchivePolicy {
        expired_before: Utc::now() + Duration::hours(1),
        min_facts: 100,
    };
    let result = engine.archive(&archive_policy).unwrap();
    assert!(result.is_none(), "expected None for empty engine");
}

#[test]
fn archive_search_finds_archived_facts() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open_file_engine(dir.path());
    let embedder = TestEmbedder;

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
                &embedder,
                None,
            )
            .unwrap();
    }

    // Expire all facts aggressively
    let forget_policy = ForgetPolicy {
        half_life_days: 0.0001,
        min_importance: 1.0, // expire everything
        ..ForgetPolicy::default()
    };
    let prune_stats = engine.forget(&forget_policy).unwrap();
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
        .unwrap()
        .expect("should produce archive stats");
    assert_eq!(stats.facts_archived, 20);

    // Normal search finds nothing (facts are gone from live DB)
    let query = MemoryQuery::new().text("deployment");
    let response = engine.execute_query(&query).unwrap();
    assert_eq!(
        response.results.len(),
        0,
        "normal search should find nothing after archive"
    );

    // Archive search finds them
    let query = MemoryQuery::new().text("deployment").include_archives();
    let response = engine.execute_query(&query).unwrap();
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
            memory_engine::search::hybrid::MatchType::Archive,
            "expected MatchType::Archive for all results"
        );
    }
}
