use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::facts::FactId;

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
/// write path returns it in [`PromotionResult`](crate::PromotionResult). Previously this struct carried a
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
/// [`LlmDreamCycle`](crate::LlmDreamCycle) should collapse into a single
/// synthesized `summary`.
///
/// The output of a [`DeltaProposer`](crate::traits::DeltaProposer), it is a raw
/// **proposal** — deliberately a plain DTO with no enforced invariants. It crosses
/// the HTTP boundary (an LLM backend deserializes it from model JSON), so rejecting a
/// malformed group at parse time would deny the cycle the chance to clamp it. The
/// `LlmDreamCycle` (A2) is responsible for clamping `source_ids` to the fed window
/// and dropping degenerate groups before turning each into a
/// [`CycleDelta::Synthesize`](crate::types::cycle_report::CycleDelta::Synthesize) (which
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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Property-based serde round-trips for `PromotionProvenance`.
    mod proptest_serde_roundtrip {
        use super::*;
        use proptest::prelude::*;

        /// Build a UTC timestamp from a second-offset proptest sample, clamped to a
        /// representable range so the strategy never produces an out-of-range value.
        fn ts_from_secs(secs: i64) -> DateTime<Utc> {
            let clamped = secs.clamp(-62_135_596_800, 253_402_300_799);
            DateTime::<Utc>::from_timestamp(clamped, 0).unwrap_or_else(Utc::now)
        }

        proptest! {
            /// `PromotionProvenance` is a lossless serde round-trip over arbitrary
            /// field values, and — post-#402 — no `lineage_id` token ever appears in
            /// the serialized JSON (the field was removed; the row PK is the
            /// authoritative id, carried on the companion record, not the envelope).
            #[test]
            fn promotion_provenance_roundtrips_and_omits_lineage_id(
                source_count in any::<u32>(),
                session_count in any::<u32>(),
                start_secs in any::<i64>(),
                end_secs in any::<i64>(),
                // Confidence is a score in [0, 1] by construction (see
                // `cluster_provenance`); this also keeps it clear of the
                // extreme-exponent magnitudes where serde_json's f64 formatting is
                // not bit-exact — a property of the encoder, not of the type.
                confidence in 0.0_f64..=1.0,
                method_version in ".*",
                representative_ids in proptest::collection::vec(any::<i64>(), 0..8),
            ) {
                let prov = PromotionProvenance {
                    source_count,
                    session_count,
                    date_range_start: ts_from_secs(start_secs),
                    date_range_end: ts_from_secs(end_secs),
                    confidence,
                    method_version,
                    representative_ids,
                };
                let value = serde_json::to_value(&prov).unwrap();
                // The phantom field is gone for good: no `lineage_id` *key* may ever
                // appear in the serialized object, for any input. Checked on the
                // parsed object (not a substring of the raw text) so an arbitrary
                // `method_version` that happens to contain the token cannot fool it.
                prop_assert!(
                    value.as_object().is_some_and(|o| !o.contains_key("lineage_id")),
                    "lineage_id key leaked into serialized provenance"
                );
                let back: PromotionProvenance = serde_json::from_value(value).unwrap();
                prop_assert_eq!(back, prov);
            }
        }
    }
}
