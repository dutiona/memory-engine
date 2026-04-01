//! Archival compression: cold storage for expired facts.
//!
//! Moves expired, non-pinned facts into zstd-compressed `.pak` files
//! alongside the live database. Consumer-triggered, never automatic.

pub(crate) mod pak;
pub(crate) mod types;

pub use types::{
    ArchiveManifestEntry, ArchivePak, ArchivePolicy, ArchiveStats, ArchiveVerifyResult,
};
