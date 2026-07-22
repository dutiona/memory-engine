use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::types::facts::Fact;

/// Status of an activity record after server-side filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ActivityStatus {
    Recorded,
    Deduplicated,
    Ignored,
    Promoted,
}

impl fmt::Display for ActivityStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Recorded => write!(f, "recorded"),
            Self::Deduplicated => write!(f, "deduplicated"),
            Self::Ignored => write!(f, "ignored"),
            Self::Promoted => write!(f, "promoted"),
        }
    }
}

impl FromStr for ActivityStatus {
    type Err = ParseActivityStatusError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "recorded" => Ok(Self::Recorded),
            "deduplicated" => Ok(Self::Deduplicated),
            "ignored" => Ok(Self::Ignored),
            "promoted" => Ok(Self::Promoted),
            other => Err(ParseActivityStatusError(other.to_string())),
        }
    }
}

/// Error returned when [`ActivityStatus`] cannot be parsed from a string.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown activity status: {0}")]
pub struct ParseActivityStatusError(pub String);

/// Outcome class of a recorded tool activity.
///
/// Replaces the previously stringly-typed `outcome_class` field on [`Activity`],
/// [`NewActivity`], and [`RecordActivityRequest`]. Encoding the known outcomes as
/// variants makes the dedup-key invariant (the `(session_id, tool_name, args_hash,
/// outcome_class, scope_id)` index keys off the *string*) misuse-resistant: a
/// consumer can no longer accidentally pass an empty string that would silently
/// corrupt deduplication.
///
/// # String representation (DB + JSON back-compat)
///
/// [`Display`](fmt::Display)/[`FromStr`] are a total round-trip with the on-disk
/// `outcome_class TEXT` column and the MCP JSON boundary:
/// `Success` ⇄ `"success"`, `Error` ⇄ `"error"`, `TestFailure` ⇄ `"test_failure"`.
/// Any other stored string parses into the open [`Other`](Self::Other) variant and
/// serializes back **verbatim**, so existing activity rows keep their exact value.
/// [`Default`] is [`Success`](Self::Success), matching the DB column default.
///
/// The [`Other`](Self::Other) arm is an escape hatch for consumer-defined classes;
/// it deliberately **bypasses** compile-time checking and should be reserved for
/// values not covered by a named variant.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum OutcomeClass {
    /// The tool invocation succeeded. Serializes as `"success"`. Default variant.
    #[default]
    Success,
    /// The tool invocation failed. Serializes as `"error"`.
    Error,
    /// A test run reported failures. Serializes as `"test_failure"`.
    TestFailure,
    /// Any consumer-defined outcome class. Serializes as its inner string verbatim;
    /// bypasses the compile-time invariant the named variants provide.
    Other(String),
}

impl fmt::Display for OutcomeClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Success => f.write_str("success"),
            Self::Error => f.write_str("error"),
            Self::TestFailure => f.write_str("test_failure"),
            Self::Other(s) => f.write_str(s),
        }
    }
}

impl FromStr for OutcomeClass {
    type Err = std::convert::Infallible;

    /// Total parse: every string maps to a variant, unknown ones into
    /// [`Other`](Self::Other). The named arms match the exact lowercase strings the
    /// store writes; parsing is therefore the inverse of [`Display`](fmt::Display)
    /// and preserves stored values byte-for-byte on round-trip.
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(match s {
            "success" => Self::Success,
            "error" => Self::Error,
            "test_failure" => Self::TestFailure,
            other => Self::Other(other.to_owned()),
        })
    }
}

impl Serialize for OutcomeClass {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for OutcomeClass {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        // `FromStr` is infallible (`Other` captures the open set), so the `Err`
        // arm is unreachable — destructure the `Infallible` `Ok` directly.
        let Ok(class) = s.parse();
        Ok(class)
    }
}

/// An activity record from a tool invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    pub id: i64,
    pub session_id: String,
    pub tool_name: String,
    pub args_hash: String,
    pub args: serde_json::Value,
    pub result_summary: Option<String>,
    pub outcome_class: OutcomeClass,
    pub status: ActivityStatus,
    pub occurrence_count: i64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub scope_id: i64,
    pub promoted_fact_id: Option<i64>,
}

/// Activity to insert (DB assigns id).
#[derive(Debug, Clone)]
pub struct NewActivity {
    pub session_id: String,
    pub tool_name: String,
    pub args_hash: String,
    pub args: serde_json::Value,
    pub result_summary: Option<String>,
    pub outcome_class: OutcomeClass,
    pub timestamp: DateTime<Utc>,
    pub scope_id: i64,
}

/// A session checkpoint (last-write-wins per `session_id`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCheckpoint {
    pub session_id: String,
    pub scope_path: Option<String>,
    pub summary: Option<String>,
    pub last_activity_id: Option<i64>,
    pub checkpoint_at: DateTime<Utc>,
    pub metadata: serde_json::Value,
}

/// Request to record a tool activity.
#[derive(Debug, Clone)]
pub struct RecordActivityRequest {
    pub tool_name: String,
    pub args: serde_json::Value,
    pub result: Option<String>,
    pub session_id: String,
    pub timestamp: DateTime<Utc>,
    pub scope_path: Option<String>,
    /// Outcome class of the activity. `None` defaults to
    /// [`OutcomeClass::Success`] (the DB column default).
    pub outcome_class: Option<OutcomeClass>,
}

/// Result of recording an activity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordActivityResult {
    pub activity_id: Option<i64>,
    pub was_deduplicated: bool,
    pub promoted_fact_id: Option<i64>,
    pub status: ActivityStatus,
}

/// Project-scoped context for session bootstrap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectContext {
    pub scope_path: String,
    pub recent_activities: Vec<Activity>,
    pub last_checkpoint: Option<SessionCheckpoint>,
    pub relevant_facts: Vec<Fact>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_status_display() {
        assert_eq!(ActivityStatus::Recorded.to_string(), "recorded");
        assert_eq!(ActivityStatus::Deduplicated.to_string(), "deduplicated");
        assert_eq!(ActivityStatus::Ignored.to_string(), "ignored");
        assert_eq!(ActivityStatus::Promoted.to_string(), "promoted");
    }

    // --- ActivityStatus::from_str round-trip + error path ---

    #[test]
    fn activity_status_from_str_all_known_variants() {
        assert_eq!(
            ActivityStatus::from_str("recorded").unwrap(),
            ActivityStatus::Recorded
        );
        assert_eq!(
            ActivityStatus::from_str("deduplicated").unwrap(),
            ActivityStatus::Deduplicated
        );
        assert_eq!(
            ActivityStatus::from_str("ignored").unwrap(),
            ActivityStatus::Ignored
        );
        assert_eq!(
            ActivityStatus::from_str("promoted").unwrap(),
            ActivityStatus::Promoted
        );
    }

    #[test]
    fn activity_status_from_str_round_trips_with_display() {
        for status in [
            ActivityStatus::Recorded,
            ActivityStatus::Deduplicated,
            ActivityStatus::Ignored,
            ActivityStatus::Promoted,
        ] {
            let rendered = status.to_string();
            assert_eq!(
                ActivityStatus::from_str(&rendered).unwrap(),
                status,
                "Display->from_str round-trip failed for {status:?}"
            );
        }
    }

    #[test]
    fn activity_status_from_str_unknown_is_error() {
        let err = ActivityStatus::from_str("bogus").unwrap_err();
        assert_eq!(err.to_string(), "unknown activity status: bogus");
        // Case-sensitivity: the matcher expects lowercase variants.
        assert!(ActivityStatus::from_str("Recorded").is_err());
        assert!(ActivityStatus::from_str("").is_err());
    }

    // --- OutcomeClass (#347) ---

    #[test]
    fn outcome_class_display_emits_db_strings() {
        // The named variants render exactly the lowercase strings the store writes.
        assert_eq!(OutcomeClass::Success.to_string(), "success");
        assert_eq!(OutcomeClass::Error.to_string(), "error");
        assert_eq!(OutcomeClass::TestFailure.to_string(), "test_failure");
        assert_eq!(OutcomeClass::Other("flaky".into()).to_string(), "flaky");
    }

    #[test]
    fn outcome_class_default_is_success() {
        // Matches the `outcome_class TEXT NOT NULL DEFAULT 'success'` column default.
        assert_eq!(OutcomeClass::default(), OutcomeClass::Success);
    }

    #[test]
    fn outcome_class_from_str_maps_known_and_open_set() {
        let Ok(s) = OutcomeClass::from_str("success");
        assert_eq!(s, OutcomeClass::Success);
        let Ok(e) = OutcomeClass::from_str("error");
        assert_eq!(e, OutcomeClass::Error);
        let Ok(tf) = OutcomeClass::from_str("test_failure");
        assert_eq!(tf, OutcomeClass::TestFailure);
        // Any unrecognized string lands in the open `Other` arm verbatim — this is
        // the back-compat guarantee for activity rows written before this enum.
        let Ok(o) = OutcomeClass::from_str("custom-thing");
        assert_eq!(o, OutcomeClass::Other("custom-thing".into()));
        // Even the empty string round-trips (it is no longer a silent footgun: a
        // consumer must spell `Other("")` to produce it).
        let Ok(empty) = OutcomeClass::from_str("");
        assert_eq!(empty, OutcomeClass::Other(String::new()));
    }

    #[test]
    fn outcome_class_string_round_trip_is_lossless() {
        for class in [
            OutcomeClass::Success,
            OutcomeClass::Error,
            OutcomeClass::TestFailure,
            OutcomeClass::Other("vendor-specific".into()),
        ] {
            let rendered = class.to_string();
            let Ok(parsed) = OutcomeClass::from_str(&rendered);
            assert_eq!(parsed, class, "Display->from_str round-trip failed");
        }
    }

    #[test]
    fn outcome_class_serde_is_a_plain_string() {
        // Wire format is a bare JSON string (back-compat with the prior `String`
        // field + the MCP `"type": "string"` schema), not an externally-tagged enum.
        assert_eq!(
            serde_json::to_string(&OutcomeClass::TestFailure).unwrap(),
            "\"test_failure\""
        );
        let back: OutcomeClass = serde_json::from_str("\"error\"").unwrap();
        assert_eq!(back, OutcomeClass::Error);
        // An unknown stored value deserializes into the open arm, never an error.
        let open: OutcomeClass = serde_json::from_str("\"legacy_value\"").unwrap();
        assert_eq!(open, OutcomeClass::Other("legacy_value".into()));
    }
}
