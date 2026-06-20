# Async / Concurrency Adversarial Review — PR #682 `SqliteBackend` (#630)

**Lens:** async seam correctness, concurrency, `Send`/`Sync`, deadlock, error-priority.
**Verdict scale:** [BLOCKER] / [HIGH] / [MEDIUM] / [LOW] / [SOUND].
**Code references** are file-relative to the worktree root.

---

## 1. `block_read` / `block_write` — seam bounds, guard placement, error wrapping

### 1a. `T: Send + 'static`, `F: Send + 'static` — SOUND

`tokio::task::spawn_blocking` requires `F: FnOnce() -> R + Send + 'static` and `R: Send + 'static`.
The actual signature of `JoinHandle<R>` being `Send` forces `R: Send + 'static`.
Both helpers declare exactly these bounds on `T` (the return value) and `F` (the closure).
The pool `Arc` is cloned on the executor thread and moved into the closure — no borrow crosses
the thread boundary. Correct.

### 1b. `!Send` guard acquired inside the closure — SOUND

`ReadConn<'_>` (which wraps `MutexGuard<'_, Connection>` for in-memory mode, or `ReadGuard<'_>`
for pooled mode) is `!Send` because `parking_lot::MutexGuard<T>` is `!Send`.
Both guards are acquired inside the `spawn_blocking` closure, never stored in a `Send + 'static`
position. The closures themselves close over only `Arc<ConnectionPool>` (which is `Send + Sync`)
and other `Send` values. Correct.

### 1c. Doubly-wrapped `Result` — SOUND

```
spawn_blocking(|| -> Result<T> { … })
    .await                     // -> Result<Result<T>, JoinError>
    .map_err(map_join)?        // -> Result<Result<T>, MemoryError>  (JoinError→Pool)
```

Wait — `.await` on a `JoinHandle<Result<T>>` returns `Result<Result<T>, JoinError>`.
`.map_err(map_join)?` propagates a `JoinError` (panic/cancel) as `MemoryError::Pool` and
unwraps the `Result<Result<T>, MemoryError>` to `Result<T, MemoryError>` via `?`. So after
the `?` we hold `Result<T>` (the closure's return). Then `map_seam_err(out)` remaps
`MemoryError::Database` to `MemoryError::Storage(Backend)`. The double-unwrap is correct;
the naming `out` for the inner `Result<T>` is accurate.

### 1d. `map_seam_err` — SOUND

D4 contract: a `MemoryError::Database` (raw rusqlite error) becomes opaque
`StorageError::Backend`; every named semantic variant passes through unchanged.
The `match` in `map_seam_err` is exhaustive: arm 1 matches `Database`, arm 2 is `other => other`
which re-returns every other variant including `NotFound`, `ReadOnly`, `EmbeddingDimension`, etc.
Confirmed by the three tests `block_read_maps_database_error_to_storage_backend`,
`block_read_passes_semantic_variant_through`, and the `get_fact_missing_yields_not_found` /
`get_missing_yields_not_found` tests across the trait files.

---

## 2. `for_each_streamed` — the cap-1 channel bridge

### 2a. Early-callback-error path — SOUND WITH CAVEATS (see 2c)

Walk the path:

1. `cb(row)` returns `Err(e)` at row k.
2. `cb_err = Some(e)`, `break` exits the `while let` loop.
3. `drop(rx)` — receiver dropped, channel closed from receiver side.
4. The scan task is blocked on `blocking_send` for row k+1 (cap-1 backpressure).
   `blocking_send` returns `Err(SendError(_))` because the receiver is gone.
5. The scan closure maps this to `stream_consumer_dropped()` via `.map_err(|_| …)?`,
   which propagates out of the scan closure as `Err(MemoryError::Internal(…))`.
6. The blocking task completes with this error.
7. `handle.await.map_err(map_join)?` succeeds (no panic): `out = Err(Internal(…))`.
8. `map_seam_err(out)` — `Internal` is not `Database`, so it passes through.
9. `cb_err.map_or(scan_res, Err)` — `cb_err` is `Some(e)`, so returns `Err(e)`.
   The scan's `Internal` error is discarded.

The callback error wins. No row is double-counted: the `break` exits the loop before
the next `recv().await`, so row k is the last row handed to `cb`. Row k+1 is
already in the channel (or not yet sent); it never reaches `cb`.

**Caveat:** rows already sent into the cap-1 channel before `rx` is dropped are
silently discarded (the channel is flushed on drop). At most 1 row can be in-flight
(cap = 1), and it belongs to the _next_ iteration the `while let` would have taken —
so there is no double-delivery, but there is a silent skip of up to 1 buffered row.
This is correct for the stop-early contract (once `cb` returns `Err`, further rows
are irrelevant), but worth acknowledging.

### 2b. `handle.await` after `drop(rx)` — SOUND

`tokio::task::JoinHandle` is safe to `.await` regardless of whether the task has
already completed. If the task finished before `.await`, the future resolves
immediately with the stored result. If the task panicked, `JoinError::is_panic()` is
true and `map_join` maps it to `MemoryError::Pool`. Dropping `rx` does not cancel the
`JoinHandle` — only `handle.abort()` would do that. So the `handle.await` here
always eventually resolves.

### 2c. Scan task cannot block indefinitely — SOUND UNDER A CONDITION [MEDIUM note]

After `drop(rx)`, the scan's next `blocking_send` returns `Err` immediately because
`blocking_send` checks the receiver count before blocking. This is the correct
behavior for `tokio::sync::mpsc::Sender::blocking_send` when the receiver is dropped.
However, if the scan _never calls `blocking_send`_ again (e.g., the SQL cursor is
exhausted before the next send attempt), the task finishes normally with `Ok(())` and
`scan_res` is `Ok(())` while `cb_err` wins.

**[MEDIUM]** — not a bug, but a documentation gap: the `for_each_streamed` doc says
"the scan's next `blocking_send` fails and it stops", which is only true if the scan
has more rows to send. If the cursor was exhausted at exactly row k (the failing row),
the scan returns `Ok(())` and `cb_err.map_or(scan_res, Err)` still correctly returns
the callback error. The implementation is correct; the doc is imprecise and could
mislead a future implementor who changes the scan body.

**Fix:** Add "or the scan finishes naturally" to the `for_each_streamed` doc comment.

### 2d. Cap-1 backpressure — SOUND

With cap-1, the scan can be at most 1 row ahead of the consumer. This satisfies O(1)
peak memory (the claim in the doc comment). Larger capacity would increase throughput
at the cost of more in-flight rows; cap-1 is the conservative correct choice for
O(1) memory. Sound.

### 2e. `for_each_streamed` uses `block_read`, not `block_write` — SOUND

The `for_each_*` streaming methods (facts, edges, scopes, events, summaries, lineage)
all call `for_each_streamed`, which internally calls `pool.read()`. Correct for read
operations. None of the streaming callsites are write paths.

---

## 3. In-memory pool: `ReadConn::InMemory` collapses reads onto the write mutex

### 3a. Potential self-deadlock in in-memory mode — [HIGH]

In-memory mode: `pool.read()` acquires `write_conn.lock()` and returns a
`ReadConn::InMemory(WriteAsReadGuard { guard })`. The guard holds the `parking_lot`
mutex for the write connection.

`pool.try_write()` also calls `write_conn.lock()`.

**`parking_lot::Mutex` is NOT re-entrant by default.** If any code path on the same
thread (or the same blocking-pool thread in a single-threaded test) attempts to call
`pool.try_write()` while a `ReadConn::InMemory` guard is alive, it will **deadlock**.

The risk is real in these scenarios:

1. **`#[tokio::test]` default (single-threaded executor) + `spawn_blocking` serialized
   on one thread:** tokio's `#[tokio::test]` without `flavor = "multi_thread"` uses a
   `current_thread` runtime. `spawn_blocking` spawns a real OS thread via the blocking
   pool, so two concurrent `block_read`/`block_write` calls do NOT share the same OS
   thread — this case is **safe** in tests.

2. **`seeded()` helper in `graph.rs` and `search_index.rs` tests:** these helpers call
   `pool.write()` (not `try_write()`) to seed data, hold the guard across a loop, then
   drop it — all synchronously, before the backend is used. No async overlap. **Safe.**

3. **Two `for_each_streamed` calls on the same in-memory backend, overlapping:** each
   `for_each_streamed` spawns a blocking task that calls `pool.read()`, which acquires
   `write_conn.lock()`. If two such tasks run concurrently on the blocking thread pool
   against the same in-memory backend, the second `pool.read()` blocks until the first
   `ReadGuard` is dropped. This is correct serialization, not a deadlock — eventually
   the first scan finishes.

4. **`block_read` then `block_write` in the same `spawn_blocking` closure:** this is
   impossible by design — each method call goes through a single `block_read` or
   `block_write` invocation; no closure acquires both.

5. **`set_config` test helper in `pool` tests calls `pool.write()` directly, not
   `try_write()`:** `write()` does not check `read_only`. If a test held a
   `ReadConn::InMemory` while simultaneously calling `write()` on the same thread
   (single-threaded test, no `spawn_blocking`), it would deadlock. Inspecting the
   pool tests: all `write()` calls are separate from all `read()` calls, with explicit
   `drop()` in between. **Safe as written.**

**Actual risk:** The current backend API never calls `pool.read()` then `pool.try_write()`
in the same closure or task without first dropping the read guard. But the abstraction
does not prevent a future implementor from doing so. The `ReadConn::InMemory` variant
holding the write mutex is a subtle invariant that is not documented in the `ConnectionPool`
or on `ReadConn`.

**[HIGH] — not a present bug but a latent trap.** Recommendation: document in
`ConnectionPool::read()` that in in-memory mode the read guard holds the write mutex
and that acquiring a write guard while a `ReadConn::InMemory` is live will deadlock.
A `debug_assert!` or compile-time guard is not possible here, but the doc comment
should make this explicit.

### 3b. Read-only pool `open_read_only` stores a read-only connection in `write_conn` — [MEDIUM]

In `open_read_only`, a connection opened with `SQLITE_OPEN_READ_ONLY` is stored in
`write_conn`. When `read()` is called on this pool in in-memory mode — which cannot
happen since `open_read_only` is always file-backed — it would return this connection
as the write mutex guard. For file-backed read-only pools, `read()` correctly serves
from `read_conns`. Not a bug as written (the read-only pool always has `read_pool_size > 0`
for file-backed), but the struct layout where `write_conn` holds a read-only connection
is surprising. `try_write()` correctly guards this with `if self.read_only { return Err(ReadOnly) }`.

No action required; noting for clarity.

---

## 4. `Send + Sync` correctness

### 4a. Compile-time witness — SOUND BUT INCOMPLETE [LOW]

`mod.rs` contains:

```rust
fn _assert_send_sync() {
    fn f<T: Send + Sync>() {}
    f::<SqliteBackend>();
    f::<UpcasterRegistry>();
    f::<ConnectionPool>();
}
```

This is a compile-time dead-code check (the function is never called, but the compiler
must prove the bounds at the `f::<T>()` call site). It correctly proves `SqliteBackend: Send + Sync`.

`SqliteBackend` fields:

- `pool: Arc<ConnectionPool>` — `Arc<T>: Send + Sync` iff `T: Send + Sync`. `ConnectionPool` is asserted `Send + Sync`.
- `embed_dim: usize` — trivially `Send + Sync`.
- `upcaster_registry: Arc<UpcasterRegistry>` — `UpcasterRegistry` is asserted `Send + Sync`.

`ConnectionPool` fields:

- `write_conn: parking_lot::Mutex<Connection>` — `parking_lot::Mutex<T>: Send + Sync` iff `T: Send`. `rusqlite::Connection: Send` (rusqlite guarantees this).
- `read_conns: parking_lot::Mutex<Vec<Connection>>` — same reasoning.
- `Condvar`, `Option<PathBuf>`, `usize`, `bool`, `Duration` — all `Send + Sync`.

**[LOW]** — the check is in `#[cfg(test)]` only, so it's not enforced in `--release`
builds or in downstream crates that don't run tests. For a library crate, the idiomatic
stronger approach is a `const _: fn() = || { … }` item at module level (not
test-gated) so the Send+Sync contract is proven for every build configuration.
Currently, if a field is added that breaks `Send+Sync`, this is only caught when tests
run, not at `cargo build`. Low severity because the CI gate runs tests, but worth fixing.

**Fix:** Move `_assert_send_sync` out of `#[cfg(test)]` into the main module body as a
`const _: fn() = || { fn f<T: Send + Sync>() {} f::<SqliteBackend>(); };`.

### 4b. `async_trait` `Send` futures — SOUND

`#[async_trait]` by default rewrites `async fn` to return `Pin<Box<dyn Future<Output=…> + Send>>`.
For the future to be `Send`, all captured references must be `Send`. In all trait impls:

- `&self` is `&SqliteBackend` — `Send` (since `SqliteBackend: Sync`).
- All temporary owned values cloned before the closure (`query.to_owned()`, `fact.clone()`, etc.) are `Send`.
- The closures themselves are `Send + 'static`.

No `!Send` type is stored across an `.await` point. **Sound.**

---

## 5. tokio feature: `"sync"` requirement for `blocking_send`

### 5a. tokio features — SOUND

`Cargo.toml`:

```toml
tokio = { version = "1", features = ["rt", "macros", "sync"], optional = true }
```

- `tokio::sync::mpsc` / `blocking_send` — requires `"sync"`. Present.
- `tokio::task::spawn_blocking` — requires `"rt"`. Present.
- `#[tokio::test]` — requires `"macros"` + `"rt"`. Both present.

All tokio feature requirements are met. **SOUND.**

### 5b. D1 async gate on the `sqlite` subtree — SOUND

`src/storage/mod.rs` (verified):

```rust
#[cfg(feature = "async")]
pub mod sqlite;           // line 38–39

#[cfg(feature = "async")]
pub use sqlite::SqliteBackend;  // line 52–53
```

The entire `storage/sqlite` directory is excluded from compilation when `feature = "async"`
is absent. Individual sibling files inside the module do not need their own `cfg` gates —
they are only reachable through `mod.rs`, which is itself only included when the parent
`pub mod sqlite` is compiled. `mod cold_storage` adds a second gate (`archive`) on top.
D1 holds. Default (no-`async`) builds pull no tokio. **SOUND.**

---

## 6. `spawn_blocking` semantics — executor vs blocking pool

### 6a. Nothing runs on the executor thread that belongs on the blocking pool — SOUND

Every rusqlite call (`pool.read()`, `pool.try_write()`, `conn.query_row()`,
`conn.execute()`, etc.) is inside a `spawn_blocking` closure. The executor threads
see only:

1. `Arc::clone(&self.pool)` — O(1) atomic increment, trivially non-blocking.
2. The `.await` of the `JoinHandle`.

No synchronous I/O or mutex acquisition occurs on the tokio executor threads. **Sound.**

### 6b. `pool.read()` Condvar wait inside `spawn_blocking` — SOUND

If all read connections are checked out, `pool.read()` calls
`read_available.wait_until(&mut conns, deadline)` — a synchronous blocking wait.
This is inside `spawn_blocking`, so it occupies a blocking-pool thread, not an
executor thread. This is the correct place to block. The `DEFAULT_READ_ACQUIRE_TIMEOUT`
(30s) provides a finite upper bound to avoid permanent thread pool exhaustion.

### 6c. `parking_lot::Mutex` inside `spawn_blocking` — SOUND

`parking_lot::Mutex::lock()` can spin-wait then OS-park. Both are acceptable on a
blocking thread. The write mutex is uncontended in almost all cases (single writer
per pool). **Sound.**

### 6d. Panic inside the closure → `JoinError::is_panic()` → `MemoryError::Pool` — SOUND

`map_join` maps any `JoinError` (panic or cancellation) to `MemoryError::Pool`. A
panic inside the closure would propagate through the `JoinHandle` and surface as
`MemoryError::Pool`, not silently swallow the panic. The blocking pool thread is
not poisoned (panics do not permanently disable blocking pool threads in tokio).
Confirmed by `block_read_panic_maps_to_pool` test. **Sound.**

---

## 7. Additional findings

### 7a. `for_each_streamed` always uses `pool.read()`, not `pool.try_write()` — SOUND

All `for_each_*` streaming paths go through `for_each_streamed`, which calls
`pool.read()`. This means streaming on a read-only pool works correctly. No streaming
path accidentally calls `pool.try_write()`. **Sound.**

### 7b. `seeded()` helper self-deadlock risk in in-memory tests — SOUND BUT FRAGILE [LOW]

`graph.rs` and `search_index.rs` test helpers call `pool.write()` to seed data with
a guard held across a loop:

```rust
let conn = pool.write();
let store = FactStore::new(&conn, DIM);
for f in facts { store.insert(f).unwrap(); }
// guard dropped here
```

Then later `pool.read()` is called by the async backend. Since the `write()` guard
is dropped before the backend is used, no deadlock. The `clippy::significant_drop_tightening`
allow attribute is correctly applied. Sound but fragile: if the seeding pattern is
ever changed to interleave with async calls, deadlock would occur in in-memory mode
(see finding 3a).

### 7c. `for_each_streamed` scan closure borrows `&tokio::sync::mpsc::Sender<T>` — SOUND

The scan closure receives `&tokio::sync::mpsc::Sender<T>` as a second parameter.
The `Sender` is owned by the `spawn_blocking` closure (moved in), not borrowed from
the async side. The `&Sender` reference is valid for the entire lifetime of the scan
closure. The `Sender` is correctly dropped when the closure returns, signaling the
channel's send side is gone (though the receiver may already be dropped). **Sound.**

### 7d. `drop(rx)` timing relative to `handle.await` — SOUND

`drop(rx)` is explicit before `handle.await`. In Rust, drops happen in order of
statements, so `rx` is definitely dropped before `handle.await` begins. This is
intentional: it ensures the scan's next `blocking_send` (or a current wait in
`blocking_send`) unblocks promptly. **Sound.**

### 7e. No `#[must_use]` on `block_read`/`block_write` — [LOW]

`block_read` and `block_write` return `Result<T>`. If a caller forgets to `.await`
the returned future (or ignores the `Result`), silent data loss or logic errors
could occur. These are `async fn` so the future is `#[must_use]` by default from
the compiler (an unawaited async call is a warning). The `Result` is not `#[must_use]`-
annotated, but `clippy::must_use_candidate` or `clippy::unused_must_use` would catch
dropping the `Result` without inspection. Given they are `async fn`, the risk is low.

---

## Summary Table

| ID  | Severity | Location                                      | Finding                                                                  |
| --- | -------- | --------------------------------------------- | ------------------------------------------------------------------------ |
| 1a  | SOUND    | `mod.rs:109–122`                              | `Send + 'static` bounds correct                                          |
| 1b  | SOUND    | `mod.rs:115–118`                              | `!Send` guard acquired inside closure                                    |
| 1c  | SOUND    | `mod.rs:119–121`                              | Double-wrapped `Result` unwrap correct                                   |
| 1d  | SOUND    | `mod.rs:94–101`                               | `map_seam_err` passes semantic variants                                  |
| 2a  | SOUND    | `mod.rs:152–183`                              | Callback error wins; no double-count                                     |
| 2b  | SOUND    | `mod.rs:179`                                  | `handle.await` after `drop(rx)` safe                                     |
| 2c  | [MEDIUM] | `mod.rs:148–150` (doc comment)                | Doc says "next send fails" — imprecise if cursor already exhausted       |
| 2d  | SOUND    | `mod.rs:164`                                  | Cap-1 backpressure correct for O(1) memory                               |
| 3a  | [HIGH]   | `connection_pool.rs:225–261`                  | In-memory `ReadConn` holds write mutex — deadlock trap undocumented      |
| 3b  | [MEDIUM] | `connection_pool.rs:198–208`                  | `write_conn` holds a read-only connection in RO mode — confusing layout  |
| 4a  | [LOW]    | `mod.rs:197–201`                              | `Send+Sync` witness is `#[cfg(test)]`-only; use `const _` for all builds |
| 4b  | SOUND    | All `impl` files                              | `async_trait` Send futures correct                                       |
| 5a  | SOUND    | `Cargo.toml`                                  | `"sync"` feature present; `blocking_send` requirement met                |
| 5b  | SOUND    | `src/storage/mod.rs:38–39`                    | D1 `#[cfg(feature = "async")]` gate confirmed in parent module           |
| 6a  | SOUND    | All trait impls                               | No sync I/O on executor threads                                          |
| 6b  | SOUND    | `connection_pool.rs:240–255`                  | Condvar wait inside `spawn_blocking` correct                             |
| 6c  | SOUND    | `connection_pool.rs:271–276`                  | `parking_lot::Mutex` on blocking thread correct                          |
| 6d  | SOUND    | `mod.rs:87–89`, test                          | Panic→`Pool` error correct                                               |
| 7a  | SOUND    | `mod.rs:152–183`                              | Streaming uses read path only                                            |
| 7b  | [LOW]    | `graph.rs:581–591`, `search_index.rs:116–126` | Seed helpers fragile under in-memory interleave                          |
| 7c  | SOUND    | `mod.rs:159,165`                              | `&Sender` borrow lifetime valid                                          |
| 7d  | SOUND    | `mod.rs:177–179`                              | `drop(rx)` before `handle.await` correct                                 |
| 7e  | [LOW]    | `mod.rs:109, 127`                             | `Result` not `#[must_use]`; mitigated by async `#[must_use]`             |

---

## Verdict

**No blockers. No HIGH issues after verification.** The seam design is fundamentally correct:

- The `Send + 'static` discipline, guard placement, and error-wrapping chain are clean.
- The `for_each_streamed` cap-1 bridge handles early-callback-error correctly with the right priority semantics (callback error wins, no double-count, no lost rows beyond the intentional early-stop).
- The `spawn_blocking` usage is disciplined — no sync I/O on executor threads.
- D1 async gating confirmed: `src/storage/mod.rs:38–39` gates the entire `sqlite` subtree with `#[cfg(feature = "async")]`. Default builds pull no tokio.
- All tokio feature requirements (`"rt"`, `"macros"`, `"sync"`) are present.

**One HIGH finding to address before merge:**

1. **[HIGH] 3a** — `ConnectionPool::read()` in in-memory mode returns a `ReadConn::InMemory` that holds the `write_conn` parking_lot mutex. This is a re-entrant deadlock trap: any code path on the same blocking-pool thread that acquires a read guard and then (directly or indirectly) calls `pool.try_write()` or `pool.write()` will deadlock silently. Not triggered by any current call site, but undocumented and a latent footgun for future implementors. Fix: add an explicit `# Panics / Deadlock` note to `ConnectionPool::read()` stating that in in-memory mode the returned guard holds the write mutex, and that calling `write()`/`try_write()` while that guard is alive deadlocks.

**Three LOW/MEDIUM findings worth a follow-up (non-blocking):**

2. **[MEDIUM] 2c** — `for_each_streamed` doc comment says "the scan's next `blocking_send` fails and it stops" — imprecise when the SQL cursor is already exhausted at the failing row. The implementation is correct; the doc should say "or the scan finishes naturally".

3. **[LOW] 4a** — The `Send + Sync` compile-time witness is `#[cfg(test)]`-only. Move it to a `const _: fn() = || { … }` at module level so it is enforced on every `cargo build`, not just `cargo test`.

4. **[MEDIUM] 3b** — In `open_read_only`, a `SQLITE_OPEN_READ_ONLY` connection is stored in `write_conn`. Harmless (guarded by `try_write()` returning `ReadOnly`), but the layout is confusing to future readers of the pool code. Consider renaming to `primary_conn` or adding a doc note.
