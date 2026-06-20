# Async/Runtime/Public-API Review — #631 engine-storage-backend plan

Reviewer lens: async mechanics, runtime topology, object-safety, feature-flag
cascade, `AsyncMemoryEngine` blast-radius, `Send`-across-await.
Files read: the plan (`2026-06-21-engine-storage-backend-631.md`), source in
`src/engine/mod.rs`, `src/storage/backend.rs`, `src/storage/graph.rs`,
`src/storage/sqlite/mod.rs`, `src/async_engine.rs`, `src/lib.rs`,
`memory-engine-cli/src/{main,db}.rs`, `memory-engine-cli/src/commands/{consolidate,batch_ingest}.rs`,
`memory-engine-mcp/src/{main,server}.rs`, root/cli/mcp/embed `Cargo.toml`.

---

## 1. CLI runtime introduction [HIGH]

**Current state.** `memory-engine-cli/src/main.rs` has a plain `fn main() ->
ExitCode`, no `#[tokio::main]`, no runtime. Each command handler is a sync
`fn run(…)`. The CLI Cargo.toml has tokio only in `[dev-dependencies]`, not in
`[dependencies]`.

**Plan's claim.** "CLI: add a tokio runtime (`#[tokio::main]`/`block_on` at the
command boundary) + `.await`". The design synthesis says "CLI pays a one-time
tokio cost"; it sketches `block_on` as an alternative to `#[tokio::main]`.

**Findings.**

a) **No nested-runtime risk if done correctly.** The CLI currently has zero
runtimes — there is no existing `block_on` or `Runtime::new()` anywhere in
`src/` or the command handlers. Converting `fn main()` to `#[tokio::main]` (or
a single `Runtime::new().block_on(…)` in `fn main`) is safe; all command
handlers then run inside that single runtime. No nested runtimes arise.

b) **`block_on` at command boundary vs `#[tokio::main]`: both are correct, but
pick one consistently.** The plan uses both phrases. `#[tokio::main]` is
cleaner (avoids manually constructing `Runtime`) and is idiomatic for a
binary. The concern that it is "viral" does not apply to a binary's `main`.
The plan should commit to `#[tokio::main]` for the CLI entry point.

c) **`consolidate` and `batch_ingest` use `reqwest::blocking` internally (via
`memory-engine-embed` / `HttpDeltaProposer` / `HttpEmbeddingProvider`, which
use `reqwest = { features = ["blocking"] }`).** `reqwest::blocking` spins its
own internal single-threaded runtime. From inside a `#[tokio::main]`
multi-thread runtime, calling `reqwest::blocking` panics with "Cannot start a
runtime from within a runtime" unless the call is wrapped in
`tokio::task::spawn_blocking`. The `consolidate` and `batch_ingest` commands
both reach `reqwest::blocking` through `HttpDeltaProposer` /
`HttpEmbeddingProvider` (the embed crate uses `reqwest` blocking features —
confirmed in `memory-engine-embed/Cargo.toml`). **After Stage E, if the CLI
main is `#[tokio::main]` and these commands call `engine.add_fact().await`
which internally calls a sync embedder that uses `reqwest::blocking`, this
panics at runtime unless those blocking calls are wrapped in
`spawn_blocking`.** Note: in the post-#631 world, the engine methods will be
async and the _engine itself_ will call `storage.*().await` (which uses
`spawn_blocking` inside `SqliteBackend`), but the _consumer-provided_
`EmbeddingProvider` is injected by the CLI command handler and called _outside_
the storage path — the CLI code calls `embedder.embed(…)` synchronously today
and will still need to call it synchronously after the cutover (the trait is
sync). Those sync embed calls (which use `reqwest::blocking`) will panic if
called on a tokio thread. The plan does not address this. **The fix is for
CLI command handlers that embed to either (a) stay sync and call the async
engine via `handle.block_on(…)` from a thread spawned outside the runtime, or
(b) wrap the `reqwest::blocking` embed call in
`tokio::task::spawn_blocking(|| embedder.embed(…))` inside the async handler.**

**Verdict: [HIGH] — not a plan blocker (the `#[tokio::main]` choice is
correct), but the `reqwest::blocking`-from-async-context panic is a real
runtime failure that will hit `consolidate --backend llm` and `batch-ingest`
immediately on Stage E. Must be addressed explicitly in the cutover plan.**

---

## 2. MCP spawn_blocking removal [MEDIUM]

**Current state.** `memory-engine-mcp/src/server.rs:102` wraps every tool call
in `tokio::task::spawn_blocking` with the comment "Engine operations are sync
(SQLite) — must not run on the async runtime." The MCP binary has
`#[tokio::main]` with a multi-thread runtime.

**Plan's claim.** "MCP: drop its `spawn_blocking` offload, await the engine
directly." Rationale: the engine's async methods will internally call
`SqliteBackend::block_read/block_write`, which use `spawn_blocking`. So the
blocking rusqlite work is still off the executor thread — just one layer deeper.

**Findings.**

a) **The blocking-work-off-executor claim is correct for the storage path.**
`SqliteBackend::block_read/block_write` each call
`tokio::task::spawn_blocking(move || { … })` internally (confirmed in
`src/storage/sqlite/mod.rs:117–148`). After the cutover, when an MCP handler
does `engine.some_method().await`, the engine awaits `self.storage.some_method()`,
which dispatches to `block_read/block_write`. The `rusqlite` work is always
inside `spawn_blocking`, never parked on the executor. Correct.

b) **The consumer-trait calls are a different matter.** Post-cutover, async
engine methods will call consumer traits (e.g. `embedder.embed()`,
`summary_gen.generate()`). In the MCP context, those traits are
`Arc<HttpEmbeddingProvider>` / `Arc<HttpSummaryGenerator>`, which use
`reqwest::blocking` internally. If those are called **on the executor thread**
(i.e. not wrapped in `spawn_blocking`), the same panic described in §1c applies.
The current MCP code is immune because the entire engine call (including embed
calls) is inside `spawn_blocking`. After dropping the outer `spawn_blocking`,
the embed/summary calls inside the (now-async) engine methods must themselves be
pushed off the executor via `spawn_blocking`, or the `EmbeddingProvider` /
`SummaryGenerator` traits must have async methods. Neither is the case today.

**Verdict: [HIGH] — the MCP `spawn_blocking` drop is safe for the storage path
but breaks the embedding path. Any engine method that calls `embedder.embed()`
or `generator.generate()` on the tokio executor thread will panic due to
`reqwest::blocking`. The plan must either (a) keep a `spawn_blocking` wrapper
around the embed/summary calls, (b) convert `EmbeddingProvider::embed` and
`SummaryGenerator::generate` to async, or (c) wrap the reqwest calls in
`spawn_blocking` inside the trait implementations (in `memory-engine-embed`).
Option (c) is the cleanest for this refactor scope: `HttpEmbeddingProvider`
and `HttpSummaryGenerator` use `reqwest::blocking` today; switching them to
`reqwest`'s async client wrapped in `spawn_blocking` (or using
`tokio::task::block_in_place`) makes them safe to call from async. This is a
distinct sub-task the plan does not name.**

---

## 3. Object-safety of the 5 new atomic methods [LOW]

**Current state.** The plan adds 5 atomic methods (`insert_fact_atomic`,
`insert_facts_batch_atomic`, `insert_cosession_edges_atomic`,
`apply_cycle_deltas_atomic`, `commit_archive_atomic`) on `FactGraph`,
`ConsolidationStore`, and `ColdStorage`. The existing trait definitions use
`#[async_trait]` (confirmed in `src/storage/graph.rs:54`), which boxes all
futures as `Box<dyn Future<Output=…> + Send>`.

**Findings.** Under `async_trait`, any `async fn` on a trait method becomes
object-safe automatically — the macro rewrites to `fn … -> Pin<Box<dyn Future

- Send>>`. The existing backend callability test in `src/storage/backend.rs`already validates that async methods are callable through`dyn SearchIndex`
(line 93–138). The planned atomic methods have slice params (`&[i64]`,
`&[NewEdge]`, etc.), all of which are object-safe. No generic type parameters
on the new methods are shown in the plan. **Provided the new methods follow the
same `async fn`pattern as the existing ~90 trait methods (no added generic
params, no`Self: Sized`bound, borrowed args are all concrete types), they
will be object-safe and callable through`Arc<dyn StorageBackend>` without
  changes to the callability test.\*\*

**Verdict: [LOW] / Sound — no action needed beyond following the established
`#[async_trait]` pattern and extending the callability test to witness at least
one new atomic method.**

---

## 4. `async` default-on feature cascade [HIGH]

**Current state.**

- Root `Cargo.toml`: `default = []`; `async = ["dep:tokio"]` is optional.
  CLI `Cargo.toml`: depends on `memory-engine = { features = ["compress-gzip", "compress-zstd"] }` — **no `async` feature**.
  MCP `Cargo.toml`: depends on `memory-engine = { features = ["async"] }` — explicit.
  Embed `Cargo.toml`: depends on `memory-engine = { path = ".." }` — no features, no async.

- The `SqliteBackend` sub-tree is gated `#[cfg(feature = "async")]` (`src/storage/sqlite/mod.rs:28`).

**Plan's claim.** "Stage D: flip `default=["async"]`; confirm the whole
workspace builds/tests with async on by default."

**Findings.**

a) **Flipping `default=["async"]` in the root `Cargo.toml` does NOT
automatically enable `async` for the CLI.** The CLI depends on
`memory-engine = { features = ["compress-gzip", "compress-zstd"] }` with no
`async` in its features list. Cargo feature unification works per-dependency,
not per-workspace-default: the CLI's feature set is the _union_ of what it
explicitly requests plus what `default` resolves to when the dependency uses
default features (which it does, since no `default-features = false`). So
`default = ["async"]` in the root WILL flow through to the CLI's transitive
dependency resolution — this is a subtle but correct point: when a crate
depends on another with `default-features = true` (the cargo default),
the dependency's default features are activated. **So the cascade IS correct.**
However, this is fragile: any crate that depends with `default-features = false`
would miss it. The CLI does not set `default-features = false`, so it is fine.

b) **The CLI binary's `[dependencies]` still has no `tokio`.** After Stage D
(making async default), the CLI will transitively compile `SqliteBackend` with
tokio features, but the CLI binary itself needs `tokio = { features =
["rt-multi-thread", "macros"] }` in `[dependencies]` (not just
`[dev-dependencies]`) to run `#[tokio::main]`. Currently tokio is only in
`[dev-dependencies]` of the CLI. **Stage E must add `tokio` to
`[dependencies]` in `memory-engine-cli/Cargo.toml`.**

c) **`memory-engine-embed` has no `async` dependency and no tokio in
`[dependencies]`.** The embed crate only has tokio in `[dev-dependencies]`.
It uses `reqwest::blocking` which does not need a tokio runtime in the
dependency graph (it spins its own). After Stage D / E, if embed's
`HttpEmbeddingProvider` needs to be called from async contexts (per §2's fix),
it may need restructuring. This is a follow-up concern, not a plan blocker for
Stage D itself.

**Verdict: [HIGH] — The plan must explicitly state: add `tokio = { version = "1", features = ["rt-multi-thread", "macros"] }` to `[dependencies]` (not
`[dev-dependencies]`) in `memory-engine-cli/Cargo.toml` as part of Stage E.
The feature-cascade logic via `default=["async"]` is correct and will work,
but the CLI binary still needs `tokio` as a direct dependency to declare its
own runtime entry point.**

---

## 5. `AsyncMemoryEngine` deletion blast-radius [SOUND]

**Plan's claim.** "only `lib.rs` re-export + docs reference it — no cli/mcp/embed/test consumers."

**Verified by grep across all workspace src/ and docs/:**

- `src/async_engine.rs` — the file itself (deleted entirely). Its tests are
  internal (all `#[tokio::test]` inside the file's `#[cfg(test)] mod tests`).
- `src/lib.rs:82` — `pub mod async_engine` behind `#[cfg(feature = "async")]`.
  Removing this line removes the public module.
- `docs/reference/crate-layout.md:117` — one mention, update in Stage E's doc
  pass.
- `docs/reference/api.md:217` — one mention, update in Stage E's doc pass.
- Zero references in `memory-engine-cli/src/`, `memory-engine-mcp/src/`,
  `memory-engine-embed/src/`.
- Zero references in tests outside `async_engine.rs` itself.

**The plan's claim is verified. `AsyncMemoryEngine` is self-contained: its
deletion requires removing `src/async_engine.rs`, the `#[cfg(feature = "async")]
pub mod async_engine;` line in `lib.rs`, and two doc file updates. No external
consumer is broken.**

**Verdict: [SOUND] — blast-radius is exactly as described.**

---

## 6. `Send` across `.await` points with `parking_lot::RwLock` guards [BLOCKER]

**Current state.** `MemoryEngine` holds:

- `graph: parking_lot::RwLock<MemoryGraph>`
- `scope_tree: parking_lot::RwLock<ScopeTree>`

Post-cutover, engine methods become `async fn`. Every async method is required
to be `Send` (standard for `#[tokio::main]` multi-thread + `Arc<MemoryEngine>
` sharing).

**The problem.** `parking_lot::RwLockReadGuard` and `RwLockWriteGuard` are
`Send` only if the inner `T: Send`. `MemoryGraph` and `ScopeTree` — need to be
verified — but more importantly: **a guard held across an `.await` point causes
the future to be `!Send`**, regardless of whether the guard is `Send`, because
the executor may park the future and wake it on a different thread while the
guard is live.

Looking at the current engine code:

- `src/engine/query.rs:58`: `self.scope_tree.read().resolve_query(sq)` — the
  guard is temporary, used and dropped in the same expression. **Safe.**
- `src/engine/ingest.rs:459`: `let mut tree = self.scope_tree.write(); … drop(tree)` — the guard is explicitly dropped before the next operation.
  Current code is sync so this is fine; post-cutover, if there is any `.await`
  between acquiring the guard and dropping it, the future becomes `!Send`.
- `src/engine/consolidation.rs:40`: `*self.graph.write() = …` — again a
  temporary.
- `src/engine/cycle/apply.rs:310`: `let mut graph = self.graph.write()` —
  used in a sync block today.

**The specific risk at cutover:** the engine's new `async fn` implementations
will be produced by translating existing sync method bodies. The pattern of
`let guard = self.graph.write(); … do work that calls await …; drop(guard)` —
which is safe in sync code — becomes `!Send` in async code if any `.await`
sits between the guard acquisition and its drop. The compiler will catch this
(`error[E0277]: … cannot be sent between threads safely`), but the plan does
not flag it as a mechanical concern to watch for at each of the ~170 sites.

**The fix is simple and mechanical:** ensure every `RwLock` guard is dropped
before the first `.await` in any converted method. The pattern `{ let g =
self.graph.read(); let value = g.something(); drop(g); value }` works. Many
current call sites already do this (the guard is a temporary). The risk is at
sites where the guard is bound to a local variable and the method body has a
subsequent `.await` before the guard goes out of scope.

An alternative for the rare cases needing both read-access and an `.await` in
the same logical operation: extract the needed value out from under the guard
first, then drop the guard, then `.await`.

**Verdict: [BLOCKER] — not because it is unsolvable (it is mechanical) but
because it is not acknowledged in the plan and will produce `!Send` compile
errors at multiple sites during Stage E if missed. The plan must add an
explicit bullet: "At every converted `async fn`, verify no `parking_lot` guard
is held across an `.await`; extract the needed value before the first
`.await`." This is a systematic review task at ~170 sites, and missing even
one yields a compile error that is easy to fix but not trivial to enumerate
upfront.**

---

## 7. `Drop`/`close()` split for snapshot [MEDIUM]

**Current state.** `src/engine/mod.rs:826–831`:

```rust
impl Drop for MemoryEngine {
    fn drop(&mut self) {
        if let Err(e) = self.write_snapshot() {
            tracing::warn!(…);
        }
    }
}
```

`write_snapshot` is sync — it serializes the in-memory `MemoryGraph` /
`ScopeTree` and writes bytes to disk, using `fs::write`. This is a sync I/O
call in `Drop`, which is fine today.

**Plan's claim.** "add `async fn close(&self)` for the port-touching flush;
keep `Drop` for the sync in-memory snapshot only." The snapshot file format
(`src/engine/snapshot.rs`) serializes `graph` + `scope_tree` + optionally HNSW
state. Post-#631, HNSW moves into `SqliteBackend`. The sync in-memory part
(graph + scope_tree) stays serializable without any `await` calls.

**Findings.** The plan's resolution is correct for the in-memory part. The
`write_snapshot` path that serializes `graph` and `scope_tree` to disk is
purely sync I/O (`fs::write`). After HNSW moves to the backend, `Drop` need
not call `storage.*` at all — it only writes the sidecar file. This is sound.

The `async fn close()` for port-touching work (e.g. explicit HNSW flush via
the backend) is the correct pattern. The plan acknowledges that "the engine
already tolerates best-effort drop-time persistence" — so a user who forgets to
call `close()` loses the HNSW snapshot but not data. This is documented.

One gap: **the plan does not say whether `close()` is a method on
`MemoryEngine` itself or on the `StorageBackend` trait.** It should be on
`MemoryEngine` (it calls `self.storage.flush_hnsw_snapshot().await` or similar),
not on the trait (backends may not all have HNSW). This is an implementation
detail but worth pinning so Stage E does not leave it as "TBD."

**Verdict: [MEDIUM] — the design is correct; the gap is the unspecified
signature/location of `close()`. Pin it as `pub async fn close(&mut self) ->
Result<()>` on `MemoryEngine`, documented as "must be called for HNSW snapshot
persistence; `Drop` handles in-memory-only graph/scope snapshot." Not a
blocker but should be explicit in the plan before Stage E starts.**

---

## Summary

| #    | Severity        | Finding                                                                                                                            |
| ---- | --------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| 6    | **BLOCKER**     | `parking_lot::RwLock` guards held across `.await` → `!Send` futures. Must audit all ~170 conversion sites. Plan is silent on this. |
| 1c   | **HIGH**        | `reqwest::blocking` called from async CLI command handlers (consolidate/batch-ingest) will panic under `#[tokio::main]`.           |
| 2b   | **HIGH**        | MCP: dropping outer `spawn_blocking` leaves `reqwest::blocking` embed/summary calls on the executor thread → panic.                |
| 4b   | **HIGH**        | CLI `Cargo.toml` must add `tokio` to `[dependencies]` (not just `[dev-deps]`) for `#[tokio::main]`.                                |
| 7    | **MEDIUM**      | `close()` method location/signature not specified; pin before Stage E.                                                             |
| 1a/b | **LOW**         | CLI runtime choice: commit to `#[tokio::main]` (not `block_on`) in the plan text.                                                  |
| 3    | **LOW / Sound** | Atomic method object-safety: follows existing pattern, no issue.                                                                   |
| 5    | **Sound**       | `AsyncMemoryEngine` blast-radius: verified, exactly as described.                                                                  |

**Consolidated recommendation before Stage E starts:**

1. Add to Stage E's checklist: "At every converted `async fn`, verify no
   `parking_lot` guard is live across any `.await` point."
2. Decide on the reqwest-blocking fix (recommended: switch `HttpEmbeddingProvider`
   and `HttpSummaryGenerator` to `reqwest` async client with `spawn_blocking`
   wrapper, or use `tokio::task::block_in_place`). File as a sub-task — it is
   a non-trivial change to the embed crate's public trait impls.
3. Add `tokio = { version = "1", features = ["rt-multi-thread", "macros"] }` to
   `[dependencies]` in `memory-engine-cli/Cargo.toml` in Stage E.
4. Pin `async fn close(&mut self) -> Result<()>` on `MemoryEngine` in the plan.
