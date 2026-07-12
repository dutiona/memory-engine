use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::types::{Edge, Fact};

/// Current on-disk format version written into every new `.pak` file.
///
/// Bump this when the `.pak` payload layout changes in a way that requires
/// version-gated reading. The value is stamped into [`ArchivePak::pak_version`].
///
/// v2 (#274): `ArchivePak` embeds `facts: Vec<Fact>`, and `Fact`'s `importance`
/// field was renamed to `base_importance` with no `#[serde(alias)]` (clean break).
/// A v1 archive's `"importance"`-keyed facts can no longer be deserialized — the
/// no-default `base_importance` field makes `read_pak` reject them with a hard
/// "missing field" error rather than silently defaulting. The bump also gives a
/// clean forward-rejection: an older library reading a v2 archive fails the
/// `pak_version > CURRENT_PAK_VERSION` check instead of mis-parsing it.
pub const CURRENT_PAK_VERSION: u32 = 2;

/// Contents of a `.pak` archive file — zstd-compressed JSON.
///
/// Contains only facts and edges; events stay in the live DB
/// (design: "archival is compaction preserving the event log").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivePak {
    /// On-disk format version; checked on read against [`CURRENT_PAK_VERSION`].
    pub pak_version: u32,
    /// **Content**-schema version of this pak — `me_types`' backend-independent
    /// `ARCHIVE_SCHEMA_VERSION`, stamped on write and checked on read
    /// (forward-compatibility: a pak from a *newer* content schema is rejected).
    ///
    /// Despite the field's historical name, this is **not** the writing engine's backend
    /// schema version. A `.pak` is a portable `zstd` + JSON blob of `me-types` DTOs, so
    /// its compat gate is about DTO shape, not any backend's migration counter (which are
    /// not even comparable across backends). The name is retained because renaming it
    /// would break the on-disk format. (Wave 2 #816 / S4, sub-PR 3a.)
    pub engine_schema_version: u32,
    /// Embedding dimension of the facts stored in this pak. Must match the engine's
    /// configured `embed_dim` before any vectors are compared.
    pub embed_dim: usize,
    /// System timestamp when this pak was written (wall-clock, UTC).
    pub created_at: DateTime<Utc>,
    /// Archived facts; ordering is by `id` ascending (insertion order).
    pub facts: Vec<Fact>,
    /// Archived edges whose both endpoints are in `facts` (internal edges only).
    pub edges: Vec<Edge>,
}

/// Policy controlling which facts are eligible for archival.
#[derive(Debug, Clone)]
pub struct ArchivePolicy {
    /// Only facts whose system-time expiry (`t_expired`) is strictly before this
    /// instant are candidates for archival.
    pub expired_before: DateTime<Utc>,
    /// Minimum number of candidate facts required to trigger an archival pass.
    /// If fewer candidates exist, `archive()` returns `None` without writing a
    /// `.pak` file.
    pub min_facts: usize,
}

impl Default for ArchivePolicy {
    /// Returns a policy with a 30-day look-back cutoff relative to the current
    /// wall-clock time and a 100-fact minimum batch size.
    ///
    /// The `expired_before` cutoff is captured from the wall clock **when
    /// `default()` is called** — a retention policy's horizon is "facts that
    /// expired at least 30 days ago", which is meaningless without anchoring to
    /// *now*. Computing it here (rather than as a fixed compile-time timestamp)
    /// avoids silently archiving nothing — or everything — depending on when the
    /// binary was built.
    ///
    /// Note: the cutoff is frozen at construction, so a once-built `default()`
    /// does **not** slide its horizon forward over time. In a long-running
    /// process, construct a fresh `ArchivePolicy` (or set `expired_before`
    /// explicitly) per archival run.
    fn default() -> Self {
        Self {
            expired_before: Utc::now() - chrono::TimeDelta::days(30),
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

/// `ArchiveManifestEntry` moved to `me-types` (Wave 2 #816 E.4b Phase B) as pure data
/// (gated behind `me-types`'s `archive` feature); re-exported from `archive::mod` so
/// `crate::archive::ArchiveManifestEntry` keeps resolving.
pub use me_types::types::archive::ArchiveManifestEntry;

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
    use crate::types::FactType;

    #[test]
    fn archive_policy_default_has_30_day_cutoff() {
        let policy = ArchivePolicy::default();
        let now = Utc::now();
        let diff = now - policy.expired_before;
        assert_eq!(diff.num_days(), 30);
        assert_eq!(policy.min_facts, 100);
    }

    /// Fully-populated `ArchivePak` for the serde-shape guard below.
    ///
    /// Both DTO literals enumerate **every** field explicitly — deliberately no
    /// `..Default::default()`. That is the enforcement mechanism: adding a field to `Fact`
    /// or `Edge` makes this stop compiling (E0063), so nobody can change a `.pak`'s
    /// serialized shape without being confronted with the question the guard asks.
    fn shape_fixture_pak() -> ArchivePak {
        use crate::types::{Edge, Fact};

        let now = Utc::now();
        ArchivePak {
            pak_version: CURRENT_PAK_VERSION,
            engine_schema_version: 7,
            embed_dim: 3,
            created_at: now,
            facts: vec![Fact {
                id: 1,
                content: "shape".into(),
                content_hash: "h".into(),
                embedding: vec![0.0; 3],
                fact_type: FactType::Semantic,
                t_created: now,
                t_expired: None,
                t_valid: None,
                t_invalid: None,
                source_event_id: None,
                base_importance: 0.5,
                importance_score: 0.5,
                access_count: 0,
                last_accessed: now,
                metadata: serde_json::json!({}),
                scope_id: 1,
                is_pinned: false,
                surfaced_at: None,
            }],
            edges: vec![Edge {
                id: 1,
                source_fact_id: 1,
                target_fact_id: 2,
                relation_type: "related".into(),
                weight: 1.0,
                t_created: now,
                t_expired: None,
                scope_id: 1,
            }],
        }
    }

    /// Golden serde-shape guard for the `.pak` payload (Wave 2 #816 / S4, sub-PR 3a).
    ///
    /// # Why this test exists
    ///
    /// `ARCHIVE_SCHEMA_VERSION` (the version stamped into every `.pak` and checked on
    /// read) is a **content**-schema version: it must be bumped whenever the serialized
    /// shape of a pak's DTOs changes. Nothing in the type system enforces that, and the
    /// failure is **silent**: `Fact` does not `deny_unknown_fields` and several of its
    /// fields are `#[serde(default)]`, so a newer build that adds a field would write
    /// paks still stamped with the old version, and an older build would accept them
    /// (`stamp <= supported`) while quietly dropping the new field.
    ///
    /// Previously the stamp was inherited from `SQLite`'s `CURRENT_SCHEMA_VERSION`, so
    /// *any* migration moved it — over-approximating, but it made the guard automatic.
    /// Decoupling the archive from a backend counter (the right call — a `.pak` is a
    /// portable DTO blob, not a backend artifact) removed that automatic bump. This test
    /// puts the enforcement back into the **structure** instead of relying on a
    /// maintainer remembering: change a serialized field name in `ArchivePak`, `Fact`, or
    /// `Edge`, and this reddens.
    ///
    /// If it fails, that is the question being asked: **does this shape change break
    /// older readers?** If yes, bump `ARCHIVE_SCHEMA_VERSION`. If no (e.g. a purely
    /// additive `#[serde(default)]` field older readers can ignore), update the expected
    /// key set below and say why in the commit message.
    #[test]
    fn archive_pak_serde_shape_is_pinned() {
        let value = serde_json::to_value(shape_fixture_pak()).expect("pak serializes");
        let keys = |v: &serde_json::Value| -> Vec<String> {
            let mut k: Vec<String> = v
                .as_object()
                .expect("object")
                .keys()
                .map(ToString::to_string)
                .collect();
            k.sort();
            k
        };

        assert_eq!(
            keys(&value),
            vec![
                "created_at",
                "edges",
                "embed_dim",
                "engine_schema_version",
                "facts",
                "pak_version",
            ],
            "ArchivePak's serialized shape changed — bump ARCHIVE_SCHEMA_VERSION if this \
             breaks older readers, or update this expectation and justify the compat"
        );
        assert_eq!(
            keys(&value["facts"][0]),
            vec![
                "access_count",
                "base_importance",
                "content",
                "content_hash",
                "embedding",
                "fact_type",
                "id",
                "importance_score",
                "is_pinned",
                "last_accessed",
                "metadata",
                "scope_id",
                "source_event_id",
                "surfaced_at",
                "t_created",
                "t_expired",
                "t_invalid",
                "t_valid",
            ],
            "Fact's serialized shape changed — a .pak embeds Fact verbatim. Bump \
             ARCHIVE_SCHEMA_VERSION if older readers cannot consume this, or update this \
             expectation and justify the compat"
        );
        assert_eq!(
            keys(&value["edges"][0]),
            vec![
                "id",
                "relation_type",
                "scope_id",
                "source_fact_id",
                "t_created",
                "t_expired",
                "target_fact_id",
                "weight",
            ],
            "Edge's serialized shape changed — a .pak embeds Edge verbatim. Bump \
             ARCHIVE_SCHEMA_VERSION if older readers cannot consume this, or update this \
             expectation and justify the compat"
        );
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

    /// A *populated* serde round-trip (#419): the empty-payload test above never
    /// exercises a single `Fact`/`Edge` through serde, so every field of those
    /// types — the four bi-temporal timestamps (both `Some` and `None`), the
    /// `Vec<f32>` embedding, `is_pinned`, `metadata`, `importance_score`,
    /// `surfaced_at` — is asserted to survive the JSON round-trip bit-for-bit.
    #[test]
    fn archive_pak_roundtrip_serde_populated() {
        use chrono::TimeZone;
        let t0 = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
        let t1 = Utc.with_ymd_and_hms(2026, 6, 7, 8, 9, 10).unwrap();

        // Fact A: every Option timestamp is `Some`, pinned, non-empty embedding,
        // populated metadata, and a non-zero importance_score / surfaced_at.
        let fact_a = Fact {
            id: 11,
            content: "a fully populated fact".into(),
            content_hash: "hash-a".into(),
            embedding: vec![0.1, -0.2, 3.5, f32::MIN_POSITIVE],
            fact_type: FactType::Procedural,
            t_created: t0,
            t_expired: Some(t1),
            t_valid: Some(t0),
            t_invalid: Some(t1),
            source_event_id: Some(99),
            base_importance: 0.75,
            access_count: 42,
            last_accessed: t1,
            metadata: serde_json::json!({"k": "v", "n": 7, "nested": [1, 2, 3]}),
            scope_id: 5,
            is_pinned: true,
            importance_score: 0.625,
            surfaced_at: Some(t1),
        };
        // Fact B: the None arm of every bi-temporal bound, empty embedding,
        // unpinned, default-ish scalars — the complementary serde path.
        let fact_b = Fact {
            id: 12,
            content: "minimal fact".into(),
            content_hash: "hash-b".into(),
            embedding: vec![],
            fact_type: FactType::Episodic,
            t_created: t0,
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            base_importance: 0.5,
            access_count: 0,
            last_accessed: t0,
            metadata: serde_json::json!({}),
            scope_id: 1,
            is_pinned: false,
            importance_score: 0.0,
            surfaced_at: None,
        };
        let edge = Edge {
            id: 21,
            source_fact_id: 11,
            target_fact_id: 12,
            relation_type: "relates_to".into(),
            weight: 0.9,
            t_created: t0,
            t_expired: Some(t1),
            scope_id: 5,
        };

        let pak = ArchivePak {
            pak_version: CURRENT_PAK_VERSION,
            engine_schema_version: 7,
            embed_dim: 4,
            created_at: Utc::now(),
            facts: vec![fact_a, fact_b],
            edges: vec![edge],
        };
        let json = serde_json::to_string(&pak).unwrap();
        let restored: ArchivePak = serde_json::from_str(&json).unwrap();

        // `Fact` and `Edge` both derive `PartialEq`, so equality covers every
        // field — no need to spell them out one by one.
        assert_eq!(restored.pak_version, pak.pak_version);
        assert_eq!(restored.engine_schema_version, pak.engine_schema_version);
        assert_eq!(restored.embed_dim, pak.embed_dim);
        assert_eq!(restored.created_at, pak.created_at);
        assert_eq!(restored.facts, pak.facts);
        assert_eq!(restored.edges, pak.edges);
    }

    /// Property-based serde round-trip over arbitrary `ArchivePak` payloads (#420).
    ///
    /// The example tests pin a handful of fixed shapes; this asserts
    /// `from_str(to_string(x)) == x` across the whole input space — arbitrary
    /// versions, embed dims, fact/edge counts, embeddings, and the `Some`/`None`
    /// axis of every bi-temporal bound.
    mod proptest_roundtrip {
        use super::*;
        use proptest::prelude::*;

        /// Build a UTC timestamp from a second-offset sample, clamped to a
        /// representable range so the strategy never overflows.
        fn ts_from_secs(secs: i64) -> DateTime<Utc> {
            let clamped = secs.clamp(-62_135_596_800, 253_402_300_799);
            DateTime::<Utc>::from_timestamp(clamped, 0).unwrap_or_else(Utc::now)
        }

        // serde_json (default features, which this crate uses) serializes f32 with
        // Grisu/Ryū shortest-decimal and parses with a *correctly-rounded* path:
        // an exhaustive 2^32 sweep of every finite f32 round-trips bit-for-bit
        // (verified by brute force, #420 — the earlier "1 ULP fast-path loss"
        // premise was wrong). So the embedding strategy draws arbitrary *finite*
        // f32 directly and asserts EXACT equality, restoring the full-coverage the
        // finding asked for instead of the discretized n/1000 proxy.

        /// An f64 in `[0, 1]` with three decimals — the importance-field band.
        fn arb_importance() -> impl Strategy<Value = f64> {
            (0u32..=1000).prop_map(|n| f64::from(n) / 1000.0)
        }

        /// An arbitrary *finite* f32 embedding component (full value space).
        /// NaN/±∞ are excluded because `serde_json` maps them to JSON `null`
        /// (documented behavior, not a precision loss), which would fail the
        /// exact-equality round-trip for reasons unrelated to float fidelity.
        fn arb_embed_component() -> impl Strategy<Value = f32> {
            any::<f32>().prop_filter("finite", |x| x.is_finite())
        }

        prop_compose! {
            fn arb_fact()(
                id in any::<i64>(),
                content in ".{0,32}",
                content_hash in ".{0,16}",
                embedding in prop::collection::vec(arb_embed_component(), 0..8),
                fact_type in prop_oneof![
                    Just(FactType::Episodic),
                    Just(FactType::Semantic),
                    Just(FactType::Procedural),
                ],
                t_created_s in any::<i64>(),
                t_expired_s in proptest::option::of(any::<i64>()),
                t_valid_s in proptest::option::of(any::<i64>()),
                t_invalid_s in proptest::option::of(any::<i64>()),
                source_event_id in proptest::option::of(any::<i64>()),
                base_importance in arb_importance(),
                access_count in any::<i64>(),
                last_accessed_s in any::<i64>(),
                scope_id in any::<i64>(),
                is_pinned in any::<bool>(),
                importance_score in arb_importance(),
                surfaced_at_s in proptest::option::of(any::<i64>()),
            ) -> Fact {
                Fact {
                    id,
                    content,
                    content_hash,
                    embedding,
                    fact_type,
                    t_created: ts_from_secs(t_created_s),
                    t_expired: t_expired_s.map(ts_from_secs),
                    t_valid: t_valid_s.map(ts_from_secs),
                    t_invalid: t_invalid_s.map(ts_from_secs),
                    source_event_id,
                    base_importance,
                    access_count,
                    last_accessed: ts_from_secs(last_accessed_s),
                    metadata: serde_json::json!({}),
                    scope_id,
                    is_pinned,
                    importance_score,
                    surfaced_at: surfaced_at_s.map(ts_from_secs),
                }
            }
        }

        prop_compose! {
            fn arb_edge()(
                id in any::<i64>(),
                source_fact_id in any::<i64>(),
                target_fact_id in any::<i64>(),
                // Mix the 4 canonical spellings (so the named variants traverse
                // the .pak round-trip, not just the Custom arm) with the original
                // bounded arbitrary-string strategy (weighted to keep Custom
                // well-covered).
                relation_type_str in prop_oneof![
                    4 => proptest::string::string_regex(".{0,16}").expect("valid regex"),
                    1 => Just("co_session".to_string()),
                    1 => Just("supplements".to_string()),
                    1 => Just("contradicts".to_string()),
                    1 => Just("supersedes".to_string()),
                ],
                // Discretized weight (n/1000) for the same exact-round-trip reason
                // as the importance/embedding strategies above. Drawn from `i32` so
                // `f64::from` is exact (no `cast_precision_loss`).
                weight in any::<i32>().prop_map(|n| f64::from(n) / 1000.0),
                t_created_s in any::<i64>(),
                t_expired_s in proptest::option::of(any::<i64>()),
                scope_id in any::<i64>(),
            ) -> Edge {
                Edge {
                    id,
                    source_fact_id,
                    target_fact_id,
                    relation_type: crate::types::RelationType::from(relation_type_str.as_str()),
                    weight,
                    t_created: ts_from_secs(t_created_s),
                    t_expired: t_expired_s.map(ts_from_secs),
                    scope_id,
                }
            }
        }

        proptest! {
            #[test]
            fn archive_pak_serde_roundtrip_prop(
                pak_version in 0u32..=10,
                engine_schema_version in 0u32..=20,
                embed_dim in 0usize..=128,
                created_at_s in any::<i64>(),
                facts in prop::collection::vec(arb_fact(), 0..4),
                edges in prop::collection::vec(arb_edge(), 0..4),
            ) {
                // Construct via struct literal (not `new`) so the arbitrary
                // `pak_version` / `created_at` are exercised through serde.
                let pak = ArchivePak {
                    pak_version,
                    engine_schema_version,
                    embed_dim,
                    created_at: ts_from_secs(created_at_s),
                    facts,
                    edges,
                };
                let json = serde_json::to_string(&pak).unwrap();
                let restored: ArchivePak = serde_json::from_str(&json).unwrap();
                prop_assert_eq!(restored.pak_version, pak.pak_version);
                prop_assert_eq!(restored.engine_schema_version, pak.engine_schema_version);
                prop_assert_eq!(restored.embed_dim, pak.embed_dim);
                prop_assert_eq!(restored.created_at, pak.created_at);
                prop_assert_eq!(&restored.facts, &pak.facts);
                prop_assert_eq!(&restored.edges, &pak.edges);
            }
        }
    }
}
