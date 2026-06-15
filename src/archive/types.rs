use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::types::{Edge, Fact};

/// Current on-disk format version written into every new `.pak` file.
///
/// Bump this when the `.pak` payload layout changes in a way that requires
/// version-gated reading. The value is stamped into [`ArchivePak::pak_version`].
pub const CURRENT_PAK_VERSION: u32 = 1;

/// Contents of a `.pak` archive file — zstd-compressed JSON.
///
/// Contains only facts and edges; events stay in the live DB
/// (design: "archival is compaction preserving the event log").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivePak {
    pub pak_version: u32,
    pub engine_schema_version: u32,
    pub embed_dim: usize,
    pub created_at: DateTime<Utc>,
    pub facts: Vec<Fact>,
    pub edges: Vec<Edge>,
}

/// Policy controlling which facts are eligible for archival.
#[derive(Debug, Clone)]
pub struct ArchivePolicy {
    pub expired_before: DateTime<Utc>,
    pub min_facts: usize,
}

impl Default for ArchivePolicy {
    fn default() -> Self {
        Self {
            expired_before: Utc::now() - chrono::Duration::days(30),
            min_facts: 100,
        }
    }
}

/// Statistics returned after a successful archival operation.
#[derive(Debug, Clone)]
pub struct ArchiveStats {
    pub facts_archived: usize,
    pub edges_archived: usize,
    pub pak_path: PathBuf,
    pub pak_size_bytes: u64,
    pub blake3_hash: String,
}

/// A row from the `archive_manifest` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveManifestEntry {
    pub id: i64,
    pub pak_path: String,
    pub created_at: DateTime<Utc>,
    pub fact_count: i64,
    pub edge_count: i64,
    pub fact_id_min: i64,
    pub fact_id_max: i64,
    pub t_created_min: DateTime<Utc>,
    pub t_created_max: DateTime<Utc>,
    pub size_bytes: i64,
    pub blake3_hash: String,
}

/// Result of verifying a `.pak` file's integrity.
#[derive(Debug, Clone)]
pub struct ArchiveVerifyResult {
    pub manifest_id: i64,
    pub pak_path: String,
    pub ok: bool,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_policy_default_has_30_day_cutoff() {
        let policy = ArchivePolicy::default();
        let now = Utc::now();
        let diff = now - policy.expired_before;
        assert_eq!(diff.num_days(), 30);
        assert_eq!(policy.min_facts, 100);
    }

    #[test]
    fn archive_pak_roundtrip_serde() {
        let pak = ArchivePak {
            pak_version: CURRENT_PAK_VERSION,
            engine_schema_version: 7,
            embed_dim: 3,
            created_at: Utc::now(),
            facts: vec![],
            edges: vec![],
        };
        let json = serde_json::to_string(&pak).unwrap();
        let restored: ArchivePak = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.pak_version, CURRENT_PAK_VERSION);
        assert_eq!(restored.embed_dim, 3);
    }
}
