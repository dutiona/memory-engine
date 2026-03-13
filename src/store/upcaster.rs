use std::collections::HashMap;

use crate::error::{MemoryError, Result};

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
    /// `(event_type, from_revision)` → upcaster function
    upcasters: HashMap<(String, u16), UpcasterFn>,
    /// `event_type` → latest known revision
    latest: HashMap<String, u16>,
}

impl std::fmt::Debug for UpcasterRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpcasterRegistry")
            .field("registered_upcasters", &self.upcasters.len())
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

    /// Register an upcaster that transforms `event_type` payloads from
    /// `from_revision` to `from_revision + 1`.
    ///
    /// The latest revision for this event type is automatically tracked as
    /// `max(current_latest, from_revision + 1)`.
    pub fn register(&mut self, event_type: &str, from_revision: u16, func: UpcasterFn) {
        let target = from_revision + 1;
        self.upcasters
            .insert((event_type.to_string(), from_revision), func);
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
        let type_key = event_type.to_string();

        while rev < latest {
            let key = (type_key.clone(), rev);
            let func = self.upcasters.get(&key).ok_or_else(|| {
                MemoryError::Migration(format!(
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
        registry.register("ToolCall", 2, |v| Ok(v));

        let payload = serde_json::json!({});
        let err = registry.upcast("ToolCall", 1, payload).unwrap_err();
        assert!(
            matches!(err, MemoryError::Migration(ref msg) if msg.contains("missing upcaster")),
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
}
