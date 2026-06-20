# Review — #630 plan — diverse-lens internal subagent panel

**Review roster (per user override of super-plan Step 3/4):** three clean-slate internal subagents, one per lens, replacing external Codex/agy AND the unavailable `advisor()`. Each read the plan + the real code from disk with no conversation context.

- Async / runtime-correctness lens → `2026-06-20-sqlite-backend-630-review-async.md`
- Refactor-safety / behavior-preservation lens → `2026-06-20-sqlite-backend-630-review-refactor.md`
- Architecture / scope / API-seam lens → `2026-06-20-sqlite-backend-630-review-arch.md`

## Findings (consolidated, by severity)

### BLOCKER

- **B-async — `std::sync::mpsc::recv()` inside an `async fn` parks a tokio executor thread** (D2 bridge). Drain side must be `tokio::sync::mpsc::channel(1)` + `rx.recv().await`; scan closure must use `tx.blocking_send(row)`. Requires adding `"sync"` to tokio's `async`-feature list in `Cargo.toml`.
- **B1-refactor — HNSW build/dispatch _policy_ lives in the engine** (`search_config` Some + `ann_threshold < usize::MAX` build gate, `active_count() >= ann_threshold` dispatch gate, `engine/mod.rs:264-272,370-378`). D3 (own HNSW in the backend) requires replicating that policy; H9's small-corpus recall test cannot exercise the threshold. Reshapes D3 — escalated to user.

### HIGH

- **H-async early-exit precedence** — on callback `Err`, dropping `rx` makes the scan's next `send` fail; the sketch would return the scan's `SendError`-derived error and discard the real callback error. Capture `cb_err`, `drop(rx)`, return `cb_err` preferentially. H6 test must pin this.
- **H-b-refactor — collateral #1 mis-states the doc drift** — the concrete `require_present` rustdoc _does_ say `Internal` (`embedding_meta.rs:126-137`); the real drift is the **trait** `SchemaManager` error menu (`storage/schema.rs:22-27`) listing `EmbeddingDimension` but omitting `Internal`.
- **F1-arch — "#631 is a four-fields-to-one swap" oversells** — the engine reads `pool`/`graph`/`hnsw` across 20+ sub-modules and composes multi-store transactions. #630 makes the handle _constructible_, not #631 _mechanical_.
- **F2-arch — seam-leakage gate has no teeth** — pin the exact grep command instead of prose.
- **F4-arch — D4 witness can't use the FTS path** — `fts_search`/`fts_count_expired` swallow errors to `Ok(empty)`; the "raw SQL fail ⇒ `Storage(Backend)`" witness must use a propagating method (e.g. `insert_event` constraint violation).

### MEDIUM

- **M-async-1** — assert `UpcasterRegistry: Send + Sync` and `HnswStrategy: Send + Sync` (the `async_trait` futures are `Send`); add `static_assertions` in T1.
- **M-async-2** — `Arc`-wrap `UpcasterRegistry` (cloned into closures).
- **M-a-refactor** — vector tie-ordering is non-deterministic (`select_nth_unstable_by`, `vector.rs:122-135`); parity on tied scores can flake → use distinct scores or set-compare ties.
- **M-b-refactor** — in-memory pool collapses reads onto the write mutex; D2's "never blocks executor" needs that caveat.
- **M-c-refactor** — `schema_version()` parse failure returns `Migration` (`schema/mod.rs:204`); pin in H10.
- **F3-arch** — parity is identity-by-construction below the wrapper (oracle and SUT share the SQL); reframe the tests as catching _wrapper_ drift, not SQL correctness.
- **F5-arch** — cap T9 at one dispatch-call per trait (semantic parity is #632).
- **F6-arch** — state the invariant: never fold `ColdStorage` into the `StorageBackend` supertrait bound (keeps the umbrella vtable feature-invariant).

### LOW

- **F7-arch** — re-rank H9 (HNSW notify is the only net-new write logic) — moot if D3 defers.
- **F8-arch** — §9 mislabels: items 3 (note on #631) and 4 (existing #652) are not new issues.
- Remove the empty reserved §6.

### Confirmed correct (no change needed)

D4 remap safe (only `Database` matched; no semantic condition emits raw `Database`); `f32→f64` value-exact + order-safe; `Some(&[])`=matches-nothing round-trips through `convert.rs`; `marker_key` guard, `ReadOnly`-via-`try_write`, engine-untouchable, default-build-runtime-free, archive oldest-first, epoch gate; `ColdStorage` `cfg(all(async,archive))` off the umbrella is correct; T9 is in-scope (not gold-plating); delegation-not-absorption is the right epic call.

## Resolution

- **B-async** → ADOPTED. D2 rewritten to `tokio::sync::mpsc` (`blocking_send`/`recv().await`); `Cargo.toml` `async` feature gains `"sync"`.
- **B1-refactor (D3)** → ESCALATED TO USER as a feature-altitude decision (own HNSW now + replicate policy, vs defer brute-force-only to #631). Plan finalized per the user's choice; H9 + T8 + struct fields adjusted accordingly.
- **H-async early-exit / H-c** → ADOPTED. `for_each_streamed` returns `cb_err` preferentially; H6 test pins both callback-error and mid-scan-SQL-error precedence.
- **H-b-refactor** → ADOPTED. Collateral #1 reworded to target the trait error-menu omission of `Internal`.
- **F1-arch** → ADOPTED. §2 reworded: #630 makes the handle constructible; #631 is non-trivial (20+ call sites + transactions).
- **F2-arch** → ADOPTED. Pinned grep added to §7.
- **F4-arch** → ADOPTED. D4 witness uses a propagating write method, not an FTS swallow.
- **M-async-1/2, M-a/b/c-refactor, F3/F5/F6/F7/F8-arch, empty §6** → ADOPTED into the relevant plan sections.
- All "confirmed correct" items → no change.

No findings were dismissed. The one substantive disagreement among the drafts (D4) was resolved against primary source (`error.rs:345-368`) and independently confirmed by the refactor lens.
