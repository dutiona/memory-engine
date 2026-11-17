//! The HTTP embedding provider the server embeds `remember` content and `recall` queries with,
//! and the bounded query embed a read falls back from (ADR-0020 §10).

use std::sync::Arc;
use std::time::Duration;

use memory_engine::error::{EmbedderError, MemoryError};
use memory_engine::traits::EmbeddingProvider as _;
pub use memory_engine_embed::HttpEmbeddingProvider;

/// ADR-0020 §10: past this, `recall` (and #1027's context build) answers from full-text
/// search, flagged.
pub const RECALL_EMBED_LIMIT: Duration = Duration::from_millis(500);

/// Why a read fell back to full-text search; [`as_str`](Self::as_str) is ADR-0020 §10's cause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Degraded {
    /// The query embed failed: no connection, an error answer, or anything else.
    EmbedderDown,
    /// No answer within [`RECALL_EMBED_LIMIT`].
    EmbedderSlow,
    /// The embedder answered 401 or 403.
    EmbedderKeyRejected,
}

impl Degraded {
    /// ADR-0020 §10's cause, as a read's answer carries it.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_engine_mcp::embedding::Degraded;
    ///
    /// assert_eq!(Degraded::EmbedderSlow.as_str(), "embedder slow");
    /// ```
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EmbedderDown => "embedder down",
            Self::EmbedderSlow => "embedder slow",
            Self::EmbedderKeyRejected => "embedder key rejected",
        }
    }
}

/// Embed the query `text`, giving the embedder [`RECALL_EMBED_LIMIT`] to answer.
///
/// The blocking call runs off the async executor. One still running at the limit is left
/// behind: it ends at the provider's own timeout and its result is dropped.
///
/// # Errors
///
/// The [`Degraded`] cause the read falls back with.
///
/// # Examples
///
/// ```no_run
/// use std::sync::Arc;
/// use memory_engine_mcp::embedding::{Degraded, HttpEmbeddingProvider, embed_query_bounded};
///
/// let provider = Arc::new(HttpEmbeddingProvider::new(
///     "http://localhost:8080/v1/embeddings".to_string(),
///     "Qwen/Qwen3-Embedding-0.6B".to_string(),
///     "llama.cpp".to_string(),
///     None,
///     1024,
///     30,
/// )?);
/// let runtime = tokio::runtime::Runtime::new()?;
/// match runtime.block_on(embed_query_bounded(&provider, "the lock file".to_owned())) {
///     Ok(vector) => assert_eq!(vector.len(), 1024),
///     Err(cause) => println!("full-text only: {}", cause.as_str()),
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub async fn embed_query_bounded(
    provider: &Arc<HttpEmbeddingProvider>,
    text: String,
) -> Result<Vec<f32>, Degraded> {
    let provider = Arc::clone(provider);
    // Dropping the blocking client's internal runtime on an executor thread panics.
    let call = tokio::task::spawn_blocking(move || provider.embed_query(&text));
    let (cause, error) = match tokio::time::timeout(RECALL_EMBED_LIMIT, call).await {
        Ok(Ok(Ok(vector))) => return Ok(vector),
        Ok(Ok(Err(e @ MemoryError::Embedder(EmbedderError::KeyRejected(_))))) => {
            (Degraded::EmbedderKeyRejected, e.to_string())
        }
        Ok(Ok(Err(e))) => (Degraded::EmbedderDown, e.to_string()),
        Ok(Err(e)) => (Degraded::EmbedderDown, e.to_string()),
        Err(_) => (
            Degraded::EmbedderSlow,
            format!("no answer within {RECALL_EMBED_LIMIT:?}"),
        ),
    };
    tracing::warn!(
        cause = cause.as_str(),
        %error,
        "the query embed failed; answering from full-text search"
    );
    Err(cause)
}
