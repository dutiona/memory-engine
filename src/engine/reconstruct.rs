//! Background reconstruction (#623): re-embed stored fact **content** (the
//! lossless source of truth) under a new same-dimension embedding identity in the
//! background, staging the vectors in a `populating` space's `fact_vectors`, then
//! (T3) atomically promote them as the active serving vectors.
//!
//! This module owns the **engine-side orchestration**: it drives the embedding —
//! off the write lock, under [`spawn_blocking`](tokio::task::spawn_blocking), so a
//! slow or `reqwest::blocking` provider neither parks the runtime nor panics with
//! a nested-runtime error (the #631 pattern) — and calls the pure-DB
//! [`SchemaManager`](crate::storage::schema::SchemaManager) reconstruction port
//! methods. The embedder never crosses the storage port; the backend does no
//! network/LLM work.
//!
//! The backfill is **resumable, crash-safe, and idempotent** with no persisted
//! cursor: the port's anti-join treats an absent `fact_vectors` row as the work
//! signal and `ON CONFLICT DO NOTHING` makes a replayed window a no-op. A crash
//! mid-backfill loses at most one in-flight batch, re-derived on restart.

use std::sync::Arc;

use super::spawn_join_err;
use crate::engine::MemoryEngine;
use crate::error::{MemoryError, Result};
use crate::traits::EmbeddingProvider;

impl MemoryEngine {
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

    use crate::engine::MemoryEngine;
    use crate::error::Result;
    use crate::traits::EmbeddingProvider;
    use crate::types::{EmbeddingFingerprint, FactType, NewFact};

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
}
