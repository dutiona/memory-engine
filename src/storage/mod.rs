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
//! ## Shape (built incrementally across #629's phases)
//!
//! Seven bounded-context traits, aggregated by the [`StorageBackend`] umbrella so
//! the engine holds one `Arc<dyn StorageBackend>`; the bounded traits are what
//! tests mock in isolation. `async_trait` everywhere (the umbrella must be
//! `dyn`-safe); timestamps cross as `chrono::DateTime<Utc>` (the `SQLite`
//! padded-RFC3339-TEXT lexicographic-ordering trick becomes a `SQLite`-private
//! serialization detail, no longer a cross-cutting invariant).

pub mod backend;
pub mod capabilities;
pub mod graph;
pub mod schema;

pub use backend::StorageBackend;
pub use capabilities::BackendCapabilities;
pub use graph::FactGraph;
pub use schema::SchemaManager;
