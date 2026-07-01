//! Ingest-time resource bounds (issue #572 / L10).
//!
//! Event `payload`, fact `metadata`, and fact `content` are free-form,
//! consumer-supplied data persisted verbatim. Without a ceiling, a hostile or
//! runaway caller could force the engine to store (and later re-materialize) an
//! arbitrarily large blob per row. Every public *consumer* write path therefore
//! caps these fields here.
//!
//! Operator-controlled paths are intentionally exempt: `restore_*` reconstructs
//! an engine from a trusted snapshot/backup and must round-trip whatever it
//! contains (consistent with the trusted-path contract on
//! `crate::inspect::dump::dump_sqlite`).

use crate::error::{ConflictError, MemoryError, Result};
use crate::types::NewFact;

/// Maximum serialized byte length of a single ingested JSON document (event
/// `payload` or fact `metadata`) and of a fact's `content` body.
///
/// 1 MiB is far above any legitimate memory fact (KB-scale) while bounding the
/// worst case to a constant. Currently a compile-time constant; making it
/// per-engine configurable is a documented follow-up (see issue #572 / L10).
pub const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;

/// Measure the serialized byte length of `value`, aborting once it provably
/// exceeds `limit`.
///
/// Bytes are counted through a zero-storage [`std::io::Write`] sink that returns
/// an error the moment the running total passes `limit`, which halts
/// `serde_json` mid-walk. This bounds **both** memory (O(1) — nothing is
/// buffered, unlike `to_vec().len()` which would allocate a second full copy and
/// be the very `DoS` we guard against) **and** CPU (O(limit) — serialization
/// stops early), so a hostile multi-gigabyte `Value` can force neither an
/// allocation nor an unbounded serialization pass.
///
/// Returns `Ok(exact_len)` when the value fits within `limit`, or `Err(len)`
/// once it exceeds — where `len` is the count at the abort point (a lower bound
/// on the true serialized size, sufficient to report "over the limit").
fn serialized_len(value: &serde_json::Value, limit: usize) -> std::result::Result<usize, usize> {
    use std::io::Write;

    struct CountingSink {
        count: usize,
        limit: usize,
    }
    impl Write for CountingSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.count += buf.len();
            if self.count > self.limit {
                return Err(std::io::Error::other("payload exceeds limit"));
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut sink = CountingSink { count: 0, limit };
    // A `Value` can only fail to serialize via our sink's limit-exceeded error
    // (it holds no non-finite floats), so `Err` unambiguously means "over limit".
    match serde_json::to_writer(&mut sink, value) {
        Ok(()) => Ok(sink.count),
        Err(_) => Err(sink.count),
    }
}

/// Reject a JSON document that serializes past [`MAX_PAYLOAD_BYTES`].
///
/// `kind` names the offending field for the error message.
///
/// # Errors
///
/// Returns [`ConflictError::PayloadTooLarge`] when the serialized form exceeds
/// the limit (measurement aborts early, so CPU is bounded to O(limit)).
pub fn check_json_size(value: &serde_json::Value, kind: &'static str) -> Result<()> {
    match serialized_len(value, MAX_PAYLOAD_BYTES) {
        Ok(_) => Ok(()),
        Err(size) => Err(MemoryError::Conflict(ConflictError::PayloadTooLarge {
            kind,
            size,
            limit: MAX_PAYLOAD_BYTES,
        })),
    }
}

/// Reject a string body longer than [`MAX_PAYLOAD_BYTES`] bytes.
///
/// # Errors
///
/// Returns [`ConflictError::PayloadTooLarge`] when `s` exceeds the limit.
pub const fn check_str_size(s: &str, kind: &'static str) -> Result<()> {
    let size = s.len();
    if size > MAX_PAYLOAD_BYTES {
        return Err(MemoryError::Conflict(ConflictError::PayloadTooLarge {
            kind,
            size,
            limit: MAX_PAYLOAD_BYTES,
        }));
    }
    Ok(())
}

/// Reject a [`NewFact`] whose `content` or `metadata` exceeds the bound.
///
/// The single chokepoint for every path that persists a caller-supplied fact
/// (`add_fact`, `add_facts_batch`, `resolve_conflict`, `bootstrap`).
///
/// # Errors
///
/// Returns [`ConflictError::PayloadTooLarge`] for the first offending field.
pub fn check_new_fact(fact: &NewFact) -> Result<()> {
    check_str_size(&fact.content, "fact content")?;
    check_json_size(&fact.metadata, "fact metadata")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialized_len_counts_exact_bytes() {
        const EXPECTED_STRING_LEN: usize = 5; // "abc" → content + two quotes
        const EXPECTED_EMPTY_OBJECT_LEN: usize = 2; // {}

        // Within a generous limit, the exact serialized length is returned.
        assert_eq!(
            serialized_len(&serde_json::Value::String("abc".into()), MAX_PAYLOAD_BYTES).unwrap(),
            EXPECTED_STRING_LEN
        );
        assert_eq!(
            serialized_len(&serde_json::json!({}), MAX_PAYLOAD_BYTES).unwrap(),
            EXPECTED_EMPTY_OBJECT_LEN
        );
    }

    #[test]
    fn serialized_len_aborts_early_over_limit() {
        // A value larger than a tiny limit returns Err with a count past the
        // limit — proving serialization halted instead of walking the whole
        // value (the CPU bound the gemini review asked for).
        let big = serde_json::Value::String("x".repeat(1000));
        let limit = 16;
        match serialized_len(&big, limit) {
            Err(n) => assert!(n > limit, "abort count {n} must exceed limit {limit}"),
            Ok(n) => panic!("expected early abort over limit {limit}, got Ok({n})"),
        }
    }

    #[test]
    fn check_json_size_boundary() {
        // Build a value serializing to exactly MAX_PAYLOAD_BYTES, then one over.
        // A JSON string of N chars serializes to N + 2 bytes (the quotes).
        let at_limit = serde_json::Value::String("x".repeat(MAX_PAYLOAD_BYTES - 2));
        assert!(check_json_size(&at_limit, "fact metadata").is_ok());

        let over = serde_json::Value::String("x".repeat(MAX_PAYLOAD_BYTES));
        let err = check_json_size(&over, "fact metadata").unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Conflict(ConflictError::PayloadTooLarge {
                kind: "fact metadata",
                ..
            })
        ));
    }

    #[test]
    fn check_str_size_boundary() {
        let at_limit = "x".repeat(MAX_PAYLOAD_BYTES);
        assert!(check_str_size(&at_limit, "fact content").is_ok());

        let over = "x".repeat(MAX_PAYLOAD_BYTES + 1);
        let err = check_str_size(&over, "fact content").unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Conflict(ConflictError::PayloadTooLarge {
                kind: "fact content",
                size,
                limit
            }) if size == MAX_PAYLOAD_BYTES + 1 && limit == MAX_PAYLOAD_BYTES
        ));
    }
}
