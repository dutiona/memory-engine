use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Semantic alias for fact identifiers. Lives in `facts` (a leaf module) so the
/// provenance/cognitive submodules can both reference it without a module cycle.
pub type FactId = i64;

/// Default `scope_id` for a `Fact` deserialized from an archive that predates
/// the scope column — the root scope (id 1). Keeps old `.pak` archives readable.
/// Module-private — called only by the serde derive machinery in this file.
const fn default_root_scope_id() -> i64 {
    1
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
    /// (via [`AddFactOptions::base_importance`], default `0.5`), a finite value in
    /// `[0.0, 1.0]`. The [`add_fact`](crate::MemoryEngine::add_fact) /
    /// `add_facts_batch` entry points validate this range (#571); a few
    /// direct-insert paths (bootstrap, snapshot restore) do not yet enforce it
    /// (#584), so a `Fact` materialized that way could carry an out-of-range value.
    /// It is a *static* hint that never decays; the engine only reads it. It feeds the materialized
    /// [`importance_score`](Self::importance_score) as one of four signals (weight
    /// `base_importance_weight`). Do not confuse with the decayed score below.
    ///
    /// Maps to the DB column `importance` (unrenamed for on-disk compatibility);
    /// the serde key, however, is `base_importance` (snapshot/export break, #274).
    pub base_importance: f64,
    pub access_count: i64,
    pub last_accessed: DateTime<Utc>,
    pub metadata: serde_json::Value,
    #[serde(default = "default_root_scope_id")]
    pub scope_id: i64,
    #[serde(default)]
    pub is_pinned: bool,
    /// **Materialized importance score** — the *computed, decaying* score the
    /// engine ranks and forgets by, normally in `[0.0, 1.0]` (seeded from the
    /// [`base_importance`](Self::base_importance) prior, which the `add_fact` entry points
    /// validate to that range — #571). It is the weighted sum of
    /// four signals (recency via Ebbinghaus decay, access frequency, graph
    /// degree, and the static [`base_importance`](Self::base_importance) prior); see
    /// `forgetting::compute_importance`. Seeded to `base_importance` at ingest, then
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

    /// Build a transient, pre-insert [`Fact`] from a [`NewFact`] to hand to
    /// [`ConflictArbiter::arbitrate`](crate::traits::ConflictArbiter::arbitrate).
    ///
    /// `id` is `0` (not yet assigned by the DB) and `importance_score` is the
    /// [`Self::UNSCORED_IMPORTANCE`] sentinel — NOT the eventual stored score.
    /// All other fields are copied verbatim from `nf`.
    ///
    /// **Clone is intentional:** the trait signature takes `&Fact`, so an owned
    /// value must be built from the borrowed `NewFact`. The fields cloned
    /// (`content`, `content_hash`, `embedding`, `metadata`) are string/vec
    /// payloads with no cheaper alternative at this call site.
    ///
    /// **Arbiter input caveat:** the returned `Fact` is synthetic. Arbiters must
    /// rely on `content`, `fact_type`, `base_importance`, and `metadata` — never
    /// on `id` (always `0`) or `importance_score` (always the sentinel `0.5`).
    pub(crate) fn from_new_for_arbiter(nf: &NewFact) -> Self {
        Self {
            id: 0, // placeholder, not yet inserted
            content: nf.content.clone(),
            content_hash: nf.content_hash.clone(),
            embedding: nf.embedding.clone(),
            fact_type: nf.fact_type,
            t_created: nf.t_created,
            t_expired: nf.t_expired,
            t_valid: nf.t_valid,
            t_invalid: nf.t_invalid,
            source_event_id: nf.source_event_id,
            scope_id: nf.scope_id,
            base_importance: nf.base_importance,
            access_count: nf.access_count,
            last_accessed: nf.last_accessed,
            metadata: nf.metadata.clone(),
            is_pinned: nf.is_pinned,
            importance_score: Self::UNSCORED_IMPORTANCE,
            surfaced_at: None,
        }
    }

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

/// Optional parameters for [`crate::engine::MemoryEngine::add_fact`].
///
/// All fields default to `None`, which uses the engine's defaults
/// (`base_importance=0.5`, metadata={}, no temporal bounds).
#[derive(Debug, Clone, Default)]
pub struct AddFactOptions {
    /// Override default base importance (0.5). Must be in [0, 1]; an out-of-range
    /// or non-finite value is rejected with `Conflict(PolicyParameter)` by
    /// [`add_fact`](crate::engine::MemoryEngine::add_fact) and
    /// [`add_facts_batch`](crate::engine::MemoryEngine::add_facts_batch).
    pub base_importance: Option<f64>,
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
    /// Optional overrides (`base_importance`, metadata, temporal bounds, pinned).
    pub opts: Option<AddFactOptions>,
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
    /// **Base importance** prior. Maps to the DB column `importance` (unrenamed
    /// for on-disk compatibility); the serde key is `base_importance` (#274).
    pub base_importance: f64,
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
    ///     .base_importance(0.8)
    ///     .scope_id(1)
    ///     .build();
    /// assert_eq!(fact.base_importance, 0.8);
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
/// `fact_type`, `base_importance`, and `metadata`.
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
    /// a finite value normally in `[0.0, 1.0]`. Mirrors [`Fact::base_importance`] /
    /// [`NewFact::base_importance`], **not** the decayed `importance_score`.
    pub base_importance: f64,
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
/// | `base_importance` | `0.5` (neutral prior; finite `[0, 1]` — not validated on this direct-insert path, #584) |
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
    base_importance: f64,
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
            base_importance: 0.5,
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
    pub const fn base_importance(mut self, base_importance: f64) -> Self {
        self.base_importance = base_importance;
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
            base_importance: self.base_importance,
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

// Storage-agnostic projection / filter rows. These are dialect-free domain rows
// (a Postgres backend returns them too), so their structural home is `types`, not
// the SQLite `store` module — the dialect-free `StorageBackend` port must not
// reference types living inside the SQLite store (#629). `pub use` shims in
// `store::facts` preserve the original paths.

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
    /// Base importance prior (DB column `importance`); see [`Fact::base_importance`].
    pub base_importance: f64,
    pub is_pinned: bool,
}

/// Lightweight fact info for session-based edge creation. Avoids deserializing
/// embeddings — only carries the fact id needed for pairwise edge wiring.
#[derive(Debug, Clone)]
pub struct SessionFact {
    pub id: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fact_type_and_consolidation_level_display() {
        assert_eq!(FactType::Episodic.to_string(), "episodic");
        assert_eq!(FactType::Semantic.to_string(), "semantic");
        assert_eq!(FactType::Procedural.to_string(), "procedural");

        assert_eq!(ConsolidationLevel::Local.to_string(), "local");
        assert_eq!(ConsolidationLevel::Cluster.to_string(), "cluster");
        assert_eq!(ConsolidationLevel::Global.to_string(), "global");
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
            base_importance: 0.5,
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
            "base_importance": 0.5,
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
            "base_importance": 0.7,
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
            "base_importance": 0.9,
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
    fn serde_fact_rejects_legacy_importance_key_no_silent_default() {
        // #274 safety invariant: `base_importance` carries NO `#[serde(default)]`,
        // so a pre-rename payload keyed on the legacy `"importance"` field (and
        // lacking `"base_importance"`) MUST hard-error on deserialize rather than
        // silently load with `base_importance = 0.0`. This is the entire safety of
        // the clean break (no `#[serde(alias)]`), and it guards EVERY Fact-bearing
        // serialized projection — `.pak` archives, the JSON dump/restore path, and
        // import — since all embed `Vec<Fact>`. If this test ever fails, a stray
        // default was added and stale archives would deserialize with a wrong 0.0
        // base importance instead of being rejected.
        let legacy_json = r#"{
            "id": 1,
            "content": "legacy",
            "content_hash": "abc",
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
        let result: Result<Fact, _> = serde_json::from_str(legacy_json);
        let err = result.expect_err(
            "a legacy \"importance\"-keyed Fact must be rejected, not silently defaulted",
        );
        assert!(
            err.to_string().contains("base_importance"),
            "rejection should name the missing `base_importance` field, got: {err}"
        );
    }

    /// Property-based serde round-trips for the core types whose invariants the
    /// example tests above only spot-check at one or two fixed inputs (#444).
    ///
    /// The example tests pin specific shapes (one `PromotionProvenance`, two
    /// `Fact` JSON blobs); these assert the round-trip and field-omission
    /// invariants hold across the *whole* input space — arbitrary counts, scores,
    /// timestamps, id vectors, and the `surfaced_at` `Some`/`None` axis.
    mod proptest_serde_roundtrip {
        use super::*;
        use proptest::prelude::*;

        /// Build a UTC timestamp from a second-offset proptest sample, clamped to a
        /// representable range so the strategy never produces an out-of-range value.
        fn ts_from_secs(secs: i64) -> DateTime<Utc> {
            // chrono's representable range is enormous; this band is more than wide
            // enough to exercise the serialization without flirting with overflow.
            let clamped = secs.clamp(-62_135_596_800, 253_402_300_799);
            DateTime::<Utc>::from_timestamp(clamped, 0).unwrap_or_else(Utc::now)
        }

        proptest! {
            /// `Fact.surfaced_at` (a `#[serde(default)] Option<DateTime<Utc>>`)
            /// round-trips for both the `Some` and `None` arms across arbitrary
            /// timestamps — the field the example tests exercise with only two fixed
            /// JSON strings.
            #[test]
            fn fact_surfaced_at_roundtrips_over_some_and_none(
                surfaced_secs in proptest::option::of(any::<i64>()),
            ) {
                let surfaced_at = surfaced_secs.map(ts_from_secs);
                let fact = Fact {
                    id: 1,
                    content: "p".into(),
                    content_hash: "h".into(),
                    embedding: vec![0.0_f32],
                    fact_type: FactType::Semantic,
                    t_created: ts_from_secs(0),
                    t_expired: None,
                    t_valid: None,
                    t_invalid: None,
                    source_event_id: None,
                    base_importance: 0.5,
                    access_count: 0,
                    last_accessed: ts_from_secs(0),
                    metadata: serde_json::json!({}),
                    scope_id: 1,
                    is_pinned: false,
                    importance_score: 0.5,
                    surfaced_at,
                };
                let json = serde_json::to_string(&fact).unwrap();
                let back: Fact = serde_json::from_str(&json).unwrap();
                prop_assert_eq!(back.surfaced_at, fact.surfaced_at);
            }
        }
    }
}
