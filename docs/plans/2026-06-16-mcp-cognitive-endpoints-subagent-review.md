# Subagent Review — MCP Cognitive Endpoints (#225)

Two clean-slate reviewers: a general-purpose fresh-eyes pass and an adversarial rust-specialist.

## Clean-slate (general-purpose) — verdict: LGTM with minor fixes

All load-bearing line-refs verified accurate: `run_dream_cycle`/`apply_cycle_report` exist; `CycleReport`
is `Serialize+Deserialize` and NOT `#[non_exhaustive]`; `to_mcp_error` lacks a `Cycle` arm; `handle_flush_insights`
stamps only `source`; `FactStore::list_by_scopes_recent`, `ScopeTree::resolve_path`/`subtree` exist as described.
Documentation/Testing/Verification sections substantive.

- [MEDIUM] T7 inherits a silent-skip on non-object metadata (marker dropped for malformed input).
- [LOW] T2 re-export line is the unbraced `pub use engine::cognitive::DreamContext;`.
- [LOW] grounding note cites the wrong store idiom for T3 (`extra_conditions` verbatim vs `list_by_scopes_recent` params).
- [LOW] document the "non-null value present" contract on the generic store method; pin the `{"insight": null}` edge.
- [LOW] decision A generic method justified (quarantine marker is a real second consumer); decision E (empty-vec) correct given lazy scope creation.

## Adversarial (rust-specialist)

- **[BLOCKER] T3** — `extra_conditions` is documented non-parameterized ("appended verbatim; must not reference bind parameters"), so `json_extract(metadata, '$.' || ?2)` would bind `?2` to the OUTER query's param, not `marker_key`. Fix: standalone query (mirror `list_by_scopes_recent` with real `params!`) + trusted-literal `format!("json_type(metadata, '$.{marker_key}') IS NOT NULL")`. Prefer `json_type` over `json_extract` (codebase idiom; robust on absent vs present-null).
- **[HIGH] T7** — normalize non-object metadata to `{}` before stamping (don't rely on the `if let Value::Object` no-op silently dropping the load-bearing marker). Mirror `promote_in_conn`.
- **[HIGH] T4** — `subtree(unknown_id)` returns the singleton `[id]`, not empty; must early-return `Ok(vec![])` on `resolve_path → None`, never call `subtree` on a fallback id.
- [HIGH→noted] T9 — `CycleReport` round-trips cleanly (incl. `PromotionProvenance.lineage_id` `skip_serializing/default`); the `scope_id` in an `AddFact` delta is a DB-internal id the client could edit — document apply_cycle_report's report-trust boundary.
- [MEDIUM] T1 — `CycleError → invalid_params` is right for `apply_cycle_report`; a hypothetical `run_dream_cycle` Cycle error would mis-tier, but `DefaultDreamCycle` never returns one. Acceptable; noted.
- [MEDIUM] T8 — `get_bool` exists; `.unwrap_or(true)` default correct.
- [LOW] T6 — add `#[must_use]` to `ok_serialized` to match `ok_json`.

## Resolution

- [BLOCKER] T3 extra_conditions/bind-param clash → **Fixed**: rewrote T3 to a standalone `list_by_scopes_recent`-style query with a trusted-literal `json_type(metadata, '$.{marker_key}') IS NOT NULL` predicate; rustdoc states marker_key is a trusted engine const (never client input); added the `{"insight": null}` exclusion test case.
- [HIGH] T7 non-object metadata → **Fixed**: T7 now normalizes metadata to `{}` before stamping + adds a non-object-input test.
- [HIGH] T4 subtree-singleton trap → **Fixed**: T4 now has the explicit `match resolve_path { Some(id)=>subtree(id), None=>return Ok(vec![]) }` snippet.
- [HIGH→noted] scope_id trust → **Documented**: noted as apply_cycle_report's report-trust boundary (the tool round-trips a dream_cycle output; arbitrary client-edited reports are validated for fact existence/bounds but trust internal scope_id). No code change.
- [MEDIUM] T1 mis-tier → **Documented** in T1 (common-case tiering correct; DefaultDreamCycle never returns Cycle).
- [LOW] T2 unbraced re-export → **Fixed** in T2 wording. [LOW] T6 `#[must_use]` → folded into T6. [LOW] grounding citation → superseded by the rewritten T3. [LOW] non-null-present contract → folded into T3 rustdoc requirement.
- Clean-slate confirmed no blockers and verified all line-refs; client-CycleReport deserialization + json_type predicate are injection-safe.
