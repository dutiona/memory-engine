use memory_engine::error::MemoryError;
use rmcp::model::ErrorData;

/// Map a [`MemoryError`] to an MCP [`ErrorData`] with appropriate JSON-RPC error codes.
#[must_use]
pub fn to_mcp_error(err: MemoryError) -> ErrorData {
    match err {
        MemoryError::NotFound(msg) => ErrorData::resource_not_found(msg, None),

        MemoryError::EmbeddingDimension { expected, actual } => ErrorData::invalid_params(
            format!("embedding dimension mismatch: expected {expected}, got {actual}"),
            None,
        ),

        // A model-identity mismatch is a client-input error: the caller declared (or a
        // configured provider reports) an identity that disagrees with the store's. Like
        // EmbeddingDimension it is invalid_params, NOT an internal server fault.
        MemoryError::EmbeddingModelMismatch { expected, actual } => ErrorData::invalid_params(
            format!("embedding model mismatch: store identity {expected:?} != declared {actual:?}"),
            None,
        ),

        MemoryError::Conflict(msg) => ErrorData::invalid_params(format!("conflict: {msg}"), None),

        // A dream-cycle report that fails validation/application is a client-input
        // error (the report is client-supplied to `memory_apply_cycle_report`), so it
        // maps to invalid_params alongside `Conflict` — not the internal-error default.
        MemoryError::Cycle(e) => ErrorData::invalid_params(format!("cycle error: {e}"), None),

        MemoryError::Serialization(e) => {
            ErrorData::internal_error(format!("serialization error: {e}"), None)
        }

        MemoryError::Migration(msg) => {
            ErrorData::internal_error(format!("migration error: {msg}"), None)
        }

        MemoryError::Pool(msg) => ErrorData::internal_error(format!("pool error: {msg}"), None),

        MemoryError::UnsupportedEpoch {
            db_epoch,
            supported_epoch,
        } => ErrorData::internal_error(
            format!("unsupported storage epoch: db={db_epoch}, supported={supported_epoch}"),
            None,
        ),

        MemoryError::Internal(msg) => {
            ErrorData::internal_error(format!("internal error: {msg}"), None)
        }

        MemoryError::Io(e) => ErrorData::internal_error(format!("I/O error: {e}"), None),

        MemoryError::Reranker(msg) => {
            ErrorData::internal_error(format!("reranker error: {msg}"), None)
        }

        MemoryError::NotImplemented(msg) => {
            ErrorData::internal_error(format!("not implemented: {msg}"), None)
        }

        MemoryError::Archive(msg) => {
            ErrorData::internal_error(format!("archive error: {msg}"), None)
        }

        MemoryError::ReadOnly => ErrorData::invalid_request(
            "engine opened in read-only mode — write operations are not available",
            None,
        ),

        MemoryError::Lineage(msg) => ErrorData::resource_not_found(format!("lineage: {msg}"), None),

        // Explicit, greppable mapping for the storage seam. Output-equivalent to
        // the wildcard below (re-prefixing the inner `StorageError`, which has no
        // prefix of its own, reproduces the `Storage` Display's "storage error: "
        // — no double prefix). The typed arm documents the mapping and guards
        // against a future wildcard change silently degrading it.
        MemoryError::Storage(e) => ErrorData::internal_error(format!("storage error: {e}"), None),

        // `MemoryError` is `#[non_exhaustive]`: future variants map to a
        // generic internal error until handled explicitly above.
        other => ErrorData::internal_error(other.to_string(), None),
    }
}

/// MCP-layer validation errors (before reaching the engine).
#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("importance must be in [0.0, 1.0], got {0}")]
    ImportanceOutOfRange(f64),

    #[error("t_valid must be before t_invalid")]
    TemporalInconsistency,

    #[error("unknown event_type: {0}")]
    UnknownEventType(String),

    #[error("unknown fact_type: {0}")]
    UnknownFactType(String),

    #[error("embedding dimensions mismatch: expected {expected}, got {actual}")]
    EmbeddingDimension { expected: usize, actual: usize },

    #[error("embedding provider not configured — required for this operation")]
    NoEmbeddingProvider,

    #[error("summary generator not configured — required for consolidation")]
    NoSummaryProvider,

    #[error("{0}")]
    Other(String),
}

impl From<ValidationError> for ErrorData {
    fn from(err: ValidationError) -> Self {
        Self::invalid_params(err.to_string(), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_maps_to_resource_not_found() {
        let err = MemoryError::NotFound("fact 42".into());
        let mcp = to_mcp_error(err);
        assert_eq!(mcp.code, rmcp::model::ErrorCode::RESOURCE_NOT_FOUND);
    }

    #[test]
    fn embedding_dim_maps_to_invalid_params() {
        let err = MemoryError::EmbeddingDimension {
            expected: 768,
            actual: 384,
        };
        let mcp = to_mcp_error(err);
        assert_eq!(mcp.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn embedding_model_mismatch_maps_to_invalid_params() {
        // A declared/configured model disagreeing with the store is a client-input
        // error (#615), not a server fault.
        use memory_engine::EmbeddingFingerprint;
        let err = MemoryError::EmbeddingModelMismatch {
            expected: Box::new(EmbeddingFingerprint::new("a", "tei", 8)),
            actual: Box::new(EmbeddingFingerprint::new("b", "ollama", 8)),
        };
        let mcp = to_mcp_error(err);
        assert_eq!(mcp.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(mcp.message.contains("model mismatch"));
    }

    #[test]
    fn cycle_maps_to_invalid_params() {
        use memory_engine::error::CycleError;
        let err = MemoryError::Cycle(CycleError::UnknownFact(7));
        let mcp = to_mcp_error(err);
        assert_eq!(mcp.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(mcp.message.contains("cycle error"));
    }

    #[test]
    fn storage_maps_to_internal() {
        use memory_engine::error::StorageError;
        let err = MemoryError::Storage(StorageError::Backend("boom".into()));
        let mcp = to_mcp_error(err);
        assert_eq!(mcp.code, rmcp::model::ErrorCode::INTERNAL_ERROR);
        assert!(mcp.message.contains("storage error"));
    }

    #[test]
    fn validation_error_maps_to_invalid_params() {
        let err = ValidationError::ImportanceOutOfRange(1.5);
        let mcp: ErrorData = err.into();
        assert_eq!(mcp.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(mcp.message.contains("1.5"));
    }
}
