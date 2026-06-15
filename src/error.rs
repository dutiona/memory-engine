/// A precondition or input-validation conflict surfaced by the engine.
///
/// This is the typed payload of [`MemoryError::Conflict`]. Each variant maps to
/// a distinct family of "the request cannot proceed as stated" failures —
/// incompatible query options, out-of-range policy parameters, malformed scope
/// labels, filesystem/restore preconditions, or a consumer-supplied trait that
/// reported a failure. Splitting these out lets callers `match` on the precise
/// cause instead of string-matching the message.
///
/// Marked `#[non_exhaustive]`: new variants may be added in minor releases, so
/// downstream `match` expressions must include a wildcard (`_`) arm.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConflictError {
    /// A [`MemoryQuery`](crate::types::MemoryQuery) combined options that are
    /// mutually exclusive or incomplete (e.g. a half-open period, `valid_at`
    /// together with a period, or a [`SearchMode`](crate::types::SearchMode)
    /// missing its required input). The string names the specific rule violated.
    #[error("{0}")]
    QueryValidation(String),

    /// A configuration or policy parameter is out of its accepted range — e.g. a
    /// non-positive half-life, a weight below zero, or a ratio/percentile outside
    /// `[0.0, 1.0]`. Raised by [`ForgetPolicy::validate`](crate::traits::ForgetPolicy::validate)
    /// and [`DreamCycleConfig::validate`](crate::types::DreamCycleConfig::validate).
    /// The string describes the offending parameter and its value.
    #[error("{0}")]
    PolicyParameter(String),

    /// A scope label or path segment is malformed — empty, containing the `/`
    /// separator, exceeding the length limit, or carrying surrounding whitespace.
    /// The string names the specific constraint violated.
    #[error("{0}")]
    ScopeLabel(String),

    /// A restore/open operation was asked to create a database at a path that
    /// already exists. The target must not exist so an existing store is never
    /// silently overwritten.
    #[error("target database path already exists")]
    TargetExists,

    /// A restore was attempted into a database that already contains data.
    /// Restore only runs against a fresh, empty engine.
    #[error("target database is not empty; restore only works on a fresh engine")]
    TargetNotEmpty,

    /// A dump was directed at a path that resolves (possibly via a symlink) to the
    /// live database file backing the engine, which would corrupt the live store.
    #[error("dump target resolves to the live database file")]
    DumpTargetIsLiveDatabase,

    /// A consumer-supplied trait implementation (e.g. a
    /// [`ConflictArbiter`](crate::traits::ConflictArbiter) or other injected
    /// provider) reported a failure that the engine surfaces as a conflict. The
    /// string carries the consumer's message.
    #[error("{0}")]
    Arbitration(String),
}

/// Errors returned by the memory engine.
///
/// Marked `#[non_exhaustive]`: new variants may be added in minor releases, so
/// downstream `match` expressions must include a wildcard (`_`) arm.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MemoryError {
    /// A requested entity (fact, event, scope, snapshot, …) does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// An underlying `SQLite` operation failed (query, statement, or connection).
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    /// JSON (de)serialization of a payload, snapshot, or config failed.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// An embedding's length does not match the engine's configured dimension.
    #[error("invalid embedding dimension: expected {expected}, got {actual}")]
    EmbeddingDimension { expected: usize, actual: usize },

    /// A precondition or input-validation conflict; see [`ConflictError`] for the
    /// specific cause (incompatible query options, out-of-range parameters,
    /// malformed scope labels, restore/dump preconditions, or a consumer-trait
    /// failure).
    #[error("conflict: {0}")]
    Conflict(#[from] ConflictError),

    /// A schema migration (forward or epoch upgrade) failed to apply.
    #[error("schema migration failed: {0}")]
    Migration(String),

    /// The requested operation is recognized but not yet implemented.
    #[error("not implemented: {0}")]
    NotImplemented(String),

    /// The connection pool could not hand out or manage a connection.
    #[error("connection pool error: {0}")]
    Pool(String),

    /// The database's on-disk storage epoch is incompatible with the epoch this
    /// build supports (too old or too new to open safely).
    #[error(
        "unsupported storage epoch: database is epoch {db_epoch}, this library supports epoch {supported_epoch}"
    )]
    UnsupportedEpoch { db_epoch: u16, supported_epoch: u16 },

    /// An invariant was violated inside the engine — a bug or corrupted state
    /// that the caller cannot meaningfully recover from.
    #[error("internal error: {0}")]
    Internal(String),

    /// A filesystem I/O operation (read, write, copy, canonicalize, …) failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A bootstrap/backdate import step failed (e.g. parsing or validating the
    /// seed dataset).
    #[error("bootstrap error: {0}")]
    Bootstrap(String),

    /// The consumer-supplied [`Reranker`](crate::traits::Reranker) failed while
    /// scoring candidates.
    #[error("reranker error: {0}")]
    Reranker(String),

    /// A cold-storage archive operation (pack/unpack of a `.pak` file) failed.
    #[error("archive error: {0}")]
    Archive(String),

    /// Attempted a write operation on a read-only engine.
    #[error("operation requires write access, but engine was opened read-only")]
    ReadOnly,

    /// A wisdom-fact lineage/provenance record is missing or inconsistent.
    #[error("lineage error: {0}")]
    Lineage(String),
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
    fn lineage_error_display() {
        let err = MemoryError::Lineage("wisdom fact 42 has no lineage record".into());
        assert_eq!(
            err.to_string(),
            "lineage error: wisdom fact 42 has no lineage record"
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

    #[test]
    fn conflict_delegates_to_inner_display() {
        // String-carrying variant: inner message is surfaced verbatim under the
        // outer `conflict: ` prefix.
        let err = MemoryError::Conflict(ConflictError::QueryValidation(
            "valid_at and period are mutually exclusive".into(),
        ));
        assert_eq!(
            err.to_string(),
            "conflict: valid_at and period are mutually exclusive"
        );
    }

    #[test]
    fn conflict_unit_variant_display() {
        // Unit variant: fixed message, still wrapped by the outer prefix.
        let err = MemoryError::Conflict(ConflictError::TargetExists);
        assert_eq!(
            err.to_string(),
            "conflict: target database path already exists"
        );
    }

    #[test]
    fn conflict_error_from_into_memory_error() {
        // `#[from]` lets a bare `ConflictError` convert via `?`/`.into()`.
        let err: MemoryError = ConflictError::TargetNotEmpty.into();
        assert!(matches!(
            err,
            MemoryError::Conflict(ConflictError::TargetNotEmpty)
        ));
    }
}
