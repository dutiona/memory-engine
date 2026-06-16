# Implementation Plan — MCP Cognitive Endpoints (#225)

**Issue:** dutiona/memory-engine#225 — `feat(mcp): cognitive pipeline endpoints (dream_cycle, get_recent_insights)`
**Worktree:** `.worktrees/feat-225-mcp-cognitive-endpoints` (branch off `origin/main`; includes merged #49 DreamCycle + #595 perf)
**Scope:** FIXED by user decisions 2026-06-16 (D1/D2/D3 below). Exposes the Phase-5a cognitive pipeline (shipped in #49) as MCP tools. Prereqs #49/#48/#55/#63 all closed.

> **Synthesis note.** Three-lens judge panel (drafts in `./2026-06-16-mcp-cognitive-endpoints-drafts/`). Spine = architecture-first (reusable core method, shared marker const, error-mapping fix); + risk-first sequencing (error-map + marker-predicate first; read-only/empty-store/scope tests); + mvp-first independently-testable tools.

## Fixed decisions

- **D1** — one `memory_dream_cycle` tool, `apply: bool` (default true): produce via `run_dream_cycle`, and if `apply`, `apply_cycle_report`. `apply=false` dry-runs (returns unapplied report). Plus a separate `memory_apply_cycle_report` tool (the gated path).
- **D2** — `dream_cycle` does **not** consolidate. Consolidation stays the existing `memory_consolidate` tool. `DefaultDreamCycle` (#49) is pure: cluster/promote/rescore/quarantine.
- **D3** — insights are stored as facts; `memory_flush_insights` will stamp an insight marker, and `memory_get_recent_insights` returns marked facts by project scope, newest-first, limited.

## Grounding — verified facts (line refs at plan time)

- **Core API from #49 (pub, re-exported from crate root):** `MemoryEngine::run_dream_cycle(&dyn DreamCycle) -> Result<CycleReport>` (`engine/cognitive.rs:189`, verifies write access → `ReadOnly`); `MemoryEngine::apply_cycle_report(&CycleReport) -> Result<ApplyResult>` (`engine/cycle/apply.rs:67`, validate-then-apply, hard-fail); `DefaultDreamCycle::with_defaults()` (`engine/cycle/default_impl.rs`, pure, guards empty buckets `< MIN_PTS=3`); `CycleReport`/`ApplyResult` derive `Serialize+Deserialize`, **not** `#[non_exhaustive]` (`engine/cycle/report.rs:129-152`); round-trip proven (`report.rs` tests).
- **MCP tool pattern (`memory-engine-mcp/src/tools/mod.rs`):** `all_tool_definitions() -> Vec<Tool>` with inline `tool_def(name, desc, json!({...}))` (~:30-370); `dispatch()` match (~:377-419) threads `engine, embedder, summary_gen, embed_dim, filter_config`; handlers are `fn handle_*(args, engine, ...) -> Result<CallToolResult, ErrorData>` using `get_str/get_bool/get_usize/get_depth` + `ok_json`. `server.rs` auto-picks new tools (no change). Tool naming: `memory_*` (house convention, all 23 tools).
- **`handle_flush_insights` (~:838-939) — DISCREPANCY:** the issue says it stamps `{"insight":true}`; it actually stamps only `metadata.source = "pre_compaction_flush"` (~:896-899). So there is **no** insight marker to read today — this plan adds it.
- **Error mapping (`memory-engine-mcp/src/error.rs:5-65`):** `MemoryError::Cycle(CycleError)` (`error.rs:431`) is **unmapped** → falls to `other => internal_error`. `ReadOnly → invalid_request`, `Conflict → invalid_params`, `EmbeddingDimension → invalid_params` already exist.
- **Scope resolution:** read-only `ScopeTree::resolve_path(path) -> Option<id>` (`scope/tree.rs:63`) + `subtree(id)` (inclusive BFS, `:107`). Do **NOT** use the private `resolve_scope_ids` (returns ancestors) or the write-requiring `ensure_scope_path`.
- **Recency query precedent:** `FactStore::list_by_scopes_recent` (`store/facts.rs:901`) — active + scope `json_each` filter + `t_created DESC` + limit (`t_created` indexed since schema v11). The new method = this + a metadata-marker predicate, via the trusted-literal `extra_conditions` idiom (`facts.rs:947-994`).
- **`--all-features` is load-bearing:** `apply_cycle_report` has `#[cfg(feature="ann")]` HNSW branches.

## Decisions flagged for review

| #   | Decision                                                                                                                                                                                                                             | Rationale                                                                                                                                                                                                              | Alt                                                                                                              |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| A   | New **generic** store method `FactStore::list_active_by_metadata_key_recent(scope_ids, marker_key, limit)` + **named** engine method `MemoryEngine::list_recent_insights(scope_path, limit)` (fixes `marker_key=INSIGHT_MARKER_KEY`) | Reuse (quarantine/other markers can read via same path) below; clear intent above. Mirrors `list_due`/`FactStore::list_due` split                                                                                      | insight-specific store method (3rd near-dup of list_by_scopes_recent)                                            |
| B   | Insight marker = structured object `{"insight": {"flushed_at": <rfc3339>}}` keyed by a shared `pub const INSIGHT_MARKER_KEY: &str = "insight"` in `engine/cognitive.rs`, re-exported from `lib.rs`, imported by the MCP writer       | Single source of truth (writer in MCP, reader in core can't drift); object mirrors the quarantine-marker shape; `flushed_at` is a future query handle; read predicate `IS NOT NULL` matches object or bool identically | bare `{"insight":true}` (throws away flush time); const in MCP (reader is in core — must live in the shared dep) |
| C   | `dream_cycle`/`apply_cycle_report` serialize reports via `serde_json::to_value` + one shared `ok_serialized` helper                                                                                                                  | `CycleReport`/`ApplyResult` derive clean serde; `#[non_exhaustive]` fields flow through automatically (no adapter edit when #57/#578 extend)                                                                           | bespoke wrapper struct (must edit on every core change)                                                          |
| D   | `CycleError → invalid_params` arm in `to_mcp_error`                                                                                                                                                                                  | a client-supplied bad report is a **client** error, not a server fault; sits with `Conflict`                                                                                                                           | leave unmapped (mis-tiers as INTERNAL_ERROR)                                                                     |
| E   | Unknown `project_path` → **empty vec** (not error)                                                                                                                                                                                   | list/query convention; project scope nodes are created lazily, so a new project legitimately has no node yet                                                                                                           | `NotFound` (catches client typos but errors on not-yet-created projects) — **flag for reviewer**                 |
| F   | Scope semantics = **subtree** (resolve_path + subtree, inclusive)                                                                                                                                                                    | a "project" is a node; insights may be filed at it or under it; matches `ScopeQuery::Subtree`                                                                                                                          | exact-only (misses nested); ancestors (leaks parent project)                                                     |

## Implementation Tasks

> TDD: failing test first. Sequenced risk-first — the error-map arm + the marker predicate (the two non-obvious correctness points) land before the tool surface that depends on them.

- [ ] **T0 — Baseline.** On `feat-225-mcp-cognitive-endpoints` at latest `origin/main`. Capture green `cargo build --workspace && cargo test --workspace`.
- [ ] **T1 — `CycleError → invalid_params` mapping (risk-first: do early).** Add `MemoryError::Cycle(e) => ErrorData::invalid_params(format!("cycle error: {e}"), None)` to `to_mcp_error` (`memory-engine-mcp/src/error.rs`), next to the `Conflict` arm. Unit test `cycle_maps_to_invalid_params` (mirror existing mapping tests). **Note (review):** this tiers ALL `MemoryError::Cycle` as a client error. It is exactly right for `apply_cycle_report` (the report is client-supplied); `run_dream_cycle` could in principle return `Cycle` from a buggy consumer cycle, but the shipped `DefaultDreamCycle` never does (only `NotFound`/`Database`), so the common-case tiering is correct. Acceptable.
- [ ] **T2 — Insight marker const.** `pub const INSIGHT_MARKER_KEY: &str = "insight";` in `src/engine/cognitive.rs` (doc: single source of truth, writer+reader). Re-export from `src/lib.rs` — the existing line is the **unbraced** `pub use engine::cognitive::DreamContext;`, so convert it to a braced group `pub use engine::cognitive::{DreamContext, INSIGHT_MARKER_KEY};` (or add a second `pub use`).
- [ ] **T3 — Generic store query (TDD). [BLOCKER fix applied].** Add `FactStore::list_active_by_metadata_key_recent(&self, scope_ids: &[i64], marker_key: &str, limit: usize) -> Result<Vec<Fact>>` in `src/store/facts.rs` as a **standalone query mirroring `list_by_scopes_recent`** (real `params![scope_json, limit_i64]`, `FACT_COLUMNS` via `format!`, `t_created DESC`). Do **NOT** route through `list_active_in_period_inner`'s `extra_conditions` — that idiom is documented non-parameterized (appended verbatim) and cannot carry an extra bind param, so `'$.' || ?key` would collide with the outer query's `?N`. Build the marker predicate as a **trusted-literal** `format!("json_type(metadata, '$.{marker_key}') IS NOT NULL", …)` — `json_type` is the established codebase idiom (#595's `list_undreamt`) and robustly treats absent-key and present-`null` as excluded, present-non-null (our `{flushed_at:…}` object) as included. Rustdoc MUST state: `marker_key` is a **trusted engine const, never client input** (it is interpolated into SQL); contract = "key present with a non-null value". Store-level unit test FIRST: seed (a) 2 marked active in-scope at distinct `t_created`, (b) 1 marked expired, (c) 1 marked out-of-scope, (d) 1 active in-scope unmarked, (e) 1 with `{"insight": null}` → only (a) returns, newest-first; `limit=1` returns newest only; (e) excluded.
- [ ] **T4 — Engine method.** `MemoryEngine::list_recent_insights(&self, scope_path: &str, limit: usize) -> Result<Vec<Fact>>` in `src/engine/inspect.rs`. **[HIGH fix]** Resolve scope with an explicit early-return — `subtree(unknown_id)` returns the singleton `[id]`, NOT empty, so you must branch on `resolve_path` returning `None`:
  ```rust
  let scope_ids = {
      let tree = self.scope_tree.read();
      match tree.resolve_path(scope_path) {
          Some(id) => tree.subtree(id),   // inclusive subtree
          None => return Ok(Vec::new()),  // unknown project → empty (decision E)
      }
  };
  self.with_read(|conn| FactStore::new(conn, self.embed_dim)
      .list_active_by_metadata_key_recent(&scope_ids, INSIGHT_MARKER_KEY, limit))
  ```
  Do NOT call `subtree` on a fallback id.
- [ ] **T5 — Engine integration test (TDD).** NEW `tests/recent_insights_test.rs` (public-API, mirror `dream_cycle_test.rs`): nested scopes `project:p` + `project:p/sub`; add facts with/without the `insight` marker at both levels + an expired marked fact; assert subtree scoping + newest-first + limit + expired-excluded + unknown-scope-empty.
- [ ] **T6 — Shared serialization helper.** `ok_serialized<T: Serialize>(&T)` next to `ok_json` in `tools/mod.rs` (serde failure → `internal_error`, since the value is engine-produced).
- [ ] **T7 — Correct `handle_flush_insights` to stamp the marker.** At `tools/mod.rs:~896-899`, additionally `m.insert(INSIGHT_MARKER_KEY.to_owned(), json!({"flushed_at": Utc::now().to_rfc3339()}))` (keep `source`). `use memory_engine::INSIGHT_MARKER_KEY;`. **[HIGH fix]** The existing code only stamps inside `if let Value::Object(ref mut m) = metadata`, so a client passing non-object `metadata` (e.g. `"foo"`, `42`) silently gets neither `source` nor the (now load-bearing) insight marker — the fact becomes invisible to `get_recent_insights`. Normalize first, mirroring `promote_in_conn` (cognitive.rs:~291): `let mut metadata = match obj.get("metadata").cloned().unwrap_or(json!({})) { Value::Object(m) => Value::Object(m), _ => json!({}) };` so the marker always lands. Add a test case for non-object metadata input. Verify existing flush tests assert on ids/`source`/counts, not metadata-equality (additive change).
- [ ] **T8 — `memory_dream_cycle`.** `tool_def` schema (`apply: bool` default true) + dispatch arm + `handle_dream_cycle(args, engine)`: `DefaultDreamCycle::with_defaults()` → `run_dream_cycle`; if `get_bool("apply").unwrap_or(true)` → `apply_cycle_report`. Output `{report, applied?, did_apply}`. No embedder. No consolidation.
- [ ] **T9 — `memory_apply_cycle_report`.** `tool_def` schema (`report` object, required) + dispatch arm + handler: `serde_json::from_value::<CycleReport>` (failure → `invalid_params`) → `apply_cycle_report` → `ok_serialized(&applied)`.
- [ ] **T10 — `memory_get_recent_insights`.** `tool_def` schema (`project_path` required, `limit` default 20, `depth` enum) + dispatch arm + handler: `list_recent_insights` → `depth::shape_fact` → `{insights, count}` envelope (mirror `handle_list_due`).
- [ ] **T11 — MCP integration tests.** NEW `memory-engine-mcp/tests/cognitive_tools.rs` (dispatch directly, mirror `tools.rs`/`tool_roundtrip.rs`): see Testing.
- [ ] **T12 — Docs** (see Documentation).
- [ ] **T13 — Full workspace + all-features verification gate** (see Verification).
- [ ] **T14 — Git ops:** plan issue, commit, PR, `/super-review`, squash-merge.

## Documentation

- `docs/reference/mcp-server.md` — add the 3 tools to the tool table, bump the tool count, document the `apply` flag + the `dream_cycle(apply=false)`→`apply_cycle_report` round-trip, and that `memory_flush_insights` now stamps an `insight` marker consumed by `memory_get_recent_insights`.
- `docs/advanced/dream-cycle.md` — add an "Exposed via MCP" subsection (note consolidation is NOT bundled; stays `memory_consolidate`).
- Rustdoc on `INSIGHT_MARKER_KEY`, `list_recent_insights`, `list_active_by_metadata_key_recent` (Errors sections; single-source-of-truth note on the const).
- Root `CLAUDE.md` Status line if it enumerates MCP tool count.

## Testing

- **Core unit (T3):** the store-query matrix above (active/expired/in-scope/out-of-scope/marked/unmarked + newest-first + limit).
- **Core integration (T5):** `recent_insights_test.rs` — subtree scoping, ordering, limit, expired-excluded, unknown-scope-empty.
- **MCP error-map unit (T1):** `MemoryError::Cycle(CycleError::UnknownFact(7)) → INVALID_PARAMS`.
- **MCP integration (T11):** (a) `dream_cycle apply=true` → `did_apply==true`, `report.deltas` non-empty, `applied.promoted==1` (seed a 3-fact identical-embedding Semantic cluster via the test embedder); (b) `dream_cycle apply=false` → store unchanged, then feed `result.report` into `apply_cycle_report` → `applied.promoted==1` (proves the serde round-trip across the boundary); (c) `apply_cycle_report` with an `AdjustScore` on a non-existent fact → `INVALID_PARAMS` (proves the new mapping end-to-end); (d) malformed `report` JSON → `invalid_params`; (e) `flush_insights(scope=project:p)` → `get_recent_insights(project:p)` returns them newest-first, an unrelated `add_fact` excluded (writer→reader marker proof); (f) `limit` truncates + `depth:sparse` shapes.
- **Regression:** `dream_cycle_test.rs` stays green (core API unchanged); existing `flush_insights` tests stay green after the additive marker.
- N/A: e2e (no live MCP transport harness; dispatch-level integration is the established MCP test layer).

## Verification

```bash
cd .worktrees/feat-225-mcp-cognitive-endpoints
cargo build --workspace && cargo test --workspace
cargo clippy --workspace --all-targets        # all=deny; pedantic+nursery=warn — keep new code clean
cargo fmt --check
cargo test --all-features                      # exercises apply_cycle_report's ann HNSW branch
cargo test -p memory-engine-mcp                # new cognitive_tools.rs + existing tool tests
cargo doc --no-deps                            # rustdoc on new public items
```

Acceptance: all green; no clippy findings in new code; new MCP tools dispatch end-to-end; `flush → get_recent_insights` round-trips.

## Git ops

- Worktree exists. **Plan issue:** `type:plan` + `area:mcp`, link #225 + this draft; link under #221/#554 via `addSubIssue`. Keep auto-close verbs (`Closes #225`) out of intermediate commits — only in the final PR body.
- Atomic Conventional Commits (no co-author): `feat(mcp): map CycleError to invalid_params`; `feat(core): INSIGHT_MARKER_KEY + list_recent_insights`; `feat(mcp): expose dream_cycle/apply_cycle_report/get_recent_insights (#225)`; `docs(mcp): document cognitive tools`.
- PR against `main`; `/super-review` (expect gemini bot); rebase + re-gate before merge (CLEAN ≠ semantically safe); squash-merge; delete branch; remove worktree.

## Risks / watch-items

- **Marker drift** — structurally prevented by the shared `INSIGHT_MARKER_KEY`; the flush→get_recent_insights test fails loudly if they diverge.
- **`flush_insights` test regression** — additive metadata; confirm no test asserts metadata-equality (T7).
- **CycleError mis-tier** — closed by T1 + its test.
- **`#[non_exhaustive]` serde passthrough** — `to_value` means #57/#578 field/variant additions flow to the wire automatically (no adapter edit).
- **Unknown-scope semantics (decision E)** — empty-vec chosen; reviewer may prefer NotFound.
- **`unsafe_code = forbid`** — satisfied (pure Rust, no FFI).

## Post-Implementation Audit

Per-task disposition (T0–T14). Plan size: 15 tasks. Modified: 2 (T3, T12). Added work: 1 (extra roundtrip test). Divergence ≈ 13% — below the 30% advisor-arbitration threshold.

| Task | Status | Notes |
| ---- | ------ | ----- |
| T0 — Baseline | Implemented | Worktree off latest `origin/main` (incl. merged #49 + #595); workspace green. |
| T1 — `CycleError → invalid_params` | Implemented | Mapping + `cycle_maps_to_invalid_params` test, exactly as planned. |
| T2 — `INSIGHT_MARKER_KEY` const | Implemented | Const in `cognitive.rs`; `lib.rs` re-export converted to braced group. |
| **T3 — Generic store query** | **Modified** | **The plan prescribed `json_type(metadata, '$.{key}') IS NOT NULL`; the shipped code uses `json_extract(...) IS NOT NULL`.** The store-level unit test (written first) caught the planned predicate as *wrong* for the present-`null` case: SQLite `json_type` returns the text `'null'` (not SQL `NULL`) for `{"insight": null}`, so that fact was incorrectly included (`[5,2,1]` vs expected `[2,1]`). `json_extract` collapses both absent-key AND present-`null` to SQL `NULL`, returning a value only for present-non-null — the correct "key present with a non-null value" contract. This **reverses** the plan/reviewer suggestion, which had conflated #595's *absent-key* case (where `json_type` is right) with this *non-null-value* case. Trusted-literal interpolation + rustdoc contract retained as planned. |
| T4 — Engine method | Implemented | `list_recent_insights` with explicit `resolve_path → None ⇒ Ok(vec![])` early-return (the HIGH fix). |
| T5 — Engine integration test | Implemented | `tests/recent_insights_test.rs`, public-API, subtree/newest-first/limit/expired/unknown-scope. |
| T6 — `ok_serialized` helper | Implemented | Next to `ok_json`; serde failure → `internal_error`. |
| T7 — `flush_insights` stamps marker | Implemented | Non-object metadata normalized to `{}` before stamping `source` + marker (the HIGH fix); marker always lands. |
| T8 — `memory_dream_cycle` | Implemented | `apply` flag (default true); `{report, did_apply, applied?}`; no consolidation, no embedder. |
| T9 — `memory_apply_cycle_report` | Implemented | `from_value::<CycleReport>` (fail → `invalid_params`) → apply → `ok_serialized`. |
| T10 — `memory_get_recent_insights` | Implemented | `project_path` required, `limit` default 20, tiered depth; unknown scope → empty. |
| T11 — MCP integration tests | Implemented + **Added** | `cognitive_tools.rs` (5 dispatch tests) as planned. **Added** a wiremock writer→reader roundtrip in `embedding_integration.rs` — it surfaced a *test-fidelity* bug (the OpenAI mock omitted the `index` field that the `embed_batch` parser requires; `flush_insights` uses the batch path). Fixed the mock; product code was correct. |
| **T12 — Docs** | **Modified** | The `mcp-server.md` tool table was already **stale** (claimed 18 tools; code had 23 pre-#225 — 3 P2 + 2 Phase-5a outcome tools were undocumented). Rather than ship a half-correct "21", the table was completed to the true **26** (incl. the 5 previously-missing rows + the 3 new cognitive tools) and the headline count corrected. Also added a "Cognitive Pipeline" subsection, an "Exposed via MCP" note in `dream-cycle.md`, and the `CLAUDE.md` status line. **Collateral:** the count-assertion test `all_tool_definitions_returns_23` → `_returns_26` was updated (would otherwise fail the workspace gate); the doc-drift fix lands in this PR, no separate issue needed. |
| T13 — Verification gate | Implemented | `cargo build/test --workspace` green; MCP crate 125 passed; `cargo test --all-features` 924 passed / 4 ignored; `cargo clippy --workspace --all-targets` exit 0; `cargo fmt --check` clean. |
| T14 — Git ops | In progress | Commit → PR → `/super-review` → rebase+re-gate → squash-merge (pending user authorization). |

**Decision E (unknown-scope semantics):** shipped as empty-vec (not `NotFound`), per the fixed decision. Flagged for reviewer preference.

### Review Round 1 (3 adversarial subagent reviewers + gemini bot)

Reviewers: correctness/SQL (rust-specialist), security/boundary (code-reviewer), tests/contract (test-engineer). Findings actioned in commit `fix(...)` on this branch:

- **[HIGH] `memory_apply_cycle_report` validation bypass** (security lens): a client-supplied `CycleReport` with an `AddFact` delta skipped the `validate_importance` / `check_str_size` / `check_json_size` guards the trusted `add_fact` path enforces — letting a hostile report write out-of-range `importance` (no column CHECK → poisons Ebbinghaus decay) or oversized payloads. **Fixed** in `validate_report` (engine, shared by all DreamCycle consumers — correct altitude), with two engine tests (`add_fact_out_of_range_importance_rejected`, `add_fact_oversized_content_rejected`). Required widening `MemoryEngine::validate_importance` to `pub(crate)`.
- **[HIGH] `limit=0` slipped through** (tests lens): schema declares `minimum:1` but the handler accepted `limit=0` → silent empty result. **Fixed** with an explicit guard in `handle_get_recent_insights` + test `get_recent_insights_limit_zero_is_invalid_params`.
- **[HIGH] empty-store `dream_cycle` untested** + MEDIUM gaps: added `dream_cycle_on_empty_store_succeeds_with_no_deltas`, `apply_cycle_report_missing_report_key_is_invalid_params`, `get_recent_insights_sparse_depth_shapes_facts`, and a 2-insight batch writer→reader roundtrip (`flush_two_insights_then_get_recent_returns_both`).
- **[MEDIUM] json_extract/json_type asymmetry**: added a docstring cross-reference to the complementary `list_undreamt_in_period` (presence vs absence predicate).
- **Dispositioned, no change:** `pub` (not `pub(crate)`) on `list_active_by_metadata_key_recent` — kept to match its sibling `list_by_scopes_recent`; `FactStore` is crate-internal so it is effectively `pub(crate)` at the boundary, and a `debug_assert` + rustdoc contract guard the trusted-literal interpolation. CycleError mis-tier for a hypothetical buggy custom `DreamCycle` — documented/benign with the shipped `DefaultDreamCycle`.

Gate after fixes: workspace 1110 passed / 4 ignored; `--all-features` 926 passed / 4 ignored; clippy exit 0; fmt clean.
