//! Fuzz target for the TEXT-column timestamp parsers (#488).
//!
//! `store::parse_timestamp` / `store::parse_optional_timestamp` are the helpers
//! every row read uses to turn a TEXT timestamp column into a `DateTime<Utc>`.
//! Both wrap `DateTime::parse_from_rfc3339` and re-map any parse failure into a
//! `rusqlite::Error::FromSqlConversionFailure`. Because the column data is
//! attacker-influenced (a corrupt or hand-edited DB file), the never-panic
//! property must hold for *every* byte sequence — the property the #487 negative
//! proptest checks over generated `String`s, here driven over raw fuzzer bytes
//! (which reach UTF-8 boundaries and control characters a `String` strategy
//! seldom produces).
//!
//! The helpers live in the `pub(crate) mod store`, so this harness reaches them
//! through the `#[cfg(fuzzing)]` `memory_engine::fuzz_seam` re-export rather than
//! widening the shipped public API. The contract: every input yields `Ok` or
//! `Err`, never a panic.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Only valid UTF-8 can reach a `&str` column; non-UTF-8 bytes never become a
    // TEXT value, so skipping them keeps the corpus on the in-contract surface.
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    // Required parser: any string is Ok(dt) or Err — never a panic.
    let _ = memory_engine::fuzz_seam::parse_timestamp(s);

    // Optional parser, present branch: same total contract on the inner string.
    let _ = memory_engine::fuzz_seam::parse_optional_timestamp(Some(s));

    // Optional parser, absent branch (input-free): must always be Ok(None).
    // Asserting it here makes the absent arm part of the never-panic fuzzing
    // surface too, and an unexpected Err/Some would trip libFuzzer.
    assert!(matches!(
        memory_engine::fuzz_seam::parse_optional_timestamp(None),
        Ok(None)
    ));
});
