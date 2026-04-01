/// Errors returned by the memory engine.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("invalid embedding dimension: expected {expected}, got {actual}")]
    EmbeddingDimension { expected: usize, actual: usize },

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("schema migration failed: {0}")]
    Migration(String),

    #[error("not implemented: {0}")]
    NotImplemented(String),

    #[error("connection pool error: {0}")]
    Pool(String),

    #[error(
        "unsupported storage epoch: database is epoch {db_epoch}, this library supports epoch {supported_epoch}"
    )]
    UnsupportedEpoch { db_epoch: u16, supported_epoch: u16 },

    #[error("internal error: {0}")]
    Internal(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("bootstrap error: {0}")]
    Bootstrap(String),

    #[error("reranker error: {0}")]
    Reranker(String),

    #[error("archive error: {0}")]
    Archive(String),

    /// Attempted a write operation on a read-only engine.
    #[error("operation requires write access, but engine was opened read-only")]
    ReadOnly,
}

/// Convenience alias for `Result<T, MemoryError>`.
pub type Result<T> = std::result::Result<T, MemoryError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display() {
        let err = MemoryError::NotFound("fact 42".into());
        assert_eq!(err.to_string(), "not found: fact 42");
    }

    #[test]
    fn from_rusqlite_error() {
        let sqlite_err = rusqlite::Error::QueryReturnedNoRows;
        let err: MemoryError = sqlite_err.into();
        assert!(matches!(err, MemoryError::Database(_)));
    }

    #[test]
    fn from_serde_json_error() {
        let json_err = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let err: MemoryError = json_err.into();
        assert!(matches!(err, MemoryError::Serialization(_)));
    }

    #[test]
    fn reranker_error_display() {
        let err = MemoryError::Reranker("cross-encoder timeout".into());
        assert_eq!(err.to_string(), "reranker error: cross-encoder timeout");
    }

    #[test]
    fn read_only_error_display() {
        let err = MemoryError::ReadOnly;
        assert_eq!(
            err.to_string(),
            "operation requires write access, but engine was opened read-only"
        );
    }

    #[test]
    fn embedding_dimension_display() {
        let err = MemoryError::EmbeddingDimension {
            expected: 768,
            actual: 512,
        };
        assert_eq!(
            err.to_string(),
            "invalid embedding dimension: expected 768, got 512"
        );
    }
}
