//! Background reconstruction (#623): re-embed stored fact **content** (the
//! lossless source of truth) under a new same-dimension embedding identity in the
//! background, staging the vectors in a `populating` space's `fact_vectors`, then
//! atomically promoting them as the active serving vectors.
//!
//! This module owns the **engine-side orchestration** ([`MemoryEngine::reconstruct`]):
//! it drives the embedding — off the write lock, under
//! [`spawn_blocking`](tokio::task::spawn_blocking), so a slow or `reqwest::blocking`
//! provider neither parks the runtime nor panics with a nested-runtime error (the
//! #631 pattern) — and calls the pure-DB
//! [`SchemaManager`](crate::storage::schema::SchemaManager) reconstruction port
//! methods. The embedder never crosses the storage port; the backend does no
//! network/LLM work.
//!
//! The backfill is **resumable, crash-safe, and idempotent** with no persisted
//! cursor: the port's anti-join treats an absent `fact_vectors` row as the work
//! signal and `ON CONFLICT DO NOTHING` makes a replayed window a no-op. A crash
//! mid-reconstruction loses at most one in-flight batch (re-derived on restart),
//! and re-running [`reconstruct`](MemoryEngine::reconstruct) resumes the same
//! `populating` space (`begin_populating_space` is idempotent).
//!
//! **Scope: same-dim only.** A different-dim transition needs the engine
//! effective-`embed_dim` rebuild (pool / dim-validation / HNSW at the new dim) and
//! is the #742 follow-up; the storage layer here is already dim-agnostic and
//! [`PromoteOutcome::new_fingerprint`] carries the new dim for it.

use std::sync::Arc;

use super::spawn_join_err;
use crate::engine::MemoryEngine;
use crate::error::{MemoryError, Result};
use crate::traits::EmbeddingProvider;
use crate::types::{EmbeddingFingerprint, PromoteOutcome};

/// Facts re-embedded per backfill window (one provider round-trip + one write tx).
/// Bounds peak memory (window content + vectors) and the crash-loss granularity.
const DEFAULT_BACKFILL_BATCH: usize = 128;

/// Derive a stable, unique `populating`/active space name from an embedding
/// identity: a sanitized `model_provider_dim` slug. Distinct identities map to
/// distinct names; the same identity maps to the same name (so a crash-resumed
/// reconstruction reopens its space). Non-alphanumeric characters in `model` /
/// `provider` (e.g. the `/` and `.` in `"Qwen/Qwen3-Embedding-0.6B"`) collapse to
/// `_`, keeping the name a clean SQL identifier.
fn space_name_for(fp: &EmbeddingFingerprint) -> String {
    let mut slug = String::with_capacity(fp.model.len() + fp.provider.len() + 8);
    for ch in fp
        .model
        .chars()
        .chain(['_'])
        .chain(fp.provider.chars())
        .chain(['_'])
    {
        slug.push(if ch.is_ascii_alphanumeric() { ch } else { '_' });
    }
    slug.push_str(&fp.dim.to_string());
    slug
}

impl MemoryEngine {
    /// Reconstruct the active embedding space under a new **same-dimension**
    /// identity, with no downtime: open a shadow space, backfill it in the
    /// background, then atomically promote it (the served vectors swap in one
    /// transaction; the old vectors are retained for an instant rollback).
    ///
    /// Sequence: open (or resume) the `populating` space → backfill → a second
    /// **catch-up** pass (facts ingested during the first pass — live
    /// reconstruction) → atomic [`promote_space`](crate::storage::schema::SchemaManager::promote_space),
    /// whose completeness gate runs *inside* the transaction so a straggler that
    /// lands after the catch-up is caught (TOCTOU-safe), never silently dropped.
    /// Returns the [`PromoteOutcome`], with `stragglers_caught` set from the
    /// catch-up pass.
    ///
    /// `new_fingerprint.dim` must equal the engine's [`embed_dim`](Self::embed_dim)
    /// (same-dim only — see the module docs; different-dim is #742). The atomic
    /// promote re-checks the dim against the active space.
    ///
    /// After the promote, [`PromoteOutcome::rebuild_index`] is `true`: the active
    /// vectors changed, so a `SQLite`+HNSW backend's in-memory index is stale until
    /// the next open (which rebuilds it via `build_from_db`). The live in-process
    /// rebuild hook is **#624**; until then a same-process `ann` query between
    /// promote and reopen may use the stale index. The brute-force vector path
    /// (default features) reads `facts.embedding` directly and is correct
    /// immediately.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::EmbeddingDimension`] if `new_fingerprint.dim` differs
    /// from the engine dimension (different-dim → #742), [`MemoryError::ReadOnly`]
    /// from the write port on a read-only engine, or any embedding-provider /
    /// storage failure surfaced by the backfill or promote.
    pub async fn reconstruct(
        &self,
        new_fingerprint: &EmbeddingFingerprint,
        embedder: &Arc<dyn EmbeddingProvider>,
    ) -> Result<PromoteOutcome> {
        // Fail fast on a different-dim request (the promote also guards, but only
        // after a full backfill — reject before that wasted work).
        if new_fingerprint.dim != self.embed_dim {
            return Err(MemoryError::EmbeddingDimension {
                expected: self.embed_dim,
                actual: new_fingerprint.dim,
            });
        }

        let space = space_name_for(new_fingerprint);

        // 1. Open (or resume) the shadow space.
        self.storage
            .begin_populating_space(&space, new_fingerprint)
            .await?;

        // 2. Backfill every fact's content under the new identity.
        self.backfill_space(&space, embedder, DEFAULT_BACKFILL_BATCH)
            .await?;

        // 3. Catch-up: re-embed facts ingested during the backfill (live
        //    reconstruction). The count is the stragglers caught before promote.
        let stragglers = self
            .backfill_space(&space, embedder, DEFAULT_BACKFILL_BATCH)
            .await?;

        // 4. Atomic promote (the completeness gate re-checks INSIDE the tx).
        let mut outcome = self.storage.promote_space(&space).await?;
        outcome.stragglers_caught = stragglers;

        Ok(outcome)
    }

    /// Backfill the `populating` space `space_name`: re-embed every fact's content
    /// with `embedder` and stage the vectors in `fact_vectors[space_name]`.
    ///
    /// Backfill the `populating` space `space_name`: re-embed every fact's content
    /// with `embedder` and stage the vectors in `fact_vectors[space_name]`.
    ///
    /// Loops `next_backfill_window` → embed (off-lock) → `write_backfill_batch`
    /// until the space is fully backfilled. Returns the total number of vectors
    /// **actually written** this call (0 on a fully-backfilled space — the
    /// idempotent/crash-resume case). The space must already exist (open it with
    /// `begin_populating_space`); this method only fills it.
    ///
    /// `after_id` advances past each window to skip the already-written prefix —
    /// an intra-run optimization only; correctness rests on the port's cursorless
    /// anti-join (`facts.id` is monotonic, so a concurrently inserted fact always
    /// has a higher id and is never skipped).
    ///
    /// # Errors
    ///
    /// Propagates embedding-provider failures, a join error from the blocking
    /// embed task, an [`EmbeddingProvider::embed_batch`] contract violation
    /// (length mismatch), or any storage-port failure.
    pub(crate) async fn backfill_space(
        &self,
        space_name: &str,
        embedder: &Arc<dyn EmbeddingProvider>,
        batch_size: usize,
    ) -> Result<usize> {
        let mut after_id = 0_i64;
        let mut total = 0_usize;

        loop {
            let window = self
                .storage
                .next_backfill_window(space_name, after_id, batch_size)
                .await?;
            let Some(&(last_id, _)) = window.last() else {
                break; // fully backfilled
            };
            after_id = last_id;

            // Embed off the async executor: the provider call may be a blocking
            // HTTP round-trip, and a `reqwest::blocking` provider would panic if
            // run on a runtime thread (the #631 add_fact pattern).
            let provider = Arc::clone(embedder);
            let texts: Vec<String> = window.iter().map(|(_, content)| content.clone()).collect();
            let embeddings = tokio::task::spawn_blocking(move || {
                let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                provider.embed_batch(&refs)
            })
            .await
            .map_err(spawn_join_err)??;

            if embeddings.len() != window.len() {
                return Err(MemoryError::Internal(format!(
                    "embed_batch returned {} vectors for {} texts (provider contract violation)",
                    embeddings.len(),
                    window.len()
                )));
            }

            let rows: Vec<(i64, Vec<f32>)> =
                window.iter().map(|(id, _)| *id).zip(embeddings).collect();
            total += self.storage.write_backfill_batch(space_name, rows).await?;
        }

        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::space_name_for;
    use crate::engine::MemoryEngine;
    use crate::error::{MemoryError, Result};
    use crate::traits::EmbeddingProvider;
    use crate::types::{AddFactRequest, EmbeddingFingerprint, FactType, NewFact};

    const DIM: usize = 4;
    const SPACE: &str = "shadow";

    /// A constant-vector embedder — the backfill *mechanism* (windowing,
    /// resumability, idempotency) is what these tests exercise; vector content is
    /// irrelevant here (T4 uses a distinguishable embedder for the promote swap).
    struct ConstEmbedder {
        dim: usize,
    }

    impl EmbeddingProvider for ConstEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![0.5_f32; self.dim])
        }

        fn fingerprint(&self) -> EmbeddingFingerprint {
            EmbeddingFingerprint::new("recon-test", "test", self.dim)
        }
    }

    fn embedder() -> Arc<dyn EmbeddingProvider> {
        Arc::new(ConstEmbedder { dim: DIM })
    }

    async fn seed(engine: &MemoryEngine, contents: &[&str]) {
        for content in contents {
            let fact = NewFact::builder(*content, vec![0.0_f32; DIM], FactType::Semantic).build();
            engine
                .storage()
                .insert_fact(&fact)
                .await
                .expect("seed fact");
        }
    }

    async fn begin(engine: &MemoryEngine) {
        engine
            .storage()
            .begin_populating_space(SPACE, &EmbeddingFingerprint::new("model-v2", "test", DIM))
            .await
            .expect("begin populating space");
    }

    #[tokio::test]
    async fn backfill_writes_one_vector_per_fact() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        seed(&engine, &["cats", "dogs", "birds", "fish", "owls"]).await;
        begin(&engine).await;

        // A small batch forces several windows — exercises the loop's advance.
        let written = engine.backfill_space(SPACE, &embedder(), 2).await.unwrap();
        assert_eq!(written, 5, "one vector per fact across multiple windows");
        assert_eq!(engine.storage().count_unbackfilled(SPACE).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn backfill_is_idempotent_on_replay() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        seed(&engine, &["a", "b", "c"]).await;
        begin(&engine).await;

        assert_eq!(
            engine.backfill_space(SPACE, &embedder(), 8).await.unwrap(),
            3
        );
        // Re-running over a fully-backfilled space writes nothing.
        assert_eq!(
            engine.backfill_space(SPACE, &embedder(), 8).await.unwrap(),
            0
        );
        assert_eq!(engine.storage().count_unbackfilled(SPACE).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn backfill_resumes_after_partial_progress_without_duplicates() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        seed(&engine, &["a", "b", "c", "d"]).await;
        begin(&engine).await;

        // Simulate a crash mid-backfill: only the first 2 facts got vectors.
        let partial = engine
            .storage()
            .next_backfill_window(SPACE, 0, 2)
            .await
            .unwrap();
        assert_eq!(partial.len(), 2);
        let rows: Vec<(i64, Vec<f32>)> = partial
            .iter()
            .map(|(id, _)| (*id, vec![0.5_f32; DIM]))
            .collect();
        assert_eq!(
            engine
                .storage()
                .write_backfill_batch(SPACE, rows)
                .await
                .unwrap(),
            2
        );
        assert_eq!(engine.storage().count_unbackfilled(SPACE).await.unwrap(), 2);

        // Restart: the loop re-derives only the remaining 2 via the anti-join.
        let written = engine.backfill_space(SPACE, &embedder(), 8).await.unwrap();
        assert_eq!(written, 2, "only the un-backfilled remainder is written");
        assert_eq!(engine.storage().count_unbackfilled(SPACE).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn backfill_picks_up_fact_inserted_mid_reconstruction() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        seed(&engine, &["a", "b"]).await;
        begin(&engine).await;
        assert_eq!(
            engine.backfill_space(SPACE, &embedder(), 8).await.unwrap(),
            2
        );

        // A fact lands after the first pass completed — a second pass catches it
        // (no persisted cursor; the absent fact_vectors row is the work signal).
        seed(&engine, &["late-arrival"]).await;
        assert_eq!(engine.storage().count_unbackfilled(SPACE).await.unwrap(), 1);
        assert_eq!(
            engine.backfill_space(SPACE, &embedder(), 8).await.unwrap(),
            1
        );
        assert_eq!(engine.storage().count_unbackfilled(SPACE).await.unwrap(), 0);
    }

    // --- reconstruct() end-to-end (#623 T4) ---

    /// A distinguishable embedder: every text maps to the constant `[value; dim]`,
    /// and a unique `model` slug — so old vs new vectors AND identities are
    /// observably different across a reconstruction.
    struct TagEmbedder {
        model: &'static str,
        value: f32,
        dim: usize,
    }

    impl EmbeddingProvider for TagEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![self.value; self.dim])
        }

        fn fingerprint(&self) -> EmbeddingFingerprint {
            EmbeddingFingerprint::new(self.model, "test", self.dim)
        }
    }

    fn req(content: &str) -> AddFactRequest {
        AddFactRequest {
            content: content.into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        }
    }

    #[tokio::test]
    async fn reconstruct_full_cycle_swaps_same_dim_model() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let old: Arc<dyn EmbeddingProvider> = Arc::new(TagEmbedder {
            model: "old-model",
            value: 0.1,
            dim: DIM,
        });
        let new: Arc<dyn EmbeddingProvider> = Arc::new(TagEmbedder {
            model: "new-model",
            value: 0.9,
            dim: DIM,
        });
        let new_fp = EmbeddingFingerprint::new("new-model", "test", DIM);

        // Ingest under the OLD identity (records it as active).
        let mut ids = Vec::new();
        for c in ["a", "b", "c"] {
            ids.push(engine.add_fact(&req(c), old.clone(), None).await.unwrap());
        }
        for &id in &ids {
            assert_eq!(
                engine.storage().get_fact(id).await.unwrap().embedding,
                vec![0.1_f32; DIM],
                "served vectors start from the old space"
            );
        }

        // Reconstruct to the new identity.
        let outcome = engine.reconstruct(&new_fp, &new).await.unwrap();
        assert_eq!(outcome.promoted, 3);
        assert_eq!(outcome.new_fingerprint, new_fp);
        assert!(outcome.rebuild_index);
        assert_eq!(outcome.stragglers_caught, 0, "no concurrent ingest in-test");

        // The served vectors are now the NEW space's, in one swap.
        for &id in &ids {
            assert_eq!(
                engine.storage().get_fact(id).await.unwrap().embedding,
                vec![0.9_f32; DIM],
                "facts.embedding now serves the new space"
            );
        }
        // The identity flipped, and the old identity is now rejected (#614).
        assert_eq!(
            engine
                .storage()
                .load_embedding_fingerprint()
                .await
                .unwrap()
                .unwrap(),
            new_fp
        );
        let err = engine
            .storage()
            .check_embedding_compatible(&EmbeddingFingerprint::new("old-model", "test", DIM))
            .await
            .unwrap_err();
        assert!(
            matches!(err, MemoryError::EmbeddingModelMismatch { .. }),
            "old identity rejected post-promote, got {err:?}"
        );
    }

    #[tokio::test]
    async fn reconstruct_rejects_different_dim() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let wide: Arc<dyn EmbeddingProvider> = Arc::new(TagEmbedder {
            model: "wide",
            value: 0.9,
            dim: DIM * 2,
        });
        let err = engine
            .reconstruct(&EmbeddingFingerprint::new("wide", "test", DIM * 2), &wide)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                MemoryError::EmbeddingDimension { expected, actual }
                    if expected == DIM && actual == DIM * 2
            ),
            "different-dim is #742, got {err:?}"
        );
    }

    #[tokio::test]
    async fn reconstruct_resumes_a_crashed_reconstruction() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let old: Arc<dyn EmbeddingProvider> = Arc::new(TagEmbedder {
            model: "old-model",
            value: 0.1,
            dim: DIM,
        });
        let new: Arc<dyn EmbeddingProvider> = Arc::new(TagEmbedder {
            model: "new-model",
            value: 0.9,
            dim: DIM,
        });
        let new_fp = EmbeddingFingerprint::new("new-model", "test", DIM);

        let mut ids = Vec::new();
        for c in ["a", "b", "c", "d"] {
            ids.push(engine.add_fact(&req(c), old.clone(), None).await.unwrap());
        }

        // Simulate a crash partway through a prior reconstruction: the populating
        // space exists and 2 of 4 facts are already backfilled.
        let space = space_name_for(&new_fp);
        engine
            .storage()
            .begin_populating_space(&space, &new_fp)
            .await
            .unwrap();
        let partial = engine
            .storage()
            .next_backfill_window(&space, 0, 2)
            .await
            .unwrap();
        let rows: Vec<(i64, Vec<f32>)> = partial
            .iter()
            .map(|(id, _)| (*id, vec![0.9_f32; DIM]))
            .collect();
        engine
            .storage()
            .write_backfill_batch(&space, rows)
            .await
            .unwrap();
        assert_eq!(
            engine.storage().count_unbackfilled(&space).await.unwrap(),
            2
        );

        // Re-running reconstruct() reopens the same space (idempotent begin),
        // backfills the remainder, and promotes.
        let outcome = engine.reconstruct(&new_fp, &new).await.unwrap();
        assert_eq!(outcome.promoted, 4);
        assert_eq!(outcome.new_fingerprint, new_fp);
        for &id in &ids {
            assert_eq!(
                engine.storage().get_fact(id).await.unwrap().embedding,
                vec![0.9_f32; DIM]
            );
        }
    }
}
