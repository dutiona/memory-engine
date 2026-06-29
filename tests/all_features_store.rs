//! Cross-feature integration test: `ann` + `archive` + `compress-zstd` driven
//! through a single end-to-end store flow.
//!
//! Issue #258 / #489 — the existing integration suite exercises each feature in
//! isolation (`ann_recall_test.rs`, `archive_test.rs`, the per-format roundtrips
//! in `restore_roundtrip_test.rs`), but nothing drives the **combination** that a
//! `cargo test --all-features` build actually links. In particular the `ann` half
//! of #489 was never covered at integration level: the HNSW index's behaviour
//! *across* an ingest→expire→archive cycle (tombstoning + the `t_expired IS NULL`
//! post-filter, the rebuild-from-DB path) was only unit-tested inside
//! `src/search/ann.rs`, never through the [`MemoryEngine`] facade.
//!
//! This file gates on all three features at once and walks one flow:
//!   ingest → HNSW vector search → forget (tombstone) → HNSW must drop expired
//!   → archive expired facts → archive search finds them, live search does not
//!   → zstd snapshot of survivors → restore → HNSW rebuilt on the restored engine.
//!
//! Every assertion is asymmetric (distinct survivor vs expired topic ids, a
//! one-hot-per-topic embedder so the nearest neighbour is *the* fact for that
//! topic) so a predicate flip or a tuple swap fails the test rather than passing
//! vacuously.

#![cfg(all(feature = "ann", feature = "archive", feature = "compress-zstd"))]
#![allow(clippy::unwrap_used)] // test/bench code: panic-on-unwrap is the intended failure signal (#725)
#![allow(clippy::cast_precision_loss)] // small loop counts; lossless in practice

use std::sync::Arc;

use chrono::{Duration, Utc};
use memory_engine::engine::MemoryEngine;
use memory_engine::error::Result;
use memory_engine::inspect_types::DumpFormat;
use memory_engine::search::SearchConfig;
use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::{AddFactRequest, FactType};
use memory_engine::{
    ArchivePolicy, EmbeddingFingerprint, EngineConfig, ForgetPolicy, MatchType, MemoryQuery,
    SearchMode, SearchQuery,
};

/// Embedding width — one slot per topic plus headroom, so each topic maps to a
/// distinct, well-separated unit vector. Sharp separation makes "the nearest
/// neighbour to topic K's query *is* topic K's fact" a hard assertion.
const DIM: usize = 16;
/// Number of topics / facts ingested. Each topic gets exactly one fact.
const TOPICS: usize = 12;

/// Deterministic, vector-separable embedder.
///
/// Text of the form `"topic <k> ..."` embeds to the `k`-th basis vector (one-hot,
/// unit norm). Distinct topics are therefore orthogonal, so cosine ranking is
/// unambiguous: the nearest neighbour to a `topic <k>` query is exactly the
/// fact whose content names topic `k`. Unparseable text embeds to a fixed
/// off-axis vector so it never collides with a real topic.
struct OneHotEmbedder;

impl OneHotEmbedder {
    /// Parse the topic index out of `"topic <k> ..."`, if present.
    fn topic_of(text: &str) -> Option<usize> {
        let rest = text.strip_prefix("topic ")?;
        let token = rest.split_whitespace().next()?;
        token.parse::<usize>().ok().filter(|k| *k < DIM)
    }
}

impl EmbeddingProvider for OneHotEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let mut v = vec![0.0_f32; DIM];
        match Self::topic_of(text) {
            Some(k) => v[k] = 1.0,
            // Off-axis fallback in the last slot: never equal to any topic vector.
            None => v[DIM - 1] = 1.0,
        }
        Ok(v)
    }
    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint::new("onehot", "test", DIM)
    }
}

/// Build a file-backed engine that always routes vector search through HNSW
/// (`ann_threshold = 0`), so the `ann` code path is exercised, not brute force.
fn open_hnsw_engine(db_path: std::path::PathBuf) -> MemoryEngine {
    MemoryEngine::builder(DIM)
        .path(db_path)
        .search_config(SearchConfig { ann_threshold: 0 })
        .build()
        .unwrap()
}

/// The single nearest-by-vector fact's content for a `topic <k>` query, or
/// `None` if HNSW surfaced nothing.
async fn nearest_content(
    engine: &MemoryEngine,
    embedder: &OneHotEmbedder,
    topic: usize,
) -> Option<String> {
    let emb = embedder.embed(&format!("topic {topic}")).unwrap();
    let q = SearchQuery::new(SearchMode::Vector, 5).embedding(emb);
    let results = engine.query(&q).await.unwrap();
    results.into_iter().next().map(|r| r.fact.content)
}

/// End-to-end flow across `ann` + `archive` + `compress-zstd`.
///
/// The split between survivors and expired facts is made deterministic by
/// **pinning** the even-topic facts (pinned facts are immune to `forget`) and
/// then forgetting aggressively — so exactly the odd-topic facts expire. That
/// asymmetry (even survive, odd expire) is what makes every later assertion
/// non-vacuous.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one linear end-to-end flow; splitting it would obscure the narrative
async fn ann_archive_compress_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open_hnsw_engine(dir.path().join("hot.db"));
    let embedder = OneHotEmbedder;
    let embedder_arc: Arc<dyn EmbeddingProvider> = Arc::new(OneHotEmbedder);

    // --- Ingest: one fact per topic, each with distinct vector + distinct text.
    let mut fact_ids = Vec::with_capacity(TOPICS);
    for topic in 0..TOPICS {
        let id = engine
            .add_fact(
                &AddFactRequest {
                    content: format!("topic {topic} memory fact"),
                    // Episodic facts are *not* decay-exempt (unlike Semantic, the
                    // knowledge-shaped default), so an aggressive `forget` can
                    // actually expire the unpinned ones — which is the whole point
                    // of this flow.
                    fact_type: FactType::Episodic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                embedder_arc.clone(),
                None,
            )
            .await
            .unwrap();
        fact_ids.push(id);
    }

    // Pin even topics → they survive forget. Odd topics will expire.
    let survivor_topics: Vec<usize> = (0..TOPICS).filter(|t| t % 2 == 0).collect();
    let expired_topics: Vec<usize> = (0..TOPICS).filter(|t| t % 2 == 1).collect();
    assert_eq!(survivor_topics.len(), 6);
    assert_eq!(expired_topics.len(), 6);
    for &t in &survivor_topics {
        engine.pin_fact(fact_ids[t]).await.unwrap();
    }

    // --- HNSW pre-expire: every topic's query finds *its own* fact (sharp,
    // one-hot separation). This both proves the index is live and pins the
    // expected mapping so the post-expire assertions are meaningful.
    for topic in 0..TOPICS {
        let got = nearest_content(&engine, &embedder, topic).await;
        assert_eq!(
            got.as_deref(),
            Some(format!("topic {topic} memory fact").as_str()),
            "pre-expire HNSW: topic {topic} must be its own nearest neighbour"
        );
    }

    // --- Forget aggressively: expire everything unpinned (the odd topics).
    // This drives the backend's expire_fact → HNSW notify_expire (tombstone)
    // path for each odd-topic fact (#713), the cross-feature interaction #489
    // calls out.
    let prune = engine
        .forget(&ForgetPolicy {
            half_life_days: 0.0001,
            min_importance: 1.0, // expire everything not pinned
            ..ForgetPolicy::default()
        })
        .await
        .unwrap();
    assert_eq!(
        prune.facts_expired,
        expired_topics.len(),
        "exactly the {} unpinned facts must expire (pinned facts are immune)",
        expired_topics.len()
    );

    // --- HNSW post-expire: tombstoned (odd) topics must NOT surface; survivor
    // (even) topics must still surface as their own nearest neighbour. A leak of
    // an expired fact (tombstone or t_expired post-filter regression) fails the
    // odd-topic branch; a wrongly-dropped survivor fails the even-topic branch.
    for &topic in &survivor_topics {
        let got = nearest_content(&engine, &embedder, topic).await;
        assert_eq!(
            got.as_deref(),
            Some(format!("topic {topic} memory fact").as_str()),
            "post-expire HNSW: survivor topic {topic} must still be found"
        );
    }
    for &topic in &expired_topics {
        let got = nearest_content(&engine, &embedder, topic).await;
        assert_ne!(
            got.as_deref(),
            Some(format!("topic {topic} memory fact").as_str()),
            "post-expire HNSW: expired topic {topic} must NOT surface (tombstone + t_expired filter)"
        );
    }

    // Live-fact accounting: exactly the survivors remain active.
    let active = engine.list_active_facts(None).await.unwrap();
    assert_eq!(
        active.len(),
        survivor_topics.len(),
        "only survivors stay live"
    );
    for f in &active {
        let topic = OneHotEmbedder::topic_of(&f.content).unwrap();
        assert!(
            topic.is_multiple_of(2),
            "active fact for odd topic {topic} leaked past forget"
        );
        assert!(f.is_pinned, "every surviving fact is pinned");
    }

    // --- Archive the expired (odd-topic) facts into a cold .pak.
    let stats = engine
        .archive(&ArchivePolicy {
            expired_before: Utc::now() + Duration::hours(1),
            min_facts: 1,
        })
        .await
        .unwrap()
        .expect("expired facts should produce an archive");
    assert_eq!(
        stats.facts_archived,
        expired_topics.len(),
        "archive must capture exactly the expired facts"
    );
    assert!(stats.pak_path.exists(), "the .pak file must exist on disk");
    assert!(stats.pak_size_bytes > 0, "the .pak must be non-empty");
    assert!(
        !stats.blake3_hash.is_empty(),
        "archive must carry an integrity hash"
    );

    // Manifest + integrity verification (archive feature).
    let manifest = engine.list_archives().await.unwrap();
    assert_eq!(manifest.len(), 1, "exactly one archive segment");
    assert_eq!(
        manifest[0].fact_count,
        i64::try_from(expired_topics.len()).unwrap()
    );
    let verify = engine.verify_archives().await.unwrap();
    assert_eq!(verify.len(), 1);
    assert!(
        verify[0].ok,
        "archive integrity check: {:?}",
        verify[0].error
    );

    // --- Archive search vs live search, for an expired topic. The live store no
    // longer has it; the archive does. Asymmetric: same query, opposite outcome.
    let expired_probe = expired_topics[0];
    let probe_text = format!("topic {expired_probe}");

    let live_resp = engine
        .execute_query(&MemoryQuery::new().text(probe_text.clone()))
        .await
        .unwrap();
    assert!(
        live_resp.results.is_empty(),
        "live search must not find archived topic {expired_probe}"
    );

    let arch_resp = engine
        .execute_query(
            &MemoryQuery::new()
                .text(probe_text.clone())
                .include_archives(),
        )
        .await
        .unwrap();
    assert!(
        !arch_resp.results.is_empty(),
        "archive search must find archived topic {expired_probe}"
    );
    assert!(
        arch_resp.diagnostics.archive_paks_scanned > 0,
        "archive search must have scanned at least one .pak"
    );
    for r in &arch_resp.results {
        assert_eq!(
            r.match_type,
            MatchType::Archive,
            "archive-search results must be tagged MatchType::Archive"
        );
    }

    // --- Snapshot roundtrip of the survivors via a zstd-compressed dump
    // (compress-zstd feature), restored into a fresh engine whose HNSW index is
    // **rebuilt from the restored rows** (ann feature). This is the compress × ann
    // interaction that no single-feature test covers.
    let snap_path = dir.path().join("survivors.json.zst");
    engine
        .dump_state(&DumpFormat::JsonZstd(snap_path.clone()))
        .await
        .unwrap();

    // Restore through `restore_json` carrying `SearchConfig { ann_threshold: 0 }`,
    // *not* `restore_json_memory` — the latter calls `init_from_pool(.., None, ..)`,
    // which drops the ANN config so `with_open_config` never materializes HNSW and
    // every "restored HNSW" query silently falls through to brute force (codex P2:
    // the assertions would pass even if restore-time HNSW rebuild were broken).
    //
    // With `ann_threshold: 0` and no sidecar at the fresh restore path,
    // `with_open_config` takes `HnswOpenSource::Rebuild` → `build_from_db`, so the
    // index is rebuilt from the restored facts and `should_use_hnsw` is true
    // (active_count >= 0). Vector search is therefore HNSW-served, making the
    // assertions below bite on the rebuild: if the rebuild produced an empty/broken
    // index, the survivor probe returns nothing (HNSW present-but-empty still wins
    // the dispatch at threshold 0) and the `assert_eq!` fails — a brute-force
    // fallback can no longer mask a regressed restore-time rebuild.
    let restore_db = dir.path().join("restored.db");
    let restored = MemoryEngine::restore_json(
        &snap_path,
        &EngineConfig::new(restore_db, DIM).with_search_config(SearchConfig { ann_threshold: 0 }),
    )
    .unwrap();
    let restored_stats = restored.statistics().await.unwrap();
    assert_eq!(
        restored_stats.facts.total,
        i64::try_from(survivor_topics.len()).unwrap(),
        "restored snapshot must hold exactly the surviving facts"
    );

    // Every survivor topic resolves to its own fact via the HNSW-served vector path
    // (threshold 0). A regressed restore-time rebuild — empty index, or HNSW never
    // materialized and then *also* a broken brute-force fallback — fails here.
    for &survivor in &survivor_topics {
        let restored_hit = nearest_content(&restored, &embedder, survivor).await;
        assert_eq!(
            restored_hit.as_deref(),
            Some(format!("topic {survivor} memory fact").as_str()),
            "restored HNSW must resolve survivor topic {survivor}"
        );
    }
    // The expired (odd) topics were not snapshotted, so the rebuilt index must not
    // surface them.
    let restored_miss = nearest_content(&restored, &embedder, expired_probe).await;
    assert_ne!(
        restored_miss.as_deref(),
        Some(format!("topic {expired_probe} memory fact").as_str()),
        "restored snapshot must not contain expired topic {expired_probe}"
    );
}
