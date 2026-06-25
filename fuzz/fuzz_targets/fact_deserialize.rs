//! Fuzz target for core-type JSON deserialization over untrusted BYTES (#445).
//!
//! `Fact`, `Event`, and `Edge` all derive `Deserialize` and are the entry-points
//! for untrusted JSON flowing in from the MCP server and the import path. This
//! target fuzzes the **bytes** directly via `serde_json::from_slice` — it does
//! NOT add `#[derive(Arbitrary)]` to the core types (that would mean editing
//! `types.rs`). The contract is total: deserialization either returns `Ok` or a
//! `serde_json::Error`, never a panic.
//!
//! Semantic note (the residual the #445 re-triage flags): serde accepts
//! structurally valid but semantically extreme values — a non-finite
//! `base_importance` (`Infinity`/`NaN` via the JSON `f64` path) or one outside
//! `[0.0, 1.0]`. Per `Fact::base_importance`'s own docs, range enforcement lives in
//! `add_fact` (#571), not in the type, and a few direct-insert paths still skip
//! it (#584). So this is an *observation* of the validation seam, not a crash
//! condition: we surface the extreme value (via `std::hint::black_box`) without
//! asserting it away, so a future range-tightening can turn it into a guard.
#![no_main]

use libfuzzer_sys::fuzz_target;

use memory_engine::{Edge, Event, Fact};

fuzz_target!(|data: &[u8]| {
    if let Ok(fact) = serde_json::from_slice::<Fact>(data) {
        // Observe (do not assert) the semantic-validation seam: a parsed Fact may
        // carry a non-finite or out-of-range base_importance, an arbitrarily long
        // embedding, etc. The type accepts it; only `add_fact` rejects it.
        std::hint::black_box(fact.base_importance.is_finite());
        std::hint::black_box(fact.embedding.len());
    }
    let _ = serde_json::from_slice::<Event>(data);
    let _ = serde_json::from_slice::<Edge>(data);
});
