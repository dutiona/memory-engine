//! Argument parsing, validation, and result-shaping helpers shared by the tool
//! handlers and the dispatcher.
//!
//! Everything here is `pub(crate)`: the handler modules and the dispatcher call
//! these helpers, but they are not part of the crate's published surface.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use memory_engine::SearchMode;
use memory_engine::inspect_types::ReplayOrder;
use memory_engine::traits::ConsolidationConfig;
use memory_engine::types::{EmbeddingFingerprint, EventType, FactType, Outcome};
use rmcp::model::{CallToolResult, Content, ErrorData};
use serde_json::{Map, Value};

use crate::depth::Depth;
use crate::error::ValidationError;

// Each scalar getter distinguishes *absent* (`Ok(None)` — the caller may apply its
// default) from *present-but-wrong-type* (`Err(invalid_params)`), rather than the old
// `and_then(Value::as_*)` that collapsed both to `None` and let an untrusted caller's
// wrong-typed value silently become the server's default (#842 — the type-mismatch twin
// of the #339 negative-value fix). `null` is treated as absent, matching JSON's
// "unset" idiom and the pre-#842 behavior (`as_*` returned `None` for `null`).

pub fn get_str(args: &Map<String, Value>, key: &str) -> Result<Option<String>, ErrorData> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_str()
            .map(|s| Some(s.to_owned()))
            .ok_or_else(|| ErrorData::invalid_params(format!("{key} must be a string"), None)),
    }
}

pub fn get_i64(args: &Map<String, Value>, key: &str) -> Result<Option<i64>, ErrorData> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_i64()
            .map(Some)
            .ok_or_else(|| ErrorData::invalid_params(format!("{key} must be an integer"), None)),
    }
}

pub fn get_f64(args: &Map<String, Value>, key: &str) -> Result<Option<f64>, ErrorData> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .map(Some)
            .ok_or_else(|| ErrorData::invalid_params(format!("{key} must be a number"), None)),
    }
}

pub fn get_bool(args: &Map<String, Value>, key: &str) -> Result<Option<bool>, ErrorData> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_bool()
            .map(Some)
            .ok_or_else(|| ErrorData::invalid_params(format!("{key} must be a boolean"), None)),
    }
}

/// Read an optional non-negative integer parameter.
///
/// Distinguishes *absent* (`Ok(None)`) from *present-but-invalid* (`Err`). A
/// present-but-negative value is rejected with `invalid_params` rather than
/// silently dropped (#339): dropping it would let the engine fall back to its
/// own default — e.g. returning more results than an untrusted caller asked for.
pub fn get_usize(args: &Map<String, Value>, key: &str) -> Result<Option<usize>, ErrorData> {
    get_i64(args, key)?.map_or(Ok(None), |v| {
        usize::try_from(v).map(Some).map_err(|_| {
            ErrorData::invalid_params(format!("{key} must be a non-negative integer"), None)
        })
    })
}

pub fn get_datetime(
    args: &Map<String, Value>,
    key: &str,
) -> Result<Option<DateTime<Utc>>, ErrorData> {
    get_str(args, key)?.map_or(Ok(None), |s| {
        s.parse::<DateTime<Utc>>()
            .map(Some)
            .map_err(|e| ErrorData::invalid_params(format!("invalid {key}: {e}"), None))
    })
}

pub fn get_depth(args: &Map<String, Value>) -> Result<Depth, ErrorData> {
    match args.get("depth") {
        None | Some(Value::Null) => Ok(Depth::default()),
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|e| ErrorData::invalid_params(format!("invalid depth: {e}"), None)),
    }
}

/// Parse an embedding from a JSON value, returning an error if present but malformed.
///
/// #294 (CWE-400/770 pre-allocation DoS): the array's length is checked against
/// `expected_dim` BEFORE `serde_json::from_value` materializes a `Vec<f32>`, so a
/// hostile client cannot force the server to allocate an arbitrarily large vector
/// only to reject it afterward. A wrong-length array is rejected on its length
/// alone — this doubles as the query-path dimension check that previously existed
/// only on the add-fact path.
pub fn parse_embedding(
    args: &Map<String, Value>,
    expected_dim: usize,
) -> Result<Option<Vec<f32>>, ErrorData> {
    match args.get("embedding") {
        None | Some(Value::Null) => Ok(None),
        Some(v @ Value::Array(arr)) => {
            // Length gate FIRST: reject the wrong-dimension array before allocating it.
            if arr.len() != expected_dim {
                return Err(ValidationError::EmbeddingDimension {
                    expected: expected_dim,
                    actual: arr.len(),
                }
                .into());
            }
            // Deserialize from the borrowed `Value` — no `arr.clone()` of the whole
            // JSON array. The length gate above still runs before any `Vec<f32>`
            // allocation, so the pre-alloc DoS guard is preserved (#498
            // `mcp/performance-parse-embedding-clone`).
            <Vec<f32> as serde::Deserialize>::deserialize(v)
                .map(Some)
                .map_err(|e| ErrorData::invalid_params(format!("invalid embedding: {e}"), None))
        }
        // Present but not an array (e.g. a string or number): malformed input.
        Some(v) => Err(ErrorData::invalid_params(
            format!("invalid embedding: expected an array of numbers, got {v}"),
            None,
        )),
    }
}

/// Parse the caller-declared embedding identity that MUST accompany a pre-computed
/// `embedding` (#615, §Design.3).
///
/// `model` and `provider` are **required** (the identity-critical pair); `dim` is the
/// vector length the caller submitted; `matryoshka_base_dim` and `element_type` are
/// optional, defaulting to no-truncation / `"float32"`. The declared tuple is compared
/// (full `Eq`) against the store's recorded identity by the engine, closing the
/// same-dimension foreign-vector hole — a vector from a different model can no longer be
/// slipped into the store's vector space unchecked.
pub fn parse_declared_fingerprint(
    args: &Map<String, Value>,
    dim: usize,
) -> Result<EmbeddingFingerprint, ErrorData> {
    let model = get_str(args, "model")?.ok_or_else(|| {
        ErrorData::invalid_params(
            "a pre-computed `embedding` requires a declared `model` (the model that produced it)",
            None,
        )
    })?;
    let provider = get_str(args, "provider")?.ok_or_else(|| {
        ErrorData::invalid_params(
            "a pre-computed `embedding` requires a declared `provider` (e.g. \"tei\", \"ollama\")",
            None,
        )
    })?;
    let matryoshka_base_dim = match args.get("matryoshka_base_dim") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| {
                    ErrorData::invalid_params(
                        "matryoshka_base_dim must be a non-negative integer",
                        None,
                    )
                })?,
        ),
    };
    // A present-but-non-string `element_type` is rejected, not silently ignored — a
    // malformed value must not fall back to the "float32" default and slip past the
    // full-tuple identity check (consistent with matryoshka_base_dim's rejection).
    let element_type = match args.get("element_type") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            return Err(ErrorData::invalid_params(
                "element_type must be a string (e.g. \"float32\")",
                None,
            ));
        }
    };
    let mut fp = match matryoshka_base_dim {
        Some(base) => EmbeddingFingerprint::with_matryoshka(model, provider, dim, base),
        None => EmbeddingFingerprint::new(model, provider, dim),
    };
    if let Some(element_type) = element_type {
        fp.element_type = element_type;
    }
    Ok(fp)
}

pub fn parse_search_mode(s: &str) -> Result<SearchMode, ErrorData> {
    match s {
        "fts" => Ok(SearchMode::Fts),
        "vector" => Ok(SearchMode::Vector),
        "hybrid" => Ok(SearchMode::Hybrid),
        other => Err(ErrorData::invalid_params(
            format!("unknown search mode: {other}"),
            None,
        )),
    }
}

/// Parse an MCP `event_type` tool parameter into the core [`EventType`].
///
/// Delegates to core's canonical [`EventType::from_str`] (the single source of
/// truth, #353/#678), so casing is reconciled across surfaces: the JSON schemas
/// advertise `PascalCase` (`"Interaction"`), but `snake_case` is also accepted.
///
/// One MCP-specific narrowing: [`EventType::OutcomeSignal`] is **rejected** here.
/// It is a system-generated event (emitted by `record_outcome`), not a
/// user-ingestible type — the `ingest` / `replay` JSON schemas deliberately omit
/// it. The core parser is complete (it accepts every variant); this boundary gate
/// preserves the schema contract without re-implementing the variant mapping.
pub fn parse_event_type(s: &str) -> Result<EventType, ValidationError> {
    // The system-only-reject arm and the unparseable arm share a body, but keeping
    // them separate is deliberate: it documents the two distinct rejection reasons
    // and keeps the `EventType` match exhaustive (no `Ok(_)` catch-all), so a new
    // variant forces a deliberate allow/reject decision here at compile time.
    #[allow(clippy::match_same_arms)]
    match s.parse::<EventType>() {
        // User-facing types — accepted at the MCP ingest/replay boundary. These are
        // exactly the variants the `ingest` / `replay` JSON schemas advertise.
        Ok(
            et @ (EventType::Interaction
            | EventType::ToolCall
            | EventType::MemoryOp
            | EventType::SystemEvent),
        ) => Ok(et),
        // System-generated only — emitted internally by `record_outcome`, never
        // user-ingestible; the schemas deliberately omit it.
        Ok(EventType::OutcomeSignal) => Err(ValidationError::UnknownEventType(s.to_owned())),
        // NOTE: intentionally NO catch-all `Ok(_)`. Adding a new `EventType` variant
        // must force a deliberate allow/reject decision here — the compiler flags
        // the non-exhaustive match instead of silently making it user-ingestible.
        Err(_) => Err(ValidationError::UnknownEventType(s.to_owned())),
    }
}

/// Parse an MCP `fact_type` tool parameter into the core [`FactType`].
///
/// Delegates to core's canonical [`FactType::from_str`] (the single source of
/// truth shared with the CLI), so casing is reconciled across surfaces: the JSON
/// schemas advertise `PascalCase` (`"Episodic"`), but `snake_case` is also accepted.
pub fn parse_fact_type(s: &str) -> Result<FactType, ValidationError> {
    s.parse()
        .map_err(|_| ValidationError::UnknownFactType(s.to_owned()))
}

/// Like `get_f64`, but returns a validation error if the key is present with a non-numeric type.
/// Prevents silent fallback to defaults on type mismatches (e.g., `"half_life_days": "1"`).
pub fn require_f64_if_present(
    args: &Map<String, Value>,
    key: &str,
) -> Result<Option<f64>, ErrorData> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().map(Some).ok_or_else(|| {
            ErrorData::invalid_params(format!("{key} must be a number, got {v}"), None)
        }),
    }
}

/// Parse an MCP `outcome` tool parameter into the core [`Outcome`].
///
/// Delegates to core's canonical [`Outcome::from_str`] (the single source of
/// truth, #353/#678), so casing is reconciled across surfaces: the JSON schema
/// advertises `PascalCase` (`"Positive"`), but lowercase is also accepted.
pub fn parse_outcome(s: &str) -> Result<Outcome, ErrorData> {
    s.parse().map_err(|_| {
        ErrorData::invalid_params(
            format!("invalid outcome: {s} (expected Positive, Negative, or Neutral)"),
            None,
        )
    })
}

pub fn parse_replay_order(s: &str) -> Result<ReplayOrder, ErrorData> {
    match s {
        "insertion" => Ok(ReplayOrder::InsertionOrder),
        "timestamp" => Ok(ReplayOrder::TimestampOrder),
        other => Err(ErrorData::invalid_params(
            format!("unknown replay order: {other}"),
            None,
        )),
    }
}

/// Parse + validate the `memory_consolidate` tuning args into a [`ConsolidationConfig`].
///
/// Extracted from the handler so the range checks (thresholds in `[0,1]`, cluster
/// size floor) are unit-testable directly: the handler short-circuits on a missing
/// provider *before* it would run, so an integration test with no providers can
/// never reach this validation (#344 review).
pub fn parse_consolidate_config(
    args: &Map<String, Value>,
) -> Result<ConsolidationConfig, ErrorData> {
    // `require_f64_if_present` rejects a present-but-wrong-type value (e.g.
    // `"0.95"`) instead of silently falling back to the default — `get_f64` would
    // return None on a type mismatch and hide the bad input. f64→f32 narrowing is
    // intentional: thresholds are similarity scores in [0,1], within f32's range.
    #[allow(clippy::cast_possible_truncation)]
    let dedup_threshold = require_f64_if_present(args, "dedup_threshold")?.unwrap_or(0.92) as f32;
    if !(0.0..=1.0).contains(&dedup_threshold) {
        return Err(ErrorData::invalid_params(
            format!("dedup_threshold must be in [0.0, 1.0], got {dedup_threshold}"),
            None,
        ));
    }

    // Clustering threshold is configurable symmetrically with dedup (#344); looser
    // than dedup by default.
    #[allow(clippy::cast_possible_truncation)]
    let cluster_threshold =
        require_f64_if_present(args, "cluster_threshold")?.unwrap_or(0.85) as f32;
    if !(0.0..=1.0).contains(&cluster_threshold) {
        return Err(ErrorData::invalid_params(
            format!("cluster_threshold must be in [0.0, 1.0], got {cluster_threshold}"),
            None,
        ));
    }

    let min_cluster_size = get_usize(args, "min_cluster_size")?.unwrap_or(3);
    if min_cluster_size < 2 {
        return Err(ErrorData::invalid_params(
            format!("min_cluster_size must be >= 2, got {min_cluster_size}"),
            None,
        ));
    }

    Ok(ConsolidationConfig::builder()
        .dedup_threshold(dedup_threshold)
        .cluster_threshold(cluster_threshold)
        .min_cluster_size(min_cluster_size)
        .build())
}

// ---------------------------------------------------------------------------
// Result shaping
// ---------------------------------------------------------------------------

pub fn ok_json(value: Value) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::success(vec![Content::json(value)?]))
}

/// Serialize an engine-produced value into a tool result. A serde failure maps to an
/// internal error (the value is engine-produced, so failure is a server bug).
#[must_use = "the serialized tool result must be returned to the caller"]
pub fn ok_serialized<T: serde::Serialize>(value: &T) -> Result<CallToolResult, ErrorData> {
    let v = serde_json::to_value(value)
        .map_err(|e| ErrorData::internal_error(format!("serialize: {e}"), None))?;
    ok_json(v)
}

// ---------------------------------------------------------------------------
// Dump-path helpers
// ---------------------------------------------------------------------------

/// Monotonic counter making default dump paths unique within a process, so
/// concurrent dumps (e.g. parallel tests) never collide on the timestamp.
static NEXT_DUMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Assemble a default dump filename from its already-resolved components.
///
/// Pure (no clock, no process state, no atomic): given a fixed `timestamp`,
/// `pid`, and `seq`, it always yields the same `memory-dump-<ts>-<pid>-<seq>.<ext>`
/// name. The atomic counter (`seq`) is the load-bearing uniqueness guard for
/// same-process dumps; the timestamp only keeps names time-ordered and the pid
/// disambiguates across processes. Factored out so a test can hold `timestamp`
/// and `pid` constant and prove that `seq` alone makes the names distinct — if
/// the counter were dropped the names would collide, which the timestamp would
/// otherwise mask on a host with a fine-grained clock.
pub fn default_dump_name(timestamp: &str, pid: u32, seq: u64, ext: &str) -> String {
    format!("memory-dump-{timestamp}-{pid}-{seq}.{ext}")
}

/// Build a collision-safe default dump path inside `base_dir`.
///
/// The filename combines a nanosecond timestamp, the process id, and a
/// process-global monotonic counter:
/// `memory-dump-<ts>-<pid>-<seq>.<ext>`. The atomic counter guarantees
/// uniqueness for any two dumps within the same process (the case that made
/// `test_dump_state_json` flaky under parallel `cargo test`), while the pid
/// disambiguates across processes and the nanosecond timestamp keeps names
/// time-ordered. Naming is delegated to [`default_dump_name`] so the uniqueness
/// invariant can be tested deterministically without wall-clock timing.
pub fn default_dump_path(base_dir: &std::path::Path, ext: &str) -> PathBuf {
    let seq = NEXT_DUMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%9f").to_string();
    let pid = std::process::id();
    base_dir.join(default_dump_name(&timestamp, pid, seq, ext))
}

/// Validate and resolve a client-supplied dump destination, confining it to the
/// system temp directory.
///
/// Without this, an MCP client could direct a dump at an arbitrary path and
/// overwrite host files. The hardening closes three lenses on one flaw
/// (issues #296 / #354 / #414):
///
/// - **CWE-22 (path traversal):** both the temp root and the target are
///   canonicalized, so the `starts_with` check compares fully-resolved paths.
///   Canonicalizing the temp root also stops *false rejects* on platforms where
///   the temp dir is itself a symlink (e.g. macOS `/tmp -> /private/tmp`).
/// - **CWE-59 (symlink-leaf follow):** the *full* target is resolved — not just
///   its parent — and a leaf that is itself a symlink is rejected outright via
///   `symlink_metadata` (lstat, which does not follow the link). A parent-only
///   guard would wave through a leaf symlink that escapes temp, and the
///   downstream `File::create`/`VACUUM INTO` would follow it.
/// - **CWE-367 (TOCTOU):** the *resolved* path is returned and handed to the
///   engine, so the value that is validated is the value that is opened — the
///   original unresolved path is never used past this point. The lib then opens
///   the destination with `O_NOFOLLOW` to fail atomically if a symlink *leaf* is
///   raced into place between this check and the write.
///
///   **Residual (tracked in #851):** `O_NOFOLLOW` guards only the leaf, so a
///   *parent directory* component swapped to a symlink after this check is still
///   followed by the open. The default dump path's only parent is the temp root
///   (sticky-bit-protected), so exposure is limited to a client-supplied
///   *multi-level* path with an attacker-writable intermediate dir. The airtight
///   fix is fd-relative opens (`openat`/`cap-std`), deferred to #851.
pub fn validate_dump_path(p: &std::path::Path) -> Result<PathBuf, ErrorData> {
    // Make the client path absolute FIRST, resolving it against the process cwd.
    // `std::path::absolute` is purely lexical — it does NOT touch the filesystem
    // (no canonicalization, no symlink resolution), it just guarantees a parent
    // component exists. Without it, a bare leaf like `"dump.json"` has
    // `parent() == Some("")`, and `canonicalize("")` fails with a confusing
    // "No such file or directory" instead of the intended containment rejection.
    // A cwd-relative path that resolves outside temp is still rejected by the
    // `starts_with` check below — that is the correct outcome.
    let p = std::path::absolute(p)
        .map_err(|e| ValidationError::Other(format!("cannot resolve dump path: {e}")))?;
    let p = p.as_path();

    // Canonicalize the temp root so the containment check compares resolved
    // paths on both sides. Fall back to the raw value if canonicalize fails
    // (e.g. a platform that does not pre-create the temp dir).
    let temp = std::env::temp_dir();
    let canonical_temp = std::fs::canonicalize(&temp).unwrap_or(temp);

    // Reject a leaf that is itself a symlink. `symlink_metadata` (lstat) does
    // NOT follow the link, so this distinguishes a malicious leaf symlink
    // (which a later `File::create`/`VACUUM INTO` would follow out of the jail)
    // from a benign regular file. A non-existent leaf is fine — the common case.
    match std::fs::symlink_metadata(p) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(
                ValidationError::Other("dump path must not be a symlink".to_owned()).into(),
            );
        }
        Ok(_) | Err(_) => {} // regular file / absent → resolve below
    }

    // Resolve the FULL target with parent symlinks collapsed. If the leaf
    // exists, canonicalize the whole path; otherwise canonicalize the parent
    // (resolving any symlinked components) and rejoin the leaf name.
    let resolved = if p.exists() {
        std::fs::canonicalize(p)
            .map_err(|e| ValidationError::Other(format!("cannot resolve dump path: {e}")))?
    } else {
        let parent = p.parent().ok_or_else(|| {
            ValidationError::Other("dump path has no parent directory".to_owned())
        })?;
        let file_name = p
            .file_name()
            .ok_or_else(|| ValidationError::Other("dump path has no file name".to_owned()))?;
        let canonical_parent = std::fs::canonicalize(parent)
            .map_err(|e| ValidationError::Other(format!("cannot resolve dump path parent: {e}")))?;
        canonical_parent.join(file_name)
    };

    if !resolved.starts_with(&canonical_temp) {
        return Err(ValidationError::Other(format!(
            "dump path must be within the temp directory ({})",
            canonical_temp.display()
        ))
        .into());
    }

    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::{
        default_dump_name, default_dump_path, get_bool, get_datetime, get_f64, get_i64, get_str,
        get_usize, parse_consolidate_config, parse_embedding, parse_event_type, parse_fact_type,
        parse_outcome, require_f64_if_present, validate_dump_path,
    };
    use memory_engine::types::{EventType, FactType, Outcome};
    use serde_json::json;
    use std::collections::HashSet;

    /// Build an argument map carrying only an `embedding` value.
    fn emb_args(embedding: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert("embedding".to_owned(), embedding);
        m
    }

    #[test]
    fn parse_embedding_absent_is_none() {
        let m = serde_json::Map::new();
        assert!(parse_embedding(&m, 8).unwrap().is_none());
    }

    #[test]
    fn parse_embedding_null_is_none() {
        let args = emb_args(json!(null));
        assert!(parse_embedding(&args, 8).unwrap().is_none());
    }

    #[test]
    fn parse_embedding_correct_length_round_trips() {
        // A correctly-sized array deserializes verbatim.
        let v: Vec<f32> = [0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7].to_vec();
        let args = emb_args(json!(v));
        let got = parse_embedding(&args, 8).unwrap().expect("present");
        assert_eq!(got.len(), 8);
        for (a, b) in got.iter().zip(v.iter()) {
            assert!((a - b).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn parse_embedding_rejects_wrong_length() {
        // #294: a wrong-length array is rejected on its *length* BEFORE any
        // `Vec<f32>` materialization (pre-alloc DoS guard, CWE-400/770). This
        // test only verifies *that* a wrong-length array is rejected with an
        // informative message; the BEFORE-alloc ordering is enforced by the code
        // (length check precedes the `Vec` build) and documented there.
        let args = emb_args(json!(vec![0.1_f32; 16]));
        let err = parse_embedding(&args, 8).unwrap_err();
        assert!(
            err.message.contains("expected 8") && err.message.contains("got 16"),
            "error should name expected vs got: {}",
            err.message
        );
    }

    #[test]
    fn parse_embedding_non_array_rejected() {
        // A present-but-non-array value is malformed input, not a silent None.
        let args = emb_args(json!("not-an-array"));
        assert!(parse_embedding(&args, 8).is_err());
    }

    #[test]
    fn parse_embedding_non_numeric_element_rejected() {
        // #317: a correctly-sized array whose elements are not all numeric passes
        // the length gate but fails the `Vec<f32>` deserialization — it must be a
        // typed error, not a silent coercion.
        let args = emb_args(json!([0.0, "oops", 0.2, 0.3, 0.4, 0.5, 0.6, 0.7]));
        let err = parse_embedding(&args, 8).unwrap_err();
        assert!(err.message.contains("invalid embedding"), "{}", err.message);
    }

    // --- parse_event_type (#317) ---

    #[test]
    fn parse_event_type_accepts_known_variants() {
        assert_eq!(
            parse_event_type("Interaction").unwrap(),
            EventType::Interaction
        );
        assert_eq!(parse_event_type("ToolCall").unwrap(), EventType::ToolCall);
        assert_eq!(parse_event_type("MemoryOp").unwrap(), EventType::MemoryOp);
        assert_eq!(
            parse_event_type("SystemEvent").unwrap(),
            EventType::SystemEvent
        );
    }

    // `parse_event_type_rejects_unknown_preserving_token` lives in the #353 block above.

    // --- parse_outcome (#317) ---

    #[test]
    fn parse_outcome_accepts_known_variants() {
        assert_eq!(parse_outcome("Positive").unwrap(), Outcome::Positive);
        assert_eq!(parse_outcome("Negative").unwrap(), Outcome::Negative);
        assert_eq!(parse_outcome("Neutral").unwrap(), Outcome::Neutral);
    }

    // `parse_outcome_rejects_unknown_preserving_token` lives in the #353 block above.

    // --- get_datetime (#317) ---

    #[test]
    fn get_datetime_absent_is_ok_none() {
        let args = cfg_args(&[]);
        assert!(get_datetime(&args, "timestamp").unwrap().is_none());
    }

    #[test]
    fn get_datetime_valid_iso_round_trips() {
        let args = cfg_args(&[("timestamp", json!("2026-06-26T12:00:00Z"))]);
        let dt = get_datetime(&args, "timestamp").unwrap().expect("present");
        assert_eq!(dt.to_rfc3339(), "2026-06-26T12:00:00+00:00");
    }

    #[test]
    fn get_datetime_malformed_non_empty_rejected() {
        // A present, non-empty, but unparseable ISO string is malformed input — it
        // must surface as an invalid-params error rather than silently defaulting.
        let args = cfg_args(&[("timestamp", json!("not-a-timestamp"))]);
        let err = get_datetime(&args, "timestamp").unwrap_err();
        assert!(err.message.contains("invalid timestamp"), "{}", err.message);
    }

    // --- require_f64_if_present (#317) ---

    #[test]
    fn require_f64_if_present_absent_is_ok_none() {
        let args = cfg_args(&[]);
        assert!(
            require_f64_if_present(&args, "half_life_days")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn require_f64_if_present_numeric_value_passes() {
        let args = cfg_args(&[("half_life_days", json!(42.0))]);
        let v = require_f64_if_present(&args, "half_life_days").unwrap();
        assert_eq!(v, Some(42.0));
    }

    #[test]
    fn require_f64_if_present_string_value_rejected() {
        // The exact regression this guard exists for: a numeric-looking *string*
        // (`"1"`) must be rejected, not silently coerced or dropped to the default.
        let args = cfg_args(&[("half_life_days", json!("1"))]);
        let err = require_f64_if_present(&args, "half_life_days").unwrap_err();
        assert!(
            err.message.contains("half_life_days must be a number"),
            "{}",
            err.message
        );
    }

    #[test]
    fn parse_fact_type_accepts_schema_pascal_case() {
        // The JSON schemas advertise PascalCase tokens — these must still parse.
        assert_eq!(parse_fact_type("Episodic").unwrap(), FactType::Episodic);
        assert_eq!(parse_fact_type("Semantic").unwrap(), FactType::Semantic);
        assert_eq!(parse_fact_type("Procedural").unwrap(), FactType::Procedural);
    }

    #[test]
    fn parse_fact_type_reconciles_cli_snake_case() {
        // After delegating to core's canonical FromStr, the MCP surface also
        // accepts the CLI's snake_case casing (#678 reconciliation).
        assert_eq!(parse_fact_type("episodic").unwrap(), FactType::Episodic);
        assert_eq!(parse_fact_type("semantic").unwrap(), FactType::Semantic);
    }

    #[test]
    fn parse_fact_type_rejects_unknown_preserving_token() {
        let err = parse_fact_type("wisdom").unwrap_err();
        // ValidationError is a thiserror enum; the offending token is preserved
        // in its Display string.
        assert!(err.to_string().contains("wisdom"), "{err}");
    }

    #[test]
    fn parse_event_type_accepts_schema_pascal_case() {
        // The JSON schemas advertise these four PascalCase tokens — all must parse.
        assert_eq!(
            parse_event_type("Interaction").unwrap(),
            EventType::Interaction
        );
        assert_eq!(parse_event_type("ToolCall").unwrap(), EventType::ToolCall);
        assert_eq!(parse_event_type("MemoryOp").unwrap(), EventType::MemoryOp);
        assert_eq!(
            parse_event_type("SystemEvent").unwrap(),
            EventType::SystemEvent
        );
    }

    #[test]
    fn parse_event_type_reconciles_snake_case() {
        // After delegating to core's canonical FromStr, the MCP surface also
        // accepts the snake_case casing (#353/#678 reconciliation).
        assert_eq!(parse_event_type("tool_call").unwrap(), EventType::ToolCall);
        assert_eq!(
            parse_event_type("interaction").unwrap(),
            EventType::Interaction
        );
    }

    #[test]
    fn parse_event_type_rejects_outcome_signal() {
        // OutcomeSignal is a system-generated event, deliberately omitted from the
        // ingest/replay JSON schemas. Even though core's FromStr parses it, the MCP
        // boundary must keep rejecting it (with the token preserved).
        let err = parse_event_type("OutcomeSignal").unwrap_err();
        assert!(err.to_string().contains("OutcomeSignal"), "{err}");
    }

    #[test]
    fn parse_event_type_rejects_unknown_preserving_token() {
        let err = parse_event_type("WisdomOp").unwrap_err();
        assert!(err.to_string().contains("WisdomOp"), "{err}");
    }

    #[test]
    fn parse_outcome_accepts_schema_pascal_case() {
        // The JSON schema advertises PascalCase tokens — these must parse.
        assert_eq!(parse_outcome("Positive").unwrap(), Outcome::Positive);
        assert_eq!(parse_outcome("Negative").unwrap(), Outcome::Negative);
        assert_eq!(parse_outcome("Neutral").unwrap(), Outcome::Neutral);
    }

    #[test]
    fn parse_outcome_reconciles_lowercase() {
        // After delegating to core's canonical FromStr, lowercase also parses.
        assert_eq!(parse_outcome("positive").unwrap(), Outcome::Positive);
        assert_eq!(parse_outcome("neutral").unwrap(), Outcome::Neutral);
    }

    #[test]
    fn parse_outcome_rejects_unknown_preserving_token() {
        let err = parse_outcome("mixed").unwrap_err();
        // ErrorData carries the offending token in its message.
        assert!(err.message.contains("mixed"), "{}", err.message);
    }

    #[test]
    fn get_usize_rejects_negative_value() {
        // #339: a present-but-negative integer must be an ERROR, not silently
        // dropped (which would let the engine apply its own default and return
        // more results than the untrusted caller asked for).
        let err = get_usize(&cfg_args(&[("limit", json!(-1))]), "limit").unwrap_err();
        assert!(
            err.message.contains("limit must be a non-negative integer"),
            "{}",
            err.message
        );
    }

    #[test]
    fn get_usize_accepts_non_negative_value() {
        let v = get_usize(&cfg_args(&[("limit", json!(7))]), "limit").unwrap();
        assert_eq!(v, Some(7));

        // Zero is a valid non-negative usize (callers ascribe their own meaning,
        // e.g. replay's 0 = "no limit").
        let z = get_usize(&cfg_args(&[("limit", json!(0))]), "limit").unwrap();
        assert_eq!(z, Some(0));
    }

    #[test]
    fn get_usize_absent_key_is_ok_none() {
        // Absent must be distinguished from present-but-invalid: Ok(None), so the
        // caller can fall back to its default.
        let v = get_usize(&cfg_args(&[]), "limit").unwrap();
        assert_eq!(v, None);
    }

    // --- #842: scalar getters reject present-but-wrong-type input ------------
    // Each distinguishes absent/null (Ok(None) — caller may default) from a
    // present-but-wrong-JSON-type value (Err(invalid_params)), instead of the old
    // `and_then(as_*)` that let a wrong-typed value silently become the server default.

    #[test]
    fn get_i64_rejects_present_wrong_type() {
        // A stringified number from an untrusted client is an ERROR, not a silent default.
        let err = get_i64(&cfg_args(&[("n", json!("50"))]), "n").unwrap_err();
        assert!(
            err.message.contains("n must be an integer"),
            "{}",
            err.message
        );
        // A non-integral float is also wrong-type for an integer param.
        assert!(get_i64(&cfg_args(&[("n", json!(1.5))]), "n").is_err());
        // Absent, null → Ok(None); a real integer → Ok(Some).
        assert_eq!(get_i64(&cfg_args(&[]), "n").unwrap(), None);
        assert_eq!(
            get_i64(&cfg_args(&[("n", json!(null))]), "n").unwrap(),
            None
        );
        assert_eq!(
            get_i64(&cfg_args(&[("n", json!(42))]), "n").unwrap(),
            Some(42)
        );
    }

    #[test]
    fn get_str_rejects_present_wrong_type() {
        let err = get_str(&cfg_args(&[("s", json!(5))]), "s").unwrap_err();
        assert!(
            err.message.contains("s must be a string"),
            "{}",
            err.message
        );
        assert_eq!(get_str(&cfg_args(&[]), "s").unwrap(), None);
        assert_eq!(
            get_str(&cfg_args(&[("s", json!(null))]), "s").unwrap(),
            None
        );
        assert_eq!(
            get_str(&cfg_args(&[("s", json!("hi"))]), "s").unwrap(),
            Some("hi".to_owned())
        );
    }

    #[test]
    fn get_f64_rejects_present_wrong_type() {
        let err = get_f64(&cfg_args(&[("w", json!("0.5"))]), "w").unwrap_err();
        assert!(
            err.message.contains("w must be a number"),
            "{}",
            err.message
        );
        assert_eq!(get_f64(&cfg_args(&[]), "w").unwrap(), None);
        // A JSON integer is a valid number for an f64 param.
        assert_eq!(
            get_f64(&cfg_args(&[("w", json!(3))]), "w").unwrap(),
            Some(3.0)
        );
        assert_eq!(
            get_f64(&cfg_args(&[("w", json!(0.25))]), "w").unwrap(),
            Some(0.25)
        );
    }

    #[test]
    fn get_bool_rejects_present_wrong_type() {
        let err = get_bool(&cfg_args(&[("b", json!("true"))]), "b").unwrap_err();
        assert!(
            err.message.contains("b must be a boolean"),
            "{}",
            err.message
        );
        assert_eq!(get_bool(&cfg_args(&[]), "b").unwrap(), None);
        assert_eq!(
            get_bool(&cfg_args(&[("b", json!(true))]), "b").unwrap(),
            Some(true)
        );
    }

    #[test]
    fn get_usize_rejects_wrong_type_not_just_negative() {
        // #842 crux: #339 fixed only the NEGATIVE case, but `get_usize` delegates to
        // `get_i64`, so a stringified number ("50") used to slip through as `Ok(None)`
        // and silently apply the default. Now the get_i64 type-gate rejects it first.
        let err = get_usize(&cfg_args(&[("limit", json!("50"))]), "limit").unwrap_err();
        assert!(
            err.message.contains("limit must be an integer"),
            "{}",
            err.message
        );
    }

    #[test]
    fn parse_consolidate_config_rejects_negative_min_cluster_size() {
        // Dispatch-level (#339): a negative `min_cluster_size` routed through a
        // real handler config-parse path must surface as an invalid-params error,
        // not be silently coerced to the default.
        let err =
            parse_consolidate_config(&cfg_args(&[("min_cluster_size", json!(-5))])).unwrap_err();
        assert!(
            err.message
                .contains("min_cluster_size must be a non-negative integer"),
            "{}",
            err.message
        );
    }

    /// Build a `memory_consolidate` argument map from key/value pairs.
    fn cfg_args(pairs: &[(&str, serde_json::Value)]) -> serde_json::Map<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn parse_consolidate_config_rejects_out_of_range_dedup_threshold() {
        let err =
            parse_consolidate_config(&cfg_args(&[("dedup_threshold", json!(2.0))])).unwrap_err();
        assert!(
            err.message
                .contains("dedup_threshold must be in [0.0, 1.0]"),
            "{}",
            err.message
        );
    }

    #[test]
    fn parse_consolidate_config_rejects_out_of_range_cluster_threshold() {
        // #344: this is the path the provider-less integration test could not reach.
        let err =
            parse_consolidate_config(&cfg_args(&[("cluster_threshold", json!(2.0))])).unwrap_err();
        assert!(
            err.message
                .contains("cluster_threshold must be in [0.0, 1.0]"),
            "{}",
            err.message
        );
    }

    #[test]
    fn parse_consolidate_config_rejects_tiny_min_cluster_size() {
        let err =
            parse_consolidate_config(&cfg_args(&[("min_cluster_size", json!(1))])).unwrap_err();
        assert!(
            err.message.contains("min_cluster_size must be >= 2"),
            "{}",
            err.message
        );
    }

    #[test]
    fn parse_consolidate_config_rejects_wrong_type_threshold() {
        // A present-but-wrong-type value must be REJECTED, not silently defaulted
        // (gemini + codex review): `"0.95"` (string) is not a number.
        let err = parse_consolidate_config(&cfg_args(&[("cluster_threshold", json!("0.95"))]))
            .unwrap_err();
        assert!(
            err.message.contains("cluster_threshold must be a number"),
            "{}",
            err.message
        );
    }

    #[test]
    fn parse_consolidate_config_applies_defaults_and_overrides() {
        // Empty args → MCP-level defaults (dedup 0.92, cluster 0.85, min 3).
        let cfg = parse_consolidate_config(&cfg_args(&[])).unwrap();
        assert!((cfg.dedup_threshold - 0.92).abs() < f32::EPSILON);
        assert!((cfg.cluster_threshold - 0.85).abs() < f32::EPSILON);
        assert_eq!(cfg.min_cluster_size, 3);

        // Provided values flow through to the config.
        let cfg = parse_consolidate_config(&cfg_args(&[
            ("dedup_threshold", json!(0.7)),
            ("cluster_threshold", json!(0.6)),
            ("min_cluster_size", json!(5)),
        ]))
        .unwrap();
        assert!((cfg.dedup_threshold - 0.7).abs() < f32::EPSILON);
        assert!((cfg.cluster_threshold - 0.6).abs() < f32::EPSILON);
        assert_eq!(cfg.min_cluster_size, 5);
    }

    /// Regression for #546: with a *frozen* timestamp and pid, the only thing
    /// that can keep default dump names distinct is the process-global atomic
    /// counter (`seq`). This pins the clock so the test isolates the counter as
    /// the load-bearing collision guard — it fails the moment `seq` is dropped
    /// from the filename, even on a host with a fine-grained clock that would
    /// otherwise mask the regression by advancing the nanosecond timestamp
    /// between calls.
    #[test]
    fn default_dump_names_are_distinguished_by_seq_alone() {
        // Constant timestamp + pid: zero entropy from the clock or process id.
        let frozen_ts = "20260616T000000000000000";
        let frozen_pid = 4242_u32;

        let n = 1024_u64;
        let names: HashSet<_> = (0..n)
            .map(|seq| default_dump_name(frozen_ts, frozen_pid, seq, "json"))
            .collect();

        assert_eq!(
            names.len() as u64,
            n,
            "names collided with frozen ts+pid: {} unique of {n} \
             (the atomic seq counter is not making paths distinct)",
            names.len()
        );

        // Every seq in 0..n must be present exactly once, proving the counter —
        // not the timestamp — supplies the distinctness.
        for seq in 0..n {
            let expected = format!("memory-dump-{frozen_ts}-{frozen_pid}-{seq}.json");
            assert!(
                names.contains(&expected),
                "missing seq segment {seq}: {expected}"
            );
        }
    }

    /// End-to-end smoke check that the live `default_dump_path` (real clock,
    /// real pid, real atomic) produces well-formed, base-rooted, unique paths.
    /// Uniqueness here may be aided by the clock — the discriminating guarantee
    /// is proven by `default_dump_names_are_distinguished_by_seq_alone`.
    #[test]
    fn default_dump_paths_are_unique_within_process() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        let n = 1024;
        let paths: HashSet<_> = (0..n).map(|_| default_dump_path(base, "json")).collect();

        assert_eq!(
            paths.len(),
            n,
            "default dump paths collided: {} unique of {n}",
            paths.len()
        );
        for p in &paths {
            assert!(p.starts_with(base));
            assert_eq!(p.extension().and_then(|e| e.to_str()), Some("json"));
            let name = p.file_name().and_then(|n| n.to_str()).unwrap();
            assert!(name.starts_with("memory-dump-"), "unexpected name: {name}");
        }
    }

    // --- Property-based tests (#471) ---

    proptest::proptest! {
        /// Round-trip: any fixed-dimension `Vec<f32>` of finite values serializes to
        /// a JSON array, parses back through `parse_embedding(args, D)`, and equals
        /// the input bit-for-bit. serde_json widens each f32 to f64 for the JSON text
        /// and narrows on the way back; that round-trip is exact for every finite f32
        /// (f64 represents all f32 values losslessly), so a strict equality check is
        /// the right contract. Non-finite values are excluded — serde_json cannot
        /// represent NaN/±inf as JSON numbers (they would serialize to null).
        #[test]
        fn parse_embedding_round_trips_fixed_dim(
            v in proptest::collection::vec(proptest::num::f32::NORMAL | proptest::num::f32::ZERO | proptest::num::f32::SUBNORMAL, 8..=8)
        ) {
            let args = emb_args(json!(v));
            let got = parse_embedding(&args, 8)
                .expect("a finite, correctly-sized array must parse")
                .expect("present");
            proptest::prop_assert_eq!(got, v);
        }
    }

    /// Finding-1 regression (Gemini #836): a client path with no directory
    /// component (a bare leaf such as `"dump.json"`) must NOT trip the confusing
    /// `canonicalize("")` parent failure. `validate_dump_path` makes the path
    /// absolute against cwd *first* (`std::path::absolute`, purely lexical), so a
    /// bare leaf gains a real parent and is then judged by the *containment*
    /// check — accepted when cwd is inside temp, rejected (with the temp-dir
    /// error) when it is not. Both branches are asserted under one `set_current_dir`
    /// guard because cwd is process-global; this is the only test that mutates it.
    #[test]
    fn validate_dump_path_handles_bare_relative_leaf() {
        // Canonicalize temp so the asserted prefix matches `validate_dump_path`'s
        // own canonical comparison (e.g. macOS `/tmp -> /private/tmp`).
        let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let outside = std::env::current_dir().expect("cargo runs tests from the crate dir");
        debug_assert!(
            !outside.starts_with(&temp),
            "test precondition: the crate dir must be outside temp"
        );

        let saved = std::env::current_dir().ok();

        // (a) cwd OUTSIDE temp → the bare leaf resolves outside the jail and is
        //     rejected by containment, with the temp-directory error (NOT a
        //     parent-resolution error).
        std::env::set_current_dir(&outside).unwrap();
        let rejected = validate_dump_path(std::path::Path::new("dump.json"));

        // (b) cwd INSIDE temp → the same bare leaf resolves into the jail and is
        //     accepted, resolving to a path under temp.
        std::env::set_current_dir(&temp).unwrap();
        let accepted = validate_dump_path(std::path::Path::new("relative-dump.json"));

        if let Some(prev) = saved {
            let _ = std::env::set_current_dir(prev);
        }

        let err = rejected.expect_err("a bare leaf under a non-temp cwd must be rejected");
        assert!(
            err.message.contains("temp"),
            "a relative leaf must be rejected by the temp-containment check, not a \
             parent-resolution error; got: {}",
            err.message
        );

        let resolved = accepted.expect("a relative leaf resolving into temp must be accepted");
        assert!(
            resolved.starts_with(&temp),
            "resolved path {} must be inside temp {}",
            resolved.display(),
            temp.display()
        );
    }
}
