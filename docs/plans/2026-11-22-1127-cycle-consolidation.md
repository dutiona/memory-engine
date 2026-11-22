# #1127 — follow-up group A2: cycle-report identity, dedup by fact id, prune rename, set_level not found, Postgres harness

Branch `fix/1127-cycle-consolidation` (worktree `.worktrees/fix-1127-cycle-consolidation`, locked), from `origin/main`
c103cfa. Tracking issue #1127, umbrella dutiona/infra#37. Route (Michaël, 2026-11-22): Bug fix · Standard path · Super
planning · Full review gate, Default severity.

## Outcome

One PR closes #1062, #1080, #1089, #1095 and #1099, one commit per member, plus separate boy-scout commits. A
`CycleReport` names the embedding identity of the vectors its `AddFact`/`Synthesize` deltas carry, and the apply refuses,
inside its transaction and before any write, a vector that is undeclared or declared under another identity than the
store's; `LlmDreamCycle` declares its embedder's, and `memory-engine-cli consolidate --backend llm` refuses a foreign
embedder before it calls the LLM. Dedup's "new" facts are the active facts whose id is above a fact-id watermark, never
selected by `t_created`, so a backdated fact, one committed or embedded after the snapshot, one unforgotten after the
watermark passed it, and every fact after a reconstruction or a JSON restore are compared. The decay prune is
`MemoryEngine::prune(&PrunePolicy)`, `DreamCtx::prune`, crate `me-prune`; `forget` names only the retraction. `set_level`
answers `not found` exactly as `forget` does. The live Postgres suite tolerates the slow container start that a
measurement under load found, by the fix those numbers select, and an unrecoverable start fails with a message naming an
infrastructure failure. Neither backend's schema changes (SQLite stays v18, Postgres v1); no migration number is taken.

## Constraints

- #1062: "Deltas with a vector declare its identity, and it must match the store's; otherwise the delta is refused with a clear error. #1020 may remove the tool; if it lands first, close this as superseded."
- #1062: "A failing test first."
- ADR-0015 §3: "a bare vector with no declared identity is rejected when a store identity exists, and a declaration that omits a field the store records does not match it."
- #1080: "A failing test first: a fact created before the watermark but committed (or embedded) after the snapshot is still compared for duplicates."
- #1080: "Selection uses a monotonic commit/embedding sequence (or equivalent) instead of `t_created`."
- #1089: "The decay prune is named `prune` everywhere (facade, `DreamCtx`, traits, `me-forget`, docs); `forget` names only the retraction."
- #1089: "No behaviour change; the existing prune tests pass under the new name."
- #1095: "`set_level` answers `not found` exactly as `forget` does (a normal answer, same wording), including for a fact of another project (which must not reveal that it exists)."
- #1095: "A test pins the shared behaviour for both tools." Michaël, 2026-11-18: align `set_level` with `forget` ("yes ok").
- #1023 D8 (approved): "`not found` is byte-identical for a missing id, a negative id, root and a fact outside the Homes."
- #1099: "Find what times out (container start, image pull, or the readiness wait) and on what limit."
- #1099: "The parity suite tolerates a slow container start: a longer or configurable start timeout, or retrying container creation only, never the test's assertions."
- #1099: "A failure in container creation still fails the run, with a message that says it is infrastructure, not the backend."
- #1127: "Each member's own acceptance, unchanged. The PR carries one `Closes #N` line per member." `Refs dutiona/infra#37`.
- Owner: no blind timeout bumps or caps; measure under real stress first and bring the numbers. Heavy runs (cargo, podman) one at a time under the machine-wide lock.
- Owner: TDD (failing test first, watched red); Conventional Commits, body says why, no co-author line on branch commits; boy-scout fixes in their own commits.
- Owner: name every function and type changed (parallel #1126 and #1026 rebase on it).
- Public repository: repository-relative paths only, no home paths, host names or tokens.
- Kept past decision (ROADMAP "Stale config handling"): an unparsable consolidation watermark is a migration error, never ignored.

## Decisions

### Where each rule lives

| Rule | Home | Why there |
|---|---|---|
| A report names the identity of its vectors | `me-types` `CycleReport::embedding_fingerprint` | the report is what crosses from a producer into the engine; `ConsolidationPlan::embedding_fingerprint` already carries summaries' identity this way |
| The producer fills it | `me-cognitive` `LlmDreamCycle::run` | only the producer knows its embedder |
| Undeclared or foreign vectors are refused | `me-backend-sqlite` `SqliteBackend::apply_cycle_deltas_atomic` (validation pass, then its transaction) | ADR-0015 enforces at the embedding boundary; inside the write transaction there is no check-then-act gap, and every port caller fails closed |
| New to dedup = id above the watermark | `me-consolidate` `compute_dedup`, over a backend `Snapshot` | selection stays backend-free; "ids grow in commit order" is the backend's guarantee, written on `Snapshot::max_fact_id` |
| The watermark and every rewind | `me-backend-sqlite` `store::facts` (`DEDUP_WATERMARK`, `rewind_dedup_watermark`), called in the transaction that makes an old fact eligible again | one key, one owner below both `consolidation` and the stores that call it |
| `not found` is a normal by-id answer | `memory-engine-mcp` `tools::set_level::run` | ADR-0020 §8's answers are the server's contract; the engine's `MemoryError::NotFound` stays an error |
| A container-runtime failure is infrastructure | `crates/me-backend-postgres/src/tests.rs` `start_pg` | the one function between the runtime and the code under test; nothing after it is retried |

### D1 (#1062, owner point O1): harden; do not close as superseded

PR #1069 (#1020) deleted the MCP tool, but not the defect. `SqliteBackend::apply_cycle_deltas_atomic`
(`crates/me-backend-sqlite/src/sqlite/consolidation.rs`) guards `AddFact`/`Synthesize` vectors with
`embedding_meta::require_present` alone, and a binary reaches it today: `memory-engine-cli consolidate --backend llm`
(`crates/memory-engine-cli/src/commands/consolidate.rs`) builds its embedder from `--embed-*`, never checks it (only
`commands/query.rs` calls `verify_embedding_identity`), and applies `LlmDreamCycle`'s `Synthesize` vectors. The repository's
own `llm_backend_synthesizes_via_wiremock_end_to_end` writes vectors declared `nomic-embed-text` into a store its
`FakeEmbed` stamped, and passes. #1123 puts `apply_cycle_report` back on a server path, nightly.

- `CycleReport` gains `#[serde(default, skip_serializing_if = "Option::is_none")] pub embedding_fingerprint:
  Option<EmbeddingFingerprint>`: the identity of every vector in `deltas`; `None` only when no delta carries one
  (`Promote` copies a stored vector).
- `LlmDreamCycle::run` sets `Some(self.embedder.fingerprint())`; `DefaultDreamCycle::run` sets `None`.
- `apply_cycle_deltas_atomic` refuses, in its validation pass, an `AddFact`/`Synthesize` in a report with `None`: new
  `CycleError::UndeclaredEmbedding` (`"cycle report carries a vector but declares no embedding identity (ADR-0015 §3)"`).
  In its transaction it keeps `require_present` (#613: a blank store stays refused; a cycle never stamps), then calls
  `embedding_meta::check_compatible(&tx, declared)` (`EmbeddingModelMismatch` on any differing field, served artifact
  included). The whole report is refused and the store is unchanged, as for a wrong dimension today.
- `MemoryEngine::apply_cycle_report` rebuilds the report field by field (redaction); it carries the new field.
- Rejected: a parameter on `apply_cycle_report` (separates the declaration from the vectors; a persisted report loses it);
  a declaration per delta (redundant, one embedder per cycle; breaks every match on `CycleDelta`); `record_if_absent`
  (would let a cycle stamp a blank store, which #1062 does not ask for).
- Breaking: `CycleReport` is a plain struct, so a consumer's `DreamCycle` that builds one adds the field (`!` commit).
- **Alternative (close as superseded):** drop items 6 and 8; item 7 stays the CLI's only guard, the facade keeps
  accepting undeclared vectors and #1123 inherits that. The PR then says `Refs #1062`, never a closing keyword beside it,
  and Michaël closes #1062 by hand.

### D2 (boy-scout, owner point O2): the promotion twin declares its vector

`DreamCtx::promote(&PromoteRequest)` → `MemoryEngine::promote_with_lineage` → `ConsolidationStore::promote_atomic` writes
the caller's `PromoteRequest::embedding` under `require_present` only: the same ADR-0015 §3 gap, reachable only by a
third-party cycle today. `PromoteRequest` gains `pub embedding_fingerprint: EmbeddingFingerprint` (required: a promotion
always carries a vector); `SqliteBackend::promote_atomic` checks it with `require_present` + `check_compatible` in its
transaction. Its own commit, independent of every other item. Alternative: accept it as a residual in the PR.

### D3 (#1080, owner point O3): the sequence is the fact id; no schema change

- `facts.id` is `INTEGER PRIMARY KEY AUTOINCREMENT` and SQLite has one writer, so ids are assigned in commit order and
  never reused in a live store (#209's `last_caller_write_fact_id` cursor relies on the same). A fact committed after a
  snapshot has a larger id than every fact it saw, whatever its `t_created` (backfill #1026 keeps original timestamps;
  `AddFactOptions::t_created`; `add_fact` stamps `t_created` before it embeds).
- Watermark: config key `last_consolidated_fact_id` (`store::facts::DEDUP_WATERMARK`), the highest id dedup has fully
  compared. `load_snapshot_capped` reads the watermark and `MAX(id)` over all facts (`FactStore::max_id`) **before**
  `any_without_vector` and the active list, so a fact committed during the load is either listed or above the next
  watermark. `Snapshot::last: Option<DateTime<Utc>>` becomes `Snapshot::watermark: Option<FactId>`, plus
  `Snapshot::max_fact_id: FactId` and `Snapshot::generation: u64`; `ConsolidationPlan` carries all three.
- Selection: `compute_dedup(.., after: Option<FactId>)` drives with the active facts whose `id > after` (all when absent).
- Generation: config key `consolidation_generation` (`store::facts::CONSOLIDATION_GENERATION`, an integer, absent = 0),
  incremented by every write that makes a fact the watermark passed eligible again (the rewinds below) or that
  protects a fact a plan may expire (a pin). The snapshot
  reads it first (`Snapshot::generation`); `apply_plan`, first in its transaction, compares it with the stored value and,
  when it changed, writes nothing (no expiry, no summary, no watermark) and returns zero counts with no expired ids: the
  plan was computed over a store that has since changed under it, and the next run recomputes. `consolidate` releases
  the lock between phases (#409), so this race is real for a library user. A counter, not a compare of the watermark
  value: a value can return to what the snapshot read (an absent key cleared again by a reconstruction, a rewind to 0),
  which a value compare reads as "unchanged". The same rule fixes a defect the old code has too: a reconstruction
  between compute and apply let a plan computed in the old vector space expire facts that the new space no longer
  pairs.
- Advance: `apply_plan` writes `max(stored, plan.max_fact_id)` only when dedup ran, the snapshot saw no vectorless fact
  (#1021's hold, unchanged) and no survivor was lost (`applied.survivor_lost`: today's comment says the kept loser "is
  reconsidered next run", which no code does). It deletes `last_consolidated_at` in the same write.
- #1021's apply-time `any_without_vector()` re-check is deleted: a fact inserted after the load has a larger id and no
  path removes a vector, so under ids it is redundant. The rule it served ("the watermark never passes a vectorless
  fact") stands.
- Rewinds, each in the transaction that causes it, via `rewind_dedup_watermark(&tx, below: FactId)` (sets the watermark
  to `min(current, below - 1)`, deleting it at 0, and increments the generation): `SqliteBackend::unforget_fact` (lowest
  restored id); `FactStore::set_pinned` when it unpins a pinned fact (that id: dedup skips pinned facts as drivers and
  as candidates, `pipeline/dedup.rs`, so two duplicates passed while one was pinned are compared once it is unpinned;
  `MemoryEngine::unpin_fact` is public API); `store::fact_vectors::promote_space` (reconstruction: every vector is new;
  `below = 1`). Pinning a fact bumps the generation without a rewind (`bump_consolidation_generation`): a plan that
  chose it as a loser before it was pinned must not expire it, and `apply_dedup` does not recheck the pin. A JSON restore never
  imports the watermark or the generation (`MANAGED_CONFIG_KEYS` in `crates/memory-engine/src/inspect/restore.rs`), because restore resets each
  sequence to the highest surviving id (`reset_autoincrement`), so an erased id can be reused.
- Upgrade: no watermark under the new key, so the first run compares every active fact once (bounded by
  `MAX_FACTS_FOR_DEDUP`, 50,000) and drops `last_consolidated_at`. No binary runs `MemoryEngine::consolidate` today; #1123
  is its first production caller.
- An unparsable key is `MigrationError::Incompatible("invalid last_consolidated_fact_id: …")`.
- Rejected: a per-row `dedup_seq` column kept by triggers (a v19 migration that #1126/#1026 may contend for, a dump-format
  change, an `AFTER UPDATE` trigger beside `facts_fts_au`, a Postgres column; its only gain is re-comparing k facts instead
  of every fact above a rewound watermark after a rare unforget or reconstruction). Rejected: a commit timestamp (still a
  clock, the defect class).
- Postgres: `PgBackend` implements only `SchemaManager` (`crates/me-backend-postgres/src/lib.rs`), no `ConsolidationStore`,
  so nothing changes there. Its IDENTITY values are allocated at insert, not commit, so a future Postgres consolidation
  (#634) cannot reuse "id above the watermark" as is: a collateral note for #634.
- Alternative for O3: the id selection alone, with no rewinds, no generation, no survivor hold and the re-check kept:
  the unforget, unpin and reconstruction sequences then still leave duplicates, a stale plan can still expire facts after
  a reconstruction, and the false #409 comment is corrected instead.

### D4 (#1095): `not found` is a normal answer, recorded without an error

`set_level::run`'s out-of-reach arm (missing, negative, root, another bucket or project, an expired or ended memory)
records `{fact_ids: [id], level}` under the calling bucket and answers `Ok(NOT_FOUND)`, as `forget` does. No `error`, no
`from`/`to`: `docs/reference/mcp-server.md` says a `set_level` record is a move only when it carries both. A failed record
write, argument errors, storage errors and the racing move (`memory N changed while it was being moved`, reachable only
for a memory the session owns) stay error answers. Owner point O5: the model-facing description ends "Answers where it is
now, or not found.", mirroring `forget`'s (it changes #1082's approved text by three words).

### D5 (#1089, owner point O4): rename every identifier that names the decay prune

| From | To |
|---|---|
| `MemoryEngine::forget(&self, &ForgetPolicy) -> Result<PruneStats>` | `MemoryEngine::prune(&self, &PrunePolicy) -> Result<PruneStats>` |
| `DreamCtx::forget` (`me-traits`), impls `EngineDreamCtx`, `CycleContext` (`crates/me-cognitive/src/context.rs`), me-cognitive `TestEngine` | `DreamCtx::prune` |
| `ForgetPolicy` in `me_types::types::forgetting`; re-export `memory_engine::ForgetPolicy` | `PrunePolicy` in `me_types::types::pruning`; `memory_engine::PrunePolicy` |
| crate `me-forget` (`crates/me-forget/`) | `me-prune` (`crates/me-prune/`); `me_prune::prune` keeps its name |
| facade alias `crate::forgetting`, module `engine::forgetting` (`engine/forgetting.rs`) | `crate::pruning`, `engine::pruning` |
| the primitive **Forget** in the five-primitive lists (AGENTS.md, CLAUDE.md, GEMINI.md, README.md, `docs/index.md`, `docs/getting-started/core-concepts.md`, the facade crate doc) | **Prune** |
| `memory-engine-mcp/clippy.toml` entry `MemoryEngine::forget` | `MemoryEngine::prune`, same reason |
| prune-named tests and helpers (`forget_prunes_stale_facts`, `*_aggressive_forget*`, `aggressive_forget_policy`, `unpin_allows_forgetting`, `bench_forgetting`, `setup_forgettable_engine`, …) | `prune_*`/`*_prune*`; a pinned fact is "never pruned", not "unforgettable" (it can be forgotten) |

Kept: `PruneStats`, `prune_atomic`, the CLI `prune`; every retraction name (`forget` tool, `forget_fact`, `ForgetOutcome`,
`ExpiredReason::Forgotten`); "forgetting" where it names the Ebbinghaus model (prose, `docs/advanced/forgetting.md`'s
title and path, ADR-0006); ADRs, `docs/ROADMAP.md` and `utils/qa/` runs (records of their time); the `area:forgetting`
label and its tooling (an issue taxonomy); the retrieval-eval text in `tests/eval/corpus.rs` (data: a rename changes
eval results). No aliases or deprecations: an alias keeps a forget-name on the prune, and every consumer is in this
repository. Settles #1087 D18, which deferred this rename here. Cost: about 46 files; `Cargo.lock` changes, which brings
the image build into the gate. Alternative for O4: rename only the facade/trait methods and `ForgetPolicy`, keeping
`me-forget` and the `forgetting` modules (about 20 fewer files, no `Cargo.lock` change; the crate keeps the retraction's
verb).

### D6 (#1099, owner point O6): measure, then fix by a rule written before the data

Read in testcontainers 0.27.3 and bollard 0.20.2:

| Phase | Limit | Configurable | Error |
|---|---|---|---|
| pull (only after a 404 from create) | bollard's per-request 120 s | no | `ClientError::PullImage` |
| create (`create_container`, outside the startup block of `runners/async_runner.rs`) | bollard's per-request 120 s (`core/client/bollard_client.rs` `DEFAULT_TIMEOUT`) | no: not in testcontainers' API, environment or properties | `Client(ClientError::CreateContainer(RequestTimeoutError))` |
| start, readiness, exec | 60 s (`DEFAULT_STARTUP_TIMEOUT`) | `ImageExt::with_startup_timeout` | `WaitContainer(WaitContainerError::StartupTimeout)` |

What the code cannot say: whether podman was slow or the answer was lost client-side (testcontainers shares one Docker
client process-wide, `core/client/factory.rs`, so with `--test-threads=3` a test can send on connections another test's
runtime opened). Item 1 measures; item 2 applies the variant its table selects. If nothing reproduces, or a variant's
bound is exceeded, the numbers go to Michaël before any value is chosen; the infrastructure message lands either way.

### Owner points (settled by approving this plan; the defaults are the recommendations)

- **O1** harden #1062 (D1) · **O2** fold in the promotion twin (D2) · **O3** the id selection with its rewinds, the
  survivor hold and the redundant re-check deleted (D3; touches #1021's approved apply-time check) · **O4** the full
  rename, crate included (D5) · **O5** `set_level`'s description gains "or not found" (D4) · **O6** #1099's parameters
  come from item 1's numbers (D6) · **O7** item 1 runs a cargo build and a podman loop beside the Postgres suite inside
  one hold of the heavy-run lock: an exception to "one heavy run at a time" that stays one job as far as other workers
  see, needed to reproduce the load the issue observed.

## Implementation sequence

Every cargo and podman command runs from the worktree root, unpiped, under the machine-wide heavy-run lock with
`CARGO_BUILD_JOBS=16` (written `$H` below; the wrapper lives outside this repository). Each member item is one commit,
its behavioural red written against today's API and watched failing before any compile-red test naming a new symbol.

- [ ] **0. Plan and baseline (operational).** `git worktree list --porcelain` shows the worktree locked. Commit this plan
  with `git add -f` (`docs/plans/` is locally excluded); the `-drafts/` directory is deleted, never committed.
  `$H cargo test --workspace --all-features` passes at the base, so every later red belongs to this plan. Commit:
  `docs(plans): plan follow-up group A2 (#1127)`.

- [ ] **1. #1099: measure what times out (operational, no commit).** One `$H` session, run detached; raw logs in a
  scratch directory outside the repository. The lock keeps every other worker's heavy job out; inside this one hold the
  load and the suite run together on purpose (owner point O7), bounded by `CARGO_BUILD_JOBS=16` and one podman loop, and
  the session aborts if `/proc/pressure/memory` `some avg10` exceeds 50 (the host's OOM-kill
  threshold); scratch instrumentation reverted with
  `git checkout -- crates/me-backend-postgres/src/tests.rs`.
  - Instrumentation: `start_pg` labels its container `me.test=<thread>-<run>` (`ImageExt::with_label`) and writes one
    stderr line per call: RFC 3339 `t0` before `start()`, then outcome (`ok`, or the error's `Debug` and `Display`) and
    elapsed to `t1`. Suite: `cargo test -p me-backend-postgres --features test-util -- --ignored --nocapture
    --test-threads=N`.
  - Recorded once: `podman version`, storage driver, cgroup version, `nproc`, memory, the testcontainers/bollard versions
    in `Cargo.lock`, whether `pgvector/pgvector:pg17` is present.
  - Recorded throughout: `podman events --format json --filter type=container` (create/start/remove times with labels:
    `tc`, the server-side create); each second `/proc/loadavg` and `/proc/pressure/{cpu,io,memory}`; after each run the
    `org.testcontainers.managed-by=testcontainers` containers left (leaks), then removed.
  - Profiles, load started before and held through the runs: **B** none (3 runs, 3 threads); **L2** a cold
    `cargo test --workspace --all-features --no-run` of this tree into a scratch `CARGO_TARGET_DIR`, restarted whenever it
    ends (10 runs, 3 threads); **L3** L2 plus a loop of `podman run --rm --entrypoint true pgvector/pgvector:pg17`
    (10 runs, 3 threads); **C** the worse of L2/L3 at `--test-threads=1` (5 runs); **P** L2 with the image removed first
    (1 run: the pull path).
  - Decision table (registered now):

    | Observed | Cause | Item 2 variant |
    |---|---|---|
    | create `Timeout error` at `t1 − t0` ≈ 120 s, `tc` late (> 120 s), failures also in C | podman's create exceeds 120 s | **R**: retry creation only |
    | create `Timeout error` with no `tc` event at all | inconclusive: podman never created it, or the event stream missed it | stop: the numbers go to Michaël |
    | create `Timeout error` at `t1 − t0` ≈ 120 s with a late `tc` (> 120 s) under 3 threads, and no create failure in C | the suite's own concurrent creates queue in podman | **M**: serialize creation in the suite |
    | create timeouts whose `tc` came within 120 s, none in C | answer lost client-side | **S**: one runtime for the suite |
    | `container startup timeout` (60 s) | readiness under load | **T**: a measured start timeout |
    | `failed to pull the image` in P | registry or network | none: the message names it; the PR says to pre-pull |
    | no failure in L2, L3, P | not reproduced | stop: the numbers go to Michaël |

    Each row's conditions are complete as written. When the runs match more than one row (for example R in one profile and
    S in another), or none, stop: the numbers go to Michaël.

    The event stream is valid only if every successful create in the same runs has its `tc`; otherwise every row that
    reads `tc` is inconclusive. R's attempts = `1 + ⌈max(tc − t0) / 120 s⌉` over every run with a `tc`; above 3, stop and
    bring the numbers. T's timeout = twice
    the slowest start-to-ready under load, rounded up to 30 s, applied only when that exceeds 30 s.
  - Summary for the commit and the PR: per profile and thread count, runs, failed runs, failed creates with error text,
    `t1 − t0`, `tc − t0`, leaks; p50/p95/max of `tc − t0` and `t1 − tc` for successes; the pressure reached.

- [ ] **2. #1099: the harness tolerates what item 1 measured and names an infrastructure failure.** File:
  `crates/me-backend-postgres/src/tests.rs` (module doc, `start_pg`, new helpers and their unit tests).
  - Red: plain `#[test]`s, not ignored, no runtime needed: `an_infrastructure_failure_says_so` (for
    `TestcontainersError::Client(ClientError::CreateContainer(bollard::errors::Error::RequestTimeoutError))`, 2 attempts
    and an elapsed time, the text contains `infrastructure failure, not a backend failure`, the error, the attempt count
    and `no backend code ran`); variant R adds `only_a_creation_failure_is_retried` (true for `CreateContainer`; false for
    `WaitContainer(StartupTimeout)` and `PullImage`). `$H cargo test -p me-backend-postgres --features test-util --
    failure` does not compile.
  - Change, common: `fn infrastructure_failure(error: &TestcontainersError, attempts: u32, elapsed: Duration) -> String`;
    `start_pg` and its `get_host_port_ipv4` panic with it instead of today's `expect`. Types come from
    `testcontainers::core::error` and `testcontainers::bollard`: no new dependency, `Cargo.lock` unchanged.
  - Variant R: `const CREATE_ATTEMPTS: u32` (item 1's count, derivation in its doc comment);
    `fn is_create_failure(&TestcontainersError) -> bool` (`Client(ClientError::CreateContainer(_))` only); `start_pg`
    retries `start()` on it alone, one stderr line per retry naming the label filter that cleans a container the runtime
    still creates after the client gave up (item 1's leak count decides whether `start_pg` removes it before retrying).
  - Variant M: `static CREATE: tokio::sync::Mutex<()>` held around `start()`, so the queue waits client-side, outside
    bollard's per-request timeout. Variant S: one `static RUNTIME: LazyLock<tokio::runtime::Runtime>`; each live test
    becomes `#[test] #[ignore = …]` running `RUNTIME.block_on(async { … })`. Variant T: `.with_startup_timeout(…)` with
    the figures in its comment.
  - Green: `$H cargo test -p me-backend-postgres --features test-util -- failure` passes; then the profile that failed
    most, 10 reruns (20 if it failed fewer than 3 of 10 before), 0 failures.
  - Commit: `test(postgres): <variant's verb> and name an infrastructure failure`; body: item 1's table.

- [ ] **3. #1080: dedup selects new facts by fact id.**
  - Files: `crates/me-types/src/types/consolidation.rs`; `crates/me-backend-sqlite/src/{consolidation/mod.rs,store/facts.rs,store/tombstones.rs,store/fact_vectors.rs,sqlite/retraction.rs}`;
    `crates/me-consolidate/src/{pipeline.rs,pipeline/dedup.rs}`; `crates/me-storage/src/{consolidation.rs,retraction.rs,schema.rs}`
    (contract docs); `crates/memory-engine/src/inspect/restore.rs`; `crates/memory-engine/src/engine/tests.rs`; docs below.
  - Red first, against today's API (connection-level in `pipeline.rs`, so a write can land between load and apply):
    `a_fact_committed_after_the_snapshot_is_compared_at_the_next_run` (A; a run; load S; insert B, A's vector,
    `t_created = S.now − 1 h`; compute and apply S; the next run removes 1); `a_fact_embedded_after_the_snapshot_is_compared`
    (A; a run; B vectorless with `t_created` 10 days back; load S; give B its vector; apply S, held; the next run removes
    1); facade `engine/tests.rs` with `DeterministicEmbedder`: `dedup_compares_a_backdated_fact` (a run; "cats meow" with
    `opts.t_created` 2000-01-01; a run removes 1), `dedup_compares_an_unforgotten_fact` (A; a run; `forget_fact` A; B with
    A's text; a run merges nothing; `unforget_fact` A; a run removes 1), `dedup_compares_every_fact_after_a_reconstruction`
    (two distinct facts; a run removes 0; reconstruct to `MockEmbedder`, whose vectors are equal; a run removes 1), and the
    guard `dedup_compares_a_reused_id_after_a_restore` (a run; `erase` the highest; JSON dump and restore; a new fact takes
    the erased id, asserted; a run removes 1). `$H cargo test -p me-consolidate --lib after_the_snapshot` and
    `$H cargo test -p memory-engine --all-features --lib dedup_compares` fail on all but the restore guard (0 removed).
  - Then, red by compilation: `dedup.rs` `only_facts_above_the_watermark_drive` replaces
    `only_compares_new_facts_against_active` (a fact with the oldest `t_created` but an id above drives);
    `consolidation/mod.rs` `an_unforget_between_load_and_apply_discards_the_plan` (snapshot with the watermark absent;
    unforget; apply writes nothing; the next run removes 1), `a_reconstruction_between_load_and_apply_discards_the_stale_plan`
    (two facts similar in the old space and dissimilar in the new; load and compute; promote the new space; apply expires
    nothing and leaves the watermark absent), `a_lost_survivor_holds_the_watermark`,
    `the_first_run_drops_the_timestamp_watermark`; `retraction.rs` `unforget_rewinds_the_dedup_watermark`;
    `facts.rs` `unpinning_rewinds_the_dedup_watermark` (pinning bumps the generation only),
    `pinning_a_planned_loser_between_load_and_apply_discards_the_plan` (the loser stays active); `fact_vectors.rs`
    `promote_clears_the_dedup_watermark`; facade `dedup_compares_an_unpinned_fact` (A pinned; B, A's text; a run merges
    nothing; `unpin_fact` A; a run removes 1). Existing tests read the new key:
    `the_watermark_holds_while_a_fact_without_a_vector_waits`, `a_waiting_fact_that_failed_is_compared_as_new_once_embedded`,
    and in `pipeline.rs` `watermark_written_after_success`, `watermark_read_scopes_dedup_to_new_facts`,
    `invalid_watermark_in_config_errors`, `empty_engine_consolidates_to_noop`, `watermark_not_advanced_when_dedup_skipped`,
    `over_both_caps_skips_load_and_is_noop`, `failing_pass_rolls_back_entire_transaction`.
  - Change (D3): `Snapshot::{watermark, max_fact_id, generation}`, `ConsolidationPlan::{watermark, max_fact_id, generation}`;
    `store::facts::{DEDUP_WATERMARK, CONSOLIDATION_GENERATION, read_dedup_watermark, read_consolidation_generation,
    rewind_dedup_watermark, bump_consolidation_generation}`, `FactStore::{max_id, set_pinned}` (`set_pinned` writes in one
    transaction with its rewind or bump, only on a real transition);
    `consolidation::{load_snapshot_capped, apply_plan}`; `pipeline::dedup::compute_dedup`, `pipeline::compute_plan_capped`
    (the over-both-caps branch keeps the stored watermark); `SqliteBackend::unforget_fact` (via `store::tombstones`);
    `store::fact_vectors::promote_space`; `MANAGED_CONFIG_KEYS` gains both keys and its doc says why.
  - Re-entry audit: `rg -n "SET embedding|SET t_expired = " crates/me-backend-sqlite/src` — every non-test hit is a fill
    (covered by the hold), `promote_space` or `unforget`, or is named in the PR with its reason; likewise every
    `SET is_pinned` (only `set_pinned`).
  - Green: `$H cargo test -p me-consolidate`, `$H cargo test -p me-backend-sqlite --all-features`, `$H cargo test -p
    memory-engine --all-features`. Mutation checks, each reverted: drop the generation check, the survivor hold, the
    restore exclusion, the unforget rewind, the unpin rewind, the pin bump, the promote reset; each makes its named test fail.
  - Commit: `fix(consolidate)!: select dedup's new facts by fact id, not t_created`.

- [ ] **4. Boy-scout: consolidation docs name code that does not exist.** If still true at implementation:
  `crates/me-backend-sqlite/src/consolidation/mod.rs`'s module doc places `compute_plan` in a facade module that no
  longer exists (it is `me-consolidate`, through the `ConsolidationStore` port); `docs/advanced/consolidation.md`
  "Graph Rebuild" shows `MemoryGraph::load_from_db(&conn)`, while `me_consolidate::consolidate` rebuilds with
  `MemoryGraph::from_active_edges`. No executable behaviour; checks: `$H cargo doc --no-deps -p me-backend-sqlite` and
  the sphinx build add no warning. Commit: `docs(consolidation): name the compute half and the graph rebuild as they are`.

- [ ] **5. #1062: a cycle report declares the identity of its vectors (dropped under O1's alternative).**
  - Files: `crates/me-types/src/types/cycle_report.rs`, `crates/me-types/src/error.rs`;
    `crates/me-backend-sqlite/src/sqlite/consolidation.rs`; `crates/me-cognitive/src/{llm_impl.rs,default_impl.rs,apply.rs}`;
    `crates/me-test-support/src/atomic.rs`; `crates/memory-engine/src/engine/cognitive.rs`;
    `crates/memory-engine-cli/tests/consolidate.rs`; docs below.
  - Red first, against today's API: `llm_backend_refuses_a_summary_embedder_of_another_identity` (the e2e flags of
    `llm_backend_synthesizes_via_wiremock_end_to_end`, whose `--embed-model nomic-embed-text` differs from the store's
    stamp): non-zero exit, JSON `outcome` `failed` with an `error` naming the mismatch, the three facts active and none
    added. `$H cargo test -p memory-engine-cli --all-features --test consolidate another_identity` fails (`outcome` is
    `ran`, one synthesized).
  - Then, red by compilation: me-types serde tests (`None` is skipped; JSON without the key reads `None`; the round trip
    with `Some`), the error Display test with `UndeclaredEmbedding`; `apply.rs`
    `an_undeclared_vector_is_refused_and_nothing_is_written` (`AddFact`, then `Synthesize`: `Cycle(UndeclaredEmbedding)`, no
    fact added, sources active, `last_dream_cycle_at` absent), `a_vector_declared_under_another_identity_is_refused`,
    `a_report_without_vectors_needs_no_identity`; `llm_impl.rs` `the_report_declares_the_embedders_identity`.
  - Change (D1): `CycleReport::embedding_fingerprint`; `CycleError::UndeclaredEmbedding`;
    `SqliteBackend::apply_cycle_deltas_atomic`; `LlmDreamCycle::run`, `DefaultDreamCycle::run`;
    `MemoryEngine::apply_cycle_report` carries the field, and both `apply_cycle_report`s' `# Errors` name the two
    refusals; every other `CycleReport` literal states the field (test helpers declare their store's identity); the
    existing e2e test passes the store's identity in `--embed-model`/`--embed-provider`. Guard:
    `tests/redaction_write_paths.rs::a_scrubbing_proposer_hands_the_cycle_redacted_summaries` still passes (the field
    survives the facade's rebuild).
  - Green: `$H cargo test -p me-types`, `-p me-cognitive`, `-p me-backend-sqlite --all-features`, `-p memory-engine
    --all-features`, `-p memory-engine-cli --all-features --test consolidate`.
  - Commit: `fix(cycle)!: a cycle report declares the identity of the vectors it carries`.

- [ ] **6. Boy-scout (O2): a promotion declares its vector.** Files: `crates/me-types/src/types/cognitive.rs`
  (`PromoteRequest`), `crates/me-storage/src/consolidation.rs` (`ConsolidationStore::promote_atomic` gains
  `declared: &EmbeddingFingerprint`), `crates/me-backend-sqlite/src/sqlite/consolidation.rs`
  (`SqliteBackend::promote_atomic`), `crates/memory-engine/src/engine/cognitive.rs` (`MemoryEngine::promote_with_lineage`,
  `EngineDreamCtx::promote`), `crates/me-cognitive/src/{context.rs,test_support.rs}`, and every `PromoteRequest` literal.
  The cycle report's `Promote` delta copies a stored vector inside `apply_cycle_deltas_atomic` and is unchanged. Red (by
  compilation: today's request has no declaration to test): `promote_atomic_refuses_another_identity` beside
  `promote_atomic_refuses_forgotten_content` (a store stamped `mock`/`test`, a request declaring another model:
  `EmbeddingModelMismatch`, nothing written) and `promote_atomic_accepts_the_stores_identity`. Green: `$H cargo test -p
  me-backend-sqlite --all-features` and `-p memory-engine --all-features`. Commit: `fix(cycle)!: a promotion declares the
  identity of its vector`.

- [ ] **7. Boy-scout: an LLM consolidation checks its embedder before it calls the LLM.** Files:
  `crates/memory-engine-cli/src/commands/consolidate.rs` (`run`, the `Llm` arm), `crates/memory-engine-cli/tests/consolidate.rs`,
  `docs/reference/cli-inspector.md`. Red: `llm_backend_refuses_a_summary_embedder_of_another_identity` now expects exit
  1, stderr `embedding identity check failed`, empty stdout and the `/api/generate` mock `.expect(0)`;
  `consolidate_failure_emits_json_and_exits_nonzero` passes the store's identity so it still reaches its dimension
  failure. Change: after building the embedder, `engine.verify_embedding_identity(&*embedder)`, its error as
  `embedding identity check failed: {e}` (`query.rs`'s wording), before the run: a setup error, so no JSON. Green:
  `$H cargo test -p memory-engine-cli --all-features --test consolidate`. Commit: `fix(cli): check the summary
  embedder's identity before an LLM consolidation`.

- [ ] **8. #1095: `set_level` answers not found as `forget` does.**
  - Files: `crates/memory-engine-mcp/src/tools/set_level.rs` (`definition`, `run`),
    `crates/memory-engine-mcp/tests/{forget.rs,project_scope.rs,lean_surface.rs}`, `docs/reference/mcp-server.md`.
  - Red: `forget_and_set_level_answer_not_found_alike` (in `forget.rs`, growing from
    `not_found_is_the_same_for_every_out_of_reach_fact`): in a project session, for 999,999, −1, a root fact, a fact in
    another machine's bucket, and a fact of another project that `recall(project:)` returned, `forget` and
    `set_level(…, "user")` each answer as a non-error with the body exactly `not found`, byte-equal between the tools;
    afterwards each fact is active in its scope with no tombstone, and each `set_level` record has `fact_ids: [id]` and
    neither `error` nor `from`. `$H cargo test -p memory-engine-mcp --all-features --test forget not_found_alike` fails
    (`set_level` answers `error: not found`).
  - Change (D4): the out-of-reach arm calls `super::record(…, "set_level", {fact_ids, level}, &ctx.bucket())` and returns
    `Ok(NOT_FOUND.to_owned())`; the description per O5. Updated: `project_scope.rs::set_level_never_reaches_another_home`,
    `concurrent_moves_leave_a_replayable_log` (drops its `error: not found` alternative),
    `lean_surface.rs::every_answer_starts_with_its_call_id` (`set_level` 999,999 not failed).
  - Green: `$H cargo test -p memory-engine-mcp --all-features`. Commit: `fix(mcp): set_level answers not found as forget
    does, not as an error`.

- [ ] **9. #1089: rename the decay prune.** First `git fetch && git rebase origin/main`, so the rename covers whatever
  #1126 or #1026 merged.
  - Files: every path the D5 table names, plus `crates/me-storage/src/graph.rs`, `crates/me-backend-sqlite/src/sqlite/graph.rs`
    (`types::pruning::PruneStats`), `crates/memory-engine-cli/src/commands/prune.rs`, root `Cargo.toml` (members),
    `Cargo.lock`, `crates/memory-engine/Cargo.toml`, `tests/`, `tests/eval/` (except `corpus.rs`), `examples/custom_traits.rs`,
    `benches/lifecycle_bench.rs`, `fuzz/` if it names one, and the docs below; the compiler and the sweep (Verification)
    find a miss.
  - Red: `crates/memory-engine/src/lib.rs` `reexports_are_accessible` names `crate::PrunePolicy` and
    `crate::types::pruning::{PrunePolicy, PruneStats}`; `$H cargo test -p memory-engine --all-features --lib
    reexports_are_accessible` does not compile.
  - Change (D5), renamed tests included.
  - Green: `$H cargo build --workspace --all-features` (rewrites `Cargo.lock`, committed here), `$H cargo test --workspace
    --all-features` (every renamed prune test passes, no other test changes outcome), `$H cargo +nightly fuzz build`.
    Guard proof: a temporary `MemoryEngine::prune` call in `crates/memory-engine-mcp/src/tools/mod.rs` makes
    `$H cargo clippy -p memory-engine-mcp --all-targets --all-features -- -D warnings` fail naming that path; reverted.
  - Commit: `refactor(forgetting)!: rename the decay prune from forget to prune`.

- [ ] **10. Rebase and Verification (operational).** `git fetch && git rebase origin/main`; if #1126 or #1026 changed a
  file the rebase surface names, rerun that item's green commands; then Verification in full at the clean head.

- [ ] **11. PR, review, finish (operational).** `gh pr create`, body: `Classification: Bug fix (user-confirmed)`,
  `Execution path: Standard`, `Planning tier: Super`, `Post-implementation review: Full (Default severity)`; the owner
  points as decided; item 1's table; deploy notes (the first consolidation after the upgrade compares every active fact
  once and drops `last_consolidated_at`; after an `import` the first consolidation is a full pass; a custom `DreamCycle`
  that emits `AddFact`/`Synthesize` declares its embedder; `consolidate --backend llm` needs `--embed-*` naming the store's
  identity; the renamed public API; `set_level`'s `not found` is a success); Collateral findings; the Verification table;
  `Closes #1062` (or `Refs #1062` under O1's alternative), `Closes #1080`, `Closes #1089`, `Closes #1095`, `Closes #1099`,
  `Refs #1127`, `Refs dutiona/infra#37`. Then `/super-review` at the Full gate (findings fixed in their own commits,
  Verification rerun), and `/finish-pr`, which archives this plan and removes `docs/plans/`. Michaël merges.

### Rebase surface (for #1126 and #1026)

| Crate | Changed | Item |
|---|---|---|
| me-types | `CycleReport::embedding_fingerprint`, `CycleError::UndeclaredEmbedding` | 5 |
| | `PromoteRequest::embedding_fingerprint` | 6 |
| | `Snapshot::{watermark, max_fact_id, generation}` (replaces `last`), `ConsolidationPlan::{watermark, max_fact_id, generation}` | 3 |
| | `types::forgetting` → `types::pruning`, `ForgetPolicy` → `PrunePolicy` | 9 |
| me-traits | `DreamCtx::forget` → `DreamCtx::prune` | 9 |
| me-storage | `ConsolidationStore::promote_atomic` gains `declared`; contract docs of `apply_cycle_deltas_atomic`, `load_consolidation_snapshot`, `apply_plan`, `Retraction::unforget_fact`, `SchemaManager::promote_space` | 3, 5, 6 |
| me-backend-sqlite | `SqliteBackend::{apply_cycle_deltas_atomic, promote_atomic, unforget_fact}`; `consolidation::{load_snapshot_capped, apply_plan}`; `store::facts::{DEDUP_WATERMARK, CONSOLIDATION_GENERATION, read_dedup_watermark, read_consolidation_generation, rewind_dedup_watermark, bump_consolidation_generation, FactStore::max_id, FactStore::set_pinned}`; `store::fact_vectors::promote_space` | 3, 5, 6 |
| me-consolidate | `pipeline::dedup::compute_dedup`, `pipeline::compute_plan_capped` | 3 |
| me-cognitive | `LlmDreamCycle::run`, `DefaultDreamCycle::run`; `CycleContext`/`TestEngine` `promote` and `prune` | 5, 6, 9 |
| me-prune (was me-forget) | crate and directory renamed; contents unchanged | 9 |
| memory-engine | `MemoryEngine::{apply_cycle_report, promote_with_lineage}`; `MemoryEngine::forget` → `prune`, `EngineDreamCtx::prune`, re-export `PrunePolicy`, `crate::pruning`, `engine::pruning`; `inspect::restore::MANAGED_CONFIG_KEYS` | 3, 5, 6, 9 |
| memory-engine-mcp | `tools::set_level::{definition, run}`; `clippy.toml` | 8, 9 |
| memory-engine-cli | `commands::consolidate::run`; `commands::prune` (types only) | 7, 9 |
| me-backend-postgres | `tests::start_pg` and its new helpers | 2 |

No migration; config keys `last_consolidated_fact_id` and `consolidation_generation` new, `last_consolidated_at` retired. #1126's restore work touches
`inspect/restore.rs` (one line here) and may touch `embedding_meta::check_compatible` and `verify_embedding_identity`, which
items 5–7 call unchanged. #1026: no shared file; its uploads are exactly the backdated facts item 3 makes comparable.
#1123 must build its `LlmDreamCycle` with the server's configured embedder so item 5's declaration matches.

## Documentation

- #1099: the `crates/me-backend-postgres/src/tests.rs` module doc: the run command, what an infrastructure failure means
  (no backend code ran; check the runtime, rerun), the variant's one-line why.
- #1080: `docs/advanced/consolidation.md` "Incremental Operation": new facts are those above `last_consolidated_fact_id`,
  why ids (commit order; backdated and late facts), when it advances, holds, rewinds, clears, and that restore never
  imports it; `docs/advanced/reconstruction.md`: the next consolidation compares every fact; `docs/reference/cli-inspector.md`:
  after `unforget`, the restored facts are compared again, after `import` the first consolidation is a full pass. Rustdoc
  on every changed item above. ADR-0005 left as written.
- #1062 and items 6–7: `docs/advanced/dream-cycle.md` (the report declares its embedder's identity; undeclared or foreign
  vectors are refused; promotions too); `docs/reference/cli-inspector.md` (`consolidate --backend llm`: `--embed-*` must
  name the store's identity, checked before the LLM is called). Rustdoc: `CycleReport::embedding_fingerprint`,
  `CycleError::UndeclaredEmbedding`, `PromoteRequest::embedding_fingerprint`, both `apply_cycle_report`s, `LlmDreamCycle`.
- #1095: `docs/reference/mcp-server.md`: the `set_level` paragraph (`not found` as `forget` answers it, for every
  out-of-reach or no-longer-live memory) and the call-record paragraph (`fact_ids` and `level`, `from` and `to` only for a
  move).
- #1089: the D5 rows' docs (`docs/reference/{api.md,glossary.md,crate-layout.md,cli-inspector.md}`,
  `docs/advanced/{forgetting.md,extensibility.md,dream-cycle.md,consolidation.md,bi-temporal-semantics.md}`,
  `docs/getting-started/core-concepts.md`, `docs/index.md`, `docs/usage/*.md` that name the prune,
  `docs/design/{architecture-overview.md,design-choices.md}` identifiers only), `AGENTS.md` (primitives line, crate tree),
  `CLAUDE.md`, `GEMINI.md`, `README.md`.

## Testing

TDD per item. Layers: pure helpers (#1099's message and classifier, `compute_dedup`); backend SQL (the generation
check, rewinds, legacy key, the apply's and promotion's identity checks); pipeline (load/compute/apply
interleavings); facade (backdated, unforgotten, reconstruction, restore; the LLM report through `apply_cycle_report`); CLI
binary (`consolidate`); HTTP (`set_level`/`forget`); the live Postgres suite under measured load. Mutation checks prove
each #1080 rule is load-bearing. Excluded: a live LLM or embedder (wiremock stands in); Postgres data paths (none exist
before #634).

| Acceptance | Tests | Item |
|---|---|---|
| #1062 vectors declare a matching identity; refused clearly; failing test first | `llm_backend_refuses_a_summary_embedder_of_another_identity` (red first), `an_undeclared_vector_is_refused_and_nothing_is_written`, `a_vector_declared_under_another_identity_is_refused`, `the_report_declares_the_embedders_identity`, serde tests | 5, 7 |
| #1080 failing first: created before the watermark, committed or embedded after the snapshot | `a_fact_committed_after_the_snapshot_is_compared_at_the_next_run`, `a_fact_embedded_after_the_snapshot_is_compared`, `dedup_compares_a_backdated_fact` | 3 |
| #1080 selection by a monotonic sequence | `only_facts_above_the_watermark_drive`, `dedup_compares_an_unforgotten_fact`, `dedup_compares_every_fact_after_a_reconstruction`, `dedup_compares_a_reused_id_after_a_restore`, `dedup_compares_an_unpinned_fact`, `an_unforget_between_load_and_apply_discards_the_plan`, `a_reconstruction_between_load_and_apply_discards_the_stale_plan`, `a_lost_survivor_holds_the_watermark`, `the_first_run_drops_the_timestamp_watermark`, `invalid_watermark_in_config_errors` | 3 |
| #1089 `prune` everywhere, `forget` only the retraction; no behaviour change | `reexports_are_accessible`, the sweep, the clippy guard proof, every renamed prune test passing | 9 |
| #1095 alike, another project included; one shared test | `forget_and_set_level_answer_not_found_alike` | 8 |
| #1099 what times out, on what limit | item 1's table | 1 |
| #1099 tolerates a slow start, creation only; infrastructure message | the variant's reruns with 0 failures, `only_a_creation_failure_is_retried` (R), `an_infrastructure_failure_says_so` | 2 |

## Verification

At the clean head after item 10, each command under `$H`, unpiped, one at a time; each exits 0, every test summary 0
failed:

```bash
cargo build --workspace --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --all-features
cargo test  --workspace --all-features
cargo check -p memory-engine
cargo check -p memory-engine --no-default-features --features backend-sqlite
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links" cargo doc --no-deps -p memory-engine
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links" cargo doc --no-deps -p memory-engine --all-features
cargo deny check
uv run --locked sphinx-build -b html -W --keep-going -E docs docs/_build
cargo +nightly fuzz build
cargo test -p me-backend-postgres --features test-util -- --ignored --test-threads=3
cargo +1.89 build --workspace --tests --examples
cargo +1.89 build --workspace --tests --examples --all-features
```

`Cargo.lock` changes (item 9), so the image is built from the committed tree as AGENTS.md prescribes (`git archive` |
`podman build --pull=newer --layers=false --memory=32g --cpu-period=100000 --cpu-quota=1600000 -t memory-engine:verify -`),
both binaries answer `--version` under `podman run --rm --network=none`, then `podman rmi memory-engine:verify`. The
plugin gate does not apply: nothing under `plugins/` changes. CI is not dispatched; this local matrix is the gate.

Structural checks:

- #1089, printing nothing: `rg -n -w 'ForgetPolicy|me_forget' crates tests examples benches fuzz` and
  `rg -n 'me-forget|types::forgetting|crate::forgetting|\.forget\(&' crates tests examples benches fuzz docs AGENTS.md CLAUDE.md GEMINI.md README.md --glob '!docs/design/adr/**' --glob '!docs/ROADMAP.md' --glob '!docs/plans/**'`.
  Reviewed line by line: `rg -n -i 'unforgettable|forgettable|forgetting'` over the same set, every hit in a kept class of D5.
- #1080: `rg -n 'last_consolidated_at' crates` names only `apply_plan`'s deletion and its test.

## Collateral findings (for the PR and the worker report)

- **Recommend filing:** a JSON restore resets each `AUTOINCREMENT` sequence to the highest surviving id, so after `erase`
  or archival of the highest ids an `import` reuses them: call records (`fact_ids`, an erase record's `linked`) then name
  unrelated new facts, and #209's caller-write cursor can sit at a reused id. Item 3 does not rely on it. Touches
  `inspect/restore.rs`, which #1126 also edits.
- **For #634 (relayed, not posted by the worker):** Postgres IDENTITY values are not commit-ordered, so a Postgres
  `ConsolidationStore` needs a commit-ordered sequence for dedup and for #209's cursor, and must enforce a report's and a
  promotion's declared identity.
- **For #1123:** ids make a precise vectorless hold possible (stop below the first vectorless id), so a failed item stops
  costing a full re-comparison every night; and its `LlmDreamCycle` must use the server's embedder.
