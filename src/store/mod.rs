pub mod events;
pub mod facts;
pub mod schema;

pub use events::{EventFilter, EventStore};
pub use facts::FactStore;
pub use schema::{get_config, init_schema, open_connection, open_memory, set_config};

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
}
