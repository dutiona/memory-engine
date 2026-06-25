# Plan Review — #623 (advisor-slot: adversarial deep-review, migration/atomicity/async lenses)

> **Provenance.** `advisor()` is unavailable in this environment. Per the super-plan fallback + the
> `feedback_review_under_budget` preference, the advisor slot is satisfied by a **5-lens multi-agent
> adversarial review Workflow** (17 agents, ~1.46M tokens) that validated each lens against the real code,
> then adversarially verified the load-bearing findings. This file records the migration-safety,
> promote-atomicity/concurrency, and async/port-shape lenses (the "advisor" substitute). The external
> Codex+agy loop is reserved for the PR stage.

## Verdict: NEEDS-WORK → resolved. Two HIGH findings reshaped the design (for the better).

### [HIGH] Read-path repoint is 17 `FACT_COLUMNS` sites, not 1 (migration-safety lens; self-verified)

`row_to_fact(row: &Row, embed_dim)` takes only a `&Row` (no DB handle) and reads `embedding` by name; it is
fed by `FACT_COLUMNS` used at **17 `SELECT` sites** (`facts.rs:22`, callers `:225/256/279/369/…`).
"Repoint all reads to `fact_vectors[active]`" therefore forces `FACT_COLUMNS` → a JOIN projection across all
17 + a NULL-handling contract — permanent hot-read-path complexity + the #622-D9 blast radius, for an O(1)-
vs-O(N) benefit that only matters on the rare promote.

### [HIGH] Different-dim reconstruction is unsatisfiable as planned (async + structural lenses)

The engine's `embed_dim` is frozen at open and threaded immutably into the pool / `FactStore` / HNSW /
`deserialize`. A different-dim swap needs an engine reopen/rebuild — not achievable through `&self` promote.

### [HIGH] Completeness gate must run INSIDE the promote tx (atomicity lens)

The plan put it "pre-tx" → a TOCTOU gap separate from the #625-deferred race. The **catch-up** (which embeds,
so must be off-lock) stays pre-tx; the **completeness COUNT gate** must run on the tx connection inside the
`block_write` transaction.

### Confirmed-correct (no defect)

Demote-before-activate ordering (partial-unique index never sees two actives) — correct + necessary. The
one-`block_write`/`unchecked_transaction` atomicity pattern matches `apply_cycle_deltas_atomic` /
`resolve_conflict_atomic`. Cursorless anti-join backfill + `ON CONFLICT(fact_id,space_id) DO NOTHING`
idempotency/crash-resumability — sound. `spawn_blocking` embed with `Arc<dyn EmbeddingProvider>` + no guard
across `.await` — matches the #631 pattern. The `Reconstruction` async-trait supertrait is object-safe and
`PgBackend` (#634)-implementable. The additive `migrate_v13_to_v14` (INSERT…SELECT byte-faithful, FK behavior)
was sound — and is now moot (D3 copies nothing).

### LOW

T3 gate name `promote_rescans_stragglers_inside_tx` contradicts the pre-tx catch-up → rename. Dump/restore FK
order is the full 3-step (spaces→facts→fact_vectors). D2 write-only invariant could be one sentence sharper.

## Resolution

- [HIGH] 17-site read-path repoint → **Design pivot (user-confirmed).** Reversed DROP/repoint: `facts.embedding`
  STAYS the active store (D2/D4 — zero read-path change); `fact_vectors` holds only non-active spaces; promote
  is an O(N) copy-swap (D6). Eliminates the finding entirely.
- [HIGH] different-dim unsatisfiable → **Scoped #623 to same-dim** (D6 same-dim guard; T3
  `promote_rejects_different_dim` gate); different-dim is a forward-compatible PR2 follow-up (Deferred;
  `PromoteOutcome` carries the new fingerprint so PR2 relaxes the guard with no API break). User chose the
  2-PR split.
- [HIGH] completeness gate TOCTOU → **D6 now runs the COUNT gate INSIDE the promote tx**; the catch-up stays
  pre-tx (off-lock embed). T3 gate renamed `promote_catches_up_stragglers_before_tx`.
- [LOW] gate name / FK 3-step / D2 sentence → folded into D4/D6/T3.
- Confirmed-correct items → retained as-is; the atomicity + backfill + async design is unchanged.
