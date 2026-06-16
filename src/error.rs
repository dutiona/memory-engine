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

    /// A dump was directed at a path that already exists and is a directory.
    /// `VACUUM INTO` writes a file, and the engine refuses to unlink a directory
    /// to make room — surfacing the mistake as a clear conflict instead of an
    /// opaque "is a directory" I/O failure (see the trusted-path contract on
    /// [`dump_sqlite`](crate::inspect::dump::dump_sqlite)).
    #[error("dump target is an existing directory")]
    DumpTargetIsDirectory,

    /// A consumer-supplied trait implementation (e.g. a
    /// [`ConflictArbiter`](crate::traits::ConflictArbiter) or other injected
    /// provider) reported a failure that the engine surfaces as a conflict. The
    /// string carries the consumer's message.
    #[error("{0}")]
    Arbitration(String),

    /// An ingested document exceeded the engine's per-field ingest size bound:
    /// an event `payload`, a fact's `metadata` (JSON), or a fact's `content`
    /// (string). The bound caps per-row memory and serialization CPU on hostile
    /// or runaway input. `kind` names the offending field and `limit` is the
    /// ceiling; `size` is the offending byte length — exact for string fields,
    /// but a lower bound for JSON fields, whose measurement aborts early once
    /// it passes the limit.
    #[error("{kind} is {size} bytes, exceeding the {limit}-byte ingest limit")]
    PayloadTooLarge {
        kind: &'static str,
        size: usize,
        limit: usize,
    },
}

/// A failure originating from the reranking stage of a query.
///
/// This is the typed payload of [`MemoryError::Reranker`]. It distinguishes two
/// origins: a failure reported by the consumer-supplied
/// [`Reranker`](crate::traits::Reranker) itself ([`Provider`](RerankerError::Provider)),
/// and the four output-contract violations the engine detects *after* the
/// reranker returns (`validate_reranker_output`). The engine-detected variants
/// carry the offending values as fields, so a caller can `match` on the precise
/// cause — and act on the data — instead of string-matching the message.
///
/// Marked `#[non_exhaustive]`: new variants may be added in minor releases, so
/// downstream `match` expressions must include a wildcard (`_`) arm.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RerankerError {
    /// The consumer-supplied [`Reranker`](crate::traits::Reranker) returned an
    /// error from its `rerank` call (e.g. an API call, timeout, or inference
    /// failure). The string carries the consumer's message verbatim.
    #[error("{0}")]
    Provider(String),

    /// The reranker returned more `(index, score)` pairs than it was given
    /// candidates, violating the subset contract (output must be a permutation
    /// of a subset of the input).
    #[error(
        "reranker violated subset contract: output length ({output_len}) exceeds input length ({input_len})"
    )]
    OutputTooLong {
        /// Number of pairs the reranker returned.
        output_len: usize,
        /// Number of candidates the reranker was given.
        input_len: usize,
    },

    /// The reranker returned an index that is not a valid position in the
    /// candidate slice (`index >= num_candidates`).
    #[error("reranker returned out-of-bounds index {index} (candidates length: {num_candidates})")]
    OutOfBoundsIndex {
        /// The offending out-of-range index.
        index: usize,
        /// Number of candidates the reranker was given.
        num_candidates: usize,
    },

    /// The reranker returned the same index more than once, violating the subset
    /// contract (each candidate may appear at most once in the output).
    #[error("reranker violated subset contract: duplicate index {index} in output")]
    DuplicateIndex {
        /// The index that appeared more than once.
        index: usize,
    },

    /// The reranker assigned a non-finite score (`NaN` or `±∞`) to a candidate,
    /// which cannot participate in a deterministic ordering.
    #[error("reranker returned non-finite score {score} for index {index}")]
    NonFiniteScore {
        /// The non-finite score value.
        score: f64,
        /// The index the score was assigned to.
        index: usize,
    },
}

/// A failure from the cold-storage archive subsystem (`.pak` pack/unpack).
///
/// This is the typed payload of [`MemoryError::Archive`]. The two
/// version-mismatch variants carry the offending and supported versions as
/// fields, so a caller can detect "archive written by a newer build" and react
/// (e.g. prompt to upgrade) without string-matching. The remaining variants
/// group the operational failure modes — preconditions, compression codec,
/// filesystem I/O, and the archive write transaction — each carrying the
/// underlying error message verbatim (these wrap arbitrary `std::io`/codec
/// errors that cannot be enumerated further).
///
/// Only constructed when the `archive` feature is enabled; the type itself is
/// always present so [`MemoryError`] has a stable shape across feature sets.
///
/// Marked `#[non_exhaustive]`: new variants may be added in minor releases, so
/// downstream `match` expressions must include a wildcard (`_`) arm.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ArchiveError {
    /// The `.pak` file's `pak_version` is newer than this build supports —
    /// forward-incompatible (a newer library wrote it). Reading older versions
    /// is allowed; only newer ones are rejected.
    #[error(
        "pak_version {found} is newer than supported {supported}; consider upgrading the memory-engine crate"
    )]
    PakVersionUnsupported {
        /// The `pak_version` read from the archive.
        found: u32,
        /// The highest `pak_version` this build supports.
        supported: u32,
    },

    /// The `.pak` file's `engine_schema_version` is newer than this build
    /// supports — same forward-only incompatibility as
    /// [`PakVersionUnsupported`](ArchiveError::PakVersionUnsupported).
    #[error(
        "engine_schema_version {found} is newer than supported {supported}; consider upgrading the memory-engine crate"
    )]
    SchemaVersionUnsupported {
        /// The `engine_schema_version` read from the archive.
        found: u32,
        /// The highest `engine_schema_version` this build supports.
        supported: u32,
    },

    /// Archival requires a file-backed engine, but the engine is in-memory (or
    /// its database path cannot be resolved). The string names the precise
    /// precondition violated.
    #[error("{0}")]
    NotFileBacked(String),

    /// A zstd compression or decompression step failed (encoder/decoder
    /// creation or stream finalization). The string carries the codec message.
    #[error("{0}")]
    Codec(String),

    /// A filesystem operation on a `.pak` file or the archive directory failed
    /// (create, open, read, rename, stat, mkdir, path resolution). The string
    /// carries the underlying I/O message.
    #[error("{0}")]
    Io(String),

    /// The archive write transaction failed to begin or commit. The string
    /// carries the underlying database message.
    #[error("{0}")]
    Transaction(String),
}

/// A failure from the schema-migration / database-compatibility subsystem.
///
/// This is the typed payload of [`MemoryError::Migration`]. The version and
/// embed-dim variants carry their operands as fields, so a caller can branch on
/// "the database is from a newer build", "needs migration", or "embedding
/// dimension mismatch" without string-matching. The remaining variants group
/// the operational failure modes — pre-migration backup, a missing event
/// upcaster, and the terminal incompatibilities (corrupt config values, an
/// uninitialized database, post-rebuild FK violations, …) whose detail is
/// carried verbatim in the message.
///
/// Marked `#[non_exhaustive]`: new variants may be added in minor releases, so
/// downstream `match` expressions must include a wildcard (`_`) arm.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MigrationError {
    /// The stored `schema_version` is newer than this build supports — a newer
    /// library wrote the database. Forward-incompatible.
    #[error(
        "schema_version {found} is newer than supported {supported}; consider upgrading the memory-engine crate"
    )]
    SchemaVersionUnsupported {
        /// The `schema_version` found in the database.
        found: u32,
        /// The highest `schema_version` this build supports.
        supported: u32,
    },

    /// The stored `schema_version` is older than current and the database was
    /// opened read-only, so migrations cannot run. Re-open read-write.
    #[error(
        "schema_version {found} needs migration to {target}; open in read-write mode first to run migrations"
    )]
    SchemaVersionNeedsMigration {
        /// The `schema_version` found in the database.
        found: u32,
        /// The `schema_version` this build expects.
        target: u32,
    },

    /// The database's stored embedding dimension does not match the dimension
    /// the engine was opened with. All vectors in a store share one dimension.
    #[error("embed_dim mismatch: stored {stored} vs requested {requested}")]
    EmbedDimMismatch {
        /// The `embed_dim` recorded in the database.
        stored: usize,
        /// The `embed_dim` the engine was opened with.
        requested: usize,
    },

    /// The pre-migration WAL-safe backup step failed (in-memory database,
    /// null-byte path, removal of a stale backup, or the `VACUUM INTO` itself).
    /// The string names the precise backup failure.
    #[error("{0}")]
    Backup(String),

    /// No registered event upcaster bridges a stored payload revision to the
    /// current one, so the event cannot be upgraded. The string names the event
    /// type and revision gap.
    #[error("{0}")]
    MissingUpcaster(String),

    /// The database or snapshot is in a state this build cannot open or migrate
    /// — a corrupt/unparseable config value, an uninitialized database, a
    /// snapshot from a newer schema, post-rebuild foreign-key violations, or a
    /// non-file path opened read-only. The string carries the specific cause.
    #[error("{0}")]
    Incompatible(String),
}

/// A failure while validating or applying a [`CycleReport`](crate::CycleReport).
///
/// This is the typed payload of [`MemoryError::Cycle`]. Each variant maps to a
/// specific reason a delta could not be applied — a dangling fact reference, an
/// out-of-range importance adjustment, a supersede whose target is missing, or an
/// operation on an already-expired fact. `MemoryEngine::apply_cycle_report`
/// validates the whole report before mutating, so an `Err` means the store was
/// left untouched.
///
/// Marked `#[non_exhaustive]`: new variants may be added in minor releases, so
/// downstream `match` expressions must include a wildcard (`_`) arm.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CycleError {
    /// A delta referenced a fact id that does not exist.
    #[error("cycle delta references unknown fact {0}")]
    UnknownFact(crate::types::FactId),

    /// An `AdjustScore` delta's resulting score would fall outside `[0.0, 1.0]`
    /// in a way that cannot be clamped meaningfully (reserved; v1 clamps silently).
    #[error("adjusted importance for fact {fact_id} out of bounds: {attempted}")]
    ScoreOutOfBounds {
        fact_id: crate::types::FactId,
        attempted: f64,
    },

    /// An `AdjustScore` delta's magnitude exceeds the ±2 symmetric limit.
    #[error("importance adjustment for fact {fact_id} out of range: {adjustment} (max ±2)")]
    AdjustmentOutOfRange {
        fact_id: crate::types::FactId,
        adjustment: i16,
    },

    /// A `Supersede` delta's `new_id` neither exists nor is produced by an
    /// earlier `AddFact` in the same report.
    #[error(
        "supersede target fact {0} is missing (not pre-existing and not added earlier in the report)"
    )]
    SupersedeMissing(crate::types::FactId),

    /// A delta targeted a fact that is already expired (soft-deleted), e.g.
    /// adjusting or quarantining a fact that is no longer active.
    #[error("fact {0} is already expired")]
    AlreadyExpired(crate::types::FactId),
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

    /// A schema migration or database-compatibility check failed; see
    /// [`MigrationError`] for the specific cause (version mismatch, embed-dim
    /// mismatch, backup failure, missing upcaster, or terminal incompatibility).
    #[error("schema migration failed: {0}")]
    Migration(#[from] MigrationError),

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

    /// A failure in the reranking stage; see [`RerankerError`] for the specific
    /// cause (a consumer-reported `rerank` failure, or one of the four
    /// engine-detected output-contract violations).
    #[error("reranker error: {0}")]
    Reranker(#[from] RerankerError),

    /// A cold-storage archive operation (pack/unpack of a `.pak` file) failed;
    /// see [`ArchiveError`] for the specific cause (version mismatch, precondition,
    /// compression codec, filesystem I/O, or the archive transaction).
    #[error("archive error: {0}")]
    Archive(#[from] ArchiveError),

    /// Attempted a write operation on a read-only engine.
    #[error("operation requires write access, but engine was opened read-only")]
    ReadOnly,

    /// A wisdom-fact lineage/provenance record is missing or inconsistent.
    #[error("lineage error: {0}")]
    Lineage(String),

    /// A dream-cycle report failed validation or application; see [`CycleError`]
    /// for the specific delta-level cause.
    #[error("cycle error: {0}")]
    Cycle(#[from] CycleError),
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
    fn reranker_provider_display() {
        // Consumer-reported failure: the provider's message is surfaced verbatim
        // under the outer `reranker error: ` prefix.
        let err = MemoryError::Reranker(RerankerError::Provider("cross-encoder timeout".into()));
        assert_eq!(err.to_string(), "reranker error: cross-encoder timeout");
    }

    #[test]
    fn reranker_contract_variants_display_byte_for_byte() {
        // Byte-preservation: each engine-detected variant must render exactly the
        // string the pre-split `format!` call sites produced (under the outer
        // `reranker error: ` prefix), so existing message-matching keeps working.
        assert_eq!(
            MemoryError::Reranker(RerankerError::OutputTooLong {
                output_len: 5,
                input_len: 3,
            })
            .to_string(),
            "reranker error: reranker violated subset contract: output length (5) exceeds input length (3)"
        );
        assert_eq!(
            MemoryError::Reranker(RerankerError::OutOfBoundsIndex {
                index: 7,
                num_candidates: 4,
            })
            .to_string(),
            "reranker error: reranker returned out-of-bounds index 7 (candidates length: 4)"
        );
        assert_eq!(
            MemoryError::Reranker(RerankerError::DuplicateIndex { index: 2 }).to_string(),
            "reranker error: reranker violated subset contract: duplicate index 2 in output"
        );
        assert_eq!(
            MemoryError::Reranker(RerankerError::NonFiniteScore {
                score: f64::NAN,
                index: 1,
            })
            .to_string(),
            "reranker error: reranker returned non-finite score NaN for index 1"
        );
    }

    #[test]
    fn reranker_error_from_into_memory_error() {
        // `#[from]` lets a bare `RerankerError` convert via `?`/`.into()`.
        let err: MemoryError = RerankerError::DuplicateIndex { index: 0 }.into();
        assert!(matches!(
            err,
            MemoryError::Reranker(RerankerError::DuplicateIndex { index: 0 })
        ));
    }

    #[test]
    fn archive_variants_display_byte_for_byte() {
        // Byte-preservation: each variant must render exactly the string the
        // pre-split `format!` call sites produced (under the outer `archive
        // error: ` prefix), so existing message consumers keep working.
        assert_eq!(
            MemoryError::Archive(ArchiveError::PakVersionUnsupported {
                found: 2,
                supported: 1,
            })
            .to_string(),
            "archive error: pak_version 2 is newer than supported 1; consider upgrading the memory-engine crate"
        );
        assert_eq!(
            MemoryError::Archive(ArchiveError::SchemaVersionUnsupported {
                found: 12,
                supported: 11,
            })
            .to_string(),
            "archive error: engine_schema_version 12 is newer than supported 11; consider upgrading the memory-engine crate"
        );
        // String-carrying variants surface their message verbatim under the prefix.
        assert_eq!(
            MemoryError::Archive(ArchiveError::NotFileBacked(
                "archival requires a file-backed engine".into()
            ))
            .to_string(),
            "archive error: archival requires a file-backed engine"
        );
        assert_eq!(
            MemoryError::Archive(ArchiveError::Codec(
                "failed to create zstd encoder: boom".into()
            ))
            .to_string(),
            "archive error: failed to create zstd encoder: boom"
        );
        assert_eq!(
            MemoryError::Archive(ArchiveError::Io("failed to open pak file /x: boom".into()))
                .to_string(),
            "archive error: failed to open pak file /x: boom"
        );
        assert_eq!(
            MemoryError::Archive(ArchiveError::Transaction(
                "failed to begin transaction: boom".into()
            ))
            .to_string(),
            "archive error: failed to begin transaction: boom"
        );
    }

    #[test]
    fn archive_error_from_into_memory_error() {
        // `#[from]` lets a bare `ArchiveError` convert via `?`/`.into()`.
        let err: MemoryError = ArchiveError::PakVersionUnsupported {
            found: 9,
            supported: 1,
        }
        .into();
        assert!(matches!(
            err,
            MemoryError::Archive(ArchiveError::PakVersionUnsupported {
                found: 9,
                supported: 1
            })
        ));
    }

    #[test]
    fn migration_variants_display_byte_for_byte() {
        // Byte-preservation: each variant renders exactly the string the
        // pre-split `format!` call sites produced, under the outer
        // `schema migration failed: ` prefix.
        assert_eq!(
            MemoryError::Migration(MigrationError::SchemaVersionUnsupported {
                found: 12,
                supported: 11,
            })
            .to_string(),
            "schema migration failed: schema_version 12 is newer than supported 11; consider upgrading the memory-engine crate"
        );
        assert_eq!(
            MemoryError::Migration(MigrationError::SchemaVersionNeedsMigration {
                found: 9,
                target: 11,
            })
            .to_string(),
            "schema migration failed: schema_version 9 needs migration to 11; open in read-write mode first to run migrations"
        );
        assert_eq!(
            MemoryError::Migration(MigrationError::EmbedDimMismatch {
                stored: 768,
                requested: 512,
            })
            .to_string(),
            "schema migration failed: embed_dim mismatch: stored 768 vs requested 512"
        );
        // String-carrying variants surface their message verbatim under the prefix.
        assert_eq!(
            MemoryError::Migration(MigrationError::Backup(
                "cannot backup in-memory database".into()
            ))
            .to_string(),
            "schema migration failed: cannot backup in-memory database"
        );
        assert_eq!(
            MemoryError::Migration(MigrationError::MissingUpcaster(
                "missing upcaster for event type 'ToolCall' from revision 1 to 2".into()
            ))
            .to_string(),
            "schema migration failed: missing upcaster for event type 'ToolCall' from revision 1 to 2"
        );
        assert_eq!(
            MemoryError::Migration(MigrationError::Incompatible(
                "invalid schema_version: abc".into()
            ))
            .to_string(),
            "schema migration failed: invalid schema_version: abc"
        );
    }

    #[test]
    fn migration_error_from_into_memory_error() {
        // `#[from]` lets a bare `MigrationError` convert via `?`/`.into()`.
        let err: MemoryError = MigrationError::SchemaVersionNeedsMigration {
            found: 9,
            target: 11,
        }
        .into();
        assert!(matches!(
            err,
            MemoryError::Migration(MigrationError::SchemaVersionNeedsMigration {
                found: 9,
                target: 11
            })
        ));
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

    #[test]
    fn cycle_error_display_per_variant() {
        assert_eq!(
            CycleError::UnknownFact(7).to_string(),
            "cycle delta references unknown fact 7"
        );
        assert_eq!(
            CycleError::AdjustmentOutOfRange {
                fact_id: 3,
                adjustment: 5
            }
            .to_string(),
            "importance adjustment for fact 3 out of range: 5 (max ±2)"
        );
        assert_eq!(
            CycleError::SupersedeMissing(9).to_string(),
            "supersede target fact 9 is missing (not pre-existing and not added earlier in the report)"
        );
        assert_eq!(
            CycleError::AlreadyExpired(4).to_string(),
            "fact 4 is already expired"
        );
        assert!(
            CycleError::ScoreOutOfBounds {
                fact_id: 1,
                attempted: 1.5
            }
            .to_string()
            .contains("out of bounds")
        );
    }

    #[test]
    fn cycle_error_from_into_memory_error() {
        // `#[from]` lets a bare `CycleError` convert via `?`/`.into()`, wrapped
        // with the outer "cycle error: " prefix.
        let err: MemoryError = CycleError::UnknownFact(2).into();
        assert!(matches!(
            err,
            MemoryError::Cycle(CycleError::UnknownFact(2))
        ));
        assert_eq!(
            err.to_string(),
            "cycle error: cycle delta references unknown fact 2"
        );
    }
}
