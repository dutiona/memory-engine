//! Fuzz target for the content-block parser (#426).
//!
//! `bootstrap::parse::parse_content_blocks` maps an arbitrary `serde_json::Value`
//! `content` array into `ContentBlock`s, dispatching `parse_single_block` on the
//! per-element `type` string (`text` / `tool_use` / unrecognized fall-through).
//! The fuzzer first parses bytes into a `Value`, then drives the block parser to
//! prove the type-string match and the `.get(..)`/`as_str` chains never panic on
//! adversarial shapes (wrong-typed fields, missing keys, deep nesting).
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(data) {
        let blocks = memory_engine::fuzz_seam::parse_content_blocks(&value);
        // A non-array `content` yields an empty vec; never a panic.
        let _ = blocks.len();
    }
});
