use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// --- Enums ---

/// Type of event in the append-only log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventType {
    Interaction,
    ToolCall,
    MemoryOp,
    SystemEvent,
    /// Outcome feedback signal for a fact (positive, negative, or neutral).
    /// Payload carries `{"fact_id": i64, "outcome": "Positive"|"Negative"|"Neutral"}`.
    OutcomeSignal,
}

/// Outcome of using a fact — consumer-supplied feedback signal.
///
/// Stored as an [`EventType::OutcomeSignal`] event in the append-only log.
/// `DreamCycle` queries outcome history to adjust importance scores:
/// consistently negative outcomes decrease importance, positive ones increase it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Outcome {
    Positive,
    Negative,
    Neutral,
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Positive => write!(f, "positive"),
            Self::Negative => write!(f, "negative"),
            Self::Neutral => write!(f, "neutral"),
        }
    }
}

/// Aggregated outcome counts for a single fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OutcomeCounts {
    pub positive: u32,
    pub negative: u32,
    pub neutral: u32,
}

/// Type of fact (`CoALA` mapping: Episodic, Semantic, Procedural).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FactType {
    Episodic,
    Semantic,
    Procedural,
}

impl fmt::Display for FactType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Episodic => write!(f, "episodic"),
            Self::Semantic => write!(f, "semantic"),
            Self::Procedural => write!(f, "procedural"),
        }
    }
}

/// Error returned when a string does not name a known [`FactType`] variant.
///
/// Carries the offending token so callers can surface an actionable message.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown fact type: {0}")]
pub struct ParseFactTypeError(pub String);

impl FromStr for FactType {
    type Err = ParseFactTypeError;

    /// Canonical, case-insensitive parse of the `FactType` variant names.
    ///
    /// This is the **single source of truth** for the string→`FactType` mapping
    /// shared by every consumer surface (the CLI's `--fact-type` arg and JSONL
    /// ingest, the MCP server's tool parameters). It is intentionally lenient on
    /// casing so it accepts both wire conventions present in the codebase:
    /// `Display` emits `snake_case` (`"episodic"`), while serde-derive and the MCP
    /// JSON-schema enums use `PascalCase` (`"Episodic"`). Parsing reconciles both
    /// to one canonical enum; [`FactType::to_string`] remains the canonical output.
    ///
    /// Note: this is orthogonal to the serde `Deserialize` derive, which stays
    /// `PascalCase` to preserve `.pak` cold-storage archive back-compat.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Zero-allocation case-insensitive match (no temporary lowercased String).
        if s.eq_ignore_ascii_case("episodic") {
            Ok(Self::Episodic)
        } else if s.eq_ignore_ascii_case("semantic") {
            Ok(Self::Semantic)
        } else if s.eq_ignore_ascii_case("procedural") {
            Ok(Self::Procedural)
        } else {
            Err(ParseFactTypeError(s.to_owned()))
        }
    }
}

/// Consolidation level for summaries.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ConsolidationLevel {
    Local,
    Cluster,
    Global,
}

impl fmt::Display for ConsolidationLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
            Self::Cluster => write!(f, "cluster"),
            Self::Global => write!(f, "global"),
        }
    }
}

// --- Full structs (with id, as returned from DB) ---

/// An event in the append-only log (source of truth).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub timestamp: DateTime<Utc>,
    pub event_type: EventType,
    pub payload: serde_json::Value,
    pub source: String,
    pub session_id: Option<String>,
    pub scope_id: i64,
    /// Node that originated this event (for future multi-node sync).
    #[serde(default = "default_origin_node_id")]
    pub origin_node_id: String,
    /// Monotonic sequence within the origin node (for ordering/dedup in sync).
    #[serde(default)]
    pub sequence_id: i64,
    /// When the event was ingested into this node's store (ingest-time).
    pub created_at: Option<DateTime<Utc>>,
    /// Schema revision of the event payload (for upcasting at read time).
    #[serde(default = "default_event_revision")]
    pub event_revision: u16,
}

const fn default_event_revision() -> u16 {
    1
}

fn default_origin_node_id() -> String {
    "local".to_string()
}

/// Default `scope_id` for a `Fact` deserialized from an archive that predates
/// the scope column — the root scope (id 1). Keeps old `.pak` archives readable.
const fn default_root_scope_id() -> i64 {
    1
}

/// A bi-temporal fact derived from events.
///
/// Two independent time axes: **transaction-time** (`t_created`/`t_expired`) records when
/// the row existed in the store; **valid-time** (`t_valid`/`t_invalid`) records when the
/// fact was true in the world. A `None` valid-time bound means "unbounded/unknown" — see
/// the per-field docs.
///
/// Importance is likewise split across two fields, easily confused:
/// [`importance`](Self::importance) is the *static, consumer-supplied prior* set at insertion,
/// while [`importance_score`](Self::importance_score) is the *computed, decaying score* the
/// engine ranks and forgets by (the prior is one of its inputs). See those fields' docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fact {
    pub id: i64,
    pub content: String,
    pub content_hash: String,
    pub embedding: Vec<f32>,
    pub fact_type: FactType,
    /// Transaction-time start: when the row was first written. Bootstrap backdates this
    /// to the historical turn timestamp so recency reflects the original session.
    pub t_created: DateTime<Utc>,
    /// Transaction-time end: when the row was soft-deleted; `None` = still present.
    pub t_expired: Option<DateTime<Utc>>,
    /// Valid-time start: when the fact became true in the world. `None` = "valid since
    /// creation" (unbounded/unknown). Active-at queries treat `None` as valid from the
    /// beginning, but the due/scheduling query requires a concrete `t_valid`. Bootstrap
    /// intentionally leaves this `None`: valid-time is not externally asserted for
    /// retro-observed session facts (see issue #521).
    pub t_valid: Option<DateTime<Utc>>,
    /// Valid-time end: when the fact stopped being true; `None` = still valid.
    pub t_invalid: Option<DateTime<Utc>>,
    pub source_event_id: Option<i64>,
    /// **Base importance** — the consumer-supplied prior set once at insertion
    /// (via [`AddFactOptions::importance`], default `0.5`), a finite value in
    /// `[0.0, 1.0]`. The [`add_fact`](crate::MemoryEngine::add_fact) /
    /// `add_facts_batch` entry points validate this range (#571); a few
    /// direct-insert paths (bootstrap, snapshot restore) do not yet enforce it
    /// (#584), so a `Fact` materialized that way could carry an out-of-range value.
    /// It is a *static* hint that never decays; the engine only reads it. It feeds the materialized
    /// [`importance_score`](Self::importance_score) as one of four signals (weight
    /// `base_importance_weight`). Do not confuse with the decayed score below.
    pub importance: f64,
    pub access_count: i64,
    pub last_accessed: DateTime<Utc>,
    pub metadata: serde_json::Value,
    #[serde(default = "default_root_scope_id")]
    pub scope_id: i64,
    #[serde(default)]
    pub is_pinned: bool,
    /// **Materialized importance score** — the *computed, decaying* score the
    /// engine ranks and forgets by, normally in `[0.0, 1.0]` (seeded from the
    /// [`importance`](Self::importance) prior, which the `add_fact` entry points
    /// validate to that range — #571). It is the weighted sum of
    /// four signals (recency via Ebbinghaus decay, access frequency, graph
    /// degree, and the static [`importance`](Self::importance) prior); see
    /// `forgetting::compute_importance`. Seeded to `importance` at ingest, then
    /// recomputed over the fact's lifetime by the forgetting pass and `DreamCycle`,
    /// so it drifts away from the base value as the fact ages and is accessed.
    /// `#[serde(default)]` (0.0) keeps pre-v?-column archives readable.
    #[serde(default)]
    pub importance_score: f64,
    #[serde(default)]
    pub surfaced_at: Option<DateTime<Utc>>,
}

impl Fact {
    /// `importance_score` assigned to a transient [`Fact`] that has never been
    /// scored — e.g. a synthetic candidate built for the
    /// [`ConflictArbiter`](crate::traits::ConflictArbiter), or a pseudo-fact
    /// derived during global consolidation. A neutral midpoint, deliberately
    /// not a real computed score.
    ///
    /// Single source of truth: reference this constant instead of re-typing the
    /// `0.5` literal. Arbiter implementations may compare against it to detect an
    /// unscored candidate.
    pub const UNSCORED_IMPORTANCE: f64 = 0.5;

    /// Whether this fact is **temporally due** at `now`: it has a concrete
    /// valid-time start that has arrived (`t_valid` is `Some` and `<= now`) and is
    /// not yet bi-temporally invalidated (`t_invalid` is `None` or strictly after
    /// `now`).
    ///
    /// This is the single source of truth for the in-Rust due predicate, shared by
    /// the resume surfacing walk and the `explain` `FactState::Due` classifier so
    /// the two cannot silently drift (#477). It is the Rust mirror of the SQL
    /// `WHERE` clause in `FactStore::list_due` / `SchemaManager::statistics` and of
    /// [`TemporalFilter::ValidDue`](crate::storage::TemporalFilter::ValidDue).
    ///
    /// # Scope (what this predicate does NOT cover)
    ///
    /// It is purely the *valid-time* test. Callers retain responsibility for the
    /// orthogonal concerns the SQL bundles in:
    /// - **System-time liveness** (`t_expired IS NULL`): the resume/explain callers
    ///   only ever evaluate facts drawn from active-only reads, so the row is
    ///   already known live. The SQL spells it out because it scans the raw table.
    /// - **Surfacing state** (`surfaced_at`): the resume walk additionally requires
    ///   `surfaced_at.is_none()` to pick the *unsurfaced* subset; that is a
    ///   surfacing concern, not a due concern, and stays at the call site.
    #[must_use]
    pub fn is_temporally_due(&self, now: DateTime<Utc>) -> bool {
        self.t_valid.is_some_and(|tv| tv <= now) && self.t_invalid.is_none_or(|ti| ti > now)
    }
}

/// A graph edge between two facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub id: i64,
    pub source_fact_id: i64,
    pub target_fact_id: i64,
    pub relation_type: String,
    pub weight: f64,
    pub t_created: DateTime<Utc>,
    pub t_expired: Option<DateTime<Utc>>,
    pub scope_id: i64,
}

/// A consolidation summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub id: i64,
    pub content: String,
    pub embedding: Vec<f32>,
    pub level: ConsolidationLevel,
    pub source_fact_ids: Vec<i64>,
    pub created_at: DateTime<Utc>,
    pub scope_id: i64,
}

// --- Scope types ---

/// A node in the hierarchical scope tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeNode {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub label: String,
    pub depth: i64,
}

/// How to resolve scopes for a search query.
/// Paths are consumer-facing strings (e.g., "user:michael/project:demo").
/// The engine resolves them to internal integer IDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeQuery {
    /// Facts at exactly this scope path.
    Exact(String),
    /// Facts at this scope path and all descendants.
    Subtree(String),
    /// Facts at this scope path and all ancestors up to root.
    Ancestors(String),
    /// Facts at ancestors + at this scope path's subtree (full inherited context).
    Inherited(String),
}

// --- Provenance (Phase 5a) ---

/// Lightweight provenance envelope attached to promoted wisdom facts.
///
/// Carries summary statistics about the promotion (how many source facts,
/// across how many sessions, confidence score). It is the **serialized envelope**:
/// every field round-trips through the `lineage.provenance` JSON column and the
/// promoted fact's `metadata.promotion_provenance` key.
///
/// The owning `lineage_id` (DB row PK) is **not** part of this envelope — it is a
/// property of the row, not of the provenance. Read paths return it alongside the
/// envelope in the companion [`LineageRecord`] / [`LineageSnapshotEntry`], and the
/// write path returns it in [`PromotionResult`]. Previously this struct carried a
/// phantom `lineage_id: i64` with `#[serde(skip_serializing, default)]` that was
/// always `0` on deserialization and reconstructed from the PK on read — a lying
/// field with an invisible "0 means not-yet-persisted" invariant. It is removed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromotionProvenance {
    pub source_count: u32,
    pub session_count: u32,
    pub date_range_start: DateTime<Utc>,
    pub date_range_end: DateTime<Utc>,
    pub confidence: f64,
    pub method_version: String,
    /// 3-5 most representative source fact IDs (for quick human review).
    pub representative_ids: Vec<i64>,
}

/// A row in the `lineage` sidecar table (full source chain).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineageRecord {
    pub lineage_id: i64,
    pub wisdom_fact_id: i64,
    pub source_fact_ids: Vec<i64>,
}

/// Insert descriptor for a new lineage record (DB assigns `lineage_id`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewLineageRecord {
    pub wisdom_fact_id: i64,
    pub source_fact_ids: Vec<i64>,
}

/// One proposed merge: a set of source facts an
/// [`LlmDreamCycle`](crate::engine::cycle) should collapse into a single
/// synthesized `summary`.
///
/// The output of a [`DeltaProposer`](crate::traits::DeltaProposer), it is a raw
/// **proposal** — deliberately a plain DTO with no enforced invariants. It crosses
/// the HTTP boundary (an LLM backend deserializes it from model JSON), so rejecting a
/// malformed group at parse time would deny the cycle the chance to clamp it. The
/// `LlmDreamCycle` (A2) is responsible for clamping `source_ids` to the fed window
/// and dropping degenerate groups before turning each into a
/// [`CycleDelta::Synthesize`](crate::engine::cycle::CycleDelta::Synthesize) (which
/// itself enforces a non-empty, all-active source set at apply time).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeGroup {
    /// Ids of the facts to merge (the proposer's raw choice; not yet window-clamped).
    pub source_ids: Vec<FactId>,
    /// The synthesized summary text that replaces the sources. The backend embeds
    /// this itself (the engine stays LLM-free).
    pub summary: String,
}

/// A consolidation backend's proposal: the merge groups it wants applied.
///
/// Returned by [`DeltaProposer::propose`](crate::traits::DeltaProposer::propose). An
/// empty `merges` (the proposer found nothing to consolidate) is a valid state, not an
/// error — the cycle turns it into a no-op report. v1 carries only merges; future
/// proposal kinds (e.g. promotions) are additive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ConsolidationProposal {
    pub merges: Vec<MergeGroup>,
}

/// Complete lineage row for snapshot dump/restore.
///
/// Combines the `LineageRecord` fields with the full `PromotionProvenance`
/// envelope into a single serializable entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LineageSnapshotEntry {
    pub lineage_id: i64,
    pub wisdom_fact_id: i64,
    pub source_fact_ids: Vec<i64>,
    pub provenance: PromotionProvenance,
}

// --- Options ---

/// Optional parameters for [`crate::engine::MemoryEngine::add_fact`].
///
/// All fields default to `None`, which uses the engine's defaults
/// (importance=0.5, metadata={}, no temporal bounds).
#[derive(Debug, Clone, Default)]
pub struct AddFactOptions {
    /// Override default importance (0.5). Must be in [0, 1]; an out-of-range
    /// or non-finite value is rejected with `Conflict(PolicyParameter)` by
    /// [`add_fact`](crate::engine::MemoryEngine::add_fact) and
    /// [`add_facts_batch`](crate::engine::MemoryEngine::add_facts_batch).
    pub importance: Option<f64>,
    /// Override default metadata (empty object).
    pub metadata: Option<serde_json::Value>,
    /// Set the real-world validity start time.
    pub t_valid: Option<DateTime<Utc>>,
    /// Set the real-world validity end time.
    pub t_invalid: Option<DateTime<Utc>>,
    /// Pin this fact (unforgettable). Overrides auto-classification.
    pub pinned: Option<bool>,
    /// Override system creation time (default: `Utc::now()`).
    /// Used by bootstrap to backdate historical facts.
    pub t_created: Option<DateTime<Utc>>,
    /// Override last-accessed time (default: `Utc::now()`).
    /// Used by bootstrap to preserve correct Ebbinghaus decay for historical facts.
    pub last_accessed: Option<DateTime<Utc>>,
}

// --- Fact request ---

/// Input descriptor for [`crate::engine::MemoryEngine::add_fact`] and
/// [`crate::engine::MemoryEngine::add_facts_batch`].
///
/// Bundles all data-level parameters for fact insertion. Infrastructure
/// concerns (embedder, classifier) remain separate method parameters.
#[derive(Debug, Clone)]
pub struct AddFactRequest {
    /// The fact text to embed and store.
    pub content: String,
    /// Semantic, episodic, procedural, etc.
    pub fact_type: FactType,
    /// Optional link to the originating event.
    pub source_event_id: Option<i64>,
    /// Scope path (e.g., `"project/sub"`). `None` → root scope.
    pub scope: Option<String>,
    /// Optional overrides (importance, metadata, temporal bounds, pinned).
    pub opts: Option<AddFactOptions>,
}

// --- New* structs (without id, for insertion) ---

/// Event to insert (DB assigns id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewEvent {
    pub timestamp: DateTime<Utc>,
    pub event_type: EventType,
    pub payload: serde_json::Value,
    pub source: String,
    pub session_id: Option<String>,
    pub scope_id: i64,
    pub origin_node_id: String,
    pub sequence_id: i64,
    pub created_at: Option<DateTime<Utc>>,
}

/// Fact to insert (DB assigns id).
///
/// Has 14 fields, most of which are optional with sensible defaults. Prefer
/// [`NewFact::builder`] over a 14-field struct literal: it requires only the
/// three fields that genuinely vary per call (`content`, `embedding`,
/// `fact_type`) and fills the rest with defaults (see [`NewFactBuilder`]). The
/// struct and its literal construction remain fully public and supported.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewFact {
    pub content: String,
    pub content_hash: String,
    pub embedding: Vec<f32>,
    pub fact_type: FactType,
    pub t_created: DateTime<Utc>,
    pub t_expired: Option<DateTime<Utc>>,
    pub t_valid: Option<DateTime<Utc>>,
    pub t_invalid: Option<DateTime<Utc>>,
    pub source_event_id: Option<i64>,
    pub importance: f64,
    pub access_count: i64,
    pub last_accessed: DateTime<Utc>,
    pub metadata: serde_json::Value,
    pub scope_id: i64,
    pub is_pinned: bool,
}

impl NewFact {
    /// Start building a [`NewFact`] from the three fields that vary at every
    /// call site. The remaining 11 fields take sensible defaults (see
    /// [`NewFactBuilder`]) and can be overridden with the builder's setters.
    ///
    /// ```
    /// use memory_engine::types::{FactType, NewFact};
    ///
    /// let fact = NewFact::builder("user prefers terse replies", vec![0.1; 384], FactType::Semantic)
    ///     .importance(0.8)
    ///     .scope_id(1)
    ///     .build();
    /// assert_eq!(fact.importance, 0.8);
    /// ```
    pub fn builder(
        content: impl Into<String>,
        embedding: Vec<f32>,
        fact_type: FactType,
    ) -> NewFactBuilder {
        NewFactBuilder::new(content, embedding, fact_type)
    }
}

/// The owned view a classifier receives at classification time.
///
/// Passed to [`PersistenceClassifier::should_pin`](crate::traits::PersistenceClassifier::should_pin),
/// it carries the *only* fields a classifier is authorised to read: `content`,
/// `fact_type`, `importance`, and `metadata`.
///
/// It deliberately carries **no** `embedding`, `id`, `scope_id`,
/// `importance_score`, or timestamps: those are either not yet assigned at
/// classification time (the fact is pre-insert) or off-limits per the trait
/// contract. Dropping the embedding alone eliminates a per-fact `Vec<f32>` clone
/// of 384–1536 dimensions (≈1.5–6 KB) that the previous synthetic-`Fact` shim
/// cloned purely to satisfy the `&Fact` parameter (#388); collapsing the 20-field
/// shim to these four removes the duplicated literal at every classify site
/// (#118) and the confusion over which fields matter (#343).
///
/// Owned (not a borrowing view) so it can be moved into the
/// `tokio::task::spawn_blocking` closure that runs a possibly-blocking classifier
/// off the async executor without borrowing engine-local temporaries across the
/// `move`.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassifierInput {
    /// The fact's text content (post-redaction, as it will be stored).
    pub content: String,
    /// The fact's type tag.
    pub fact_type: FactType,
    /// The consumer-supplied base importance prior (the `add_fact` caller hint),
    /// a finite value normally in `[0.0, 1.0]`. Mirrors [`Fact::importance`] /
    /// [`NewFact::importance`], **not** the decayed `importance_score`.
    pub importance: f64,
    /// The fact's metadata object (post-redaction).
    pub metadata: serde_json::Value,
}

/// Fluent builder for [`NewFact`], mirroring the ergonomics of
/// [`MemoryEngineBuilder`](crate::MemoryEngineBuilder).
///
/// Constructed via [`NewFact::builder`]. The three essential fields (`content`,
/// `embedding`, `fact_type`) are required up front; every other field defaults:
///
/// | Field             | Default                                                  |
/// | ----------------- | -------------------------------------------------------- |
/// | `content_hash`    | empty — `FactStore::insert` computes the blake3 hash     |
/// | `t_created`       | `Utc::now()` at [`build`](NewFactBuilder::build)         |
/// | `t_expired`       | `None` (not soft-deleted)                                |
/// | `t_valid`         | `None` (valid since creation)                            |
/// | `t_invalid`       | `None` (still valid)                                     |
/// | `source_event_id` | `None`                                                   |
/// | `importance`      | `0.5` (neutral prior; finite `[0, 1]` — not validated on this direct-insert path, #584) |
/// | `access_count`    | `0`                                                      |
/// | `last_accessed`   | the resolved `t_created` (coherent for backdated facts)  |
/// | `metadata`        | `{}` (empty JSON object)                                 |
/// | `scope_id`        | `1` (root scope)                                         |
/// | `is_pinned`       | `false`                                                  |
///
/// `t_created` defaults to `Utc::now()` at `build()`, and `last_accessed` defaults
/// to that resolved `t_created`, so a fact (fresh or backdated) stays coherent.
#[must_use = "a builder does nothing until `.build()` is called"]
#[derive(Debug, Clone)]
pub struct NewFactBuilder {
    content: String,
    content_hash: String,
    embedding: Vec<f32>,
    fact_type: FactType,
    t_created: Option<DateTime<Utc>>,
    t_expired: Option<DateTime<Utc>>,
    t_valid: Option<DateTime<Utc>>,
    t_invalid: Option<DateTime<Utc>>,
    source_event_id: Option<i64>,
    importance: f64,
    access_count: i64,
    last_accessed: Option<DateTime<Utc>>,
    metadata: serde_json::Value,
    scope_id: i64,
    is_pinned: bool,
}

impl NewFactBuilder {
    /// Create a builder from the three required fields. Prefer
    /// [`NewFact::builder`], which forwards here.
    pub fn new(content: impl Into<String>, embedding: Vec<f32>, fact_type: FactType) -> Self {
        Self {
            content: content.into(),
            content_hash: String::new(),
            embedding,
            fact_type,
            t_created: None,
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            importance: 0.5,
            access_count: 0,
            last_accessed: None,
            metadata: serde_json::json!({}),
            scope_id: default_root_scope_id(),
            is_pinned: false,
        }
    }

    /// Pre-set the content hash. Normally left empty so `FactStore::insert`
    /// computes the canonical blake3 hash from `content`.
    pub fn content_hash(mut self, hash: impl Into<String>) -> Self {
        self.content_hash = hash.into();
        self
    }

    /// Override the transaction-time creation timestamp (default: `Utc::now()`
    /// at `build()`). Used to backdate bootstrapped historical facts.
    pub const fn t_created(mut self, t_created: DateTime<Utc>) -> Self {
        self.t_created = Some(t_created);
        self
    }

    /// Set the transaction-time expiry (soft-delete marker).
    pub const fn t_expired(mut self, t_expired: DateTime<Utc>) -> Self {
        self.t_expired = Some(t_expired);
        self
    }

    /// Set the valid-time start (when the fact became true in the world).
    pub const fn t_valid(mut self, t_valid: DateTime<Utc>) -> Self {
        self.t_valid = Some(t_valid);
        self
    }

    /// Set the valid-time end (when the fact stopped being true).
    pub const fn t_invalid(mut self, t_invalid: DateTime<Utc>) -> Self {
        self.t_invalid = Some(t_invalid);
        self
    }

    /// Link the fact to the originating event.
    pub const fn source_event_id(mut self, source_event_id: i64) -> Self {
        self.source_event_id = Some(source_event_id);
        self
    }

    /// Set the base importance prior (default `0.5`), finite in `[0, 1]`. A
    /// `NewFact` built here is inserted directly, bypassing the `add_fact`
    /// range check (#571), so the value is stored verbatim — not validated (#584).
    pub const fn importance(mut self, importance: f64) -> Self {
        self.importance = importance;
        self
    }

    /// Set the initial access count (default `0`).
    pub const fn access_count(mut self, access_count: i64) -> Self {
        self.access_count = access_count;
        self
    }

    /// Override the last-accessed timestamp (default: `Utc::now()` at `build()`).
    pub const fn last_accessed(mut self, last_accessed: DateTime<Utc>) -> Self {
        self.last_accessed = Some(last_accessed);
        self
    }

    /// Set the metadata JSON (default: empty object `{}`).
    pub fn metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }

    /// Set the scope id (default `1`, the root scope).
    pub const fn scope_id(mut self, scope_id: i64) -> Self {
        self.scope_id = scope_id;
        self
    }

    /// Pin the fact (unforgettable). Default `false`.
    pub const fn is_pinned(mut self, is_pinned: bool) -> Self {
        self.is_pinned = is_pinned;
        self
    }

    /// Finalize into a [`NewFact`]. Unset timestamps (`t_created`,
    /// `last_accessed`) are sampled from a single `Utc::now()` call so they are
    /// coherent.
    #[must_use]
    pub fn build(self) -> NewFact {
        let now = Utc::now();
        // Default last_accessed to the resolved t_created (not `now`): when a fact
        // is backdated (historical import/bootstrap), treating it as freshly
        // accessed would reset its Ebbinghaus decay and skew importance.
        let t_created = self.t_created.unwrap_or(now);
        NewFact {
            content: self.content,
            content_hash: self.content_hash,
            embedding: self.embedding,
            fact_type: self.fact_type,
            t_created,
            t_expired: self.t_expired,
            t_valid: self.t_valid,
            t_invalid: self.t_invalid,
            source_event_id: self.source_event_id,
            importance: self.importance,
            access_count: self.access_count,
            last_accessed: self.last_accessed.unwrap_or(t_created),
            metadata: self.metadata,
            scope_id: self.scope_id,
            is_pinned: self.is_pinned,
        }
    }
}

/// Edge to insert (DB assigns id).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewEdge {
    pub source_fact_id: i64,
    pub target_fact_id: i64,
    pub relation_type: String,
    pub weight: f64,
    pub t_created: DateTime<Utc>,
    pub t_expired: Option<DateTime<Utc>>,
    pub scope_id: i64,
}

/// Summary to insert (DB assigns id).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewSummary {
    pub content: String,
    pub embedding: Vec<f32>,
    pub level: ConsolidationLevel,
    pub source_fact_ids: Vec<i64>,
    pub created_at: DateTime<Utc>,
    pub scope_id: i64,
}

// --- Phase 5a: Cognitive pipeline types ---

/// Semantic alias for fact identifiers.
pub type FactId = i64;

/// Semantic alias for lineage record identifiers.
pub type LineageId = i64;

/// A high-value observation captured by the intelligence layer.
///
/// Used as input to [`crate::traits::InsightStream::record`].
/// The consumer creates `Insight` values during conversations to capture
/// reasoning, decisions, and connections that only the model can make.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Insight {
    /// The insight text to store as a fact.
    pub content: String,
    /// Categorization (typically `Semantic` for insights).
    pub fact_type: FactType,
    /// Optional importance hint in [0.0, 1.0]. Defaults to 0.7 if `None`.
    pub importance: Option<f64>,
    /// Arbitrary JSON metadata (e.g., `{"source": "pre_compaction_flush"}`).
    pub metadata: Option<serde_json::Value>,
    /// Scope path (e.g., `"project/memory-engine"`). `None` → root scope.
    pub scope: Option<String>,
}

// `CycleReport` (delta-based, R7) now lives in `crate::engine::cycle::report` and is
// re-exported from the crate root. The old counts-based struct was removed in #49.

/// Per-`FactType` compression configuration for `DreamCycle`.
///
/// Controls what fraction of facts to retain per type and the percentile
/// threshold for promotion candidates.
#[derive(Debug, Clone)]
pub struct DreamCycleConfig {
    /// Fraction of facts to retain per `FactType` (0.0 = compress all, 1.0 = keep all).
    ///
    /// Defaults: Episodic=0.2, Semantic=0.8, Procedural=0.8
    pub compression_ratios: HashMap<FactType, f64>,
    /// Importance percentile threshold for promotion candidates.
    /// Facts above this percentile (within their type) are candidates.
    ///
    /// Default: 0.75 (P75).
    pub promotion_percentile: f64,
}

impl Default for DreamCycleConfig {
    fn default() -> Self {
        let mut ratios = HashMap::new();
        ratios.insert(FactType::Episodic, 0.2);
        ratios.insert(FactType::Semantic, 0.8);
        ratios.insert(FactType::Procedural, 0.8);
        Self {
            compression_ratios: ratios,
            promotion_percentile: 0.75,
        }
    }
}

// --- Activity stream types ---

/// Status of an activity record after server-side filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ActivityStatus {
    Recorded,
    Deduplicated,
    Ignored,
    Promoted,
}

impl fmt::Display for ActivityStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Recorded => write!(f, "recorded"),
            Self::Deduplicated => write!(f, "deduplicated"),
            Self::Ignored => write!(f, "ignored"),
            Self::Promoted => write!(f, "promoted"),
        }
    }
}

impl DreamCycleConfig {
    /// Validate configuration parameters.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Conflict` if any ratio or percentile is out of [0, 1].
    pub fn validate(&self) -> crate::error::Result<()> {
        use crate::error::{ConflictError, MemoryError};

        for (ft, &ratio) in &self.compression_ratios {
            if !ratio.is_finite() || !(0.0..=1.0).contains(&ratio) {
                return Err(MemoryError::Conflict(ConflictError::PolicyParameter(
                    format!("compression ratio for {ft:?} must be in [0.0, 1.0], got {ratio}"),
                )));
            }
        }
        if !self.promotion_percentile.is_finite()
            || !(0.0..=1.0).contains(&self.promotion_percentile)
        {
            return Err(MemoryError::Conflict(ConflictError::PolicyParameter(
                format!(
                    "promotion_percentile must be in [0.0, 1.0], got {}",
                    self.promotion_percentile
                ),
            )));
        }
        Ok(())
    }
}

/// Request to promote a fact to wisdom with provenance tracking.
///
/// Carries a precomputed embedding so the engine does not need an
/// [`crate::traits::EmbeddingProvider`] at promotion time — the `DreamCycle`
/// consumer owns its embedder and computes the embedding before calling
/// [`DreamContext::promote`](crate::engine::cognitive::DreamContext::promote).
#[derive(Debug, Clone)]
pub struct PromoteRequest {
    /// The promoted fact text.
    pub content: String,
    /// Fact type for the promoted wisdom (typically `Semantic`).
    pub fact_type: FactType,
    /// Precomputed embedding vector.
    pub embedding: Vec<f32>,
    /// Importance score for the promoted fact.
    pub importance: f64,
    /// Metadata JSON (will have `promotion_provenance` key injected).
    pub metadata: serde_json::Value,
    /// Scope path. `None` → root scope.
    pub scope: Option<String>,
    /// Source fact IDs for the lineage sidecar table.
    pub source_fact_ids: Vec<FactId>,
    /// Provenance envelope (serialized into metadata automatically).
    pub provenance: PromotionProvenance,
}

/// Result of a successful promotion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionResult {
    /// Database ID of the newly created promoted fact.
    pub fact_id: FactId,
    /// Database ID of the lineage record in the sidecar table.
    pub lineage_id: LineageId,
}

/// Error returned when [`ActivityStatus`] cannot be parsed from a string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown activity status: {0}")]
pub struct ParseActivityStatusError(pub String);

/// Outcome class of a recorded tool activity.
///
/// Replaces the previously stringly-typed `outcome_class` field on [`Activity`],
/// [`NewActivity`], and [`RecordActivityRequest`]. Encoding the known outcomes as
/// variants makes the dedup-key invariant (the `(session_id, tool_name, args_hash,
/// outcome_class, scope_id)` index keys off the *string*) misuse-resistant: a
/// consumer can no longer accidentally pass an empty string that would silently
/// corrupt deduplication.
///
/// # String representation (DB + JSON back-compat)
///
/// [`Display`](fmt::Display)/[`FromStr`] are a total round-trip with the on-disk
/// `outcome_class TEXT` column and the MCP JSON boundary:
/// `Success` ⇄ `"success"`, `Error` ⇄ `"error"`, `TestFailure` ⇄ `"test_failure"`.
/// Any other stored string parses into the open [`Other`](Self::Other) variant and
/// serializes back **verbatim**, so existing activity rows keep their exact value.
/// [`Default`] is [`Success`](Self::Success), matching the DB column default.
///
/// The [`Other`](Self::Other) arm is an escape hatch for consumer-defined classes;
/// it deliberately **bypasses** compile-time checking and should be reserved for
/// values not covered by a named variant.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum OutcomeClass {
    /// The tool invocation succeeded. Serializes as `"success"`. Default variant.
    #[default]
    Success,
    /// The tool invocation failed. Serializes as `"error"`.
    Error,
    /// A test run reported failures. Serializes as `"test_failure"`.
    TestFailure,
    /// Any consumer-defined outcome class. Serializes as its inner string verbatim;
    /// bypasses the compile-time invariant the named variants provide.
    Other(String),
}

impl fmt::Display for OutcomeClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Success => f.write_str("success"),
            Self::Error => f.write_str("error"),
            Self::TestFailure => f.write_str("test_failure"),
            Self::Other(s) => f.write_str(s),
        }
    }
}

impl FromStr for OutcomeClass {
    type Err = std::convert::Infallible;

    /// Total parse: every string maps to a variant, unknown ones into
    /// [`Other`](Self::Other). The named arms match the exact lowercase strings the
    /// store writes; parsing is therefore the inverse of [`Display`](fmt::Display)
    /// and preserves stored values byte-for-byte on round-trip.
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(match s {
            "success" => Self::Success,
            "error" => Self::Error,
            "test_failure" => Self::TestFailure,
            other => Self::Other(other.to_owned()),
        })
    }
}

impl Serialize for OutcomeClass {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for OutcomeClass {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        // `FromStr` is infallible (`Other` captures the open set), so the `Err`
        // arm is unreachable — destructure the `Infallible` `Ok` directly.
        let Ok(class) = s.parse();
        Ok(class)
    }
}

impl std::str::FromStr for ActivityStatus {
    type Err = ParseActivityStatusError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "recorded" => Ok(Self::Recorded),
            "deduplicated" => Ok(Self::Deduplicated),
            "ignored" => Ok(Self::Ignored),
            "promoted" => Ok(Self::Promoted),
            other => Err(ParseActivityStatusError(other.to_string())),
        }
    }
}

/// An activity record from a tool invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    pub id: i64,
    pub session_id: String,
    pub tool_name: String,
    pub args_hash: String,
    pub args: serde_json::Value,
    pub result_summary: Option<String>,
    pub outcome_class: OutcomeClass,
    pub status: ActivityStatus,
    pub occurrence_count: i64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub scope_id: i64,
    pub promoted_fact_id: Option<i64>,
}

/// Activity to insert (DB assigns id).
#[derive(Debug, Clone)]
pub struct NewActivity {
    pub session_id: String,
    pub tool_name: String,
    pub args_hash: String,
    pub args: serde_json::Value,
    pub result_summary: Option<String>,
    pub outcome_class: OutcomeClass,
    pub timestamp: DateTime<Utc>,
    pub scope_id: i64,
}

/// A session checkpoint (last-write-wins per `session_id`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCheckpoint {
    pub session_id: String,
    pub scope_path: Option<String>,
    pub summary: Option<String>,
    pub last_activity_id: Option<i64>,
    pub checkpoint_at: DateTime<Utc>,
    pub metadata: serde_json::Value,
}

/// Request to record a tool activity.
#[derive(Debug, Clone)]
pub struct RecordActivityRequest {
    pub tool_name: String,
    pub args: serde_json::Value,
    pub result: Option<String>,
    pub session_id: String,
    pub timestamp: DateTime<Utc>,
    pub scope_path: Option<String>,
    /// Outcome class of the activity. `None` defaults to
    /// [`OutcomeClass::Success`] (the DB column default).
    pub outcome_class: Option<OutcomeClass>,
}

/// Result of recording an activity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordActivityResult {
    pub activity_id: Option<i64>,
    pub was_deduplicated: bool,
    pub promoted_fact_id: Option<i64>,
    pub status: ActivityStatus,
}

/// Project-scoped context for session bootstrap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectContext {
    pub scope_path: String,
    pub recent_activities: Vec<Activity>,
    pub last_checkpoint: Option<SessionCheckpoint>,
    pub relevant_facts: Vec<Fact>,
}

/// Outcome of an atomic background-reconstruction promote (#623 D6).
///
/// Returned by [`SchemaManager::promote_space`](crate::storage::schema::SchemaManager::promote_space)
/// after the new (`populating`) space's vectors have been copy-swapped into the
/// active serving store (`facts.embedding`) and the registry status flipped — all
/// in one transaction. Internal-only this wave; the operator surface that renders
/// it is #689.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromoteOutcome {
    /// Facts whose active vector was copy-swapped to the new space (the homogeneous
    /// active space covers every fact, so this is the total fact count).
    pub promoted: usize,
    /// The previously-active space, now `deprecated` and retained in `fact_vectors`
    /// for an instant rollback (the inverse copy-swap, #689).
    pub deprecated_space: String,
    /// The now-active embedding identity. Carries the new `dim`, so the different-dim
    /// follow-up (#742) reads it to drive the engine effective-dim transition with no
    /// API change.
    pub new_fingerprint: EmbeddingFingerprint,
    /// Stragglers embedded by the engine's pre-tx catch-up pass (facts ingested after
    /// their backfill cursor passed). `0` from the storage port's perspective — the
    /// engine orchestration sets this; reserved for the live-write race work (#625).
    pub stragglers_caught: usize,
    /// The active vectors changed, so a downstream vector index (HNSW) must rebuild.
    /// Always `true` for a promote — even same-dim, the vectors are new. The engine
    /// fires a full `build_from_db` until the incremental rebuild hook lands (#624).
    pub rebuild_index: bool,
}

/// Identity of the embedding model that produced a stored vector.
///
/// The canonical **identity tuple** shared across the Memory and Knowledge layers
/// (see ADR 0015, `docs/design/adr/0015-cross-layer-embedding-identity-policy.md`).
///
/// An embedding is only meaningful within the vector space of the exact model that
/// produced it. Vector *dimension* alone is insufficient identity — two different
/// models can share a dimension and silently corrupt retrieval. This tuple is the
/// full identity; mismatch detection (issue #614) compares two fingerprints with
/// [`PartialEq`]/[`Eq`].
///
/// # Cross-layer parity contract
///
/// The field **names** (`model`, `provider`, `dim`, `matryoshka_base_dim`,
/// `element_type`) are **normative** and shared verbatim with the `knowledge-base`
/// repository's `embed_spaces` registry. Do not rename without updating ADR 0015 in
/// both repos. `model` is an operator-declared slug, not a weight hash.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EmbeddingFingerprint {
    /// Model identity slug, e.g. `"Qwen/Qwen3-Embedding-0.6B"`. Operator-declared.
    pub model: String,
    /// Serving backend, e.g. `"tei"`, `"ollama"`, `"openai"`.
    pub provider: String,
    /// Stored vector dimension (post-truncation). Literal field name `dim` per ADR 0015.
    pub dim: usize,
    /// Native model dimension before Matryoshka (MRL) truncation; `None` if untruncated.
    pub matryoshka_base_dim: Option<usize>,
    /// Vector element storage type: `"float32"` today (reserved: `"int8"`).
    pub element_type: String,
}

impl EmbeddingFingerprint {
    /// The default vector element type (`"float32"`).
    pub const ELEMENT_F32: &'static str = "float32";

    /// Construct a fingerprint for an untruncated `float32` embedding space.
    ///
    /// Sets `matryoshka_base_dim` to `None` and `element_type` to
    /// [`ELEMENT_F32`](Self::ELEMENT_F32).
    #[must_use]
    pub fn new(model: impl Into<String>, provider: impl Into<String>, dim: usize) -> Self {
        Self {
            model: model.into(),
            provider: provider.into(),
            dim,
            matryoshka_base_dim: None,
            element_type: Self::ELEMENT_F32.to_string(),
        }
    }

    /// Construct a fingerprint for a Matryoshka-truncated `float32` embedding space,
    /// recording the native `base_dim` the model emits before truncation to `dim`.
    #[must_use]
    pub fn with_matryoshka(
        model: impl Into<String>,
        provider: impl Into<String>,
        dim: usize,
        base_dim: usize,
    ) -> Self {
        Self {
            matryoshka_base_dim: Some(base_dim),
            ..Self::new(model, provider, dim)
        }
    }
}

// --- Storage-agnostic projection / filter rows ---
//
// These are dialect-free domain rows (a Postgres backend returns them too), so
// their structural home is `types`, not the SQLite `store` module. Relocated here
// for #629 (the dialect-free `StorageBackend` port must not reference types that
// live inside the SQLite store). `pub use` shims in `store::facts` /
// `store::events` preserve the original paths.

/// Lightweight importance-scoring projection of a fact.
///
/// Only the scalar fields the forgetting policy needs, so a full active-set scan
/// never deserializes embeddings. See `FactGraph::list_active_facts_scoring`.
#[derive(Debug, Clone)]
pub struct FactScoringRow {
    pub id: i64,
    pub fact_type: FactType,
    pub last_accessed: DateTime<Utc>,
    pub access_count: i64,
    pub importance: f64,
    pub is_pinned: bool,
}

/// Lightweight fact info for session-based edge creation. Avoids deserializing
/// embeddings — only carries the fact id needed for pairwise edge wiring.
#[derive(Debug, Clone)]
pub struct SessionFact {
    pub id: i64,
}

/// Filter for querying the append-only event log.
#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub session_id: Option<String>,
    pub event_type: Option<EventType>,
    pub source: Option<String>,
    pub limit: Option<usize>,
    pub id_min: Option<i64>,
    pub id_max: Option<i64>,
    pub order_by_id: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Display impl tests ---

    #[test]
    fn display_impls_all_variants() {
        // Outcome
        assert_eq!(Outcome::Positive.to_string(), "positive");
        assert_eq!(Outcome::Negative.to_string(), "negative");
        assert_eq!(Outcome::Neutral.to_string(), "neutral");

        // FactType
        assert_eq!(FactType::Episodic.to_string(), "episodic");
        assert_eq!(FactType::Semantic.to_string(), "semantic");
        assert_eq!(FactType::Procedural.to_string(), "procedural");

        // ConsolidationLevel
        assert_eq!(ConsolidationLevel::Local.to_string(), "local");
        assert_eq!(ConsolidationLevel::Cluster.to_string(), "cluster");
        assert_eq!(ConsolidationLevel::Global.to_string(), "global");

        // (FactType FromStr parsing is exercised in `fact_type_from_str_*` below)

        // ActivityStatus
        assert_eq!(ActivityStatus::Recorded.to_string(), "recorded");
        assert_eq!(ActivityStatus::Deduplicated.to_string(), "deduplicated");
        assert_eq!(ActivityStatus::Ignored.to_string(), "ignored");
        assert_eq!(ActivityStatus::Promoted.to_string(), "promoted");
    }

    // --- FactType FromStr (canonical string parse, shared by CLI + MCP) ---

    #[test]
    fn fact_type_from_str_canonical_snake_case() {
        assert_eq!("episodic".parse::<FactType>().unwrap(), FactType::Episodic);
        assert_eq!("semantic".parse::<FactType>().unwrap(), FactType::Semantic);
        assert_eq!(
            "procedural".parse::<FactType>().unwrap(),
            FactType::Procedural
        );
    }

    #[test]
    fn fact_type_from_str_accepts_pascal_case() {
        // PascalCase is the serde-derive / MCP-JSON-schema wire form; the parser
        // accepts it so both casings reconcile to one canonical enum.
        assert_eq!("Episodic".parse::<FactType>().unwrap(), FactType::Episodic);
        assert_eq!("Semantic".parse::<FactType>().unwrap(), FactType::Semantic);
        assert_eq!(
            "Procedural".parse::<FactType>().unwrap(),
            FactType::Procedural
        );
    }

    #[test]
    fn fact_type_from_str_is_case_insensitive() {
        assert_eq!("EPISODIC".parse::<FactType>().unwrap(), FactType::Episodic);
        assert_eq!("sEmAnTiC".parse::<FactType>().unwrap(), FactType::Semantic);
    }

    #[test]
    fn fact_type_from_str_rejects_unknown() {
        let err = "wisdom".parse::<FactType>().unwrap_err();
        // The unknown token is preserved in the error for actionable messages.
        assert!(err.to_string().contains("wisdom"));
    }

    #[test]
    fn fact_type_from_str_rejects_surrounding_whitespace() {
        // The parser is intentionally strict on whitespace — it does not trim.
        // Surrounding whitespace is a malformed token, not a valid variant.
        assert!(" episodic".parse::<FactType>().is_err());
        assert!("semantic ".parse::<FactType>().is_err());
        assert!("".parse::<FactType>().is_err());
    }

    #[test]
    fn fact_type_display_round_trips_through_from_str() {
        for ft in [FactType::Episodic, FactType::Semantic, FactType::Procedural] {
            assert_eq!(ft.to_string().parse::<FactType>().unwrap(), ft);
        }
    }

    // --- NewFactBuilder ---

    #[test]
    fn builder_backdated_t_created_sets_last_accessed_to_match() {
        use chrono::TimeZone;
        let past = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        // Backdate t_created without an explicit last_accessed.
        let nf = NewFact::builder("historical fact", vec![0.1; 4], FactType::Semantic)
            .t_created(past)
            .build();
        assert_eq!(nf.t_created, past);
        assert_eq!(
            nf.last_accessed, past,
            "last_accessed must follow a backdated t_created, not default to now"
        );
        // An explicit last_accessed still wins.
        let later = Utc.with_ymd_and_hms(2021, 6, 1, 0, 0, 0).unwrap();
        let nf2 = NewFact::builder("x", vec![0.1; 4], FactType::Semantic)
            .t_created(past)
            .last_accessed(later)
            .build();
        assert_eq!(nf2.last_accessed, later);
    }

    // --- ActivityStatus::from_str round-trip + error path ---

    #[test]
    fn activity_status_from_str_all_known_variants() {
        use std::str::FromStr;

        assert_eq!(
            ActivityStatus::from_str("recorded").unwrap(),
            ActivityStatus::Recorded
        );
        assert_eq!(
            ActivityStatus::from_str("deduplicated").unwrap(),
            ActivityStatus::Deduplicated
        );
        assert_eq!(
            ActivityStatus::from_str("ignored").unwrap(),
            ActivityStatus::Ignored
        );
        assert_eq!(
            ActivityStatus::from_str("promoted").unwrap(),
            ActivityStatus::Promoted
        );
    }

    #[test]
    fn activity_status_from_str_round_trips_with_display() {
        use std::str::FromStr;

        for status in [
            ActivityStatus::Recorded,
            ActivityStatus::Deduplicated,
            ActivityStatus::Ignored,
            ActivityStatus::Promoted,
        ] {
            let rendered = status.to_string();
            assert_eq!(
                ActivityStatus::from_str(&rendered).unwrap(),
                status,
                "Display->from_str round-trip failed for {status:?}"
            );
        }
    }

    #[test]
    fn activity_status_from_str_unknown_is_error() {
        use std::str::FromStr;

        let err = ActivityStatus::from_str("bogus").unwrap_err();
        assert_eq!(err.to_string(), "unknown activity status: bogus");
        // Case-sensitivity: the matcher expects lowercase variants.
        assert!(ActivityStatus::from_str("Recorded").is_err());
        assert!(ActivityStatus::from_str("").is_err());
    }

    // --- OutcomeClass (#347) ---

    #[test]
    fn outcome_class_display_emits_db_strings() {
        // The named variants render exactly the lowercase strings the store writes.
        assert_eq!(OutcomeClass::Success.to_string(), "success");
        assert_eq!(OutcomeClass::Error.to_string(), "error");
        assert_eq!(OutcomeClass::TestFailure.to_string(), "test_failure");
        assert_eq!(OutcomeClass::Other("flaky".into()).to_string(), "flaky");
    }

    #[test]
    fn outcome_class_default_is_success() {
        // Matches the `outcome_class TEXT NOT NULL DEFAULT 'success'` column default.
        assert_eq!(OutcomeClass::default(), OutcomeClass::Success);
    }

    #[test]
    fn outcome_class_from_str_maps_known_and_open_set() {
        use std::str::FromStr;
        let Ok(s) = OutcomeClass::from_str("success");
        assert_eq!(s, OutcomeClass::Success);
        let Ok(e) = OutcomeClass::from_str("error");
        assert_eq!(e, OutcomeClass::Error);
        let Ok(tf) = OutcomeClass::from_str("test_failure");
        assert_eq!(tf, OutcomeClass::TestFailure);
        // Any unrecognized string lands in the open `Other` arm verbatim — this is
        // the back-compat guarantee for activity rows written before this enum.
        let Ok(o) = OutcomeClass::from_str("custom-thing");
        assert_eq!(o, OutcomeClass::Other("custom-thing".into()));
        // Even the empty string round-trips (it is no longer a silent footgun: a
        // consumer must spell `Other("")` to produce it).
        let Ok(empty) = OutcomeClass::from_str("");
        assert_eq!(empty, OutcomeClass::Other(String::new()));
    }

    #[test]
    fn outcome_class_string_round_trip_is_lossless() {
        use std::str::FromStr;
        for class in [
            OutcomeClass::Success,
            OutcomeClass::Error,
            OutcomeClass::TestFailure,
            OutcomeClass::Other("vendor-specific".into()),
        ] {
            let rendered = class.to_string();
            let Ok(parsed) = OutcomeClass::from_str(&rendered);
            assert_eq!(parsed, class, "Display->from_str round-trip failed");
        }
    }

    #[test]
    fn outcome_class_serde_is_a_plain_string() {
        // Wire format is a bare JSON string (back-compat with the prior `String`
        // field + the MCP `"type": "string"` schema), not an externally-tagged enum.
        assert_eq!(
            serde_json::to_string(&OutcomeClass::TestFailure).unwrap(),
            "\"test_failure\""
        );
        let back: OutcomeClass = serde_json::from_str("\"error\"").unwrap();
        assert_eq!(back, OutcomeClass::Error);
        // An unknown stored value deserializes into the open arm, never an error.
        let open: OutcomeClass = serde_json::from_str("\"legacy_value\"").unwrap();
        assert_eq!(open, OutcomeClass::Other("legacy_value".into()));
    }

    // --- Phase 5a type tests ---

    #[test]
    fn dream_cycle_config_default_has_expected_ratios() {
        let cfg = DreamCycleConfig::default();
        assert!(
            (cfg.compression_ratios[&FactType::Episodic] - 0.2).abs() < f64::EPSILON,
            "Episodic should be 0.2"
        );
        assert!(
            (cfg.compression_ratios[&FactType::Semantic] - 0.8).abs() < f64::EPSILON,
            "Semantic should be 0.8"
        );
        assert!(
            (cfg.compression_ratios[&FactType::Procedural] - 0.8).abs() < f64::EPSILON,
            "Procedural should be 0.8"
        );
        assert!(
            (cfg.promotion_percentile - 0.75).abs() < f64::EPSILON,
            "promotion_percentile should be 0.75"
        );
    }

    #[test]
    fn dream_cycle_config_validate_ok() {
        DreamCycleConfig::default().validate().unwrap();
    }

    #[test]
    fn dream_cycle_config_validate_rejects_ratio_above_one() {
        let mut cfg = DreamCycleConfig::default();
        cfg.compression_ratios.insert(FactType::Episodic, 1.5);
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("Episodic"), "error: {err}");
    }

    #[test]
    fn dream_cycle_config_validate_rejects_negative_ratio() {
        let mut cfg = DreamCycleConfig::default();
        cfg.compression_ratios.insert(FactType::Semantic, -0.1);
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn dream_cycle_config_validate_rejects_nan_ratio() {
        let mut cfg = DreamCycleConfig::default();
        cfg.compression_ratios
            .insert(FactType::Procedural, f64::NAN);
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn dream_cycle_config_validate_rejects_bad_percentile() {
        let cfg = DreamCycleConfig {
            promotion_percentile: 1.5,
            ..DreamCycleConfig::default()
        };
        assert!(cfg.validate().is_err());

        let cfg = DreamCycleConfig {
            promotion_percentile: -0.1,
            ..DreamCycleConfig::default()
        };
        assert!(cfg.validate().is_err());

        let cfg = DreamCycleConfig {
            promotion_percentile: f64::NAN,
            ..DreamCycleConfig::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn dream_cycle_config_validate_accepts_boundaries() {
        let cfg = DreamCycleConfig {
            promotion_percentile: 0.0,
            ..DreamCycleConfig::default()
        };
        cfg.validate().unwrap();

        let cfg = DreamCycleConfig {
            promotion_percentile: 1.0,
            ..DreamCycleConfig::default()
        };
        cfg.validate().unwrap();
    }

    #[test]
    fn insight_serde_round_trip() {
        let insight = Insight {
            content: "User prefers terse responses".into(),
            fact_type: FactType::Semantic,
            importance: Some(0.8),
            metadata: Some(serde_json::json!({"source": "model_observation"})),
            scope: Some("project/demo".into()),
        };
        let json = serde_json::to_string(&insight).unwrap();
        let back: Insight = serde_json::from_str(&json).unwrap();
        assert_eq!(insight, back);
    }

    #[test]
    fn insight_serde_with_none_fields() {
        let insight = Insight {
            content: "test".into(),
            fact_type: FactType::Episodic,
            importance: None,
            metadata: None,
            scope: None,
        };
        let json = serde_json::to_string(&insight).unwrap();
        let back: Insight = serde_json::from_str(&json).unwrap();
        assert_eq!(insight, back);
    }

    // `CycleReport` serde round-trip moved to `engine::cycle::report` tests (#49).

    // --- Existing tests ---

    #[test]
    fn event_round_trip_json() {
        let event = Event {
            id: 1,
            timestamp: Utc::now(),
            event_type: EventType::Interaction,
            payload: serde_json::json!({"key": "value"}),
            source: "test".into(),
            session_id: Some("sess-1".into()),
            scope_id: 1,
            origin_node_id: "local".into(),
            sequence_id: 0,
            created_at: None,
            event_revision: 1,
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "UNSCORED_IMPORTANCE is a sentinel constant (0.5); exact equality is the correct check here"
    )]
    fn unscored_importance_sentinel_is_half() {
        // Locks the canonical value of the unscored-fact placeholder. The two
        // production construction sites (conflict::resolve_conflict and
        // consolidation::global_integration) reference this constant; this is the
        // single, deliberate place to change it.
        assert_eq!(Fact::UNSCORED_IMPORTANCE, 0.5);
    }

    #[test]
    fn fact_defaults_none_temporals() {
        let fact = Fact {
            id: 1,
            content: "test".into(),
            content_hash: "abc".into(),
            embedding: vec![0.1; 768],
            fact_type: FactType::Episodic,
            t_created: Utc::now(),
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: Some(1),
            importance: 0.5,
            access_count: 0,
            last_accessed: Utc::now(),
            metadata: serde_json::json!({}),
            scope_id: 1,
            is_pinned: false,
            importance_score: 0.5,
            surfaced_at: None,
        };
        assert!(fact.t_expired.is_none());
        assert!(fact.t_valid.is_none());
    }

    #[test]
    fn serde_fact_without_surfaced_at() {
        // JSON without the surfaced_at field — serde(default) should yield None.
        let json = r#"{
            "id": 1,
            "content": "hello",
            "content_hash": "abc123",
            "embedding": [0.1, 0.2],
            "fact_type": "Semantic",
            "t_created": "2026-01-01T00:00:00Z",
            "t_expired": null,
            "t_valid": null,
            "t_invalid": null,
            "source_event_id": null,
            "importance": 0.5,
            "access_count": 0,
            "last_accessed": "2026-01-01T00:00:00Z",
            "metadata": {},
            "scope_id": 1,
            "is_pinned": false,
            "importance_score": 0.5
        }"#;
        let fact: Fact = serde_json::from_str(json).unwrap();
        assert!(
            fact.surfaced_at.is_none(),
            "missing surfaced_at should deserialize as None"
        );

        // JSON with surfaced_at present — should round-trip correctly.
        let json_with = r#"{
            "id": 2,
            "content": "world",
            "content_hash": "def456",
            "embedding": [0.3],
            "fact_type": "Episodic",
            "t_created": "2026-01-01T00:00:00Z",
            "t_expired": null,
            "t_valid": null,
            "t_invalid": null,
            "source_event_id": null,
            "importance": 0.7,
            "access_count": 1,
            "last_accessed": "2026-01-01T00:00:00Z",
            "metadata": {},
            "scope_id": 1,
            "is_pinned": false,
            "importance_score": 0.7,
            "surfaced_at": "2026-03-15T12:00:00Z"
        }"#;
        let fact2: Fact = serde_json::from_str(json_with).unwrap();
        let ts = fact2
            .surfaced_at
            .expect("surfaced_at should deserialize when present");
        assert_eq!(ts.to_rfc3339(), "2026-03-15T12:00:00+00:00");
    }

    #[test]
    fn serde_fact_legacy_archive_applies_field_defaults() {
        // A Fact from a .pak archive predating scope_id/is_pinned/importance_score
        // omits those fields; they must deserialize to defaults so old archives
        // remain readable (super-qa #505 / read_pak backward-compat).
        let json = r#"{
            "id": 7,
            "content": "legacy",
            "content_hash": "h",
            "embedding": [0.1],
            "fact_type": "Semantic",
            "t_created": "2024-01-01T00:00:00Z",
            "t_expired": null,
            "t_valid": null,
            "t_invalid": null,
            "source_event_id": null,
            "importance": 0.9,
            "access_count": 0,
            "last_accessed": "2024-01-01T00:00:00Z",
            "metadata": {}
        }"#;
        let fact: Fact = serde_json::from_str(json).unwrap();
        assert_eq!(fact.scope_id, 1, "missing scope_id defaults to root (1)");
        assert!(!fact.is_pinned, "missing is_pinned defaults to false");
        // importance_score is missing from JSON → serde default of 0.0 (exact bit-for-bit).
        #[allow(
            clippy::float_cmp,
            reason = "serde default produces exact 0.0; no arithmetic involved"
        )]
        {
            assert_eq!(
                fact.importance_score, 0.0,
                "missing importance_score defaults to 0.0"
            );
        }
    }

    #[test]
    fn promotion_provenance_round_trip_json() {
        let prov = PromotionProvenance {
            source_count: 5,
            session_count: 3,
            date_range_start: Utc::now(),
            date_range_end: Utc::now(),
            confidence: 0.87,
            method_version: "dreamcycle-v1".into(),
            representative_ids: vec![10, 20, 30],
        };
        let json = serde_json::to_string(&prov).unwrap();
        // The owning lineage_id is no longer a field — it belongs to the row, not
        // the envelope — so it must never leak into the serialized provenance.
        assert!(!json.contains("lineage_id"));
        let back: PromotionProvenance = serde_json::from_str(&json).unwrap();
        // Every remaining field is a true, lossless round-trip.
        assert_eq!(back, prov);
    }

    #[test]
    fn lineage_record_round_trip_json() {
        let rec = LineageRecord {
            lineage_id: 1,
            wisdom_fact_id: 42,
            source_fact_ids: vec![10, 20, 30, 40, 50],
        };
        let json = serde_json::to_string(&rec).unwrap();
        let back: LineageRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(rec, back);
    }

    /// `ConsolidationProposal` crosses the HTTP boundary (A3 deserializes it from an
    /// LLM's JSON), so its wire shape must be stable: a top-level `merges` array of
    /// `{ source_ids, summary }` objects.
    #[test]
    fn consolidation_proposal_round_trip_json() {
        let proposal = ConsolidationProposal {
            merges: vec![
                MergeGroup {
                    source_ids: vec![1, 2, 3],
                    summary: "user prefers terse, code-first answers".into(),
                },
                MergeGroup {
                    source_ids: vec![7],
                    summary: "singleton group is structurally legal".into(),
                },
            ],
        };
        let json = serde_json::to_string(&proposal).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["merges"][0]["source_ids"], serde_json::json!([1, 2, 3]));
        assert_eq!(
            v["merges"][0]["summary"],
            "user prefers terse, code-first answers"
        );
        let back: ConsolidationProposal = serde_json::from_str(&json).unwrap();
        assert_eq!(proposal, back);
    }

    /// An empty proposal (the LLM found nothing to merge) is a valid, representable
    /// state — not an error. A2's `LlmDreamCycle` turns it into a no-op report.
    #[test]
    fn consolidation_proposal_empty_is_representable() {
        let empty = ConsolidationProposal { merges: vec![] };
        let json = serde_json::to_string(&empty).unwrap();
        let back: ConsolidationProposal = serde_json::from_str(&json).unwrap();
        assert_eq!(empty, back);
        assert!(back.merges.is_empty());
    }

    // --- EmbeddingFingerprint (#612) ---

    #[test]
    fn fingerprint_new_sets_float32_untruncated_defaults() {
        let fp = EmbeddingFingerprint::new("m", "p", 768);
        assert_eq!(fp.model, "m");
        assert_eq!(fp.provider, "p");
        assert_eq!(fp.dim, 768);
        assert_eq!(fp.matryoshka_base_dim, None);
        assert_eq!(fp.element_type, EmbeddingFingerprint::ELEMENT_F32);
        assert_eq!(fp.element_type, "float32");
    }

    #[test]
    fn fingerprint_with_matryoshka_records_base_dim() {
        let fp = EmbeddingFingerprint::with_matryoshka("qwen", "tei", 512, 1024);
        assert_eq!(fp.dim, 512);
        assert_eq!(fp.matryoshka_base_dim, Some(1024));
        assert_eq!(fp.element_type, "float32");
    }

    #[test]
    fn fingerprint_eq_requires_every_field() {
        // Equality is the #614 mismatch contract: any differing field => incompatible.
        let base = EmbeddingFingerprint::new("m", "p", 768);
        assert_eq!(base, EmbeddingFingerprint::new("m", "p", 768));
        assert_ne!(base, EmbeddingFingerprint::new("other", "p", 768));
        assert_ne!(base, EmbeddingFingerprint::new("m", "other", 768));
        assert_ne!(base, EmbeddingFingerprint::new("m", "p", 384));
        assert_ne!(
            base,
            EmbeddingFingerprint::with_matryoshka("m", "p", 768, 1024)
        );
        let mut int8 = EmbeddingFingerprint::new("m", "p", 768);
        int8.element_type = "int8".to_string();
        assert_ne!(base, int8);
    }

    #[test]
    fn fingerprint_hash_consistent_with_eq() {
        let mut set = std::collections::HashSet::new();
        set.insert(EmbeddingFingerprint::new("m", "p", 768));
        assert!(set.contains(&EmbeddingFingerprint::new("m", "p", 768)));
        assert!(!set.contains(&EmbeddingFingerprint::new("m", "p", 384)));
    }

    #[test]
    fn fingerprint_serde_pins_adr0015_key_set() {
        // The JSON key set is the normative ME<->KB parity contract (ADR 0015):
        // a field rename MUST break this test.
        let fp = EmbeddingFingerprint::with_matryoshka("qwen", "tei", 512, 1024);
        let v = serde_json::to_value(&fp).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "dim",
                "element_type",
                "matryoshka_base_dim",
                "model",
                "provider"
            ]
        );
        let back: EmbeddingFingerprint = serde_json::from_value(v).unwrap();
        assert_eq!(fp, back);
    }
}
