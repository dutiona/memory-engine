//! Shared embedding-provider CLI configuration (#619, §Design.6).
//!
//! Every command that builds an [`HttpEmbeddingProvider`] (query, batch-ingest,
//! bootstrap, consolidate) flattens [`EmbeddingArgs`] so the identity/asymmetry config
//! keys match the MCP server's `[embedding]` section: `provider` (feeds the
//! [`EmbeddingFingerprint`] that #614 enforces — replaces the old hardcoded provider),
//! `query_instruction` (asymmetric query prefix), `mrl_dim` (Matryoshka truncation), and
//! the four served-artifact keys (`artifact_sha256`, `pooling`, `normalization`,
//! `max_input_tokens`; ADR 0015 §1). Centralizing construction here keeps the
//! native-vs-stored dimension handling — the same model as the MCP
//! `config::resolve_embedding` — in one place.

use anyhow::Context;
use memory_engine_embed::HttpEmbeddingProvider;

/// Embedding-provider configuration shared across CLI commands.
///
/// `embed_url` + `embed_model` are optional at the clap layer (a query with neither is
/// FTS-only); commands that require an embedder call [`build_required`](Self::build_required),
/// which errors clearly when they are absent.
// No `Debug` derive: this struct holds `embed_api_key` in cleartext, and a
// derived `Debug` would print it under any `{:?}` / `tracing::debug!(?args)`.
// (The built `HttpEmbeddingProvider` likewise has no `Debug`.)
#[derive(clap::Args, Clone)]
pub struct EmbeddingArgs {
    /// OpenAI-compatible embedding endpoint URL (e.g. `http://localhost:11434/v1/embeddings`).
    #[arg(long, env = "MEMORY_ENGINE_EMBED_URL")]
    pub embed_url: Option<String>,

    /// Embedding model name / slug (e.g. `Qwen/Qwen3-Embedding-0.6B`).
    #[arg(long, env = "MEMORY_ENGINE_EMBED_MODEL")]
    pub embed_model: Option<String>,

    /// Serving backend — operator-declared, feeds the embedding fingerprint (#614).
    /// e.g. `llama.cpp`, `ollama`, `tei`, `openai`. Defaults to
    /// [`HttpEmbeddingProvider::DEFAULT_PROVIDER`] for back-compat.
    #[arg(
        long,
        env = "MEMORY_ENGINE_EMBED_PROVIDER",
        default_value = HttpEmbeddingProvider::DEFAULT_PROVIDER
    )]
    pub embed_provider: String,

    /// Bearer API key for the embedding endpoint.
    #[arg(long, env = "MEMORY_ENGINE_EMBED_API_KEY", hide_env_values = true)]
    pub embed_api_key: Option<String>,

    /// Native model dimension before MRL truncation. Required only with `--mrl-dim`
    /// (then the provider validates raw responses against this while the engine stores
    /// the truncated `--mrl-dim`). Named distinctly from `--embed-dim` (the engine's
    /// stored dimension on `--create`) to avoid confusion.
    #[arg(long)]
    pub native_dim: Option<usize>,

    /// Query-only instruction prefix, applied by the query path's `embed_query`; document
    /// embedding is never prefixed. Absent = ME's pinned instruction (ADR 0015 §6); `''` =
    /// no prefix, for a symmetric model.
    #[arg(long)]
    pub query_instruction: Option<String>,

    /// Matryoshka (MRL) truncation target. Must equal the engine `embed_dim` (the engine
    /// stores post-truncation vectors); `--native-dim` gives the native length.
    #[arg(long)]
    pub mrl_dim: Option<usize>,

    /// HTTP timeout in seconds for embedding calls.
    #[arg(long, default_value = "30")]
    pub embed_timeout: u64,

    /// The served artifact (ADR 0015 §1): all four flags or none.
    #[command(flatten)]
    pub served: ServedArgs,
}

/// The served-artifact flags (ADR 0015 §1), mirroring the MCP server's `[embedding]` keys.
/// All four or none; validated by `ServedArtifact::from_parts`.
#[derive(clap::Args, Clone, Default)]
pub struct ServedArgs {
    /// SHA-256 of the served model file (for llama.cpp, the GGUF), 64 lowercase hex digits.
    #[arg(
        long = "embed-artifact-sha256",
        env = "MEMORY_ENGINE_EMBED_ARTIFACT_SHA256"
    )]
    pub artifact_sha256: Option<String>,

    /// The pooling the server applies, e.g. `last`.
    #[arg(long = "embed-pooling", env = "MEMORY_ENGINE_EMBED_POOLING")]
    pub pooling: Option<String>,

    /// The normalization of each output vector, e.g. `l2`.
    #[arg(
        long = "embed-normalization",
        env = "MEMORY_ENGINE_EMBED_NORMALIZATION"
    )]
    pub normalization: Option<String>,

    /// The maximum input length, in tokens, the server embeds.
    #[arg(
        long = "embed-max-input-tokens",
        env = "MEMORY_ENGINE_EMBED_MAX_INPUT_TOKENS"
    )]
    pub max_input_tokens: Option<usize>,
}

impl ServedArgs {
    /// The validated served artifact, or `None` when no flag is set.
    fn resolve(&self) -> anyhow::Result<Option<memory_engine::ServedArtifact>> {
        memory_engine::ServedArtifact::from_parts(
            self.artifact_sha256.clone(),
            self.pooling.clone(),
            self.normalization.clone(),
            self.max_input_tokens,
        )
        .map_err(|e| anyhow::anyhow!("invalid served-artifact flags: {e}"))
    }

    const fn any(&self) -> bool {
        self.artifact_sha256.is_some()
            || self.pooling.is_some()
            || self.normalization.is_some()
            || self.max_input_tokens.is_some()
    }
}

impl EmbeddingArgs {
    /// Build the provider when configured, returning `None` when **both** `embed_url`
    /// and `embed_model` are absent (e.g. an FTS-only query). A partial configuration
    /// (one present, one missing) is an error.
    ///
    /// `engine_dim` is the engine's stored dimension; see [`build_inner`](Self::build_inner)
    /// for how it relates to the native dimension under MRL.
    ///
    /// # Errors
    ///
    /// Returns an error on a partial configuration or any provider construction failure.
    pub fn build_optional(
        &self,
        engine_dim: usize,
    ) -> anyhow::Result<Option<HttpEmbeddingProvider>> {
        match (self.embed_url.as_deref(), self.embed_model.as_deref()) {
            // Served flags declare an identity to check; without an embedder there is none.
            (None, None) if self.served.any() => {
                anyhow::bail!("the served-artifact flags need --embed-url and --embed-model")
            }
            (None, None) => Ok(None),
            (Some(url), Some(model)) => Ok(Some(self.build_inner(url, model, engine_dim)?)),
            _ => anyhow::bail!("--embed-url and --embed-model must be provided together"),
        }
    }

    /// Build the provider, erroring if `embed_url` / `embed_model` are not set. For
    /// commands that always embed (batch-ingest, bootstrap).
    ///
    /// # Errors
    ///
    /// Returns an error if the embedder is not configured or construction fails.
    pub fn build_required(&self, engine_dim: usize) -> anyhow::Result<HttpEmbeddingProvider> {
        self.build_optional(engine_dim)?
            .context("--embed-url and --embed-model are required for this command")
    }

    /// [`build_required`](Self::build_required) on a blocking thread.
    ///
    /// A blocking reqwest client builds and drops a runtime of its own in debug builds, which
    /// panics on the async executor; async commands build the provider through this.
    ///
    /// # Errors
    ///
    /// As [`build_required`](Self::build_required), or if the blocking task fails to join.
    pub async fn build_required_blocking(
        &self,
        engine_dim: usize,
    ) -> anyhow::Result<HttpEmbeddingProvider> {
        let args = self.clone();
        tokio::task::spawn_blocking(move || args.build_required(engine_dim))
            .await
            .map_err(|e| anyhow::anyhow!("join error building embedder: {e}"))?
    }

    /// [`build_optional`](Self::build_optional) on a blocking thread, for the same reason as
    /// [`build_required_blocking`](Self::build_required_blocking).
    ///
    /// # Errors
    ///
    /// As [`build_optional`](Self::build_optional), or if the blocking task fails to join.
    pub async fn build_optional_blocking(
        &self,
        engine_dim: usize,
    ) -> anyhow::Result<Option<HttpEmbeddingProvider>> {
        let args = self.clone();
        tokio::task::spawn_blocking(move || args.build_optional(engine_dim))
            .await
            .map_err(|e| anyhow::anyhow!("join error building embedder: {e}"))?
    }

    /// Construct the provider, wiring `provider`, `query_instruction`, and `mrl_dim`.
    ///
    /// The provider's `expected_dim` is the **native** dimension it validates raw
    /// responses against: with MRL it is `--native-dim` (falling back to the engine
    /// dim) and the engine stores the truncated `--mrl-dim`; without MRL native == stored
    /// == `engine_dim`. `--mrl-dim` MUST equal `engine_dim` (the engine stores the
    /// post-truncation vector), and without MRL `--native-dim` must too — surfaced
    /// at build time, mirroring the MCP `build_embedder`.
    fn build_inner(
        &self,
        url: &str,
        model: &str,
        engine_dim: usize,
    ) -> anyhow::Result<HttpEmbeddingProvider> {
        // MRL truncates a NATIVE-length response down to the stored dim, so the native
        // dim must be declared explicitly: defaulting it to the (truncated) engine dim
        // would make the provider validate the raw response against the wrong length and
        // reject every real embedding. Require --native-dim whenever --mrl-dim is set.
        anyhow::ensure!(
            self.mrl_dim.is_none() || self.native_dim.is_some(),
            "--native-dim is required with --mrl-dim: it is the native dimension the model \
             returns before truncation (the engine stores the truncated --mrl-dim)"
        );
        // Otherwise honour an explicit --native-dim (default: the engine dim) so a stray
        // --native-dim without --mrl-dim surfaces as a mismatch below rather than being
        // silently ignored.
        let native_dim = self.native_dim.unwrap_or(engine_dim);
        let mut provider = HttpEmbeddingProvider::new(
            url.to_owned(),
            model.to_owned(),
            self.embed_provider.clone(),
            self.embed_api_key.clone(),
            native_dim,
            self.embed_timeout,
        )
        .map_err(|e| anyhow::anyhow!("failed to create embedding provider: {e}"))?;

        if let Some(instruction) = &self.query_instruction {
            provider = provider.with_query_instruction(instruction.clone());
        }
        if let Some(served) = self.served.resolve()? {
            provider = provider.with_served_artifact(served);
        }
        if let Some(target) = self.mrl_dim {
            anyhow::ensure!(
                target == engine_dim,
                "--mrl-dim ({target}) must equal the engine embed_dim ({engine_dim}): \
                 the engine stores post-truncation vectors"
            );
            provider = provider
                .with_mrl_dim(target)
                .map_err(|e| anyhow::anyhow!("invalid --mrl-dim: {e}"))?;
        } else {
            anyhow::ensure!(
                native_dim == engine_dim,
                "--native-dim ({native_dim}) must equal the engine embed_dim \
                 ({engine_dim}) when --mrl-dim is unset"
            );
        }
        Ok(provider)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_engine::EmbeddingFingerprint;
    use memory_engine::traits::EmbeddingProvider;

    /// An `EmbeddingArgs` with the given url/model and otherwise minimal config.
    fn args(url: Option<&str>, model: Option<&str>) -> EmbeddingArgs {
        EmbeddingArgs {
            embed_url: url.map(String::from),
            embed_model: model.map(String::from),
            embed_provider: "tei".into(),
            embed_api_key: None,
            native_dim: None,
            query_instruction: None,
            mrl_dim: None,
            embed_timeout: 5,
            served: ServedArgs::default(),
        }
    }

    const URL: &str = "http://127.0.0.1:0/v1/embeddings";

    /// #1064: the clap default is the provider the MCP server defaults to.
    #[test]
    fn embed_provider_defaults_to_the_shared_provider() {
        use clap::{Args, FromArgMatches};
        let cmd = EmbeddingArgs::augment_args(clap::Command::new("t"));
        let parsed = EmbeddingArgs::from_arg_matches(&cmd.get_matches_from(["t"])).unwrap();
        assert_eq!(
            parsed.embed_provider,
            HttpEmbeddingProvider::DEFAULT_PROVIDER
        );
    }

    #[test]
    fn build_optional_none_when_both_absent() {
        assert!(args(None, None).build_optional(8).unwrap().is_none());
    }

    #[test]
    fn build_optional_partial_config_errors() {
        assert!(args(Some(URL), None).build_optional(8).is_err());
        assert!(args(None, Some("m")).build_optional(8).is_err());
    }

    #[test]
    fn build_required_errors_when_absent() {
        assert!(args(None, None).build_required(8).is_err());
    }

    #[test]
    fn build_reports_declared_identity() {
        let provider = args(Some(URL), Some("m")).build_required(8).unwrap();
        assert_eq!(
            provider.fingerprint(),
            EmbeddingFingerprint::new("m", "tei", 8)
        );
    }

    #[test]
    fn mrl_dim_must_equal_engine_dim() {
        let mut a = args(Some(URL), Some("m"));
        a.native_dim = Some(16);
        a.mrl_dim = Some(4); // != engine_dim 8
        assert!(a.build_optional(8).is_err());
    }

    #[test]
    fn native_dim_must_equal_engine_dim_without_mrl() {
        let mut a = args(Some(URL), Some("m"));
        a.native_dim = Some(16); // != engine_dim 8, and no --mrl-dim
        assert!(a.build_optional(8).is_err());
    }

    #[test]
    fn mrl_dim_requires_native_dim() {
        // Without --native-dim, the provider would validate the raw native-length
        // response against the truncated engine dim and reject every embedding — so
        // --mrl-dim must be accompanied by --native-dim (fails at build time).
        let mut a = args(Some(URL), Some("m"));
        a.mrl_dim = Some(8); // == engine_dim, but native_dim omitted
        // `.err()` avoids unwrap_err's Debug bound on the Ok type (HttpEmbeddingProvider
        // deliberately has no Debug — it holds an api_key).
        let err = a.build_optional(8).err().expect("must error").to_string();
        assert!(
            err.contains("--native-dim is required"),
            "expected a --native-dim requirement error, got: {err}"
        );
    }

    #[test]
    fn valid_mrl_config_reports_matryoshka_fingerprint() {
        let mut a = args(Some(URL), Some("m"));
        a.native_dim = Some(16);
        a.mrl_dim = Some(8); // == engine_dim (stored), native 16
        let fp = a.build_optional(8).unwrap().unwrap().fingerprint();
        assert_eq!(fp.dim, 8, "stored dim is the truncated mrl_dim");
        assert_eq!(fp.matryoshka_base_dim, Some(16), "native dim recorded");
    }

    const SHA: &str = "06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439";

    fn served_args(sha: Option<&str>, pooling: Option<&str>) -> ServedArgs {
        ServedArgs {
            artifact_sha256: sha.map(String::from),
            pooling: pooling.map(String::from),
            normalization: Some("l2".into()),
            max_input_tokens: Some(2048),
        }
    }

    #[test]
    fn served_flags_feed_the_fingerprint() {
        let mut a = args(Some(URL), Some("m"));
        a.served = served_args(Some(SHA), Some("last"));
        let fp = a.build_required(8).unwrap().fingerprint();
        let served = fp.served.expect("served artifact declared");
        assert_eq!(served.artifact_sha256, SHA);
        assert_eq!(served.pooling, "last");
        assert_eq!(served.normalization, "l2");
        assert_eq!(served.max_input_tokens, 2048);
    }

    #[test]
    fn partial_served_flags_name_the_missing_fields() {
        let mut a = args(Some(URL), Some("m"));
        a.served = served_args(Some(SHA), None);
        let err = a.build_optional(8).err().expect("must error").to_string();
        assert!(err.contains("pooling"), "{err}");
    }

    #[test]
    fn a_non_canonical_sha256_is_rejected() {
        let mut a = args(Some(URL), Some("m"));
        a.served = served_args(Some(&SHA.to_uppercase()), Some("last"));
        assert!(a.build_optional(8).is_err());
    }

    #[test]
    fn served_flags_without_an_embedder_are_refused() {
        let mut a = args(None, None);
        a.served = served_args(Some(SHA), Some("last"));
        let err = a.build_optional(8).err().expect("must error").to_string();
        assert!(err.contains("--embed-url"), "{err}");
    }
}
