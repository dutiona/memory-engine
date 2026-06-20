# Async / Runtime-Correctness Review — #630 `SqliteBackend`

> Lens: compile-level and runtime-level traps in the proposed async seam only.
> Verdict per finding: [BLOCKER] / [HIGH] / [MEDIUM] / [LOW].
> Plan citations reference `docs/plans/2026-06-20-sqlite-backend-630.md`.
> Code citations reference the files as read for this review.

---

## 1. `block_read` / `block_write` helper bounds

**Plan §3 signatures:**

```rust
async fn block_read<T, F>(&self, f: F) -> Result<T>
where T: Send + 'static, F: FnOnce(&rusqlite::Connection) -> Result<T> + Send + 'static
```

### 1a. The `T: Send + 'static` + `F: Send + 'static` bounds are necessary and sufficient — SOUND

`tokio::task::spawn_blocking` requires the closure to be `FnOnce() -> R + Send + 'static`, and the return type `R` has no explicit bound (it is moved back through the `JoinHandle` on the current thread, not across a thread boundary at the type-system level). However, tokio's `spawn_blocking` internally requires `R: Send` since Rust 1.x because the `JoinHandle<R>` must be `Send` for the `.await` to work on any executor thread. Concretely:

```
pub fn spawn_blocking<F, R>(f: F) -> JoinHandle<R>
where F: FnOnce() -> R + Send + 'static, R: Send + 'static
```

Both `T: Send + 'static` and `F: FnOnce(...) -> Result<T> + Send + 'static` are required. The plan states exactly these bounds. **Sound.**

### 1b. The `!Send` guard acquired **inside** the closure — SOUND

`ReadConn<'_>` borrows `&ConnectionPool`, which makes it `!Send` (a lifetime-carrying reference to a non-`Send` pool field). `MutexGuard<'_, Connection>` from `parking_lot` is also `!Send`. Both are acquired inside the closure, so they never cross an `await` point, and the closure itself is `FnOnce() -> Result<T>` — the guard's lifetime is entirely within the blocking thread's stack frame. The compiler sees no `!Send` type in the closure's captured environment (only `Arc<ConnectionPool>` which is `Send`). **Sound.**

### 1c. The doubly-wrapped `Result` and `map_err` / `map_seam_err` composition — SOUND WITH ONE NUANCE

`spawn_blocking(...).await` returns `Result<Result<T>, JoinError>`. The plan's shape is:

```rust
let out: Result<T> = spawn_blocking(...).await  // Result<Result<T>, JoinError>
    .map_err(map_join)?;                        // after ?:  Result<T>
map_seam_err(out)                              // Result<T>
```

The `?` after `.map_err(map_join)` unwraps the outer `JoinError` layer, leaving `out: Result<T>` (the inner `Result`). `map_seam_err` then remaps `Database → Storage(Backend)` on the inner `Result`. This is correct — `map_seam_err` sees exactly the `Result<T>` produced by `f`. **Sound.**

The nuance: `map_seam_err` and `map_join` are free functions, not methods on `SqliteBackend`, so they must be visible from within the `impl` block. The plan shows them as module-level free functions — fine as long as they are `pub(super)` or live in the same module as the `impl`. Not a blocker, but confirm visibility during implementation.

---

## 2. `#[async_trait]` object-safety

### 2a. `StorageBackend` stays `dyn`-safe — SOUND

The blanket impl at `backend.rs:60-63` and the object-safety assertions at `backend.rs:72-79` already compile today (A1 is merged). The plan adds only **inherent** methods on the concrete `SqliteBackend` struct (`block_read`, `block_write`, `for_each_streamed`), not new trait methods. Inherent methods on a concrete type cannot break object-safety of a trait. No generic methods are added to any of the six bounded-context traits. **No E0038 risk from #630's additions.**

### 2b. `#[async_trait]` `Self: Sync` hidden bound — SOUND

`async_trait` desugars `async fn foo(&self) -> T` to a boxed future `Box<dyn Future<Output=T> + Send + '_>`. For the boxed future to be `Send`, the `Self` type must be `Send + Sync` (because `&self` is captured). The `SqliteBackend` struct fields are:

- `pool: Arc<ConnectionPool>` — `Arc<T>: Send + Sync` iff `T: Send + Sync`. `ConnectionPool` contains `Mutex<Connection>` and `Condvar` (from `parking_lot`). `parking_lot::Mutex<T>: Send + Sync` iff `T: Send`. `rusqlite::Connection` is `Send` (verified: rusqlite marks `Connection: Send`). So `Arc<ConnectionPool>: Send + Sync`. ✓
- `embed_dim: usize` — trivially `Send + Sync`. ✓
- `upcaster_registry: UpcasterRegistry` — assumed `Send + Sync` (it is a registry of upcast functions, not verified here, but the engine already holds one and passes it through blocking tasks in the existing code). Flag if `UpcasterRegistry` is not `Sync`.
- `#[cfg(feature = "ann")] hnsw: Option<Arc<parking_lot::RwLock<HnswStrategy>>>` — `Arc<RwLock<T>>: Send + Sync` iff `T: Send + Sync`. Needs verification that `HnswStrategy: Send + Sync`.

[MEDIUM] **Unverified `Send + Sync` on `UpcasterRegistry` and `HnswStrategy`**: the plan assumes these are `Send + Sync` because the existing engine holds them, but the existing engine does **not** place them behind `async_trait` futures. If either type is `!Sync`, the compiler will reject `impl FactGraph for SqliteBackend` with a future-`Send` error that looks like an `async_trait` puzzle, not a struct-field error. Verify before T1; add `static_assertions::assert_impl_all!(UpcasterRegistry: Send + Sync)` and `assert_impl_all!(HnswStrategy: Send + Sync)` in the `SqliteBackend` module under `#[cfg(test)]`.

---

## 3. `for_each_streamed` — deadlock, backpressure, early-exit, and `handle.await` correctness

Plan §3 (D2):

```rust
async fn for_each_streamed<T, S>(&self, scan: S, cb: &mut (dyn FnMut(T) -> Result<()> + Send))
    -> Result<()>
where T: Send + 'static,
      S: FnOnce(&rusqlite::Connection, &std::sync::mpsc::SyncSender<T>) -> Result<()>
           + Send + 'static
{
    let pool = Arc::clone(&self.pool);
    let (tx, rx) = std::sync::mpsc::sync_channel::<T>(1);
    let handle = tokio::task::spawn_blocking(move || { let conn = pool.read()?; scan(&conn, &tx) });
    while let Ok(row) = rx.recv() { cb(row)?; }
    map_seam_err(handle.await.map_err(map_join)?)
}
```

### 3a. Cap-1 channel backpressure — SOUND

A `sync_channel(1)` means the blocking producer can at most have 1 row queued (plus whatever is in `send` if that send blocks). The consumer drains synchronously (`rx.recv()` blocks until a row arrives). Because the consumer and producer run on separate threads (blocking pool thread vs async task), this is a producer–consumer ping-pong that never exceeds 2 rows in flight (1 buffered + 1 being processed by the callback). Peak memory O(1) per scan is correct.

### 3b. Early-exit on callback error — SOUND

When `cb(row)?` returns `Err`, the `?` propagates out of `for_each_streamed`. `rx` is dropped. The producer's next `tx.send(next_row)` will return `Err(SendError(...))` because the receiver is gone. The `scan` closure should propagate this as an error (or ignore it). However:

**The plan's scan closures must handle `SendError` gracefully.** If a scan closure does:

```rust
|conn, tx| { for row in query { tx.send(row).map_err(|_| MemoryError::Internal(...))? } }
```

then the `SendError` on early-exit will cause `scan` to return an `Err`, which propagates through `handle.await`. This is fine — `map_seam_err` will remap it. But if the caller's early-exit `Err` and the `SendError`-induced scan `Err` are both live, which one surfaces?

The sequence:

1. `cb(row)?` returns `Err(E_cb)` and exits the `while` loop.
2. `rx` is dropped.
3. `handle.await` resumes the blocking task, which returns `Err(E_send)` (from the next `tx.send`).
4. `map_seam_err(handle.await.map_err(map_join)?)` returns `Err(E_send)`, **discarding** `E_cb`.

[HIGH] **The callback's early-exit error `E_cb` is silently discarded.** The plan states "early callback Err drops rx → scan's send errs → stops" but does not address the fact that the error surfaced to the caller is `E_send` (a `SendError` from the blocking task), not `E_cb` (the semantic error from the callback). The caller can't distinguish "scan had a SQL error" from "callback said stop" from the returned error. The fix: capture the callback error before the loop exits and use it if the scan result is an artificial `SendError`:

```rust
let mut cb_err: Option<MemoryError> = None;
while let Ok(row) = rx.recv() {
    if let Err(e) = cb(row) {
        cb_err = Some(e);
        break;  // drop rx on next iteration (or explicitly drop rx below)
    }
}
drop(rx);  // ensure producer unblocks
// Drain handle result; prefer cb_err if the scan error is merely SendError
let scan_result = map_seam_err(handle.await.map_err(map_join)?);
if let Some(e) = cb_err { return Err(e); }
scan_result
```

Without this fix, test H6 (streaming early-error + correct error propagation) cannot pass as specified, because the error the test asserts is `E_cb` but the function returns `E_send`.

### 3c. `handle.await` after the drain loop — potential hang on consumer panic

If the `async fn for_each_streamed` itself panics (not the blocking task) before `handle.await` is reached, the `JoinHandle` is dropped. `tokio::task::spawn_blocking` returns a detached task on `JoinHandle` drop — the blocking thread keeps running until `scan` blocks on `tx.send` (which will `SendError` immediately since `rx` was dropped by the panic). This is safe: the blocking task will finish soon. No hang.

If `cb` panics (rather than returning `Err`), the panic propagates out of the async function. Same analysis: handle dropped, blocking task unblocks on next send, finishes. No hang.

**`handle.await` correctness**: after `rx.recv()` returns `Err` (meaning `tx` was dropped because `scan` completed or panicked), the blocking task has already finished. `handle.await` on an already-completed `JoinHandle` returns immediately. **Sound.**

### 3d. The `&mut (dyn FnMut(T) -> Result<()> + Send)` callback and the async drain loop — SOUND

The callback is invoked synchronously inside the `while let Ok(row) = rx.recv()` loop, which is `await`-free. `rx.recv()` is a synchronous call; the loop itself is not `async`. However, the function `for_each_streamed` is `async`, so its entire body is a future. The `rx.recv()` call will block the async executor thread.

[BLOCKER] **`rx.recv()` is a synchronous blocking call inside an `async fn` — this blocks the tokio executor thread.** `while let Ok(row) = rx.recv()` is equivalent to a `std::thread::park` on the executor thread. Under `tokio::test` (which uses a single-threaded runtime by default), this is a guaranteed deadlock if `spawn_blocking` exhausts the blocking thread pool — but more critically, `rx.recv()` unconditionally parks the current executor thread (not a `spawn_blocking` worker) until a row arrives from the blocking thread. Even on a multi-thread runtime, parking an executor thread degrades throughput and, on a 1-thread runtime, deadlocks if the blocking pool is also at capacity.

The correct fix is to use `tokio::sync::mpsc` (async channel) instead of `std::sync::mpsc`, and `.await` on the receiver:

```rust
let (tx, mut rx) = tokio::sync::mpsc::channel::<T>(1);
// producer sends via tx (must be adapted — tokio channel's send is async)
```

But the producer runs in `spawn_blocking`, so it cannot `.await`. The clean solution is `tokio::sync::mpsc::channel` with `tx.blocking_send(row)` in the `spawn_blocking` closure, and `rx.recv().await` in the async drain loop:

```rust
let (tx, mut rx) = tokio::sync::mpsc::channel::<T>(1);
let handle = tokio::task::spawn_blocking(move || {
    let conn = pool.read()?;
    scan(&conn, &tx)  // scan uses tx.blocking_send(row)
});
while let Some(row) = rx.recv().await { cb(row)?; }
drop(rx);
map_seam_err(handle.await.map_err(map_join)?)
```

This requires changing the `scan` closure signature to accept `&tokio::sync::mpsc::Sender<T>` instead of `&std::sync::mpsc::SyncSender<T>`, and the blocking side calls `tx.blocking_send(row).ok()?` (or maps the error). The `tokio::sync::mpsc::Sender<T>` is `Send + 'static` when `T: Send`, so the closure bound remains satisfiable.

Alternatively, use `tokio::task::block_in_place` around `rx.recv()`, but that requires `rt-multi-thread` which is not in the production feature set (only in `[dev-dependencies]`).

The `std::sync::mpsc` approach is not suitable here. **This is a BLOCKER.**

### 3e. `#[tokio::test]` default flavor for parity tests — compound with 3d

The plan (§10) specifies `#[cfg(feature="async")] #[tokio::test]` for parity tests. `#[tokio::test]` defaults to the `current_thread` (single-thread) flavor. With `rx.recv()` blocking an executor thread as described in 3d, a single-thread runtime will deadlock: the executor thread is parked waiting for the blocking task to send, but the blocking task cannot run because... actually `spawn_blocking` uses a dedicated thread pool separate from the executor thread pool. So on a single-thread runtime:

- Executor thread parks on `rx.recv()`.
- `spawn_blocking` task runs on a separate blocking thread.
- Blocking thread sends a row.
- Executor thread unblocks, processes one row, loops back, parks again.

This is technically not a deadlock on `current_thread` because `spawn_blocking` threads are independent of executor threads. However, it is still **incorrect async practice** — parking an executor thread is prohibited. On a saturated blocking pool (which is bounded at 512 threads by default in tokio), this creates backpressure that could deadlock in production.

[HIGH] **The parity tests should use `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]`** regardless: the streaming tests need the blocking task and the drain loop to run concurrently, and the single-thread flavor serializes them through cooperative yielding — it works only because `rx.recv()` happens to unpark when `spawn_blocking` sends. With the async-channel fix from 3d, `rx.recv().await` yields properly and `#[tokio::test]` (single-thread) works correctly. So this item is secondary to the BLOCKER in 3d.

---

## 4. tokio feature surface — `spawn_blocking` vs `rt` vs `rt-multi-thread`

**Cargo.toml production dependency (line 29):** `tokio = { version = "1", features = ["rt", "macros"], optional = true }`

**`spawn_blocking` feature requirement:** `tokio::task::spawn_blocking` is gated on `tokio/rt` only — it does not require `rt-multi-thread`. Confirmed from tokio's documented feature matrix: `spawn_blocking` ships with the basic `rt` feature because it uses a separate blocking thread pool, not the `rt-multi-thread` scheduler.

**`#[tokio::test]` default flavor:** uses `current_thread` runtime, which only requires `tokio/rt`. This is available in `[dev-dependencies]` where tokio is pulled with `rt-multi-thread` + `macros` (line 37). The `current_thread` runtime provided by `tokio/rt` is sufficient for `#[tokio::test]`.

**Verdict on tokio features:** [LOW] The production feature `async = ["dep:tokio"]` enables `tokio` with `["rt", "macros"]`. This is sufficient for `spawn_blocking` and `#[tokio::test]`. The dev-dependency pulls `rt-multi-thread` for the existing `async_engine.rs` concurrency tests. No gap here for the basic `block_read`/`block_write` pattern.

However, as noted in 3d, the `std::sync::mpsc::recv()` call inside the async drain loop can park an executor thread. If the fix (3d) uses `tx.blocking_send`, the `tokio::sync::mpsc` channel is also available under `tokio/rt` — no new feature dependency.

---

## 5. `map_seam_err` / `map_join` placement and D4 interaction with `?` inside closures

### 5a. Error mapping at the seam boundary — SOUND

The closures passed to `block_read`/`block_write` call the sync store functions which emit `MemoryError::Database(rusqlite::Error)` via `#[from]`. The `?` inside the closure propagates those as `Result<T>` (i.e., `Err(MemoryError::Database(...))`). `spawn_blocking` returns `Ok(Err(MemoryError::Database(...)))`. After `.map_err(map_join)?`, `out` is `Err(MemoryError::Database(...))`. `map_seam_err(out)` then remaps it to `Err(MemoryError::Storage(StorageError::Backend(...)))`. **Correct and atomic for the common case.**

### 5b. Semantic variants pass through `map_seam_err` unchanged — SOUND

The `map_seam_err` match arm:

```rust
Err(MemoryError::Database(e)) => Err(MemoryError::Storage(StorageError::Backend(e.to_string()))),
other => other,
```

Any non-`Database` variant (`NotFound`, `ReadOnly`, `Internal`, `EmbeddingDimension`, `Migration`, etc.) passes through unchanged. This correctly implements D4. **Sound.**

### 5c. `map_seam_err` applied to a `SendError`-caused scan failure (compound with 3b)

As identified in 3b: on early callback exit, the scan closure's `tx.send(row)` returns `Err(SendError(...))`. The scan closure maps this to some `MemoryError` variant. If mapped to `MemoryError::Internal` (a semantic variant), it passes through `map_seam_err` unchanged. If mapped to `MemoryError::Database`, it gets remapped to `Storage(Backend)`. Either way, the wrong error (scan's `SendError` instead of callback's `E_cb`) is surfaced. This is the same issue as 3b — listed there as [HIGH].

---

## 6. `Send` holes in `async_trait` boxed futures and the `for_each_*` callbacks

### 6a. `+ Send` on `async_trait` futures — SOUND

`async_trait` in the `proc-macro` sense generates, for each `async fn foo(&self, ...) -> T`, a method returning `Pin<Box<dyn Future<Output=T> + Send + '_>>`. For this to typecheck, all captured references in the async body must be `Send`. The callbacks passed to `for_each_*` are `&mut (dyn FnMut(T) -> Result<()> + Send)` — explicitly `Send`. The pool `Arc<ConnectionPool>` is `Send + Sync`. The channel types (`std::sync::mpsc::SyncSender<T>` or `tokio::sync::mpsc::Sender<T>`) are `Send` when `T: Send`. All rows `T: Send + 'static` by the helper's bound. **No `Send` hole here.**

### 6b. The `&mut dyn FnMut` callback is not `Sync`

`&mut (dyn FnMut(T) -> Result<()> + Send)` is a mutable reference to a non-`Sync` trait object. In the drain loop (inside `for_each_streamed`), the callback is called from the async context only — never from the blocking task. The blocking task only touches `tx` (the sender). So there is no concurrent access to `cb`. **No `Sync` requirement on `cb`, no hole.**

### 6c. Rows `T: Send + 'static` — sufficient

The rows travel across thread boundaries via the channel (blocking thread → async context). `T: Send` is required for channel transport; `T: 'static` is required by `spawn_blocking`. Both bounds are on the helper. All concrete row types (`Fact`, `Edge`, `ScopeNode`, `Event`, etc.) from `crate::types` are `'static` (no borrowed fields) and `Send` (no `Rc`, no raw pointers). **Sound.**

---

## 7. Additional findings not in the scrutiny list

### 7a. `pool.read()` inside `spawn_blocking` can block for up to 30 seconds — [MEDIUM]

`ConnectionPool::read()` uses a `Condvar::wait_until` with `DEFAULT_READ_ACQUIRE_TIMEOUT = 30s` (`connection_pool.rs:21`). If all read connections are checked out, `pool.read()` blocks the blocking thread for up to 30 seconds before returning `MemoryError::Pool`. Under a `spawn_blocking` task, this occupies a thread in tokio's blocking pool. This is acceptable behavior (blocking threads are meant to block), but the 30s timeout means a streaming scan can appear hung for 30s before failing. Document in `block_read`'s rustdoc.

### 7b. `block_write` uses `try_write` which calls `parking_lot::Mutex::lock()` — potential priority inversion — [LOW]

`try_write` at `connection_pool.rs:271-276` calls `self.write_conn.lock()` (unconditional lock, despite the name). If two concurrent `block_write` calls are in-flight, the second blocking task will park on the `parking_lot::Mutex::lock()` for the write connection, occupying a second blocking thread while waiting. This is fine — that is the intended serialization mechanism. However, if many concurrent write calls are made, the blocking pool can saturate. This is a pre-existing design constraint, not introduced by #630. **Informational.**

### 7c. In-memory pool: `block_read` and `block_write` both lock `write_conn` — [MEDIUM]

For in-memory pools, `pool.read()` returns `ReadConn::InMemory(WriteAsReadGuard { guard: self.write_conn.lock() })` (`connection_pool.rs:226-230`). This means `block_read` and `block_write` both contend on the same `write_conn` mutex. A concurrent `block_read` and `block_write` will serialize correctly. However, the H8 parity test (file-backed concurrent-read routing) is specified for file-backed pools only — in-memory tests will not catch a mis-route (which is documented in the plan). This is already acknowledged. Confirmed sound for the plan's intent.

### 7d. `UpcasterRegistry` across `spawn_blocking` — [MEDIUM]

The plan mentions `SqliteBackend` holds `upcaster_registry: UpcasterRegistry`. The blocking closures for `get_upcasted_event` / `list_upcasted_events` will need to call into the registry. If the registry is referenced from the closure, it must be `Send` (captured by `Arc<SqliteBackend>` or by cloning). Verify: the closures likely call `self.upcaster_registry.upcast(event)` but `self` is not captured by the closure — `pool` is. The registry must be cloned into each closure, or accessed via `Arc`. Cloning a registry on each call is expensive if it contains many upcast functions. The plan says the registry is a field of `SqliteBackend`, so consider wrapping it in `Arc<UpcasterRegistry>` to make clone cheap.

---

## Summary table

| #   | Severity    | Finding                                                                                                                                                                                                                                                                              | Plan ref               |
| --- | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------- |
| 3d  | **BLOCKER** | `rx.recv()` (sync) inside `async fn` blocks the executor thread. Use `tokio::sync::mpsc` + `rx.recv().await` + `tx.blocking_send()` instead.                                                                                                                                         | §3 D2                  |
| 3b  | **HIGH**    | On callback early-exit, `E_cb` is discarded; `E_send` (SendError from scan) is returned instead. Capture `cb_err` and prefer it over the artificial scan error.                                                                                                                      | §3 D2, H6              |
| 2b  | **MEDIUM**  | `UpcasterRegistry` and `HnswStrategy` must be `Send + Sync` for the `async_trait` futures to be `Send`. Not verified. Add `static_assertions` in `#[cfg(test)]`.                                                                                                                     | §2 struct              |
| 7c  | **MEDIUM**  | In-memory pool: `block_read` and `block_write` both lock `write_conn`; concurrent streaming scans will serialize. Not a correctness bug but kills read concurrency on in-memory instances. Document.                                                                                 | §3, pool               |
| 7d  | **MEDIUM**  | `UpcasterRegistry` cloned per closure call if it is a plain struct field. Wrap in `Arc<>` for cheap cross-`spawn_blocking` sharing.                                                                                                                                                  | §2 struct              |
| 3e  | **HIGH**    | Parity tests must use `#[tokio::test(flavor = "multi_thread")]` (or fix 3d first). Single-thread runtime + sync `recv()` in executor is technically non-deadlocking with `spawn_blocking` but violates async contract; with the async-channel fix, `current_thread` works correctly. | §10                    |
| 4   | **LOW**     | `spawn_blocking` requires only `tokio/rt` — the production feature set is sufficient. No gap.                                                                                                                                                                                        | §7                     |
| 5   | **LOW**     | `map_seam_err`/`map_join` visibility must be confirmed (`pub(super)` or same module). Not a blocking issue; will surface as a compile error if wrong.                                                                                                                                | §3                     |
| 7b  | **LOW**     | `try_write` (`parking_lot::Mutex::lock`) can saturate the blocking pool under high write concurrency. Pre-existing design constraint; document.                                                                                                                                      | connection_pool.rs:271 |

---

## Required actions before T1

1. **Fix D2 channel type** (BLOCKER): replace `std::sync::mpsc::sync_channel` with `tokio::sync::mpsc::channel(1)`, `tx.blocking_send(row)` in the scan closure, `rx.recv().await` in the drain loop.
2. **Fix early-exit error priority** (HIGH): capture `cb_err`, drain `rx`, await handle, return `cb_err` if set and the scan error is an artifact.
3. **Verify `UpcasterRegistry: Send + Sync` and `HnswStrategy: Send + Sync`** (MEDIUM): add compile-time assertions in the sqlite module's `#[cfg(test)]` block.
4. **Wrap `UpcasterRegistry` in `Arc`** (MEDIUM) if it is not already.
5. **Update test annotations** (HIGH consequence of fix 1): after fixing to async channel, `#[tokio::test]` single-thread flavor is correct — no `multi_thread` override needed. But confirm with T1's no-hang test.
