# PR #682 (#630 `SqliteBackend`) — Adversarial Review: SECURITY / ERROR-FIDELITY / SCOPE-DISCIPLINE

**Range reviewed:** `git diff 24f9d65..HEAD` (7 commits, 20 files, +4247/−18).
**Reviewer lens:** security, error fidelity, scope discipline. No prior context.
**Verdict: APPROVE.** Zero BLOCKER / HIGH / MEDIUM. Two LOW (doc-only). The "zero behavior change" claim holds for the security/error/scope surface.

Build/test/clippy gate (run, not asserted):

- `cargo build --all-features` → exit 0.
- `cargo test --all-features --lib storage::sqlite` → **72 passed, 0 failed**.
- `cargo clippy --all-features --all-targets` → clean (no sqlite warnings).

---

## Point-by-point

### 1. D4 error remap (`map_seam_err`, `src/storage/sqlite/mod.rs:94-101`) — SOUND

`map_seam_err` matches **only** `Err(MemoryError::Database(e))` → `Storage(StorageError::Backend(e.to_string()))`; everything else (`other => other`) passes through verbatim. This matches the `StorageError` contract doc (`src/error.rs:345-368`): the seam opacifies driver failures with no precise home; causes that _do_ have a home (`NotFound`, `Migration`, `Pool`, `Serialization`, `Conflict`, `ReadOnly`, `Lineage`, …) stay typed.

**Not-found integrity (the failure mode the blanket remap could cause):** every read method that can "miss" maps the miss to a _semantic_ variant in the underlying store, so the remap never opacifies a not-found:

| Read seam method                   | Underlying store                   | Not-found maps to     | Source                                                    |
| ---------------------------------- | ---------------------------------- | --------------------- | --------------------------------------------------------- |
| `get_fact`                         | `FactStore::get`                   | `NotFound`            | `src/store/facts.rs` (`None => NotFound("fact {id}")`)    |
| `get_edge`                         | `EdgeStore::get`                   | `NotFound`            | `src/store/edges.rs` (`QueryReturnedNoRows => NotFound`)  |
| `get_scope`                        | `ScopeStore::get`                  | `NotFound`            | `src/store/scopes.rs` (`QueryReturnedNoRows => NotFound`) |
| `get_event` / `get_upcasted_event` | `EventStore::get`                  | `NotFound`            | `src/store/events.rs:154`                                 |
| `get_summary`                      | `SummaryStore::get`                | `NotFound`            | `src/store/summaries.rs:89-90`                            |
| `get_activity`                     | `ActivityStore::get`               | `NotFound`            | `src/store/activities.rs:115-116`                         |
| `get_lineage_by_wisdom_fact`       | `LineageStore::get_by_wisdom_fact` | `Lineage` (semantic)  | `src/store/lineage.rs:126`                                |
| `get_facts`                        | `FactStore::get_many`              | absent key (no error) | HashMap projection                                        |
| `get_checkpoint*`                  | `CheckpointStore::get`             | `Ok(None)` (Option)   | not an error path                                         |

No method returns a RAW `Database` for a semantic "not found". Witness tests at the seam: `mod.rs:238` `block_read_passes_semantic_variant_through` (NotFound survives), `mod.rs:221` `block_read_maps_database_error_to_storage_backend` (Database opacified), `event_log.rs:118` `get_missing_yields_not_found`, `graph.rs:602` `get_fact_missing_yields_not_found`. **Contract honored.**

### 2. `marker_key` injection guard — SOUND

`list_active_facts_by_metadata_key_recent` (`src/storage/sqlite/graph.rs:310-322`) delegates straight to `FactStore::list_active_by_metadata_key_recent` (`src/store/facts.rs:976`) passing `marker_key` through **unchanged**. The guard (`facts.rs:986-996`) runs _inside_ the delegate — it cannot be bypassed by the seam:

```rust
if marker_key.is_empty() || !marker_key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
    return Err(MemoryError::Conflict(ConflictError::QueryValidation(...)));
}
```

`marker_key` is interpolated (not bound) into a `json_extract(metadata, '$.{marker_key}')` path because JSON paths can't be portably parameterized — so the `[A-Za-z0-9_]+` allowlist is the actual defense, and it is a runtime check (not `debug_assert`, so it survives release). `Conflict(QueryValidation)` is a semantic variant ⇒ passes through `map_seam_err` untouched (no opacification). Seam reject-test `graph.rs:754` exercises `["", "in'sight", "$.x", "a b", "a;b"]` — meaningful set covering empty, single-quote SQLi, JSON-path traversal, whitespace, and `;` statement-chaining — all asserted to yield `QueryValidation`. **Guard intact end-to-end.**

### 3. read_only enforcement — SOUND

Every production WRITE trait method routes through `SqliteBackend::block_write` (`mod.rs:127-140`), which acquires via `ConnectionPool::try_write()` → `MemoryError::ReadOnly` on a read-only pool. Verified all 27 production write sites use `block_write` (`grep block_write` across the backend; no production write uses `.write()`).

The 7 raw `.write()` calls flagged by grep are **all below their file's `#[cfg(test)]` line** — test-only seeding helpers, never compiled into the lib:

- `graph.rs:584/660/697/728/780` — below `#[cfg(test)]` at `graph.rs:537` (`fn seeded`, empty-scope tests).
- `consolidation.rs:215` — below `#[cfg(test)]` at `:155` (`seeded_with_facts`).
- `search_index.rs:119` — below `#[cfg(test)]` at `:73` (`seeded`).

Seam witness: `mod.rs:259` `block_write_on_read_only_pool_yields_read_only` opens a real file-backed RO pool and asserts `ReadOnly`. Key Design Decision #6 preserved for free. **No bypass.**

### 4. The 5 `#[cfg(test)]` removals — BODY-NEUTRAL

`git diff 24f9d65..HEAD -- src/store/activities.rs src/store/checkpoints.rs` shows each of the 5 hunks (`activities.rs`: `get`, `list_by_session`, `count_by_session`; `checkpoints.rs`: `get`, `list_recent`) changes **only** the doc comment + the `#[cfg(test)]` line; the hunk context ends at the `pub fn …` signature and **no body line appears in any diff**. Behavior-neutral — the methods become reachable in non-test builds (now called by the `SessionStore` impl), nothing else. (LOW doc nit below.)

### 5. Scope discipline — SOUND

- **Engine untouched:** `git diff 24f9d65..HEAD -- src/engine/` is **empty**.
- **No HNSW-in-backend (D3 deferred to #631):** grep for `hnsw|Hnsw|HNSW|ann::|feature = "ann"` under `src/storage/sqlite/` → **none**. `vector_search` (`search_index.rs:37`) is brute-force via the verbatim `search::vector` free function.
- **No driver leak in the seam:** no `rusqlite::Connection|Error|Row` appears in any `pub`/`pub(crate)`/trait-method signature. The only `rusqlite` references outside private `block_*`/`for_each_streamed` closure types are two `#[cfg(test)]` `MemoryError::Database(...)` constructions (`mod.rs:227,343`). `convert.rs::search_params` is `pub(super)` and port-neutral. `realization.rs` is fully `#[cfg(test)]`.
- **No engine rewire:** `storage/mod.rs` adds only `#[cfg(feature="async")] pub mod sqlite;` + `pub use sqlite::SqliteBackend;` (D1 async-gated). `Cargo.toml` adds only the tokio `sync` feature (for the `for_each_streamed` mpsc channel) — minimal, scoped.

`from_pool(Arc<ConnectionPool>)` (`mod.rs:70`) is a _constructor_, not a trait method, and `ConnectionPool` is memory-engine's own pool abstraction (`src/pool/connection_pool.rs`), not a re-exported `rusqlite` type — so the port boundary is not violated.

### 6. `unsafe_code` — SOUND

No `unsafe` in the diff (`git diff | grep -i unsafe` → none). `unsafe_code = "forbid"` intact at `Cargo.toml:59`.

---

## Findings

### [LOW] Doc imprecision: "promoted to unconditional `pub(crate)`" — `src/store/activities.rs` / `src/store/checkpoints.rs`

The 5 replacement doc comments say _"Promoted to unconditional `pub(crate)`"_, but the signatures are and remain `pub fn` (they were `pub fn` under `#[cfg(test)]` too). Only the `#[cfg(test)]` gate was removed; the visibility keyword did not change. Harmless, but the comment misdescribes its own edit.
**Fix:** reword to "promoted to unconditional (was `#[cfg(test)]`)" — drop the inaccurate `pub(crate)` claim, or change the keyword to actually match if `pub(crate)` was intended.

### [LOW] `stream_consumer_dropped` internal error is structurally unreachable as a surfaced value — `src/storage/sqlite/mod.rs:189`

`stream_consumer_dropped()` builds a `MemoryError::Internal` that, per the `for_each_streamed` contract (`mod.rs:182`, `cb_err.map_or(scan_res, Err)`), can never be the returned error — the callback error always wins. This is intentional and documented (`mod.rs:186-188`), and the value does stop the scan, so it is correct. Flagged only as a readability note: a future refactor that reorders the "callback error wins" precedence could silently start surfacing this string to callers. The existing test `for_each_streamed_callback_error_wins_and_stops_early` (`mod.rs:304`) pins the precedence, which mitigates this — **no action required**, recorded for completeness.

---

## What's sound (summary)

D4 remap + not-found integrity; `marker_key` allowlist non-bypassable and `Conflict`-typed; read_only via `try_write` on every production write (0 bypasses); the 5 cfg removals body-neutral; engine byte-identical; no HNSW, no driver leak, no engine rewire; `unsafe_code = forbid` intact; 72 backend tests green, clippy clean. **APPROVE.**
