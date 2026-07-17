//! # me-traits
//!
//! Layer-0.5 (L0.5) of the memory-engine crate workspace (Wave 2, #816).
//!
//! The consumer/contract traits the engine delegates all LLM/network work to
//! (`EmbeddingProvider`, `SummaryGenerator`, `ConflictArbiter`,
//! `PersistenceClassifier`, `Reranker`, `DreamCycle`, `DeltaProposer`,
//! `InsightStream`), plus the read-only `DreamCtx`/`CycleCtx` capability handles and
//! the non-DTO companion types. Depends only on `me-types` — a thin leaf over the
//! data layer.

// Panic-safety gate (#725): `unwrap_used = "deny"` (workspace lints) forbids
// `.unwrap()` in production paths. This crate's own `#[cfg(test)]` tests are exempt.
#![cfg_attr(test, allow(clippy::unwrap_used))]

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use me_types::error::Result;
use me_types::types::cycle_report::{CycleMetadata, CycleReport, TimeWindow};
use me_types::types::forgetting::{ForgetPolicy, PruneStats};
use me_types::types::search::{SearchQuery, SearchResult};
use me_types::types::{
    ClassifierInput, ConsolidationProposal, EmbeddingFingerprint, Fact, OutcomeCounts,
    PromoteRequest, PromotionResult,
};

/// Shared `EmbeddingProvider` test double (`MockEmbedder`).
///
/// Gated behind the `test-util` feature (Wave 2 #816, `me-backend-sqlite` carve,
/// Commit 2): `#[cfg(test)]` does not reach across a crate boundary — only a
/// Cargo feature does, so every consumer crate's own tests opt in via this.
#[cfg(feature = "test-util")]
pub mod test_util;

// --- Phase 1: Embedding provider (fully used) ---

/// Trait for computing text embeddings.
///
/// Consumers implement this to integrate their embedding model (local or API).
/// The engine calls `embed` during `add_fact` to compute the embedding vector.
///
/// Requires `Send + Sync`: providers are shared across the engine's worker
/// threads (`spawn_blocking` connection pool, async facade).
pub trait EmbeddingProvider: Send + Sync {
    /// Compute an embedding vector for the given text.
    ///
    /// # Errors
    ///
    /// Returns an error if embedding computation fails.
    fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Compute embedding vectors for multiple texts in a single call.
    ///
    /// The default implementation loops `embed()` sequentially.
    /// Providers with native batch APIs (e.g., `OpenAI` `/v1/embeddings`)
    /// should override this for a single HTTP round-trip.
    ///
    /// # Contract
    ///
    /// The returned `Vec` **must** have the same length as `texts`.
    /// Each element corresponds positionally to the input text at that index.
    ///
    /// # Errors
    ///
    /// Returns an error if any embedding computation fails.
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        texts.iter().map(|t| self.embed(t)).collect()
    }

    /// Compute an embedding vector for a **query** string.
    ///
    /// Asymmetric models (e.g. `Qwen3-Embedding` via TEI) prepend a query-only
    /// instruction prefix here, while [`embed`](Self::embed) (documents) stays
    /// prefix-free. The default delegates to [`embed`](Self::embed), so symmetric
    /// providers (`Mock`, `Hash`, `Passthrough`) need no change and queries embed
    /// identically to documents.
    ///
    /// The **core never calls this** — `MemoryQuery` carries a
    /// pre-computed vector, so query embedding happens only at the consumer layer
    /// (MCP/CLI). The method lives on the provider trait so that consumer layer has a
    /// single place to express the asymmetry.
    ///
    /// # Errors
    ///
    /// Returns an error if embedding computation fails.
    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.embed(text)
    }

    /// Compute embedding vectors for multiple **query** strings in a single call.
    ///
    /// The default loops [`embed_query`](Self::embed_query) sequentially — mirroring
    /// how [`embed_batch`](Self::embed_batch) loops [`embed`](Self::embed). This makes
    /// the default **correct by construction**: an asymmetric provider that overrides
    /// only `embed_query` (e.g. to prepend a query instruction prefix) automatically
    /// gets prefixed batch queries too, with no silent document-space leak.
    ///
    /// Providers with a native batch API (e.g. `OpenAI`/TEI `/v1/embeddings`) should
    /// override this for a single HTTP round-trip — correctness is the default;
    /// batch efficiency is the opt-in.
    ///
    /// # Contract
    ///
    /// The returned `Vec` **must** have the same length as `texts`. Each element
    /// corresponds positionally to the input query at that index.
    ///
    /// # Errors
    ///
    /// Returns an error if any embedding computation fails.
    fn embed_query_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        texts.iter().map(|t| self.embed_query(t)).collect()
    }

    /// Declare this provider's [`EmbeddingFingerprint`] — the identity of the vector
    /// space it produces (`model`, `provider`, `dim`, ...).
    ///
    /// **No default impl, by design.** A placeholder fingerprint would let the
    /// mismatch guard (issue #614) compare two placeholders, find them equal, and
    /// admit incompatible vector spaces — the exact silent corruption this identity
    /// exists to prevent. Every provider declares its real identity.
    ///
    /// The returned identity describes the **document** vector space this provider
    /// writes, and must stay congruent with what the provider actually computes.
    fn fingerprint(&self) -> EmbeddingFingerprint;
}

// --- Phase 2: Consolidation traits and types (summary, conflict arbitration) ---

/// Trait for generating a textual summary from a set of related items.
///
/// Used by consolidation: the cluster-fusion pass summarizes related *facts*, and
/// the global-integration pass summarizes the resulting *cluster summaries* — both
/// feed [`SummaryGenerator::summarize`] a slice of [`SummarizableContent`].
///
/// Summaries are embedded by the [`EmbeddingProvider`] passed alongside the
/// generator into `consolidate`, so
/// summary text shares the fact/summary vector space. The generator only produces
/// text — it never embeds (issue #116: embedding lived here as a duplicate of
/// [`EmbeddingProvider::embed`]).
///
/// Requires `Send + Sync`: summary generators are shared across the engine's
/// worker threads alongside the other consumer providers.
pub trait SummaryGenerator: Send + Sync {
    /// Generate a textual summary from a slice of items, each carrying its text
    /// and that text's embedding.
    ///
    /// # Errors
    ///
    /// Returns an error if summarization fails.
    fn summarize(&self, items: &[SummarizableContent<'_>]) -> Result<String>;
}

/// A minimal, borrowed view of something to summarize: its `text` and that text's
/// `embedding` in the shared fact/summary vector space.
///
/// Both consolidation passes feed [`SummaryGenerator::summarize`] through this type
/// — the cluster pass over facts, the global pass over cluster summaries — so
/// neither fabricates throwaway `Fact` structs with phantom field values just to
/// satisfy the trait (#273). It is `Copy` (two shared references) and borrows its
/// inputs, so building it clones nothing.
///
/// `embedding` is provided for summarizers that want embedding-aware merging
/// (ordering, centroid weighting, dedup hints); a plain text summarizer ignores
/// it. `#[non_exhaustive]`: build it via [`SummarizableContent::new`] so a future
/// field (e.g. scope or metadata for richer prompting) won't break callers.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct SummarizableContent<'a> {
    /// The text to summarize.
    pub text: &'a str,
    /// The embedding of `text`, in the shared fact/summary vector space.
    pub embedding: &'a [f32],
}

impl<'a> SummarizableContent<'a> {
    /// Create a summarizable view from borrowed text and its embedding.
    #[must_use]
    pub const fn new(text: &'a str, embedding: &'a [f32]) -> Self {
        Self { text, embedding }
    }
}

/// Trait for proposing how to consolidate a window of facts (#554).
///
/// This is the seam that makes the consolidation backend **pluggable**: an
/// `LlmDreamCycle` (a [`DreamCycle`] impl) delegates the "what to merge" decision to
/// a `DeltaProposer`, while the shipped `DefaultDreamCycle`
/// makes that decision deterministically in pure Rust. Choosing a backend is choosing
/// a `&dyn DreamCycle`; choosing *this* trait's impl selects the LLM that drives one.
///
/// The proposer returns **ids + summary text only** — it does not embed and does not
/// touch the store. The owning `LlmDreamCycle` clamps each group's `source_ids` to the
/// window it fed, embeds the summary via its own [`EmbeddingProvider`], and emits a
/// `CycleDelta::Synthesize`. This keeps
/// the engine LLM-free and the proposer side-effect-free.
///
/// Requires `Send + Sync`: like the other consumer providers, a proposer is shared
/// across the engine's worker threads. The method is **synchronous** — an HTTP impl
/// uses a blocking client (mirroring [`EmbeddingProvider`]), so async does not leak
/// into the [`DreamCycle`] contract.
pub trait DeltaProposer: Send + Sync {
    /// Propose merge groups over `window` (the candidate facts to consolidate),
    /// given `prior_wisdom` (already-promoted wisdom facts) as retrieve-before-reflect
    /// context so the proposer can avoid re-deriving existing wisdom.
    ///
    /// An empty proposal (nothing worth merging) is a valid, non-error result.
    ///
    /// # Errors
    ///
    /// Returns an error if the proposer fails (e.g. an LLM call or a parse failure).
    fn propose(&self, window: &[Fact], prior_wisdom: &[Fact]) -> Result<ConsolidationProposal>;
}

/// Trait for arbitrating conflicts between contradicting facts.
///
/// Requires `Send + Sync`: arbiters are shared across the engine's worker
/// threads alongside the other consumer providers.
pub trait ConflictArbiter: Send + Sync {
    /// Decide how to resolve a conflict between an existing and a new fact.
    ///
    /// **Arbiter input caveat:** when called from
    /// `MemoryEngine::resolve_conflict`,
    /// `new_fact` is a pre-insert synthetic [`Fact`] built via
    /// [`Fact::from_new_for_arbiter`](me_types::types::Fact) from a
    /// [`NewFact`](me_types::types::NewFact) before it has been persisted or scored.
    /// Its `id` is always `0` (not yet assigned by the DB) and `importance_score`
    /// is always the
    /// [`Fact::UNSCORED_IMPORTANCE`](me_types::types::Fact::UNSCORED_IMPORTANCE)
    /// sentinel (`0.5`), NOT the eventual stored score. Implementations must rely
    /// on `content`, `fact_type`, `base_importance`, and `metadata` — never on
    /// `id` or `importance_score`.
    ///
    /// # Errors
    ///
    /// Returns an error if arbitration fails.
    fn arbitrate(&self, old_fact: &Fact, new_fact: &Fact) -> Result<CrudDecision>;
}

/// Decision for conflict resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CrudDecision {
    Add,
    Update,
    Delete,
    Noop,
}

// --- Phase 3b: Persistence classifier ---

/// Trait for classifying whether a fact should be pinned (unforgettable).
///
/// Consumers implement this to apply domain-specific rules:
/// LLM-based classification, regex matching, importance thresholds, etc.
///
/// Default implementation returns `false` — opt-in, zero behavior change.
///
/// **Classifier input:** `should_pin` receives a [`ClassifierInput`] — an owned
/// view of the *only* four fields a classifier is authorised to read at
/// classification time: `content`, `fact_type`, `importance` (the caller hint),
/// and `metadata`. The fact is pre-insert, so its `id`, `scope_id`,
/// `importance_score`, graph connectivity, and timestamps are either unassigned
/// or off-limits and are deliberately **not** exposed. This replaced an earlier
/// `&Fact` signature whose callers built a 20-field synthetic `Fact` (cloning the
/// embedding) purely to satisfy the parameter type (#118/#343/#388).
///
/// Requires `Send + Sync`: classifiers are shared across the engine's worker
/// threads alongside the other consumer providers.
pub trait PersistenceClassifier: Send + Sync {
    /// Decide if a fact should be pinned (never forgotten).
    fn should_pin(&self, input: &ClassifierInput) -> bool {
        let _ = input;
        false
    }
}

// --- Phase 4a: Reranker ---

/// Trait for reranking search results after initial retrieval.
///
/// Cross-encoder rerankers score (query, candidate) pairs precisely,
/// improving nDCG@10 by 5-15% on top-K candidates after RRF merge.
///
/// Optional — when no reranker is provided, RRF results pass through unchanged.
///
/// # Contract
///
/// - Input: query text + candidates from hybrid search (FTS + vector + RRF)
/// - Output: `Vec<(usize, f64)>` — each tuple is `(index_into_candidates, new_score)`
/// - The returned vec length must be <= input length
/// - Every index must be in range `0..candidates.len()`
/// - No duplicate indices in the output
/// - All scores must be finite (not NaN or Inf)
///
/// Returning indices instead of full `SearchResult` values **structurally prevents**
/// the reranker from mutating fact content, embeddings, or match types (issue #144).
///
/// These invariants are enforced at runtime by `MemoryEngine::query()`.
/// Violations produce `MemoryError::Reranker` with the matching
/// [`RerankerError`](me_types::error::RerankerError) variant
/// (`OutputTooLong`, `OutOfBoundsIndex`, `DuplicateIndex`, `NonFiniteScore`).
pub trait Reranker: Send + Sync {
    /// Rerank candidates for the given query text.
    ///
    /// Returns `(index, score)` pairs referencing positions in the `candidates` slice.
    /// The engine reconstructs the final result set from these indices, preserving
    /// the original `Fact` and `MatchType` values unchanged.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Reranker` (wrapping
    /// [`RerankerError::Provider`](me_types::error::RerankerError::Provider)) if
    /// reranking fails (e.g., API call, inference error).
    fn rerank(&self, query: &str, candidates: &[SearchResult]) -> Result<Vec<(usize, f64)>>;

    /// Human-readable name for logging and debug output.
    fn name(&self) -> &str;
}

// --- Phase 5a: Cognitive pipeline traits ---

/// Fast-path capture for high-value observations from the intelligence layer.
///
/// Consumer-implemented. Called in real-time during conversations to record
/// insights that should resist decay. Implementations should be cheap —
/// typically just writing to a buffer or database.
///
/// Passed as `&dyn InsightStream` per-call (not stored in the engine).
///
/// Requires `Send + Sync` for consistency with the other consumer traits
/// (#386): although `record` is invoked synchronously and the trait object is
/// never sent across an `.await`, carrying the same bound as every sibling
/// provider keeps the consumer-facing API surface uniform — a downstream
/// `Arc<dyn InsightStream>` behaves like every other `Arc<dyn …Provider>`.
pub trait InsightStream: Send + Sync {
    /// Record a high-value insight.
    ///
    /// # Errors
    ///
    /// Returns an error if recording fails.
    fn record(&self, insight: me_types::types::Insight) -> Result<()>;
}

/// The engine capability bag handed to a [`DreamCycle`] (via [`CycleCtx`]'s supertrait).
///
/// More broadly, this is the "narrowed `MemoryEngine`" surface: read, engine-internal
/// batch ops (consolidate/forget), and the one new write path (promotion).
///
/// # History (ADR 0014 decision #3, restored — Wave 2 #816, S5, closes #981)
///
/// ADR 0014 deliberately preserved this capability bag **by composition**:
/// `CycleContext` wrapped a concrete `DreamContext` struct (`engine: &'a
/// MemoryEngine`), and `CycleContext::dream()` exposed it. S1 (#816) re-typed
/// `DreamCycle::run` from `&CycleContext` to `&dyn CycleCtx` — necessary so `me-traits`
/// (L0.5) never names the facade type that owns the cycle's read-set — but that made
/// `DreamContext` **unreachable**: its constructor was `pub(crate)`, its sole accessor
/// was `CycleContext::dream()`, and a `&dyn CycleCtx` cannot be downcast. Seven of its
/// nine methods (everything except the two `CycleCtx` happened to duplicate) went from
/// "capability bag" to "dead code with zero call sites" — a regression no ADR amendment
/// recorded and no green build could catch (`dead_code` does not fire on `pub` items).
///
/// S5 restores the contract properly: the bag is promoted **into the trait layer**
/// itself, so a `&dyn DreamCtx` — or, via the [`CycleCtx`] supertrait, a `&dyn
/// CycleCtx` — carries the full capability set natively, no downcast and no engine
/// type in `me-traits`.
///
/// There are exactly two implementors: `EngineDreamCtx`, a **private borrow-newtype**
/// over `&MemoryEngine` in the facade, and `CycleContext` in `me-cognitive`, which
/// forwards to a held `&dyn DreamCtx`.
///
/// # ⚠️ Before you add a third
///
/// **Do not `impl DreamCtx for T` when `T` has inherent methods sharing these names**
/// (`query`, `list_active_facts`, `get_fact`, `consolidate`, `forget` all collide on
/// `MemoryEngine`). Rust resolves inherent-before-trait, so such an impl works only
/// until the inherent method is renamed — after which the call silently re-resolves to
/// the trait method being defined: unbounded recursion, stack overflow, in the
/// **consumer's** process. Qualifying (`Self::query(self, q)`) does **not** help — same
/// resolution order — and `unconditional_recursion` does **not** fire through
/// `#[async_trait]`, so `-D warnings` stays green. Route the impl through a newtype
/// whose inner type has no `DreamCtx` impl in scope; a rename is then `E0599`. See
/// `EngineDreamCtx`'s doc and ADR 0014's Wave 2 / S5 amendment.
///
/// `Send + Sync` (like every sibling consumer trait, #631/#386): a `&dyn DreamCtx` is
/// borrowed across `.await` inside a `Send` future.
#[async_trait]
pub trait DreamCtx: Send + Sync {
    /// Run a hybrid query (FTS5 + vector + graph, RRF merge).
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    async fn query(&self, query: &SearchQuery) -> Result<Vec<SearchResult>>;

    /// List all active (non-expired) facts, optionally limited.
    ///
    /// # Errors
    ///
    /// Returns an error if the store read fails.
    async fn list_active_facts(&self, limit: Option<usize>) -> Result<Vec<Fact>>;

    /// Retrieve a single fact by ID.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if no fact with this ID exists.
    async fn get_fact(&self, id: i64) -> Result<Fact>;

    /// Run engine-internal consolidation (dedup → cluster → global summaries).
    ///
    /// `generator` produces the summary text; `embedder` projects it into the fact
    /// vector space (issue #116). Both are `Arc<dyn _>` so the implementor can offload
    /// the (possibly blocking) consumer calls off the async executor.
    ///
    /// # Errors
    ///
    /// Returns an error if consolidation fails.
    async fn consolidate(
        &self,
        generator: Arc<dyn SummaryGenerator>,
        embedder: Arc<dyn EmbeddingProvider>,
        config: &ConsolidationConfig,
    ) -> Result<ConsolidationStats>;

    /// Run Ebbinghaus decay + pruning on stale facts.
    ///
    /// # Errors
    ///
    /// Returns an error if the forget operation fails.
    async fn forget(&self, policy: &ForgetPolicy) -> Result<PruneStats>;

    /// Atomically promote a fact to wisdom with lineage tracking.
    ///
    /// # Errors
    ///
    /// Returns an error if the promotion fails (dimension mismatch, DB error, etc.).
    async fn promote(&self, req: &PromoteRequest) -> Result<PromotionResult>;

    /// List active facts in `window` that have not yet been dream-cycled.
    ///
    /// This is a cycle's input-selection query: the metadata `dream_cycle` marker
    /// excludes facts a previous cycle already processed (idempotency). Root scope,
    /// all fact types.
    ///
    /// # Errors
    ///
    /// Returns an error if the store read fails.
    async fn list_undreamt_in_period(&self, window: TimeWindow) -> Result<Vec<Fact>>;

    /// Aggregated outcome counts for a fact (for importance rescoring).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if the fact does not exist, or a store error.
    async fn outcome_counts(&self, fact_id: i64) -> Result<OutcomeCounts>;

    /// Aggregated outcome counts for many facts in a single query (batch rescoring).
    ///
    /// The batch form of [`Self::outcome_counts`] — one `GROUP BY` scan instead of one
    /// query per fact. Facts with no outcomes (or unknown ids) are absent from the map;
    /// callers treat a missing key as
    /// [`OutcomeCounts::default`](me_types::types::OutcomeCounts).
    ///
    /// # Errors
    ///
    /// Returns a store error if the query fails.
    async fn outcome_counts_batch(&self, fact_ids: &[i64]) -> Result<HashMap<i64, OutcomeCounts>>;
}

/// The read-capability handle a [`DreamCycle`] receives during [`run`](DreamCycle::run).
///
/// `DreamCycle::run` is **planning**, not writing: it reads the time window and prior
/// state and returns a delta-based [`CycleReport`] for the caller to apply. `CycleCtx`
/// adds the retrieve-before-reflect surface (the window, prior wisdom, prior cycle
/// metadata) on top of the [`DreamCtx`] supertrait, which supplies the engine
/// capability bag — including `list_undreamt_in_period` / `outcome_counts_batch`,
/// **inherited**, not duplicated (see [`DreamCtx`]'s own doc for why that duplication
/// existed and was removed in Wave 2 #816 / S5). It is implemented by the engine's
/// concrete `CycleContext`; abstracting it behind this trait is what lets the
/// `DreamCycle` contract name no engine/consolidation type (Wave 2 #816 — the trait
/// layer stays a leaf over the data layer). The write capabilities the supertrait
/// exposes (consolidate / forget / promote) are available here too, but the shipped
/// cycles run them later, through the engine's apply path — not through this handle.
#[async_trait]
pub trait CycleCtx: DreamCtx {
    /// The window of facts this cycle was asked to process.
    fn time_window(&self) -> TimeWindow;

    /// Previously promoted wisdom (active, pinned) — read to avoid re-detecting
    /// already-promoted patterns (generative-output isolation).
    fn prior_wisdom(&self) -> &[Fact];

    /// Metadata of recent prior cycles (newest last), for cycle-id sequencing and
    /// drift detection against existing wisdom.
    fn prior_reports(&self) -> &[CycleMetadata];
}

/// Periodic batch processing: consolidation, pattern detection, promotion.
///
/// Consumer-implemented. Called on a schedule (e.g., end of session, daily). `run`
/// receives a [`CycleCtx`] — the retrieve-before-reflect read surface (time window +
/// prior wisdom + prior cycle metadata, plus `list_undreamt_in_period` /
/// `outcome_counts_batch`). The engine's concrete
/// `CycleContext` implements it.
///
/// `run` **proposes** mutations as a delta-based `CycleReport`;
/// it does not write to the store. The caller applies the report via
/// `MemoryEngine::apply_cycle_report` — the
/// produce/apply split is what enables a human review gate before promotion.
///
/// Passed as `&dyn DreamCycle` per-call (not stored in the engine).
///
/// `run` is async (`#[async_trait]`): the [`CycleCtx`] read methods await the storage
/// port.
///
/// The `Send + Sync` supertrait bound (#631) keeps the engine's
/// `run_dream_cycle_guarded(&self, cycle: &dyn DreamCycle)` future `Send`: borrowing a
/// trait object across the `.await` requires `dyn DreamCycle: Sync` (so `&dyn DreamCycle:
/// Send`), and `#[async_trait]` additionally needs `Self: Send` for the boxed future. A
/// multi-threaded async consumer (the MCP `ServerHandler`, whose `call_tool` future must
/// be `Send`) cannot drive the cycle otherwise. All shipped impls (`DefaultDreamCycle`,
/// `LlmDreamCycle`, the deterministic test doubles) are already `Send + Sync` — the bound
/// only forbids a future implementor with interior, thread-unsafe state.
#[async_trait]
pub trait DreamCycle: Send + Sync {
    /// Run one cycle of the cognitive pipeline, returning a delta-based report.
    ///
    /// `ctx` is the [`CycleCtx`] read surface (the engine passes its concrete
    /// `CycleContext`).
    ///
    /// # Contract
    ///
    /// **Every fact the cycle touches — selects for its window, adjusts (`AdjustScore`),
    /// or tags (`TagOutcome`) — MUST appear in
    /// `CycleMetadata::processed_ids`, whether or not it produced
    /// a delta.** At apply time those ids are stamped with the `dream_cycle` marker, which
    /// (a) makes a re-run idempotent and (b) removes them from the #209 caller-write
    /// signal (`MemoryEngine::run_dream_cycle_guarded`).
    /// An implementation that omits a selected-but-no-delta fact leaves it permanently
    /// "caller-written" — the guarded cycle would then defer forever. The shipped
    /// `DefaultDreamCycle` satisfies this by construction.
    ///
    /// # Errors
    ///
    /// Returns an error if the cycle fails.
    async fn run(&self, ctx: &dyn CycleCtx) -> Result<CycleReport>;
}

/// Configuration for the consolidation process.
///
/// `#[non_exhaustive]` (#344): construct it from outside the crate via
/// [`ConsolidationConfig::builder`] or [`ConsolidationConfig::default`], not a
/// struct literal — so adding a future tuning field never breaks downstream call
/// sites. Defaults are the research-backed values (dedup 0.90, cluster 0.85,
/// `min_cluster_size` 2).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ConsolidationConfig {
    /// Minimum cosine similarity for two facts to be treated as duplicates.
    ///
    /// The dedup pass merges any pair whose similarity is **≥** this value
    /// (expiring the lower-importance fact). Lives in `[0.0, 1.0]`:
    /// `1.0` deduplicates only exact duplicates (identical embeddings), lower
    /// values fold in progressively looser near-duplicates.
    pub dedup_threshold: f32,
    /// Cosine-similarity threshold for grouping related facts into a cluster
    /// (single-linkage). Lower than `dedup_threshold`: clustering is looser than
    /// dedup. Previously a hard-coded `CLUSTER_SIMILARITY_THRESHOLD` constant (#344).
    pub cluster_threshold: f32,
    /// Minimum number of facts a similarity group must contain to be summarized
    /// into a cluster. Must be ≥ 2 — a single-fact "cluster" is not a cluster.
    pub min_cluster_size: usize,
}

impl Default for ConsolidationConfig {
    /// Canonical defaults: `dedup_threshold = 0.90`, `cluster_threshold = 0.85`,
    /// `min_cluster_size = 2`.
    ///
    /// Hand-written rather than derived: a derived `Default` would yield
    /// `0.0` / `0`, and `min_cluster_size = 0` fails [`validate`](Self::validate).
    fn default() -> Self {
        Self {
            dedup_threshold: 0.90,
            cluster_threshold: 0.85,
            min_cluster_size: 2,
        }
    }
}

impl ConsolidationConfig {
    /// Start building a config from the [defaults](ConsolidationConfig::default).
    ///
    /// Preferred construction path now that the struct is `#[non_exhaustive]`;
    /// only override the fields you care about.
    #[must_use]
    pub fn builder() -> ConsolidationConfigBuilder {
        ConsolidationConfigBuilder(Self::default())
    }

    /// Validate configuration parameters.
    ///
    /// Enforced at the consolidation entry point
    /// (`MemoryEngine::consolidate`), mirroring
    /// `ForgetPolicy::validate` at the
    /// forget entry point.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Conflict` if `dedup_threshold` or `cluster_threshold`
    /// is not a finite value in `[0.0, 1.0]` (each is a cosine-similarity gate), or
    /// if `min_cluster_size < 2` (a cluster requires at least two members to be
    /// fused into a summary).
    pub fn validate(&self) -> Result<()> {
        use me_types::error::{ConflictError, MemoryError};

        if !self.dedup_threshold.is_finite() || !(0.0..=1.0).contains(&self.dedup_threshold) {
            return Err(MemoryError::Conflict(ConflictError::PolicyParameter(
                format!(
                    "dedup_threshold must be a finite value in [0.0, 1.0], got {}",
                    self.dedup_threshold
                ),
            )));
        }
        if !self.cluster_threshold.is_finite() || !(0.0..=1.0).contains(&self.cluster_threshold) {
            return Err(MemoryError::Conflict(ConflictError::PolicyParameter(
                format!(
                    "cluster_threshold must be a finite value in [0.0, 1.0], got {}",
                    self.cluster_threshold
                ),
            )));
        }
        if self.min_cluster_size < 2 {
            return Err(MemoryError::Conflict(ConflictError::PolicyParameter(
                format!(
                    "min_cluster_size must be >= 2 (a cluster requires at least two members), \
                     got {}",
                    self.min_cluster_size
                ),
            )));
        }
        Ok(())
    }
}

/// Builder for [`ConsolidationConfig`].
///
/// Exists because the config is `#[non_exhaustive]` and so cannot be built with a
/// struct literal (or functional-update) from another crate. New config fields get
/// a setter here without breaking any existing call site (#344).
#[derive(Debug, Clone)]
pub struct ConsolidationConfigBuilder(ConsolidationConfig);

impl ConsolidationConfigBuilder {
    /// Set the dedup near-duplicate cosine threshold.
    #[must_use]
    pub const fn dedup_threshold(mut self, value: f32) -> Self {
        self.0.dedup_threshold = value;
        self
    }

    /// Set the cluster single-linkage cosine threshold.
    #[must_use]
    pub const fn cluster_threshold(mut self, value: f32) -> Self {
        self.0.cluster_threshold = value;
        self
    }

    /// Set the minimum cluster size that earns a summary.
    #[must_use]
    pub const fn min_cluster_size(mut self, value: usize) -> Self {
        self.0.min_cluster_size = value;
        self
    }

    /// Finalize the configuration.
    #[must_use]
    pub const fn build(self) -> ConsolidationConfig {
        self.0
    }
}

/// Statistics returned by consolidation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsolidationStats {
    pub duplicates_removed: usize,
    pub clusters_created: usize,
    pub global_summaries: usize,
}

/// Result of a conflict resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictResolution {
    pub decision: CrudDecision,
    pub old_fact_id: i64,
    pub new_fact_id: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use me_types::types::FactType;

    fn stub_classifier_input() -> ClassifierInput {
        ClassifierInput {
            content: String::new(),
            fact_type: FactType::Semantic,
            base_importance: 0.5,
            metadata: serde_json::Value::Null,
        }
    }

    // --- ConsolidationConfig::default() ---

    #[test]
    fn consolidation_config_default_has_expected_field_values() {
        let c = ConsolidationConfig::default();
        assert!((c.dedup_threshold - 0.90).abs() < f32::EPSILON);
        assert_eq!(c.min_cluster_size, 2);
    }

    #[test]
    fn consolidation_config_default_validates_ok() {
        ConsolidationConfig::default().validate().unwrap();
    }

    // --- ConsolidationConfig::validate(): dedup_threshold ∈ [0, 1] ---

    #[test]
    fn validate_rejects_dedup_threshold_above_one() {
        let c = ConsolidationConfig {
            dedup_threshold: 1.01,
            ..Default::default()
        };
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("dedup_threshold"), "error: {err}");
    }

    #[test]
    fn validate_rejects_negative_dedup_threshold() {
        let c = ConsolidationConfig {
            dedup_threshold: -0.01,
            ..Default::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_accepts_boundary_dedup_threshold() {
        for val in [0.0_f32, 1.0_f32] {
            let c = ConsolidationConfig {
                dedup_threshold: val,
                ..Default::default()
            };
            c.validate().unwrap();
        }
    }

    #[test]
    fn validate_rejects_nan_dedup_threshold() {
        let c = ConsolidationConfig {
            dedup_threshold: f32::NAN,
            ..Default::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_rejects_infinity_dedup_threshold() {
        for val in [f32::INFINITY, f32::NEG_INFINITY] {
            let c = ConsolidationConfig {
                dedup_threshold: val,
                ..Default::default()
            };
            assert!(c.validate().is_err(), "{val} should be rejected");
        }
    }

    // --- ConsolidationConfig::validate(): cluster_threshold ∈ [0, 1] (#344) ---

    #[test]
    fn validate_rejects_cluster_threshold_out_of_range() {
        for val in [1.01_f32, -0.01_f32] {
            let c = ConsolidationConfig {
                cluster_threshold: val,
                ..Default::default()
            };
            let err = c.validate().unwrap_err().to_string();
            assert!(
                err.contains("cluster_threshold"),
                "cluster_threshold={val} should reject; error: {err}"
            );
        }
    }

    #[test]
    fn validate_rejects_non_finite_cluster_threshold() {
        for val in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let c = ConsolidationConfig {
                cluster_threshold: val,
                ..Default::default()
            };
            assert!(c.validate().is_err(), "{val} should be rejected");
        }
    }

    // --- ConsolidationConfig::validate(): min_cluster_size >= 2 ---

    #[test]
    fn validate_rejects_min_cluster_size_below_two() {
        for val in [0_usize, 1_usize] {
            let c = ConsolidationConfig {
                min_cluster_size: val,
                ..Default::default()
            };
            let err = c.validate().unwrap_err().to_string();
            assert!(
                err.contains("min_cluster_size"),
                "min_cluster_size={val} should reject; error: {err}"
            );
        }
    }

    #[test]
    fn validate_accepts_min_cluster_size_two_and_above() {
        for val in [2_usize, 3, 10] {
            let c = ConsolidationConfig {
                min_cluster_size: val,
                ..Default::default()
            };
            c.validate().unwrap();
        }
    }

    // --- Trait object safety ---

    #[test]
    fn embedding_provider_is_object_safe() {
        struct Dummy;
        impl EmbeddingProvider for Dummy {
            fn embed(&self, _text: &str) -> me_types::error::Result<Vec<f32>> {
                Ok(vec![0.0])
            }
            fn fingerprint(&self) -> me_types::types::EmbeddingFingerprint {
                me_types::types::EmbeddingFingerprint::new("mock", "test", 1)
            }
        }
        let provider: &dyn EmbeddingProvider = &Dummy;
        // fingerprint() must be callable through the trait object (vtable) — guards
        // against the new required method accidentally breaking object-safety.
        assert_eq!(provider.fingerprint().dim, 1);
    }

    #[test]
    fn summary_generator_is_object_safe() {
        struct Dummy;
        impl SummaryGenerator for Dummy {
            fn summarize(
                &self,
                _items: &[SummarizableContent<'_>],
            ) -> me_types::error::Result<String> {
                Ok(String::new())
            }
        }
        let _: &dyn SummaryGenerator = &Dummy;
    }

    #[test]
    fn conflict_arbiter_is_object_safe() {
        struct Dummy;
        impl ConflictArbiter for Dummy {
            fn arbitrate(&self, _old: &Fact, _new: &Fact) -> me_types::error::Result<CrudDecision> {
                Ok(CrudDecision::Noop)
            }
        }
        let _: &dyn ConflictArbiter = &Dummy;
    }

    #[test]
    fn persistence_classifier_is_object_safe() {
        struct Dummy;
        impl PersistenceClassifier for Dummy {}
        let p: &dyn PersistenceClassifier = &Dummy;
        // Default impl returns false
        assert!(!p.should_pin(&stub_classifier_input()));
    }

    #[test]
    fn reranker_is_object_safe() {
        struct Dummy;
        impl Reranker for Dummy {
            fn rerank(
                &self,
                _query: &str,
                _candidates: &[SearchResult],
            ) -> me_types::error::Result<Vec<(usize, f64)>> {
                Ok(vec![])
            }
            fn name(&self) -> &'static str {
                "dummy"
            }
        }
        let r: &dyn Reranker = &Dummy;
        assert_eq!(r.name(), "dummy");
    }

    // --- EmbeddingProvider::embed_batch default ---

    #[test]
    fn embed_batch_default_loops_embed() {
        struct Counter(std::sync::atomic::AtomicUsize);
        impl EmbeddingProvider for Counter {
            fn embed(&self, _text: &str) -> me_types::error::Result<Vec<f32>> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(vec![1.0, 2.0])
            }
            fn fingerprint(&self) -> me_types::types::EmbeddingFingerprint {
                me_types::types::EmbeddingFingerprint::new("mock", "test", 2)
            }
        }
        let c = Counter(std::sync::atomic::AtomicUsize::new(0));
        let results = c.embed_batch(&["a", "b", "c"]).unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(c.0.load(std::sync::atomic::Ordering::Relaxed), 3);
        assert!(results.iter().all(|v| v.len() == 2
            && (v[0] - 1.0).abs() < f32::EPSILON
            && (v[1] - 2.0).abs() < f32::EPSILON));
    }

    #[test]
    fn embed_batch_empty_returns_empty() {
        struct Dummy;
        impl EmbeddingProvider for Dummy {
            fn embed(&self, _text: &str) -> me_types::error::Result<Vec<f32>> {
                Ok(vec![0.0])
            }
            fn fingerprint(&self) -> me_types::types::EmbeddingFingerprint {
                me_types::types::EmbeddingFingerprint::new("mock", "test", 1)
            }
        }
        assert!(Dummy.embed_batch(&[]).unwrap().is_empty());
    }

    #[test]
    fn embed_batch_propagates_error() {
        struct Failing;
        impl EmbeddingProvider for Failing {
            fn embed(&self, _text: &str) -> me_types::error::Result<Vec<f32>> {
                Err(me_types::error::MemoryError::Conflict(
                    me_types::error::ConflictError::Arbitration("boom".into()),
                ))
            }
            fn fingerprint(&self) -> me_types::types::EmbeddingFingerprint {
                me_types::types::EmbeddingFingerprint::new("mock", "test", 1)
            }
        }
        assert!(Failing.embed_batch(&["a"]).is_err());
    }

    // --- EmbeddingProvider::embed_query / embed_query_batch defaults (#616) ---

    #[test]
    #[allow(clippy::cast_precision_loss)] // test data, precision irrelevant
    fn embed_query_default_delegates_to_embed() {
        // A symmetric provider leaves both query methods at their defaults, so a
        // query embeds identically to a document — Mock/Hash/Passthrough are
        // unaffected by the asymmetric trait extension.
        struct Symmetric;
        impl EmbeddingProvider for Symmetric {
            fn embed(&self, text: &str) -> me_types::error::Result<Vec<f32>> {
                Ok(vec![text.len() as f32])
            }
            fn fingerprint(&self) -> me_types::types::EmbeddingFingerprint {
                me_types::types::EmbeddingFingerprint::new("mock", "test", 1)
            }
        }
        let p = Symmetric;
        assert_eq!(p.embed_query("hello").unwrap(), p.embed("hello").unwrap());
    }

    #[test]
    #[allow(clippy::cast_precision_loss)] // test data, precision irrelevant
    fn embed_query_batch_default_loops_embed_query() {
        // The default `embed_query_batch` must loop `embed_query` (the asymmetric
        // singular seam), NOT delegate to `embed_batch`. This guarantees that an
        // asymmetric provider overriding only `embed_query` (a query prefix) still
        // gets prefixed batch queries — correctness by default, no silent leak into
        // document space. Batch efficiency is the explicit opt-in (override this).
        struct QueryCounter {
            query_calls: std::sync::atomic::AtomicUsize,
        }
        impl EmbeddingProvider for QueryCounter {
            fn embed(&self, text: &str) -> me_types::error::Result<Vec<f32>> {
                Ok(vec![text.len() as f32])
            }
            fn embed_query(&self, text: &str) -> me_types::error::Result<Vec<f32>> {
                self.query_calls
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                // Simulate a query prefix: documents would NOT see this.
                self.embed(&format!("Q:{text}"))
            }
            fn fingerprint(&self) -> me_types::types::EmbeddingFingerprint {
                me_types::types::EmbeddingFingerprint::new("mock", "test", 1)
            }
        }
        let p = QueryCounter {
            query_calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let out = p.embed_query_batch(&["a", "bb", "ccc"]).unwrap();
        // Each element carries the "Q:" prefix length offset (+2), proving the
        // default routed through the overridden `embed_query`, not `embed`/`embed_batch`.
        assert_eq!(out, vec![vec![3.0], vec![4.0], vec![5.0]]);
        assert_eq!(
            p.query_calls.load(std::sync::atomic::Ordering::Relaxed),
            3,
            "embed_query_batch default must call embed_query once per input"
        );
    }

    #[test]
    #[allow(clippy::cast_precision_loss)] // test data, precision irrelevant
    fn embed_query_override_does_not_affect_documents() {
        // An asymmetric provider overrides the query path (here: prefix the text)
        // while documents stay on the prefix-free `embed`. Proves the two paths are
        // independently dispatchable through the trait object (vtable).
        struct Asymmetric;
        impl EmbeddingProvider for Asymmetric {
            fn embed(&self, text: &str) -> me_types::error::Result<Vec<f32>> {
                Ok(vec![text.len() as f32])
            }
            fn embed_query(&self, text: &str) -> me_types::error::Result<Vec<f32>> {
                // Simulate a query instruction prefix lengthening the input.
                self.embed(&format!("Q:{text}"))
            }
            fn fingerprint(&self) -> me_types::types::EmbeddingFingerprint {
                me_types::types::EmbeddingFingerprint::new("mock", "test", 1)
            }
        }
        let p: &dyn EmbeddingProvider = &Asymmetric;
        assert_eq!(p.embed("hi").unwrap(), vec![2.0]);
        assert_eq!(p.embed_query("hi").unwrap(), vec![4.0]); // "Q:hi" → len 4
    }

    // --- PersistenceClassifier default ---

    #[test]
    fn persistence_classifier_default_returns_false() {
        struct Blank;
        impl PersistenceClassifier for Blank {}
        assert!(!Blank.should_pin(&stub_classifier_input()));
    }
}
