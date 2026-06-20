# #631 Design Synthesis — engine → `Arc<dyn StorageBackend>`

Synthesized from 3 adversarial advocate briefs (`*-drafts/draft-A{1,2,3}-*.md`). Forks B and C converged across advocates; **Fork A (async-vs-sync) is the open decision.**

## Fork A — the three strategies

| | A1 async-native | A2 block_on bridge | A3 sync port |
|---|---|---|---|
| **Engine API** | `pub async fn` (await the port) | stays sync (owned `Runtime`, `block_on`) | stays sync (calls a sync port) |
| **CLI** | + tokio runtime at `main` boundary; `.await` in commands | unchanged | unchanged |
| **MCP** | awaits directly; drops its `spawn_blocking` hop | unchanged (relies on its existing `spawn_blocking`) | unchanged |
| **#630 async port** | kept | kept | **reverted to sync** (Stage 0 backtrack; nothing consumes it yet) |
| **AsyncMemoryEngine** | deleted (663 lines) | kept | kept (becomes the only async adapter) |
| **PgBackend concurrency** | native async, web-scale | thread-per-query (double offload) | thread-per-query (sync `postgres` crate or block_on at PgBackend boundary) |
| **Spec decision #1 (async-native)** | honors | mild tension | **reverses** |
| **Biggest weakness** | tokio in CLI (~30 crates), public API break (pre-1.0 OK) | double thread-hop + `Drop` footgun + keeps HNSW engine-side (vs spec #3) | reverses just-merged #630 + caps PG concurrency |

## Assessment

- **A2 is dominated** — structural cost (double-offload, `Drop` footgun) + it doesn't actually move HNSW into the backend (Fork C punt). Not recommended.
- **A1 vs A3 pivots on PgBackend's target scale:**
  - **Web-scale / multi-tenant cloud** (many concurrent agents/queries) → **A1**. Only async-native delivers high-fan-out concurrency on few threads; thread-per-query (A2/A3) caps it. Honors the locked spec decision. Deletes the AsyncMemoryEngine duplication. CLI pays a one-time tokio cost.
  - **Embedded / single-agent modest cloud persistence** (the crate's stated identity) → **A3** is honestly simpler: revert #630's async ceremony (cheap now — no consumer), keep engine + CLI sync, get the cleanest transaction primitive (sync `transaction(|tx| …)` closure, no async-closure/object-safety pain). But it reverses a locked decision and forfeits web-scale PG concurrency.

## Forks B & C (settled across A1/A3)

- **Fork B — transactions:** coarse-grained **atomic port methods** for the 4 `unchecked_transaction` sites (`insert_fact_atomic`/identity-stamp, edge-batch, archive, `apply_cycle_deltas_atomic`), each one DB transaction inside the backend. (A3 additionally enables a generic sync `transaction` closure — only viable if Fork A = A3.) Faithful for SQLite (one tx) and a future PgBackend.
- **Fork C — HNSW:** move `hnsw_strategy` + `search_config`/`ann_threshold` (build + dispatch + notify) **into `SqliteBackend`**; engine drops them and calls `storage.vector_search().await` (or sync under A3); index never crosses the port. (A2 alone punted this — another reason to drop A2.)

## Recommendation

**A1 (async-native)** unless the PgBackend roadmap is explicitly embedded/single-agent-only. A1 is the spec-faithful, structurally-coherent end state; its CLI cost is one-time and contained (a `block_on` at the command boundary, not async-viral through the CLI), and it removes the AsyncMemoryEngine duplication. A3 is the right call *only* if you're willing to revert #630's async port AND PG will never need web-scale concurrency — in which case it is genuinely the simplest, lowest-risk path.

**Decision required:** lock Fork A. Forks B/C then follow (B = coarse atomic methods; C = HNSW into the backend).

## DECISION (locked 2026-06-21 by maintainer)

- **Fork A = A1 async-native.** `MemoryEngine` methods become `async fn` and await `Arc<dyn StorageBackend>`. `AsyncMemoryEngine` is deleted (its spawn_blocking duplication is subsumed). CLI adopts a `tokio` runtime via one `block_on`/`#[tokio::main]` at the command boundary + `.await` at engine call sites; MCP awaits the engine directly and drops its `spawn_blocking` offload. Honors spec decision #1; future PgBackend is natively async (web-scale concurrency).
- **Fork B = coarse atomic port methods.** The 4 `unchecked_transaction` sites become dedicated atomic async port methods (`insert_fact_atomic` for identity-stamp+fact+vector+FTS, the co-session edge batch, the archive select+delete+manifest, and `apply_cycle_deltas_atomic`), each one DB transaction inside the backend (object-safe; faithful for SQLite and a future async PgBackend).
- **Fork C = HNSW into `SqliteBackend`.** Move `hnsw_strategy` + `search_config`/`ann_threshold` (build + dispatch + notify) into the backend; the engine drops them and calls `storage.vector_search().await`; the index never crosses the port.

super-plan will stage this (the A1 brief sketches ~7 reviewable stages — port-additions → backend HNSW+atomic methods → engine cutover → CLI runtime → MCP simplify → AsyncMemoryEngine deletion → audit); given the breadth (~200 sites, cross-crate), expect the plan to recommend splitting across multiple PRs/sub-issues.
