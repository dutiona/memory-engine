use std::collections::HashMap;

use me_types::error::{MigrationError, Result};

/// Function that transforms an event payload from revision N to N+1.
pub type UpcasterFn = fn(serde_json::Value) -> Result<serde_json::Value>;

/// Registry of per-event-type upcaster functions.
///
/// Each upcaster transforms a payload from revision `N` to `N+1`.
/// Chains are applied sequentially: if an event is at revision 1 and
/// the latest is 3, upcasters `(1→2)` then `(2→3)` run in order.
///
/// Raw reads (`EventStore::get`/`list`) bypass this entirely — audit-log
/// semantics are preserved. Only `get_upcasted`/`list_upcasted` apply the chain.
#[derive(Clone)]
pub struct UpcasterRegistry {
    /// `event_type` → (`from_revision` → upcaster function).
    ///
    /// Two-level map so the hot upcasting loop can look up the event type's chain
    /// once (by `&str` borrow, no allocation) and then index into it by revision
    /// without cloning the event-type string on every step.
    upcasters: HashMap<String, HashMap<u16, UpcasterFn>>,
    /// `event_type` → latest known revision
    latest: HashMap<String, u16>,
}

impl std::fmt::Debug for UpcasterRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count: usize = self.upcasters.values().map(HashMap::len).sum();
        f.debug_struct("UpcasterRegistry")
            .field("registered_upcasters", &count)
            .field("latest_revisions", &self.latest)
            .finish()
    }
}

impl Default for UpcasterRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl UpcasterRegistry {
    /// Create an empty registry. All event types default to revision 1.
    #[must_use]
    pub fn new() -> Self {
        Self {
            upcasters: HashMap::new(),
            latest: HashMap::new(),
        }
    }

    /// Number of registered upcaster functions. Used by the construction
    /// equivalence harness to prove the builder preserves the registry verbatim
    /// (deterministic, unlike the `Debug` impl's `HashMap` ordering).
    ///
    /// Counts individual (`event_type`, `from_revision`) pairs across all chains.
    // `pub` (not `pub(crate)`): the construction-equivalence harness that observes this
    // now lives in the facade crate (Wave 2 #816), so it reads it cross-crate.
    #[must_use]
    pub fn registered_count(&self) -> usize {
        self.upcasters.values().map(HashMap::len).sum()
    }

    /// Register an upcaster that transforms `event_type` payloads from
    /// `from_revision` to `from_revision + 1`.
    ///
    /// The latest revision for this event type is automatically tracked as
    /// `max(current_latest, from_revision + 1)`.
    pub fn register(&mut self, event_type: &str, from_revision: u16, func: UpcasterFn) {
        let target = from_revision.saturating_add(1);
        self.upcasters
            .entry(event_type.to_string())
            .or_default()
            .insert(from_revision, func);
        let current = self.latest.entry(event_type.to_string()).or_insert(1);
        if target > *current {
            *current = target;
        }
    }

    /// Get the latest known revision for an event type.
    /// Returns 1 if no upcasters are registered for this type.
    #[must_use]
    pub fn latest_revision(&self, event_type: &str) -> u16 {
        self.latest.get(event_type).copied().unwrap_or(1)
    }

    /// Apply the upcaster chain to transform a payload from `current_revision`
    /// to the latest revision for `event_type`.
    ///
    /// Returns `(transformed_payload, final_revision)`.
    ///
    /// If no upcasters are registered or the payload is already at latest,
    /// returns the payload unchanged.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Migration` if there's a gap in the upcaster chain
    /// (e.g., revision 2→3 is registered but 1→2 is missing).
    pub fn upcast(
        &self,
        event_type: &str,
        current_revision: u16,
        payload: serde_json::Value,
    ) -> Result<(serde_json::Value, u16)> {
        let latest = self.latest_revision(event_type);
        if current_revision >= latest {
            return Ok((payload, current_revision));
        }

        let mut value = payload;
        let mut rev = current_revision;
        // Look up the inner chain once by &str — no String allocation in the loop.
        let chain = self.upcasters.get(event_type);

        while rev < latest {
            let func = chain.and_then(|inner| inner.get(&rev)).ok_or_else(|| {
                MigrationError::MissingUpcaster(format!(
                    "missing upcaster for event type '{event_type}' from revision {rev} to {}",
                    rev + 1
                ))
            })?;
            value = func(value)?;
            rev += 1;
        }

        Ok((value, rev))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use me_types::error::MemoryError;

    #[test]
    fn empty_returns_unchanged() {
        let registry = UpcasterRegistry::new();
        let payload = serde_json::json!({"key": "value"});
        let (result, rev) = registry.upcast("Interaction", 1, payload.clone()).unwrap();
        assert_eq!(result, payload);
        assert_eq!(rev, 1);
    }

    #[test]
    fn latest_revision_defaults_to_1() {
        let registry = UpcasterRegistry::new();
        assert_eq!(registry.latest_revision("Interaction"), 1);
        assert_eq!(registry.latest_revision("NonExistent"), 1);
    }

    #[test]
    fn single_upcaster_transforms() {
        let mut registry = UpcasterRegistry::new();
        registry.register("Interaction", 1, |mut v| {
            v["version"] = serde_json::json!("v2");
            Ok(v)
        });

        assert_eq!(registry.latest_revision("Interaction"), 2);

        let payload = serde_json::json!({"msg": "hello"});
        let (result, rev) = registry.upcast("Interaction", 1, payload).unwrap();
        assert_eq!(rev, 2);
        assert_eq!(result["version"], "v2");
        assert_eq!(result["msg"], "hello");
    }

    #[test]
    fn chain_transforms_sequentially() {
        let mut registry = UpcasterRegistry::new();
        // 1→2: add field
        registry.register("ToolCall", 1, |mut v| {
            v["step1"] = serde_json::json!(true);
            Ok(v)
        });
        // 2→3: rename field
        registry.register("ToolCall", 2, |mut v| {
            v["step2"] = serde_json::json!(true);
            Ok(v)
        });

        assert_eq!(registry.latest_revision("ToolCall"), 3);

        let payload = serde_json::json!({"original": true});
        let (result, rev) = registry.upcast("ToolCall", 1, payload).unwrap();
        assert_eq!(rev, 3);
        assert!(result["original"].as_bool().unwrap());
        assert!(result["step1"].as_bool().unwrap());
        assert!(result["step2"].as_bool().unwrap());
    }

    #[test]
    fn missing_in_chain_errors() {
        let mut registry = UpcasterRegistry::new();
        // Register 2→3 but NOT 1→2 — creates a gap
        registry.register("ToolCall", 2, Ok);

        let payload = serde_json::json!({});
        let err = registry.upcast("ToolCall", 1, payload).unwrap_err();
        assert!(
            matches!(err, MemoryError::Migration(MigrationError::MissingUpcaster(ref msg)) if msg.contains("missing upcaster")),
            "expected missing upcaster error, got: {err:?}"
        );
    }

    #[test]
    fn noop_at_latest() {
        let mut registry = UpcasterRegistry::new();
        registry.register("Interaction", 1, |mut v| {
            v["transformed"] = serde_json::json!(true);
            Ok(v)
        });

        // Already at latest revision 2
        let payload = serde_json::json!({"data": 42});
        let (result, rev) = registry.upcast("Interaction", 2, payload.clone()).unwrap();
        assert_eq!(result, payload); // unchanged
        assert_eq!(rev, 2);
    }

    #[test]
    fn independent_event_types() {
        let mut registry = UpcasterRegistry::new();
        registry.register("Interaction", 1, |mut v| {
            v["interaction_v2"] = serde_json::json!(true);
            Ok(v)
        });
        registry.register("ToolCall", 1, |mut v| {
            v["toolcall_v2"] = serde_json::json!(true);
            Ok(v)
        });

        assert_eq!(registry.latest_revision("Interaction"), 2);
        assert_eq!(registry.latest_revision("ToolCall"), 2);
        assert_eq!(registry.latest_revision("MemoryOp"), 1); // no upcasters

        let (r1, _) = registry
            .upcast("Interaction", 1, serde_json::json!({}))
            .unwrap();
        assert!(r1["interaction_v2"].as_bool().unwrap());
        assert!(r1.get("toolcall_v2").is_none());
    }

    /// Composition invariants for arbitrary-length upcaster chains (#486).
    ///
    /// The example tests above only cover chains of length 0/1/2. These proptests
    /// drive chains of `0..=10` registered steps and pin the three algebraic laws
    /// the chain must obey for *any* length and *any* starting revision.
    mod proptest_chain {
        use super::*;
        use proptest::prelude::*;

        const TYPE: &str = "ChainEvt";

        /// Upper bound on the number of registered chain steps a proptest drives.
        /// `from_revision` ranges over `1..=MAX_CHAIN`, so we need one distinct
        /// step function per value in that range.
        const MAX_CHAIN: u16 = 10;

        /// Generate a *distinct* step function per `from_revision`, each one
        /// hard-coding the revision it is registered at and appending **that
        /// constant** (not a length-derived marker) to the `"trace"` array.
        ///
        /// This is what makes the chain *order-sensitive against the registry's
        /// own lookup*: `UpcasterRegistry::upcast` fetches the step keyed by the
        /// current revision, so if it ever looks up the wrong key (a swapped or
        /// misordered sequence), the executed trace records the *wrong* revision
        /// constant — even when the chain *length* is identical. A marker derived
        /// only from `trace.len()` could not tell those apart.
        ///
        /// The `Result` return is structurally required: each fn is registered as
        /// an [`UpcasterFn`] (`fn(Value) -> Result<Value>`), so the signature is
        /// fixed by the API even though these steps never fail — hence the
        /// `unnecessary_wraps` allow.
        macro_rules! rev_steps {
            ($($rev:literal),+ $(,)?) => {
                /// Step functions indexed so that `STEP_FNS[r]` is the step
                /// registered at `from_revision == r` (index 0 is an unused
                /// placeholder — revisions are 1-based).
                static STEP_FNS: &[UpcasterFn] = &[
                    // index 0: never registered (revisions start at 1).
                    |v| Ok(v),
                    $({
                        #[allow(clippy::unnecessary_wraps)]
                        fn step(mut v: serde_json::Value) -> Result<serde_json::Value> {
                            let prev = v["trace"].as_array().cloned().unwrap_or_default();
                            let mut trace = prev;
                            // Append THIS step's own from_revision, not a
                            // length-derived value: an order-blind impl cannot
                            // reproduce the right sequence.
                            trace.push(serde_json::json!($rev));
                            v["trace"] = serde_json::Value::Array(trace);
                            Ok(v)
                        }
                        step as UpcasterFn
                    }),+
                ];
            };
        }
        rev_steps!(1, 2, 3, 4, 5, 6, 7, 8, 9, 10);

        /// Fetch the step function registered at `from_revision`.
        fn step_fn(from_revision: u16) -> UpcasterFn {
            STEP_FNS[usize::from(from_revision)]
        }

        /// Build a registry whose `TYPE` chain has `n` steps registered at
        /// `from_revision = 1..=n` (so `latest_revision == n + 1`, since an empty
        /// chain already starts everything at revision 1). Each `from_revision`
        /// gets its *own* step function (see [`rev_steps!`]).
        fn registry_with_chain(n: u16) -> UpcasterRegistry {
            assert!(
                n <= MAX_CHAIN,
                "chain length {n} exceeds MAX_CHAIN {MAX_CHAIN}"
            );
            let mut r = UpcasterRegistry::new();
            for from in 1..=n {
                r.register(TYPE, from, step_fn(from));
            }
            r
        }

        proptest! {
            /// Law 1 — idempotent at the latest revision: upcasting a payload that
            /// is *already* at `latest_revision` is a no-op and returns the payload
            /// byte-for-byte plus the unchanged revision. Distinct expected values
            /// (unchanged payload AND unchanged rev) catch a stray transform or a
            /// rev mutation.
            #[test]
            fn idempotent_at_latest(n in 0u16..=10) {
                let registry = registry_with_chain(n);
                let latest = registry.latest_revision(TYPE);
                prop_assert_eq!(latest, n + 1);

                let payload = serde_json::json!({"trace": [], "marker": "untouched"});
                let (out, rev) = registry.upcast(TYPE, latest, payload.clone()).unwrap();
                prop_assert_eq!(rev, latest);
                prop_assert_eq!(out, payload);
            }

            /// Law 2 — chained == direct, *in the right order*: starting at any
            /// revision `start` in `1..=latest`, a single `upcast` to latest must
            /// execute the steps for revisions `start, start+1, …, latest-1` —
            /// **in that exact sequence**. Each step appends its own
            /// `from_revision` constant to `"trace"`, so the executed trace is the
            /// literal revision sequence the registry visited. Asserting it equals
            /// `start..latest` is order-sensitive: an order-blind impl that runs
            /// the same number of steps but looks up the wrong keys (e.g. swaps two
            /// adjacent steps, or iterates the span in reverse) produces a
            /// permuted trace and fails. The length check is then a corollary
            /// (catches off-by-one on the loop bound).
            #[test]
            fn chained_equals_direct(n in 1u16..=10, start_off in 0u16..=10) {
                let registry = registry_with_chain(n);
                let latest = registry.latest_revision(TYPE); // == n + 1
                // Pick a start within [1, latest]; clamp the offset into range.
                let start = 1 + (start_off % latest);

                let payload = serde_json::json!({"trace": []});

                // Direct: one upcast call over the whole span. (Payload is not
                // reused afterwards, so it moves in without a clone.)
                let (direct, direct_rev) = registry.upcast(TYPE, start, payload).unwrap();

                // Expected: the steps keyed at revisions start..latest, applied in
                // ascending order, each recording its own from_revision.
                let expected_trace: Vec<serde_json::Value> =
                    (start..latest).map(|rev| serde_json::json!(rev)).collect();

                prop_assert_eq!(direct_rev, latest);
                // Order-sensitive: the exact revision sequence the registry ran.
                prop_assert_eq!(
                    direct["trace"].as_array(),
                    Some(&expected_trace)
                );
                // Corollary — the trace length equals the number of steps applied
                // (catches an off-by-one on the loop bound independently).
                prop_assert_eq!(
                    direct["trace"].as_array().map(Vec::len),
                    Some(usize::from(latest - start))
                );
            }

            /// Law 3 — revisions strictly past `latest` are a no-op: a payload
            /// claiming a revision `> latest` is returned unchanged with its own
            /// revision preserved (NOT clamped to latest). The `> latest` guard and
            /// the `>= latest` early-return share the same branch, so a flip from
            /// `>=` to `>` would corrupt the at-latest case Law 1 already pins.
            #[test]
            fn future_revision_is_noop(n in 0u16..=10, beyond in 1u16..=100) {
                let registry = registry_with_chain(n);
                let latest = registry.latest_revision(TYPE);
                let future = latest.saturating_add(beyond);

                let payload = serde_json::json!({"trace": [], "v": 7});
                let (out, rev) = registry.upcast(TYPE, future, payload.clone()).unwrap();
                prop_assert_eq!(rev, future);
                prop_assert_eq!(out, payload);
            }
        }
    }
}
