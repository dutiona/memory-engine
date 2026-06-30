use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Typed edge-relation label for the knowledge graph.
///
/// An **open enum**: the four canonical well-known relation types are represented
/// as variants for exhaustive pattern matching and zero-cost equality; any other
/// label is carried transparently as [`RelationType::Custom`].
///
/// ## Construction (canonical, infallible)
///
/// Use [`From<&str>`] / [`From<String>`]: both are **canonicalizing**, meaning
/// the four known spellings always produce their named variant — never
/// `Custom("co_session")`. Unknown strings become `Custom`.
///
/// ```
/// use memory_engine::RelationType;
///
/// assert_eq!(RelationType::from("co_session"), RelationType::CoSession);
/// assert_eq!(RelationType::from("supplements"), RelationType::Supplements);
/// assert_eq!(RelationType::from("contradicts"), RelationType::Contradicts);
/// assert_eq!(RelationType::from("supersedes"),  RelationType::Supersedes);
/// assert_eq!(RelationType::from("my_custom"),   RelationType::Custom("my_custom".into()));
/// ```
///
/// ## Wire format (byte-identical to `String`)
///
/// Serializes as a **plain string** via
/// `#[serde(into = "String", from = "String")]`, preserving byte-for-byte
/// compatibility with the old `relation_type: String` field across every
/// payload — a JSON string in JSON payloads (`EngineSnapshot` dumps and archive
/// `.pak` files, which are zstd-compressed JSON) and a `MessagePack` string in
/// the `MessagePack` snapshot sidecar (`GraphEdgeSnapshot`).
///
/// ## Equality
///
/// Equality is **semantic**, not structural: two values are equal iff their
/// canonical [`as_str`](RelationType::as_str) spelling matches. `PartialEq`,
/// `Eq` and `Hash` are hand-written (not derived) so the redundant
/// representation `Custom("co_session")` compares and hashes equal to
/// `CoSession`. Without this, structural equality would break the
/// `deserialize(serialize(x)) == x` round-trip law (deserialize folds
/// `Custom("co_session")` back to `CoSession`) and defeat `HashSet` dedup.
/// `PartialEq<&str>` / `PartialEq<str>` are also provided so call sites that
/// compare `edge.relation_type == "supersedes"` continue to compile without
/// churn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(into = "String", from = "String")]
pub enum RelationType {
    /// Edges linking facts that co-occur in the same session.
    /// On-wire string: `"co_session"`.
    CoSession,
    /// An edge indicating one fact supplements (adds to / coexists with) another.
    /// On-wire string: `"supplements"`.
    Supplements,
    /// An edge indicating one fact contradicts another.
    /// On-wire string: `"contradicts"`.
    Contradicts,
    /// An edge indicating one fact supersedes (replaces) another.
    /// On-wire string: `"supersedes"`.
    Supersedes,
    /// Any other relation label not covered by the four canonical variants.
    Custom(String),
}

impl RelationType {
    /// Return the canonical on-wire string for this relation type.
    ///
    /// This is the single source of truth for the enum → string mapping. `Display`
    /// delegates here so `format!("{}", rt)` and `rt.to_string()` both emit the
    /// wire-format string.
    #[must_use]
    pub const fn as_str(&self) -> &str {
        match self {
            Self::CoSession => "co_session",
            Self::Supplements => "supplements",
            Self::Contradicts => "contradicts",
            Self::Supersedes => "supersedes",
            Self::Custom(s) => s.as_str(),
        }
    }
}

impl fmt::Display for RelationType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for RelationType {
    /// Infallible, canonicalizing construction from a string slice.
    ///
    /// The four canonical spellings map to their named variants; everything else
    /// becomes [`RelationType::Custom`]. Case-sensitive: the canonical spellings are
    /// lowercase snake\_case as stored on-disk.
    fn from(s: &str) -> Self {
        match s {
            "co_session" => Self::CoSession,
            "supplements" => Self::Supplements,
            "contradicts" => Self::Contradicts,
            "supersedes" => Self::Supersedes,
            other => Self::Custom(other.to_owned()),
        }
    }
}

impl From<String> for RelationType {
    /// Infallible, canonicalizing construction from an owned `String`.
    ///
    /// Delegates to [`From<&str>`] so the canonical-variant logic lives in
    /// exactly one place.
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

impl From<RelationType> for String {
    /// Convert to the canonical on-wire string.
    ///
    /// Required by `#[serde(into = "String")]`. Delegates to [`RelationType::as_str`].
    fn from(rt: RelationType) -> Self {
        rt.as_str().to_owned()
    }
}

impl FromStr for RelationType {
    type Err = std::convert::Infallible;

    /// Infallible parse — delegates to [`From<&str>`].
    ///
    /// Satisfies generic bounds that require `str::parse::<RelationType>()`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::from(s))
    }
}

// --- Semantic equality (hand-written, not derived) ---
//
// Two relation types are equal iff their canonical on-wire string (`as_str`)
// matches. The canonicalizing `From` admits a redundant representation —
// `Custom("co_session")` means the same as `CoSession` — and structural
// equality would treat those as distinct, breaking the
// `deserialize(serialize(x)) == x` round-trip (deserialize folds the redundant
// form back to the variant) and `HashSet` dedup. `Hash` delegates to the same
// `as_str` key so the `Eq`/`Hash` contract (`a == b ⇒ hash(a) == hash(b)`) holds.

impl PartialEq for RelationType {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for RelationType {}

impl std::hash::Hash for RelationType {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

// --- PartialEq<&str> / PartialEq<str> for minimal churn at comparison sites ---

impl PartialEq<&str> for RelationType {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<str> for RelationType {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<RelationType> for &str {
    fn eq(&self, other: &RelationType) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<RelationType> for str {
    fn eq(&self, other: &RelationType) -> bool {
        self == other.as_str()
    }
}

// --- String PartialEq convenience ---

impl PartialEq<String> for RelationType {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<RelationType> for String {
    fn eq(&self, other: &RelationType) -> bool {
        self.as_str() == other.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Canonical round-trip invariant ---

    #[test]
    fn canonical_variants_round_trip_through_from_str() {
        for (input, expected) in [
            ("co_session", RelationType::CoSession),
            ("supplements", RelationType::Supplements),
            ("contradicts", RelationType::Contradicts),
            ("supersedes", RelationType::Supersedes),
        ] {
            let got = RelationType::from(input);
            assert_eq!(
                got, expected,
                "canonical string '{input}' must produce its named variant"
            );
            // Reverse: the variant's as_str() must reproduce the canonical string.
            assert_eq!(
                got.as_str(),
                input,
                "as_str() of {expected:?} must equal '{input}'"
            );
            // Display matches as_str().
            assert_eq!(
                got.to_string(),
                input,
                "Display of {expected:?} must equal '{input}'"
            );
        }
    }

    #[test]
    fn from_string_owned_matches_from_str() {
        for s in [
            "co_session",
            "supplements",
            "contradicts",
            "supersedes",
            "custom_rel",
        ] {
            assert_eq!(
                RelationType::from(s.to_owned()),
                RelationType::from(s),
                "From<String> must agree with From<&str> for '{s}'"
            );
        }
    }

    // --- Custom round-trip ---

    #[test]
    fn custom_round_trips() {
        let input = "my_arbitrary_relation";
        let rt = RelationType::from(input);
        assert!(
            matches!(&rt, RelationType::Custom(s) if s == input),
            "unknown string must produce Custom variant"
        );
        assert_eq!(
            rt.as_str(),
            input,
            "Custom::as_str must return the inner string"
        );
        assert_eq!(rt.to_string(), input);
        // Round-trip: from(rt.to_string()) must reproduce the original value.
        assert_eq!(RelationType::from(rt.to_string()), rt);
    }

    #[test]
    fn empty_string_becomes_custom() {
        let rt = RelationType::from("");
        assert!(
            matches!(&rt, RelationType::Custom(s) if s.is_empty()),
            "empty string must become Custom(\"\"), not a canonical variant"
        );
        assert_eq!(rt.as_str(), "");
    }

    // --- Canonicalization: known strings NEVER produce Custom ---

    #[test]
    fn known_strings_never_produce_custom() {
        for s in ["co_session", "supplements", "contradicts", "supersedes"] {
            let rt = RelationType::from(s);
            assert!(
                !matches!(rt, RelationType::Custom(_)),
                "canonical string '{s}' must not produce Custom — it must canonicalize to its variant"
            );
        }
    }

    #[test]
    fn case_mismatch_becomes_custom() {
        // Construction is case-sensitive: non-canonical casing → Custom.
        for s in ["Co_Session", "SUPPLEMENTS", "Contradicts", "Supersedes"] {
            let rt = RelationType::from(s);
            assert!(
                matches!(rt, RelationType::Custom(_)),
                "non-canonical casing '{s}' must produce Custom, not a named variant"
            );
        }
    }

    // --- PartialEq<&str> ---

    #[test]
    fn partial_eq_str_matches_as_str() {
        assert_eq!(RelationType::CoSession, "co_session");
        assert_eq!(RelationType::Supplements, "supplements");
        assert_eq!(RelationType::Contradicts, "contradicts");
        assert_eq!(RelationType::Supersedes, "supersedes");
        assert_eq!(RelationType::Custom("foo".into()), "foo");

        assert_ne!(RelationType::CoSession, "supplements");
        assert_ne!(RelationType::Custom("bar".into()), "baz");
    }

    #[test]
    fn partial_eq_reflexive_for_str_lhs() {
        // PartialEq<RelationType> for &str
        assert!("co_session" == RelationType::CoSession);
        assert!("supersedes" == RelationType::Supersedes);
        assert!("other" != RelationType::CoSession);
    }

    // --- Semantic self-equality: redundant Custom(canonical) == named variant ---

    #[test]
    fn custom_canonical_equals_named_variant_and_dedups() {
        let pairs = [
            (
                RelationType::Custom("co_session".into()),
                RelationType::CoSession,
            ),
            (
                RelationType::Custom("supplements".into()),
                RelationType::Supplements,
            ),
            (
                RelationType::Custom("contradicts".into()),
                RelationType::Contradicts,
            ),
            (
                RelationType::Custom("supersedes".into()),
                RelationType::Supersedes,
            ),
        ];
        for (custom, named) in pairs {
            assert_eq!(
                custom, named,
                "Custom(canonical) must equal the named variant"
            );
            // Eq/Hash contract: equal values must hash equally.
            let mut h1 = std::collections::hash_map::DefaultHasher::new();
            let mut h2 = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(&custom, &mut h1);
            std::hash::Hash::hash(&named, &mut h2);
            assert_eq!(
                std::hash::Hasher::finish(&h1),
                std::hash::Hasher::finish(&h2),
                "equal RelationType values must hash equally"
            );
            // HashSet collapses the redundant representation into one.
            let set: std::collections::HashSet<RelationType> =
                [custom.clone(), named.clone()].into_iter().collect();
            assert_eq!(set.len(), 1, "redundant representations must dedup to one");
        }
        // The serialize -> deserialize round-trip is identity under == even for
        // the redundant representation (deserialize canonicalizes it).
        let custom = RelationType::Custom("supersedes".into());
        let json = serde_json::to_string(&custom).unwrap();
        let back: RelationType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, custom);
        assert_eq!(back, RelationType::Supersedes);
    }

    // --- Serde: plain-string wire format ---

    #[test]
    fn serde_serializes_to_plain_string() {
        // The serde representation must be a plain JSON string, NOT a tagged enum.
        assert_eq!(
            serde_json::to_string(&RelationType::CoSession).unwrap(),
            "\"co_session\""
        );
        assert_eq!(
            serde_json::to_string(&RelationType::Supplements).unwrap(),
            "\"supplements\""
        );
        assert_eq!(
            serde_json::to_string(&RelationType::Contradicts).unwrap(),
            "\"contradicts\""
        );
        assert_eq!(
            serde_json::to_string(&RelationType::Supersedes).unwrap(),
            "\"supersedes\""
        );
        assert_eq!(
            serde_json::to_string(&RelationType::Custom("my_rel".into())).unwrap(),
            "\"my_rel\""
        );
    }

    #[test]
    fn serde_deserializes_from_plain_string() {
        assert_eq!(
            serde_json::from_str::<RelationType>("\"co_session\"").unwrap(),
            RelationType::CoSession
        );
        assert_eq!(
            serde_json::from_str::<RelationType>("\"supplements\"").unwrap(),
            RelationType::Supplements
        );
        assert_eq!(
            serde_json::from_str::<RelationType>("\"custom_rel\"").unwrap(),
            RelationType::Custom("custom_rel".into())
        );
    }

    #[test]
    fn serde_roundtrip_canonical_and_custom() {
        for rt in [
            RelationType::CoSession,
            RelationType::Supplements,
            RelationType::Contradicts,
            RelationType::Supersedes,
            RelationType::Custom("arbitrary".into()),
            RelationType::Custom(String::new()),
        ] {
            let json = serde_json::to_string(&rt).unwrap();
            let back: RelationType = serde_json::from_str(&json).unwrap();
            assert_eq!(back, rt, "serde round-trip must be identity for {rt:?}");
        }
    }

    // --- FromStr (Infallible) ---

    #[test]
    fn from_str_infallible_delegates_to_from() {
        let rt: RelationType = "co_session".parse().unwrap();
        assert_eq!(rt, RelationType::CoSession);

        let rt: RelationType = "unknown_rel".parse().unwrap();
        assert_eq!(rt, RelationType::Custom("unknown_rel".into()));
    }

    // --- From<RelationType> for String ---

    #[test]
    fn into_string_returns_wire_form() {
        let s: String = RelationType::CoSession.into();
        assert_eq!(s, "co_session");
        let s: String = RelationType::Custom("x".into()).into();
        assert_eq!(s, "x");
    }
}
