//! The embedding drain (#1021): it gives each fact `remember` stored its vector, one input
//! at a time.
//!
//! One embed call in flight is the drain's whole rate rule (ADR-0020 §6): the embedder is
//! shared with recall's query embeds and with knowledge-base, and more in flight drained no
//! faster while slowing every query. The queue's rules (what an answer does to an item, the
//! per-item backoff) are the engine's; this loop only paces itself by their outcome.

use std::sync::Arc;
use std::time::Duration;

use memory_engine::error::MemoryError;
use memory_engine::traits::EmbeddingProvider;
use memory_engine::{Backoff, MemoryEngine, NextEmbedding, Settled};
use tokio::sync::RwLock;

/// Drain `engine`'s embedding queue with `provider`, until the task is aborted.
///
/// Each settle holds `fence` for reading, so a stop that takes it for writing waits for a
/// settle in progress and lets none start. After an outcome that embedded nothing, the drain
/// sleeps by its streak of such outcomes on `backoff`; a wake never shortens that sleep.
///
/// # Examples
///
/// ```no_run
/// use std::sync::Arc;
/// use memory_engine::{Backoff, MemoryEngine};
/// use memory_engine_mcp::embedding::HttpEmbeddingProvider;
///
/// let provider = HttpEmbeddingProvider::new(
///     "http://localhost:8080/v1/embeddings".to_string(),
///     "Qwen/Qwen3-Embedding-0.6B".to_string(),
///     "llama.cpp".to_string(),
///     None,
///     1024,
///     30,
/// )?;
/// let runtime = tokio::runtime::Runtime::new()?;
/// runtime.block_on(async {
///     let engine = Arc::new(MemoryEngine::builder(1024).build()?);
///     let fence = Arc::new(tokio::sync::RwLock::new(()));
///     let drain = tokio::spawn(memory_engine_mcp::drain::run(
///         engine,
///         Arc::new(provider),
///         fence,
///         Backoff::DEFAULT,
///     ));
///     drain.abort();
///     Ok::<(), memory_engine::MemoryError>(())
/// })?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub async fn run(
    engine: Arc<MemoryEngine>,
    provider: Arc<dyn EmbeddingProvider>,
    fence: Arc<RwLock<()>>,
    backoff: Backoff,
) {
    let fingerprint = provider.fingerprint();
    // Consecutive outcomes that embedded nothing.
    let mut streak = 0_u32;
    loop {
        let item = match engine.next_embedding().await {
            Ok(NextEmbedding::Due(item)) => item,
            Ok(NextEmbedding::NotBefore(at)) => {
                let wait = (at - chrono::Utc::now()).to_std().unwrap_or_default();
                tokio::select! {
                    () = engine.embed_work_arrived() => {}
                    () = tokio::time::sleep(wait) => {}
                }
                continue;
            }
            Ok(NextEmbedding::Idle) => {
                engine.embed_work_arrived().await;
                continue;
            }
            Err(e) => {
                tracing::warn!(error = %e, "the embedding queue cannot be read");
                tokio::time::sleep(backoff.cap).await;
                continue;
            }
        };
        let embedder = Arc::clone(&provider);
        let content = item.content.clone();
        // Dropping the blocking client's internal runtime on an executor thread panics.
        let result = tokio::task::spawn_blocking(move || embedder.embed(&content))
            .await
            .unwrap_or_else(|e| Err(MemoryError::Internal(format!("the embed call failed: {e}"))));
        let settled = {
            let _fence = fence.read().await;
            engine
                .settle_embedding(&item, result, &fingerprint, backoff)
                .await
        };
        let pause = match settled {
            Ok(Settled::Embedded | Settled::Rejected) => {
                streak = 0;
                Duration::ZERO
            }
            // The item stays due: only the sleep keeps the drain from probing at once.
            Ok(Settled::Unreachable | Settled::KeyRejected) => {
                streak = streak.saturating_add(1);
                backoff.delay(streak)
            }
            // Only this item backs off, so the others go on at once; an embedder failing
            // every item is still probed at most once per delay.
            Ok(Settled::Retrying) => {
                streak = streak.saturating_add(1);
                backoff.delay(streak - 1)
            }
            Err(e) => {
                tracing::warn!(fact_id = item.fact_id, error = %e, "an embedding could not be settled");
                backoff.cap
            }
        };
        tokio::time::sleep(pause).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use memory_engine::types::{AddFactRequest, FactType};
    use memory_engine::{EmbeddingFingerprint, Result};

    use super::*;

    /// A provider that counts the embed calls running at once.
    #[derive(Default)]
    struct Counting {
        running: AtomicUsize,
        most: AtomicUsize,
    }

    impl EmbeddingProvider for Counting {
        fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            let running = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.most.fetch_max(running, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(5));
            self.running.fetch_sub(1, Ordering::SeqCst);
            Ok(vec![1.0, 0.0, 0.0, 0.0])
        }
        fn fingerprint(&self) -> EmbeddingFingerprint {
            EmbeddingFingerprint::new("counting", "test", 4)
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn never_two_inputs_in_flight() {
        let engine = Arc::new(MemoryEngine::builder(4).build().unwrap());
        let provider = Arc::new(Counting::default());
        let drain = tokio::spawn(run(
            Arc::clone(&engine),
            Arc::clone(&provider) as Arc<dyn EmbeddingProvider>,
            Arc::new(RwLock::new(())),
            Backoff::DEFAULT,
        ));
        for i in 0..8 {
            let request = AddFactRequest {
                content: format!("fact {i}"),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            };
            engine.add_fact_deferred(&request).await.unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while engine.next_embedding().await.unwrap() != NextEmbedding::Idle {
            assert!(Instant::now() < deadline, "the queue did not drain");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        drain.abort();
        assert_eq!(provider.most.load(Ordering::SeqCst), 1);
    }
}
