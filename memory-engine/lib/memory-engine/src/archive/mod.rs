//! Archival compression: cold storage for expired facts.
//!
//! Moves expired, non-pinned facts into zstd-compressed `.pak` files
//! alongside the live database. Consumer-triggered, never automatic.

pub mod pak;
pub mod search;
pub mod types;

pub use types::{ArchiveManifestEntry, ArchivePolicy, ArchiveStats, ArchiveVerifyResult};
