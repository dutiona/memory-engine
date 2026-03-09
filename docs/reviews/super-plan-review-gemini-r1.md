The implementation plan for Phase 3 is solid and addresses the core requirements for production readiness. However, I have identified several **critical gaps** and **concurrency risks** that should be addressed before implementation.

### 1. [Critical] Connection Pool Overgrowth
In **Task 6**, the `ReadGuard::drop` implementation unconditionally pushes the connection back into the `read_conns` pool. Since `read()` opens a *new* connection when the pool is empty (and `path` is `Some`), the pool will grow indefinitely during traffic bursts.
- **Risk:** Exhausting file descriptors or memory.
- **Fix:** `ReadGuard` should only return the connection if `pool.lock().len() < read_pool_size`. Alternatively, `read()` should block or return an error if a `max_pool_size` is reached.

### 2. [Critical] Migration Transactionality
In **Task 1**, the `migrate` function iterates through `MIGRATIONS` but does not wrap the loop in a `rusqlite` transaction.
- **Risk:** If a migration fails halfway (e.g., power loss or disk full), the database will be in an inconsistent state (some tables updated, `schema_version` not yet bumped).
- **Fix:** Wrap the entire migration loop in `conn.transaction()?`.

### 3. [Risk] In-memory Pool Read Fallback
**Task 6** contains a `todo!` for in-memory read fallback.
- **Pitfall:** SQLite in-memory databases (`:memory:`) are isolated per connection unless using the `file::memory:?cache=shared` URI. If you don't use a shared cache, the "readers" won't see the data written by the "writer."
- **Recommendation:** For `:memory:` databases, the `read()` method should simply return a lock on the `write_conn`. This serializes access but ensures correctness for tests.

### 4. [Risk] Missing `busy_timeout` in Pool
While the current `open_connection` (Phase 2) sets a `busy_timeout`, Task 6 should explicitly ensure that **every** connection created for the pool (especially new ones opened during `read()`) has `PRAGMA busy_timeout` set.
- **Risk:** Without a timeout, concurrent readers will immediately return `SQLITE_BUSY` if the writer is active, even in WAL mode.

### 5. [Concurrency] Scope Tree Consistency
In **Task 7**, `add_fact` reloads the entire `ScopeTree` into a `write()` lock after `ensure_path`. 
- **Efficiency:** This is acceptable for a "rare" operation, but for an autonomous agent creating many sub-scopes (e.g., per-task scopes), this is O(N) overhead per scope creation. 
- **Minor Fix:** Consider having `ScopeStore::insert` return the `ScopeNode` so it can be surgically inserted into the in-memory `ScopeTree` without a full reload.

### 6. [API Design] Scope Path Delimiter
**Task 3** splits paths by `/` in `ensure_path`.
- **Gap:** There is no mention of escaping or forbidden characters. If a user provides a label containing `/` (e.g., a URL or file path), it will be incorrectly parsed as multiple scopes.
- **Recommendation:** Add validation to reject `/` in labels or implement an escaping mechanism.

### 7. [Correctness] `AsyncMemoryEngine` Trait Bounds
**Task 8** requires `Arc<dyn Trait + Send + Sync>` for providers.
- **Observation:** This is correct for `spawn_blocking`, but ensure that the base traits in `src/traits.rs` are compatible (i.e., they don't have `!Send` components).

### Minor Suggestions:
- **Task 5 (SQL Push-down):** When using `json_each(?3)`, ensure the input is a valid JSON array string (e.g., `"[1,2,3]"`). `rusqlite` won't automatically convert a `Vec<i64>` to this format.
- **Task 10 (Benchmarks):** Since vector search is currently brute-force in Rust, pushing the `t_expired` and `fact_type` filters into SQL (Task 5) will show massive gains in the benchmarks. Ensure the baseline is recorded *before* that change if you want to measure the specific impact of SQL push-down.

**Overall Status:** **Conditional LGTM** (pending fixes for the connection pool overgrowth and migration transactionality).
