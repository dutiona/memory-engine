//! SQLite persistence layer: events, facts, edges, summaries, scopes, and schema.
//!
//! Uses WAL mode for concurrent reads during writes.

pub mod activities;
#[cfg(feature = "archive")]
pub mod archive_manifest;
pub mod checkpoints;
pub mod edges;
pub mod events;
pub mod facts;
pub mod lineage;
pub mod schema;
pub mod scopes;
pub mod summaries;
pub mod upcaster;

pub use scopes::ScopeStore;
pub use upcaster::UpcasterRegistry;

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
pub(crate) fn parse_optional_timestamp(s: Option<&str>) -> rusqlite::Result<Option<DateTime<Utc>>> {
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
pub(crate) fn parse_timestamp(s: &str) -> rusqlite::Result<DateTime<Utc>> {
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
}
