//! The persistence **port** — the trait family the engine talks to instead of
//! `rusqlite` directly.
//!
//! Deliberately separate from [`crate::traits`] (consumer *capability-injection*
//! traits like [`EmbeddingProvider`](crate::traits::EmbeddingProvider)): this is
//! an *infrastructure* port (how facts are persisted/retrieved), and conflating
//! the two is how such seams rot. No SQL string or driver type appears in this
//! module — backends translate the closed [`FactFilter`] / bi-temporal
//! [`TemporalFilter`] to their dialect and map driver errors to the
//! driver-opaque [`StorageError`](crate::error::StorageError) at the seam.
//!
//! ## Shape
//!
//! Six bounded-context traits — [`FactGraph`], [`EventLog`], [`SearchIndex`],
//! [`ConsolidationStore`], [`SessionStore`], [`SchemaManager`] — aggregated by the
//! [`StorageBackend`] umbrella so the engine holds one `Arc<dyn StorageBackend>`;
//! the bounded traits are what tests mock in isolation (a forgetting test mocks
//! only [`FactGraph`]). `ColdStorage` is a **separate**, feature-gated
//! (`archive`) trait held as `Option<Arc<dyn ColdStorage>>`, not a supertrait
//! bound — so the umbrella's type stays feature-invariant.
//!
//! `async_trait` everywhere (the umbrella must be `dyn`-safe); timestamps cross as
//! `chrono::DateTime<Utc>` (the `SQLite` padded-RFC3339-TEXT lexicographic-ordering
//! trick becomes a `SQLite`-private serialization detail, no longer a
//! cross-cutting invariant a contributor can break from the engine side).

pub mod backend;
pub mod capabilities;
#[cfg(feature = "archive")]
pub mod cold_storage;
pub mod consolidation;
pub mod event_log;
pub mod filter;
pub mod graph;
pub mod schema;
pub mod search_index;
pub mod session;

pub use backend::StorageBackend;
pub use capabilities::{BackendCapabilities, LexicalRanker};
#[cfg(feature = "archive")]
pub use cold_storage::ColdStorage;
pub use consolidation::ConsolidationStore;
pub use event_log::EventLog;
pub use filter::{FactFilter, MetadataPredicate, TemporalFilter};
pub use graph::FactGraph;
pub use schema::SchemaManager;
pub use search_index::SearchIndex;
pub use session::SessionStore;
