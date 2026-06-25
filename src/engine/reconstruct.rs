//! Background reconstruction (#623, #742): re-embed stored fact **content** (the
//! lossless source of truth) under a new embedding identity in the background,
//! staging the vectors in a `populating` space's `fact_vectors`, then atomically
//! promoting them as the active serving vectors.
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
//! **Same-dim vs different-dim.** A same-dim reconstruction (#623) swaps with no
//! downtime — the engine keeps serving. A **different-dim** reconstruction (#742)
//! also succeeds, but because the engine's `embed_dim` is cached immutably at open,
//! the promote **fences** the handle ([`MemoryEngine::reopen_required`]): every
//! embedding-touching op then returns [`MemoryError::EmbeddingReopenRequired`] until
//! the consumer drops the handle and reopens the engine at the new dimension (which
//! re-validates cleanly and rebuilds the index at the new dim). A truly in-place,
//! no-reopen dimension transition is deferred (see the issue).

use std::sync::Arc;
use std::sync::atomic::Ordering;

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
    /// Reconstruct the active embedding space under a new `new_fingerprint`
    /// identity: open a shadow space, backfill it in the background, then atomically
    /// promote it (the served vectors swap in one transaction; the old vectors are
    /// retained for an instant rollback).
    ///
    /// Sequence: open (or resume) the `populating` space → backfill → a second
    /// **catch-up** pass (facts ingested during the first pass — live
    /// reconstruction) → atomic [`promote_space`](crate::storage::schema::SchemaManager::promote_space),
    /// whose completeness gate runs *inside* the transaction so a straggler that
    /// lands after the catch-up is caught (TOCTOU-safe), never silently dropped.
    /// Returns the [`PromoteOutcome`], with `stragglers_caught` set from the
    /// catch-up pass.
    ///
    /// **Dimension change → fence.** `new_fingerprint.dim` may differ from the
    /// engine's current [`embed_dim`](Self::embed_dim) — that is a different-dimension
    /// reconstruction (#742). It succeeds, but since `embed_dim` is cached immutably
    /// at open, a different-dim promote leaves this handle stale, so it is **fenced**:
    /// subsequent embedding-touching ops return [`MemoryError::EmbeddingReopenRequired`]
    /// (and [`reopen_required`](Self::reopen_required) reports the new dim) until the
    /// consumer reopens the engine at that dimension. A same-dim reconstruction does
    /// not fence. The provider must actually produce `new_fingerprint.dim`-wide
    /// vectors (checked up front).
    ///
    /// After the promote, [`PromoteOutcome::rebuild_index`] is `true`: the active
    /// vectors changed. For a **same-dim** reconstruction this method now rebuilds a
    /// `SQLite`+HNSW backend's in-memory index in place via
    /// [`SearchIndex::rebuild_vector_index`](crate::storage::SearchIndex::rebuild_vector_index)
    /// **before returning** (#624), so queries reflect the new model immediately. A
    /// **different-dim** reconstruction rebuilds the index on the required reopen
    /// (#742). The brute-force vector path (default features) reads `facts.embedding`
    /// directly and is correct immediately (same-dim) or on reopen (different-dim).
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::EmbeddingDimension`] if `new_fingerprint.dim` disagrees
    /// with the provider's own `fingerprint().dim` (a misconfiguration, rejected
    /// before any backfill), [`MemoryError::ReadOnly`] from the write port on a
    /// read-only engine, or any embedding-provider / storage failure surfaced by the
    /// backfill or promote.
    ///
    /// **Post-promote rebuild failure (same-dim):** the promote commits *before* the
    /// in-process index rebuild, so if `rebuild_vector_index` fails this returns the
    /// error **but the promote is durable** — `facts.embedding` holds the new vectors,
    /// the identity has flipped, and the brute-force / on-disk read path is correct.
    /// Only the in-memory HNSW index is left stale (it serves until the next open or a
    /// retry). The rebuild is idempotent, so the consumer may simply call
    /// [`reconstruct`](Self::reconstruct) again (it resumes/no-ops the already-promoted
    /// space) or reopen the engine to recover the index.
    pub async fn reconstruct(
        &self,
        new_fingerprint: &EmbeddingFingerprint,
        embedder: &Arc<dyn EmbeddingProvider>,
    ) -> Result<PromoteOutcome> {
        // Fail fast on a genuine misconfiguration: the declared target identity's
        // dimension must match what the provider actually produces. (A target dim
        // that differs from the engine's *current* dim is the whole point of #742
        // and is allowed — that is the different-dimension transition.)
        if new_fingerprint.dim != embedder.fingerprint().dim {
            return Err(MemoryError::EmbeddingDimension {
                expected: new_fingerprint.dim,
                actual: embedder.fingerprint().dim,
            });
        }

        let target_dim = new_fingerprint.dim;
        let space = space_name_for(new_fingerprint);

        // 1. Open (or resume) the shadow space.
        self.storage
            .begin_populating_space(&space, new_fingerprint)
            .await?;

        // 2. Backfill every fact's content under the new identity. Vectors are
        //    validated against `target_dim` (which may differ from the engine's
        //    current `embed_dim` — a different-dim reconstruction).
        self.backfill_space(&space, embedder, target_dim, DEFAULT_BACKFILL_BATCH)
            .await?;

        // 3. Catch-up: re-embed facts ingested during the backfill (live
        //    reconstruction). The count is the stragglers caught before promote.
        let stragglers = self
            .backfill_space(&space, embedder, target_dim, DEFAULT_BACKFILL_BATCH)
            .await?;

        // 4. Atomic promote (the completeness gate re-checks INSIDE the tx).
        let mut outcome = self.storage.promote_space(&space).await?;
        outcome.stragglers_caught = stragglers;

        // 5. The active vectors all changed — refresh the index. Two mutually
        //    exclusive cases, gated structurally on the dimension (NOT on
        //    `outcome.rebuild_index`, which is unconditionally true): a wrong-dim
        //    rebuild is unreachable by construction because the rebuild arm only runs
        //    when the dims match.
        if outcome.new_fingerprint.dim == self.embed_dim {
            // SAME-dim (#624): the cached dim stays valid (#623 behavior preserved),
            // so the handle keeps serving — but a live in-process vector index (HNSW)
            // was built on the now-replaced vectors and is stale. Rebuild it before
            // returning so queries reflect the new model immediately (a no-op on the
            // brute-force path, which reads `facts.embedding` directly).
            //
            // This is the designated "refresh embedding-derived caches after a same-dim
            // promote" point: if a persisted associative similarity-edge graph is added
            // later (a future cognitive-layer feature), its invalidation/recompute
            // slots in here, alongside the index rebuild.
            self.storage.rebuild_vector_index().await?;
        } else {
            // DIFFERENT-dim (#742): this handle's cached `embed_dim` is now stale
            // (`facts.embedding` is `target_dim`-wide while every read deserializes at
            // the old dim). Fence the handle — embedding-touching ops refuse with
            // `EmbeddingReopenRequired` until the consumer reopens at the new dim,
            // which rebuilds the index for free on open. (Rebuilding in place here
            // would read the new D′-wide blobs at the old dim → `EmbeddingDimension`.)
            self.reopen_required
                .store(outcome.new_fingerprint.dim, Ordering::Release);
        }

        Ok(outcome)
    }

    /// Backfill the `populating` space `space_name`: re-embed every fact's content
    /// with `embedder` and stage the vectors in `fact_vectors[space_name]`.
    ///
    /// Loops `next_backfill_window` → embed (off-lock) → `write_backfill_batch`
    /// until the space is fully backfilled. Returns the total number of vectors
    /// **actually written** this call (0 on a fully-backfilled space — the
    /// idempotent/crash-resume case). The space must already exist (open it with
    /// `begin_populating_space`); this method only fills it.
    ///
    /// `target_dim` is the dimension every produced vector must have — the
    /// **populating space's** declared dim, NOT the engine's current `embed_dim`
    /// (which differs for a different-dimension reconstruction, #742). This
    /// per-vector check is the sole width invariant once `promote_space`'s storage
    /// guard is gone, so it is load-bearing.
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
    /// (length mismatch), an [`MemoryError::EmbeddingDimension`] if a produced
    /// vector is not `target_dim`-wide, or any storage-port failure.
    async fn backfill_space(
        &self,
        space_name: &str,
        embedder: &Arc<dyn EmbeddingProvider>,
        target_dim: usize,
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

            // Defense in depth (the SOLE width invariant since `promote_space` no
            // longer guards dim, #742 D4): a provider whose declared fingerprint dim
            // matches the target space but which actually returns wrong-width vectors
            // would otherwise write corrupt blobs that the promote copy-swaps straight
            // into `facts.embedding`. Reject any vector not `target_dim`-wide before it
            // lands. Note `target_dim`, NOT `self.embed_dim` — they differ for a
            // different-dimension reconstruction.
            for emb in &embeddings {
                if emb.len() != target_dim {
                    return Err(MemoryError::EmbeddingDimension {
                        expected: target_dim,
                        actual: emb.len(),
                    });
                }
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
        let written = engine
            .backfill_space(SPACE, &embedder(), DIM, 2)
            .await
            .unwrap();
        assert_eq!(written, 5, "one vector per fact across multiple windows");
        assert_eq!(engine.storage().count_unbackfilled(SPACE).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn backfill_is_idempotent_on_replay() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        seed(&engine, &["a", "b", "c"]).await;
        begin(&engine).await;

        assert_eq!(
            engine
                .backfill_space(SPACE, &embedder(), DIM, 8)
                .await
                .unwrap(),
            3
        );
        // Re-running over a fully-backfilled space writes nothing.
        assert_eq!(
            engine
                .backfill_space(SPACE, &embedder(), DIM, 8)
                .await
                .unwrap(),
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
        let written = engine
            .backfill_space(SPACE, &embedder(), DIM, 8)
            .await
            .unwrap();
        assert_eq!(written, 2, "only the un-backfilled remainder is written");
        assert_eq!(engine.storage().count_unbackfilled(SPACE).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn backfill_picks_up_fact_inserted_mid_reconstruction() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        seed(&engine, &["a", "b"]).await;
        begin(&engine).await;
        assert_eq!(
            engine
                .backfill_space(SPACE, &embedder(), DIM, 8)
                .await
                .unwrap(),
            2
        );

        // A fact lands after the first pass completed — a second pass catches it
        // (no persisted cursor; the absent fact_vectors row is the work signal).
        seed(&engine, &["late-arrival"]).await;
        assert_eq!(engine.storage().count_unbackfilled(SPACE).await.unwrap(), 1);
        assert_eq!(
            engine
                .backfill_space(SPACE, &embedder(), DIM, 8)
                .await
                .unwrap(),
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
    async fn reconstruct_different_dim_fences_handle() {
        // #742 (inverts the old `reconstruct_rejects_different_dim`): a different-dim
        // reconstruction now SUCCEEDS (the same-dim guard is gone) and fences the
        // handle — `facts.embedding` is now D′-wide, so the engine refuses reads
        // until the consumer reopens at D′. (In-memory engine = terminal fence, no
        // reopen possible; the full file-backed D→D′→reopen cycle is a separate test.)
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let old: Arc<dyn EmbeddingProvider> = Arc::new(TagEmbedder {
            model: "old",
            value: 0.1,
            dim: DIM,
        });
        let mut ids = Vec::new();
        for c in ["a", "b"] {
            ids.push(engine.add_fact(&req(c), old.clone(), None).await.unwrap());
        }
        let wide: Arc<dyn EmbeddingProvider> = Arc::new(TagEmbedder {
            model: "wide",
            value: 0.9,
            dim: DIM * 2,
        });
        let new_fp = EmbeddingFingerprint::new("wide", "test", DIM * 2);

        let outcome = engine.reconstruct(&new_fp, &wide).await.unwrap();
        assert_eq!(outcome.promoted, 2);
        assert_eq!(outcome.new_fingerprint.dim, DIM * 2);
        assert!(outcome.rebuild_index);

        // The handle is fenced at the new dim; reads refuse until reopen.
        assert_eq!(engine.reopen_required(), Some(DIM * 2));
        assert!(matches!(
            engine.get_fact(ids[0]).await,
            Err(MemoryError::EmbeddingReopenRequired { new_dim }) if new_dim == DIM * 2
        ));
    }

    #[tokio::test]
    async fn reconstruct_rejects_embedder_target_dim_mismatch() {
        // The retained fail-fast: the declared target identity's dim must match what
        // the provider actually produces (a genuine misconfiguration), rejected
        // before any backfill work.
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let mismatched: Arc<dyn EmbeddingProvider> = Arc::new(TagEmbedder {
            model: "wide",
            value: 0.9,
            dim: DIM, // provider produces DIM-wide, but the target declares DIM*2
        });
        let err = engine
            .reconstruct(
                &EmbeddingFingerprint::new("wide", "test", DIM * 2),
                &mismatched,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                MemoryError::EmbeddingDimension { expected, actual }
                    if expected == DIM * 2 && actual == DIM
            ),
            "embedder/target dim mismatch rejected, got {err:?}"
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

    #[tokio::test]
    async fn backfill_rejects_off_dimension_vectors() {
        // Defense in depth (review): a provider whose fingerprint claims `DIM` but
        // actually returns wider vectors must be rejected before its blobs reach
        // `fact_vectors` — otherwise the promote copy-swaps corruption straight into
        // `facts.embedding` (the same-dim guard only checks the declared dim).
        struct LyingEmbedder {
            declared: usize,
            actual: usize,
        }
        impl EmbeddingProvider for LyingEmbedder {
            fn embed(&self, _text: &str) -> Result<Vec<f32>> {
                Ok(vec![0.5_f32; self.actual])
            }
            fn fingerprint(&self) -> EmbeddingFingerprint {
                EmbeddingFingerprint::new("liar", "test", self.declared)
            }
        }

        let engine = MemoryEngine::builder(DIM).build().unwrap();
        seed(&engine, &["a"]).await;
        begin(&engine).await;
        let liar: Arc<dyn EmbeddingProvider> = Arc::new(LyingEmbedder {
            declared: DIM,
            actual: DIM + 1,
        });

        let err = engine
            .backfill_space(SPACE, &liar, DIM, 8)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                MemoryError::EmbeddingDimension { expected, actual }
                    if expected == DIM && actual == DIM + 1
            ),
            "off-dim vectors rejected before write, got {err:?}"
        );
        // Nothing was written: the fact stays un-backfilled.
        assert_eq!(engine.storage().count_unbackfilled(SPACE).await.unwrap(), 1);
    }

    // --- #624: same-dim live HNSW rebuild on promote (ann feature) ---
    #[cfg(feature = "ann")]
    mod ann_rebuild {
        use super::*;
        use crate::search::hybrid::{SearchMode, SearchQuery};
        use crate::search::strategy::SearchConfig;

        #[tokio::test]
        async fn same_dim_reconstruct_rebuilds_index_without_fencing() {
            // #624 wiring guard: with a live HNSW index (ann_threshold=0), a same-dim
            // reconstruction rebuilds the in-process index and does NOT fence the
            // handle. The *rigorous* proof that the rebuild refreshes/reclaims the index
            // is the strategy-level white-box test in `search::ann`
            // (`rebuild_from_db_reclaims_tombstones_and_matches_live_rows`); at this
            // corpus size an engine query resolves via re-scoring regardless, so this
            // test guards: same-dim ⇒ no fence, the rebuild runs cleanly under ann
            // (a rebuild error would surface as `reconstruct` returning `Err`), and the
            // engine stays queryable through the HNSW path afterward.
            let engine = MemoryEngine::builder(DIM)
                .search_config(SearchConfig { ann_threshold: 0 })
                .build()
                .unwrap();
            let old: Arc<dyn EmbeddingProvider> = Arc::new(TagEmbedder {
                model: "old",
                value: 0.1,
                dim: DIM,
            });
            let new: Arc<dyn EmbeddingProvider> = Arc::new(TagEmbedder {
                model: "new",
                value: 0.9,
                dim: DIM,
            });
            let new_fp = EmbeddingFingerprint::new("new", "test", DIM);

            let mut ids = Vec::new();
            for c in ["a", "b", "c"] {
                ids.push(engine.add_fact(&req(c), old.clone(), None).await.unwrap());
            }

            let outcome = engine.reconstruct(&new_fp, &new).await.unwrap();
            assert_eq!(outcome.promoted, 3);
            assert!(outcome.rebuild_index);
            // Same-dim ⇒ the handle is NOT fenced (#623 behavior preserved).
            assert_eq!(engine.reopen_required(), None);

            // Served vectors are the new model's, and the engine remains queryable
            // through the rebuilt HNSW path (ann_threshold=0 ⇒ HNSW active).
            for &id in &ids {
                assert_eq!(
                    engine.storage().get_fact(id).await.unwrap().embedding,
                    vec![0.9_f32; DIM]
                );
            }
            let results = engine
                .query(&SearchQuery {
                    text: None,
                    embedding: Some(vec![0.9_f32; DIM]),
                    mode: SearchMode::Vector,
                    limit: 5,
                    rerank_depth: None,
                    valid_at: None,
                    fact_type: None,
                    scope: None,
                })
                .await
                .unwrap();
            assert_eq!(results.len(), 3, "all facts served from the rebuilt index");
        }

        #[tokio::test]
        async fn different_dim_reconstruct_fences_and_skips_rebuild() {
            // #742 preserved under ann: a different-dim reconstruction takes the FENCE
            // arm, never the same-dim rebuild arm (which would read D′-wide blobs at the
            // old dim → EmbeddingDimension). It must succeed and set reopen_required.
            let engine = MemoryEngine::builder(DIM)
                .search_config(SearchConfig { ann_threshold: 0 })
                .build()
                .unwrap();
            let old: Arc<dyn EmbeddingProvider> = Arc::new(TagEmbedder {
                model: "old",
                value: 0.1,
                dim: DIM,
            });
            for c in ["a", "b"] {
                engine.add_fact(&req(c), old.clone(), None).await.unwrap();
            }
            let wide: Arc<dyn EmbeddingProvider> = Arc::new(TagEmbedder {
                model: "wide",
                value: 0.9,
                dim: DIM * 2,
            });
            let new_fp = EmbeddingFingerprint::new("wide", "test", DIM * 2);

            let outcome = engine.reconstruct(&new_fp, &wide).await.unwrap();
            assert_eq!(outcome.promoted, 2);
            assert_eq!(outcome.new_fingerprint.dim, DIM * 2);
            assert_eq!(
                engine.reopen_required(),
                Some(DIM * 2),
                "different-dim fences, does not rebuild at the wrong dim"
            );
        }
    }
}
