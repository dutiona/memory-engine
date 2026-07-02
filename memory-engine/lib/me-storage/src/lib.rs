//! # me-storage
//!
//! Layer-1 (L1) of the memory-engine crate workspace (Wave 2, #816): the persistence
//! **port**. Depends only on `me-types` (L0) and `me-traits` (L0.5) — the layering
//! invariant `cargo` enforces.
//!
//! It owns three things:
//! - the `StorageBackend` trait family — six bounded-context traits
//!   ([`FactGraph`], [`EventLog`], [`SearchIndex`], [`ConsolidationStore`],
//!   [`SessionStore`], [`SchemaManager`]) aggregated by the [`StorageBackend`]
//!   umbrella, plus the feature-gated `ColdStorage` trait and the closed
//!   [`FactFilter`]/[`TemporalFilter`] query vocabulary;
//! - [`MemoryCtx`] — the universal capability handle the L3 primitives operate on;
//! - [`UpcasterRegistry`] — the event-payload versioning policy the port applies on
//!   event read (both backends consume one definition).
//!
//! No SQL string or driver type appears here: backends (`me-backend-sqlite`,
//! `me-backend-postgres`) implement these traits and map driver errors to the
//! driver-opaque [`StorageError`](me_types::error::StorageError) at the seam. The
//! concrete backends are NOT re-exported here — the facade selects one.
//!
//! `async_trait` everywhere (the umbrella must be `dyn`-safe); timestamps cross as
//! `chrono::DateTime<Utc>`.

// Panic-safety gate (#725, workspace lints). This crate's own `#[cfg(test)]` unit
// tests are exempt — a panic there is the intended failure signal.
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod backend;
pub mod capabilities;
#[cfg(feature = "archive")]
pub mod cold_storage;
pub mod consolidation;
pub mod ctx;
pub mod event_log;
pub mod filter;
pub mod graph;
pub mod schema;
pub mod search_index;
pub mod session;
pub mod upcaster;

pub use backend::StorageBackend;
pub use capabilities::{BackendCapabilities, LexicalRanker};
#[cfg(feature = "archive")]
pub use cold_storage::ColdStorage;
pub use consolidation::ConsolidationStore;
pub use ctx::MemoryCtx;
pub use event_log::EventLog;
pub use filter::{FactFilter, MetadataPredicate, TemporalFilter};
pub use graph::{BootstrapIngestOutcome, FactGraph};
pub use schema::SchemaManager;
pub use search_index::SearchIndex;
pub use session::SessionStore;
pub use upcaster::{UpcasterFn, UpcasterRegistry};
