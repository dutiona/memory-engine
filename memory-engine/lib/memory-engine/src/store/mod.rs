//! `SQLite` persistence layer: events, facts, edges, summaries, scopes, and schema.
//!
//! Uses WAL mode for concurrent reads during writes.

pub mod activities;
#[cfg(feature = "archive")]
pub mod archive_manifest;
pub mod checkpoints;
pub mod edges;
pub mod embedding_meta;
pub mod embedding_spaces;
pub mod events;
pub mod fact_vectors;
pub mod facts;
pub mod lineage;
pub mod schema;
pub mod scopes;
pub mod summaries;

pub use scopes::ScopeStore;
// `UpcasterRegistry`/`UpcasterFn` carved into the me-storage (L1) port (Wave 2 #816);
// re-exported here (type + module) so `crate::store::UpcasterRegistry` AND
// `crate::store::upcaster::{UpcasterRegistry, UpcasterFn}` keep resolving unchanged.
pub use me_storage::{UpcasterRegistry, upcaster};

use chrono::{DateTime, Utc};

use crate::error::{MemoryError, Result};

/// Serialize an embedding (`&[f32]`) to little-endian bytes for `SQLite` BLOB storage.
#[must_use]
pub fn serialize_embedding(embedding: &[f32]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(embedding.len() * 4);
    for &val in embedding {
        buf.extend_from_slice(&val.to_le_bytes());
    }
    buf
}

/// Deserialize a BLOB back to `Vec<f32>`, validating dimension.
///
/// # Errors
///
/// Returns `MemoryError::EmbeddingDimension` if the blob size doesn't match
/// `dim * 4` bytes.
///
/// # Panics
///
/// Cannot panic. The `expect` call is guarded by `chunks_exact(4)` which
/// guarantees each chunk is exactly 4 bytes.
pub fn deserialize_embedding(blob: &[u8], dim: usize) -> Result<Vec<f32>> {
    if blob.len() != dim * 4 {
        return Err(MemoryError::EmbeddingDimension {
            expected: dim,
            actual: blob.len() / 4,
        });
    }
    Ok(blob
        .chunks_exact(4)
        .map(|chunk| {
            let arr: [u8; 4] = chunk.try_into().expect("chunks_exact guarantees 4 bytes");
            f32::from_le_bytes(arr)
        })
        .collect())
}

/// Parse an optional ISO 8601 timestamp from a nullable TEXT column.
pub fn parse_optional_timestamp(s: Option<&str>) -> rusqlite::Result<Option<DateTime<Utc>>> {
    s.map_or(Ok(None), |ts| {
        DateTime::parse_from_rfc3339(ts)
            .map(|dt| Some(dt.with_timezone(&Utc)))
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
    })
}

/// Parse a required ISO 8601 timestamp from a TEXT column.
pub fn parse_timestamp(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_round_trip() {
        let original = vec![1.0_f32, -0.5, 0.0, std::f32::consts::PI];
        let blob = serialize_embedding(&original);
        let recovered = deserialize_embedding(&blob, original.len()).unwrap();
        assert_eq!(original, recovered);
    }

    #[test]
    fn embedding_wrong_dimension() {
        let blob = serialize_embedding(&[1.0_f32, 2.0]);
        let err = deserialize_embedding(&blob, 3).unwrap_err();
        assert!(matches!(
            err,
            MemoryError::EmbeddingDimension {
                expected: 3,
                actual: 2
            }
        ));
    }

    #[test]
    fn embedding_empty() {
        let blob = serialize_embedding(&[]);
        let recovered = deserialize_embedding(&blob, 0).unwrap();
        assert!(recovered.is_empty());
    }

    mod proptest_embedding {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn roundtrip(embedding in proptest::collection::vec(
                any::<f32>().prop_filter("NaN != NaN in PartialEq", |f| !f.is_nan()),
                0..512,
            )) {
                let blob = serialize_embedding(&embedding);
                let recovered = deserialize_embedding(&blob, embedding.len()).unwrap();
                prop_assert_eq!(recovered, embedding);
            }

            #[test]
            fn wrong_dim_rejects(
                embedding in proptest::collection::vec(any::<f32>(), 1..128usize),
                delta in 1..64usize,
            ) {
                let blob = serialize_embedding(&embedding);
                let wrong_dim = embedding.len() + delta;
                let result = deserialize_embedding(&blob, wrong_dim);
                prop_assert!(result.is_err());
            }
        }
    }

    /// Property coverage for the two TEXT-column timestamp parsers (#487).
    ///
    /// `parse_timestamp` / `parse_optional_timestamp` wrap
    /// `DateTime::parse_from_rfc3339` and re-map any parse failure into a
    /// `rusqlite::Error::FromSqlConversionFailure`. The example tests above (none
    /// existed for these helpers) cannot cover the arbitrary-instant roundtrip nor
    /// the arbitrary-garbage rejection; these proptests do both from two angles.
    mod proptest_timestamps {
        use super::*;
        use proptest::prelude::*;

        prop_compose! {
            /// A `DateTime<Utc>` at whole-second precision over a wide but valid
            /// range (1970-01-01 .. ~2096). Whole seconds (`nanos == 0`) render
            /// without a fractional part in `to_rfc3339`, so the string roundtrips
            /// back to the *exact* same instant — the property under test. The
            /// range stays inside what `from_timestamp` always accepts, so the
            /// `expect` is infallible.
            fn arb_instant()(s in 0i64..=4_000_000_000) -> DateTime<Utc> {
                DateTime::<Utc>::from_timestamp(s, 0)
                    .expect("0..=4e9 is a valid Unix second range")
            }
        }

        proptest! {
            /// Roundtrip: any whole-second UTC instant survives a
            /// `to_rfc3339` -> `parse_timestamp` cycle byte-for-byte. A bug that
            /// dropped the timezone conversion or shifted the instant by a second
            /// would make `parsed != dt` and fail here.
            #[test]
            fn parse_timestamp_roundtrips_arbitrary_instant(dt in arb_instant()) {
                let rendered = dt.to_rfc3339();
                let parsed = parse_timestamp(&rendered)
                    .expect("a string produced by to_rfc3339 must parse");
                prop_assert_eq!(parsed, dt);
            }

            /// `parse_optional_timestamp(Some(valid))` yields `Ok(Some(dt))` with
            /// the identical instant — distinct from the `None` and error arms
            /// below, so a predicate flip between the three arms is caught.
            #[test]
            fn parse_optional_timestamp_some_valid_roundtrips(dt in arb_instant()) {
                let rendered = dt.to_rfc3339();
                let parsed = parse_optional_timestamp(Some(&rendered))
                    .expect("a string produced by to_rfc3339 must parse");
                prop_assert_eq!(parsed, Some(dt));
            }

            /// Negative: arbitrary strings that are NOT valid RFC3339 must return
            /// `Err(FromSqlConversionFailure)` — never `Ok`, never a different
            /// error variant, never a panic. We filter out the (vanishingly rare)
            /// case where a random string happens to be valid RFC3339 so the
            /// property is exactly "garbage -> the documented error".
            #[test]
            fn parse_timestamp_rejects_non_rfc3339(
                s in any::<String>().prop_filter(
                    "exclude accidentally-valid RFC3339 strings",
                    |s| DateTime::parse_from_rfc3339(s).is_err(),
                )
            ) {
                let err = parse_timestamp(&s).unwrap_err();
                prop_assert!(
                    matches!(err, rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, _)),
                    "expected FromSqlConversionFailure for {s:?}, got {err:?}"
                );
            }

            /// Same negative property through the optional wrapper: a present but
            /// invalid string is an `Err`, NOT silently coerced to `Ok(None)`
            /// (which would be the dangerous failure mode — losing the column).
            #[test]
            fn parse_optional_timestamp_some_invalid_errors(
                s in any::<String>().prop_filter(
                    "exclude accidentally-valid RFC3339 strings",
                    |s| DateTime::parse_from_rfc3339(s).is_err(),
                )
            ) {
                let err = parse_optional_timestamp(Some(&s)).unwrap_err();
                prop_assert!(
                    matches!(err, rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, _)),
                    "expected FromSqlConversionFailure for Some({s:?}), got {err:?}"
                );
            }
        }

        /// `None` maps to `Ok(None)` unconditionally — the third, input-free arm
        /// of `parse_optional_timestamp`. Kept as a plain test (no inputs to
        /// generate) so the three arms (None / Some-valid / Some-invalid) are all
        /// pinned.
        #[test]
        fn parse_optional_timestamp_none_is_ok_none() {
            let parsed = parse_optional_timestamp(None).expect("None must be Ok(None)");
            assert_eq!(parsed, None);
        }
    }
}
