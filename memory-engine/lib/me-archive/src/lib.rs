//! Archive primitive: `.pak` cold-storage compaction over `MemoryCtx` +
//! `ColdStorage`.
//!
//! Extracted from the facade in Wave 2 #816 / S4, sub-PR 3b. Two layers:
//!
//! - **The pure `.pak` primitive** ([`pak`], [`search`], [`types`]) — depends on
//!   nothing but `me-types` (L0): `me_types::math::cosine_similarity`,
//!   `me_types::error::*`, `me_types::types::*`,
//!   `me_types::types::archive::ARCHIVE_SCHEMA_VERSION`. Zero backend coupling,
//!   zero engine coupling.
//! - **The orchestration** ([`manage`]) — `MemoryEngine::{archive, list_archives,
//!   verify_archives, search_archives_fallback}`'s bodies, extracted as free
//!   functions over [`MemoryCtx`](me_storage::MemoryCtx) + the
//!   [`ColdStorage`](me_storage::ColdStorage) port + the graph + an explicit
//!   `archive_dir` path. `MemoryCtx` carries no path state — the facade resolves
//!   the archive directory (a sibling of the DB file) and passes it in.
//!
//! # The structural prize
//!
//! `manage::build_pak` (the `.pak` write stamp) used to live in the facade,
//! where `me-backend-sqlite`'s `CURRENT_SCHEMA_VERSION` was *nameable*. Now that
//! it lives here — a crate with **no** dependency on any `me-backend-*` crate — a
//! backend's schema-version constant is *unnameable* from either half of the
//! `.pak` write/read schema guard (`manage::build_pak` stamps,
//! [`pak::read_pak`] checks). The write/read symmetry sub-PR 3a established by
//! convention (both read `me_types::types::archive::ARCHIVE_SCHEMA_VERSION`) is
//! now compiler-enforced: there is no `me-backend-*` edge in this crate's
//! dependency graph for a maintainer to accidentally re-point the stamp at.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod manage;
pub mod pak;
mod search;
pub mod types;

pub use manage::{archive, list_archives, search_archives_fallback, verify_archives};
pub use search::ArchiveSearchResult;
pub use types::{ArchiveManifestEntry, ArchivePolicy, ArchiveStats, ArchiveVerifyResult};
