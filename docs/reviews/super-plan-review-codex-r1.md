Not LGTM.

**Blocking Findings**

1. `High` — The scope tree model is internally inconsistent. In `Cross-Cutting Invariants / Scope Isolation Contract`, root is `id=1`, but in `Task 3 Step 2/3/4`, `ensure_path()` starts with `parent_id = NULL`, and the unique indexes are built around `parent_id IS NULL` roots. That makes top-level scopes siblings of the `'root'` row, not descendants of it. `ScopeTree::ancestors()`, `Inherited`, default-root semantics, and `Task 9 resume_context()` all become wrong or ambiguous. You need one model: either a real sentinel root that owns all top-level scopes, or no root row at all.

2. `High` — The plan claims “multi-session, multi-machine deployment” in `Goal`/`Architecture`, but the implementation is a single-process cache over SQLite. `Task 7` adds in-memory `RwLock<MemoryGraph>` and `RwLock<ScopeTree>` with no cross-process invalidation. Separate sessions/processes will diverge immediately, and “multi-machine” is not a credible target for SQLite-backed local files. Narrow the claim to single-process/single-host, or add a real shared backend and cache invalidation strategy.

3. `High` — `Task 7 Step 4` proposes making `with_read`/`with_write` public after removing store accessors. That breaks the engine’s invariants: callers can mutate tables directly without rebuilding `graph` or `scope_tree`, so the engine becomes internally inconsistent by design. If you keep in-memory caches, raw `Connection` access cannot be public.

4. `High` — `Task 6 Step 2` is not production-complete. `ConnectionPool::read()` has a `todo!()` for the `read_pool_size == 0` path, and the fallback behavior is only described in prose. Also, when the pool is exhausted, it creates transient connections and returns them to the pool, so the pool can grow without bound under load. That is a correctness and resource-exhaustion risk, not a minor implementation detail.

5. `High` — `Task 4 Step 1` relies on `ALTER TABLE ... ADD COLUMN scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id)`. SQLite `ADD COLUMN` has restrictions around foreign keys/defaults, and this pattern is fragile enough that it should not be assumed safe for a production migration. If this fails on the bundled SQLite version, the whole migration strategy breaks. A table-rebuild migration is safer.

6. `High` — The connection pool plan is missing critical per-connection SQLite initialization in `Task 6 / Connection Pool Contract`. In a concurrent SQLite design, every connection needs consistent PRAGMAs/open flags, especially `foreign_keys=ON` and a `busy_timeout`. `query_only=ON` alone is not enough. Without `busy_timeout`, “database is locked” becomes a normal production failure mode.

7. `High` — The public scope API violates its own contract. `Cross-Cutting Invariants / Scope Isolation Contract` says scope paths are consumer-facing and IDs are internal, but `Task 3 Step 1` and `Task 5 Step 4` expose `ScopeQuery::{Exact,Subtree,Ancestors,Inherited}(i64)`. That leaks internal IDs into the API and makes the contract false.

8. `High` — Scope isolation is only partially implemented despite the invariant claiming it applies to facts, edges, events, summaries, consolidation, and forgetting. `Task 4` adds `scope_id` everywhere, but only `add_fact()` gets scope resolution. `Task 5` only scopes search. `Task 7`/`Task 9` do not define scoped `ingest`, scoped summary creation, scoped edge/event insertion, or scope-aware forgetting/consolidation/conflict resolution. The data model and the operational model are out of sync.

9. `High` — Derived-entity scoping is undefined. In `Task 4`, edges/events/summaries get `scope_id`, but the plan never states what scope a summary created from mixed-scope facts should live in, whether edges may cross scopes, or whether event/fact scope mismatches are legal. Without those invariants, scope-based isolation will be inconsistent and hard to test.

10. `Medium` — Scope path auto-vivification in `Task 3 Step 2` has no normalization/validation. The current plan allows arbitrary labels and path shapes with no limits on segment count, segment length, empty segments, whitespace, or normalization rules. In production this becomes both a data-bloat vector and an isolation bug source (`user:michael/project:x` vs `user:michael/ project:x`, Unicode variants, trailing slash variants, etc.).

11. `Medium` — The SQL push-down story is overstated in `Task 5` and `Verification`. `valid_at` remains a post-filter, so hybrid/vector search can still rank top-k rows that later get dropped, which means result quality can still degrade. If correctness matters, temporal filtering needs push-down too, or the engine needs over-fetching before post-filter.

12. `Medium` — `Task 4 Step 4` does not fit the proposed sequencing. It assigns `self.scope_tree = ScopeTree::load(...)` before `Task 7` adds interior mutability. If `add_fact()` is still `&self` at that stage, the step is not implementable as written.

13. `Medium` — `Task 3 Step 2` is only race-safe inside one engine instance. `ensure_path()` does `find_by_label` then `insert`, which is serialized by the single writer mutex only per process. Separate processes can still race and hit the unique index. The insert path needs retry-on-constraint-violation if shared DB access is a goal.

14. `Medium` — `Task 1` and `Task 4` disagree on fresh-database semantics. `Task 1` tests assume `init_schema()` creates version 1 then `migrate()` moves to 2, while `Task 4` says new databases should create v2 directly. Pick one behavior early; otherwise migration tests and open/init logic will churn.

15. `Medium` — `Task 7 Step 5` exposes `graph()` as a lock guard. That lets callers hold the read lock indefinitely and block `forget()` / `resolve_conflict()` writers. For a production library, returning snapshots or query methods is safer than exposing the live lock.

16. `Medium` — `Task 8` should probably be feature-gated. Pulling `tokio` into core dependencies for all users is unnecessary for sync-only consumers, and `spawn_blocking` around potentially slow embedder/network work can saturate Tokio’s blocking pool if this library is used heavily.

If you want this to get to LGTM, I would first force decisions on: the actual scope-tree root model, the public scope API shape, whether raw SQL access stays internal, the real migration strategy for `scope_id`, and whether Phase 3 is single-process-only or truly shared across processes.
