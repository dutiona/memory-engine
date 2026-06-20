# Review — #631 plan — diverse-lens internal subagent panel

**Roster (per user override of super-plan Step 3/4):** three clean-slate internal subagents (staging-feasibility / correctness-atomicity / async-runtime), replacing external Codex/agy and the unavailable `advisor()`. Each read the plan + real code with no conversation context.

- Staging/feasibility → `2026-06-21-engine-storage-backend-631-review-staging.md`
- Correctness/behavior-preservation → `2026-06-21-engine-storage-backend-631-review-correctness.md`
- Async/runtime/API → `2026-06-21-engine-storage-backend-631-review-async.md`

## Verdicts: all APPROVE-with-changes. Core thesis CONFIRMED SOUND.

The irreducible-big-bang claim survived adversarial probing (3 incremental paths ruled out: dual-field, per-method block_on, sync-facade). A–D are genuinely additive + green under `-D warnings`. `AsyncMemoryEngine` deletion is clean (only `lib.rs` + docs). Object-safety of the 5 atomic methods is automatic under `#[async_trait]`. The `apply_cycle_deltas` single-connection deadlock + full-push-down is correct. RRF + temporal filter correctly stay engine-side.

## Findings & Resolution

### BLOCKER

- **Atomic methods are not pure verbatim moves** (ingest touches in-memory `scope_tree` mid-tx) → **ADOPTED**: §3 now splits — DB work below the seam returns `scope_ids_to_cache`; the engine applies `scope_tree.write()` above the seam. Return types tabled.
- **`parking_lot::RwLock` guard across `.await` ⇒ `!Send`** → **ADOPTED**: §6 finding 4 + an explicit Stage-E audit bullet (extract value, drop guard, then `.await`).

### HIGH

- **`DreamCycle::run` sync-contract reaches async backend** → **ADOPTED**: §6 finding 1; make it `#[async_trait]` in new **Stage A2**.
- **`reqwest::blocking` panics under `#[tokio::main]`** (embed crate HTTP providers; CLI+MCP) → **ADOPTED**: §6 finding 2; switch to async `reqwest` in **Stage A2**; added a verification grep.
- **3 un-tabled `archive.rs` pool accesses incl. `pool.path()`** → **ADOPTED**: §6 finding 5; candidate-select becomes a port read, `archive_dir` path from `EngineConfig`/backend accessor; folded into Stage A.
- **`write_snapshot` fingerprint read incompatible with `Drop`** → **ADOPTED**: §6 finding 3; `Drop` writes nothing (warns), `close()` owns the full snapshot; documented behavior change.
- **CLI missing `tokio` in `[dependencies]`** → **ADOPTED**: Stage D.

### MEDIUM

- **`validate_report` also pushes below the seam** → **ADOPTED**: §3 + §6 finding 6.
- **f32→f64 vector widening at the backend boundary; value-parity oracle** → **ADOPTED**: §5 + Stage C.
- **`search_config==None`→never-HNSW boundary** → **ADOPTED**: §4 + Stage B test.
- **`close()` belongs on `MemoryEngine`** → **ADOPTED**: §2 signature.
- **Figure corrections** (85 `pub fn` not 14; ~67 direct sites/~150 await points; ~16-18 test helpers not 50-80; ~245 tests accurate) → **ADOPTED** throughout §1/§7.

### LOW

- **Grep to bound helper count before Stage E** → folded into Stage E ("re-point the ~16-18 … helpers"); a grep is implied by the cutover gate.
- **Stage B/C green only because parity tests consume them** → **ADOPTED**: §7 prep-stages note.

## Resolution

All BLOCKER/HIGH/MEDIUM findings adopted into the revised plan (rewrite, not patch). No findings dismissed. Net structural change: added **Stage A2** (async-ready the consumer-trait surface — `DreamCycle::run` async + embed async HTTP) and refined Fork B (scope-ids-return split) — both surfaced by the review as async-contract leaks the front-loading strategy exists to catch. The core staging thesis was confirmed by all three lenses and is unchanged.
