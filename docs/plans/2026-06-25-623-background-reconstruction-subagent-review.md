# Plan Review — #623 (clean-slate subagent: read-path-completeness + structural/scope lenses)

Two independent lenses of the 5-lens review Workflow, validated against the real code.

## Read-path completeness lens — verdict: LGTM (the original DROP enumeration WAS exhaustive)

D4's 8-site enumeration of `facts.embedding` readers was correct and complete; no missed raw reader, no
frozen-migration site over-listed, the seam was FilterSql-compatible, the restore reorder + back-compat were
correctly scoped. (One LOW clarity nit on the seam SQL.) **This lens validated that the DROP design was
_implementable_ — but the migration-safety lens then priced it (17 query sites via `FACT_COLUMNS`) and the
cheaper keep-active design won.** With the pivot, this lens's concern is moot (no reader changes).

## Structural / scope / over-engineering lens — verdict: NEEDS-WORK → resolved

- **Over-structuring:** a new `src/reconstruct/` module + `src/storage/reconstruction.rs` +
  `src/engine/reconstruct.rs` is too many seams for #623's scope.
- **Different-dim T4 gate is unachievable** by the described mechanism (engine `embed_dim` frozen at open) —
  scope to same-dim or specify an engine-rebuild path. (Corroborates the async lens.)
- Documentation/Testing/Verification sections present + adequate. Scope boundaries vs #624/#625/#689 crisp.
- Feasibility of the migration-first phasing: sound.
- The "keep `facts.embedding` vestigial, drop in v15" half-measure was questioned — "just keep it active or
  full-drop." (The pivot resolves this: keep it **active**, not vestigial; no v15 drop.)

## Resolution

- [MEDIUM] over-structured modules → **Trimmed (D7):** dropped the separate `src/reconstruct/` module —
  orchestration folds into `src/engine/reconstruct.rs`. Kept `store::fact_vectors` + the `Reconstruction`
  port trait + the registry seams (justified bounded concern).
- [HIGH] different-dim gate unachievable → **Scoped to same-dim** (see advisor.md); T4 gate is
  `reconstruct_full_cycle_swaps_same_dim_model`; different-dim → PR2.
- [MEDIUM] keep-vestigial half-measure → **Resolved by the pivot:** `facts.embedding` stays **active** (the
  authoritative serving store), not vestigial; no v15 drop; `fact_vectors` holds non-active spaces only.
- Read-path-completeness LGTM + Doc/Test/Verify-present + scope-crisp → no change; confirmations retained.
