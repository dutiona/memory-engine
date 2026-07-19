# memory-engine — Agent Instructions

> For OpenAI Codex, Devin, and other autonomous coding agents.

## What This Is

Embedded Rust library (edition 2024, **Rust 1.88+** — the workspace uses let-chains) providing durable long-term memory for AI agents. Single facade type `MemoryEngine` (async-native; DB methods are `async fn` over an `Arc<dyn StorageBackend>` — `tokio` is non-optional) with 5 primitives: Ingest, Query, Consolidate, Forget, Resolve. Zero LLM/network dependencies — consumers implement traits (`EmbeddingProvider`, `SummaryGenerator`, `ConflictArbiter`, `PersistenceClassifier`, `Reranker`).

Part of a four-layer cognitive architecture. Companion repos: `knowledge-base` (Python MCP server, Knowledge layer), `autonomous-agent-project` (research, no code).

## Setup

```bash
cargo build --workspace
```

No external services needed — SQLite is bundled via `rusqlite`.

## Verify

⚠️ **CI is `workflow_dispatch`-only — it does NOT run on push or PR** (#989). This matrix is therefore the **primary** verification, not a pre-check with CI as a backstop: nothing runs automatically to catch you skipping it. `.github/workflows/ci.yml` runs these same commands *when dispatched* (`gh workflow run ci.yml --ref <branch>`). A local pass that diverges from them — weaker features, narrower scope, or piped output — is a **false pass**, and now an *undetected* one. Treat a PR with no CI run as **unverified by machine**:

```bash
cargo fmt --all --check                                                # Format
cargo clippy --workspace --all-targets --all-features -- -D warnings   # Clippy (deny warnings)
cargo build --workspace                                                # Build (default features)
cargo build --workspace --all-features                                 # Build (all features)
cargo test  --workspace --all-features                                 # Test — --all-features is mandatory, or ann/archive/eval tests never run
cargo check -p memory-engine                                           # Facade-alone, default features — the TRUE archive-OFF gate (#978)
cargo check -p memory-engine --no-default-features --features backend-sqlite  # Facade-alone, no-default
export RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links"
cargo doc --no-deps -p memory-engine                                   # Docs, default features (core crate only)
cargo doc --no-deps -p memory-engine --all-features                    # Docs, all features
cargo deny check                                                       # Supply-chain (advisories/licenses/bans/sources)
cargo +nightly fuzz build                                              # Fuzz gate (#993) — detached fuzz/ consumes the facade API; needs nightly + cargo-fuzz
```

Run all of them after every change. Do not skip any, and do not substitute a weaker variant — with CI on manual dispatch, there is no second chance to catch it. The last line is the **facade-API gate** (#993): `fuzz/` is a detached workspace that no `--workspace` command compiles, yet it consumes the facade public API by path — so a removal or rename can break it silently. It needs nightly + cargo-fuzz (`just fuzz-build` wraps it) and is incremental (a seconds-long no-op when the facade is unchanged). The workflow also carries an **MSRV** job — `cargo +1.88 build --workspace --tests --examples` (default _and_ all-features); reproduce it locally if you touch let-chains or edition-sensitive code.

**Dispatching and monitoring CI** (it will not run itself):

```bash
gh workflow run ci.yml --ref <branch>       # dispatch
gh run list --workflow=ci.yml --limit 1     # find the run
gh run watch <run-id>                       # follow it
```

**Critical:** The workspace contains **18 crates** — the Wave 2 (#816) decomposition is done (S1–S5): a strictly acyclic layered DAG, every dependency pointing **downward**, enforced by `cargo`.

```
L0    me-types            data + error vocabulary
L0.5  me-traits           consumer/contract traits (EmbeddingProvider, DreamCtx, CycleCtx, …)
L1    me-storage          persistence PORT: StorageBackend family + MemoryCtx + spawn_join_err
L2    me-index            backend-free MemoryGraph / ScopeTree projections
      me-backend-sqlite / me-backend-postgres
L3    me-ingest  me-query  me-consolidate  me-forget  me-resolve  me-archive  me-cognitive
L4    memory-engine       the facade (MemoryEngine + builder + bootstrap + inspect + re-exports)
```

Plus `me-test-support` (dev-only) and the three facade consumers: `memory-engine-cli`, `memory-engine-mcp`, `memory-engine-embed`.

An L3 primitive depends on the **port**, never a concrete backend — the facade selects it. Changes to `me-types`' error/type vocabulary, `me-traits`' signatures, or the facade's public API can break the CLI, MCP, and embed crates silently if only one crate is checked. **Always use `--workspace`.** Module map: `docs/reference/crate-layout.md`.

**Verification traps** (each cost a real rework cycle):

1. **Never pipe a cargo gate through `head`/`tail`** — truncation hides RED results and the pipe drops cargo's exit code → false green. Run unpiped or redirect to a file. (`grep` doesn't truncate but also masks the exit code — check `${PIPESTATUS[0]}` if you must filter.)
2. **`clippy --all-features` compiles tests but does not run them** — only `cargo test --all-features` runs them.
3. **`cargo build` green ≠ tests green** for file moves / `include_str!` / `[[test]]` registration — only `cargo test` catches a dark test target.
4. **Triage findings against current `main`, not the issue snapshot** — file:lines drift across reorgs; a "magic constant" or "dead" item may be an intentional sentinel or epic-foundation (`git log -S` + check the roadmap before changing it).

## Project Layout

The workspace is a virtual root (no root package); crates live under `memory-engine/`. Paths are **`memory-engine/lib/memory-engine/src/…`**, not `src/…` (the #814 bin/lib reorg).

```
memory-engine/
  lib/memory-engine/src/   # core crate (the library)
    lib.rs                 # crate root, re-exports; backend compile_error! guard
    engine/                # MemoryEngine facade, EngineConfig, conflict resolution (was engine.rs + conflict/)
    types/                 # core data types (Event, Fact, Edge, Summary, enums) — split into submodules
    traits.rs              # consumer-implemented traits (zero LLM/network deps in core)
    error.rs               # MemoryError enum (thiserror)
    store/                 # original SQLite stores (events, facts, edges, summaries, scopes)
    storage/               # #628 pluggable StorageBackend: sqlite/ (live) + postgres/ (skeleton)
    search/                # hybrid retrieval (FTS5 + vector + HNSW, RRF merge)
    graph/                 # in-memory petgraph knowledge graph
    consolidation/         # 3-pass pipeline (dedup, cluster, global)
    forgetting/            # Ebbinghaus decay + multi-signal importance
    pool/                  # ConnectionPool (N readers + 1 writer)
    scope/                 # hierarchical scope tree
    resume/                # 4-tier cognitive boot (pinned → importance → due → recent)
    bootstrap/             # Claude Code JSONL session import
    inspect/               # debugging APIs (explain, replay, dump, restore, statistics)
    limits.rs              # resource/size guards
  lib/embed/               # memory-engine-embed (HTTP embedding provider + HttpDeltaProposer)
  bin/cli/                 # memory-engine-cli (inspector binary)
  bin/mcp/                 # memory-engine-mcp (MCP server)
tests/                     # workspace integration tests
benches/                   # Criterion benchmarks
examples/                  # runnable examples
fuzz/                      # cargo-fuzz targets (detached workspace, nightly-only)
utils/                     # scripts (github-pm tooling, hooks)
docs/                      # Sphinx narrative documentation
```

## Rules

1. **Never commit secrets** — no API keys, tokens, or .env files
2. **TDD** — write the failing test first (`#[test]` + integration tests in `tests/`), then implement
3. **Conventional Commits** — `feat:`, `fix:`, `refactor:`, `docs:`, `test:`, `chore:`, imperative mood
4. **Atomic commits** — one logical change per commit
5. **`unsafe_code = "forbid"`** — no unsafe code. The crate forbids it via Cargo.toml lints.
6. **Soft deletion only** — facts are expired (`t_expired` set), never hard-deleted. Full audit trail.
7. **Event-sourced** — the append-only event log is the source of truth. Facts are consumer-derived via `add_fact`.
8. **Trait boundary** — never add LLM or network dependencies to the core crate. All intelligence comes through consumer traits.
9. **Error handling** — `thiserror` for all error types, `Result<T, MemoryError>` everywhere. No `unwrap()` in library code.
10. **Document public items** — rustdoc with `# Errors` and `# Examples` sections on all public API items.

## Architecture Docs

For deeper context, read these files (only when working on the relevant area):

- `docs/reference/crate-layout.md` — module map
- `docs/design/architecture-overview.md` — threading model, data flow diagrams
- `docs/design/design-choices.md` — rationale for key decisions
- `docs/ROADMAP.md` — frozen historical reference (phase record + research→design narrative); live status, open issues, and dependency graph → [GitHub Projects #6](https://github.com/users/dutiona/projects/6)
- `docs/design/adr/` — 9 Architecture Decision Records

<!-- pm-contract:start -->

## Issue Labeling Convention

This repository is managed with a label-routed GitHub Projects-v2 system. Every issue you open or triage MUST follow this contract. The canonical label lists live in `utils/scripts/github-pm/manifests/labels.core.json` (cross-repo) and `utils/scripts/github-pm/manifests/labels.area.memory-engine.json` (per-repo); the operator guide is `docs/reference/project-management.md`.

### Title format

Use Conventional-Commits grammar: `type(area): description` (e.g. `refactor(retrieval): consolidate RRF merge ordering`). The `area` in the title MUST match the `area:*` label. The `type` maps to the `type:*` label by stem — most are 1:1 (`refactor`→`type:refactor`, `docs`→`type:docs`), with the conventional aliases `fix`→`type:bug`, `feat`→`type:feature`, `perf`→`type:refactor`.

### Hard rule

Every issue MUST carry **exactly one** `type:*` label and **exactly one** `area:*` label. Epics (`type:epic`) MAY omit `area:*` when they span multiple areas. `type:*` labels are **mutually exclusive** — never apply two. `severity:*`, `status:*`, `priority:*`, and `super-qa*` labels are **additive** — apply as many as apply.

### `type:*` — pick exactly one (mutually exclusive)

| Label              | Meaning                                      |
| ------------------ | -------------------------------------------- |
| `type:bug`         | Something broken                             |
| `type:feature`     | New capability                               |
| `type:enhancement` | Improve existing functionality               |
| `type:refactor`    | Restructure, no behavior change (incl. perf) |
| `type:test`        | Test coverage or infrastructure              |
| `type:chore`       | Deps, config, cleanup                        |
| `type:docs`        | Documentation only                           |
| `type:plan`        | Implementation plan (links to PR)            |
| `type:epic`        | Umbrella tracking issue with sub-issues      |
| `type:research`    | Investigation, design-spec, spike, PoC       |
| `type:security`    | Security finding or hardening                |

### `area:*` — pick exactly one (epics may omit)

| Label                | Meaning                                                    |
| -------------------- | ---------------------------------------------------------- |
| `area:core`          | Engine facade, config, and cross-cutting core types        |
| `area:storage`       | Persistence, SQLite store, snapshots, cold storage         |
| `area:retrieval`     | Hybrid search: FTS5 + vector + graph, RRF merge            |
| `area:consolidation` | Consolidation pipeline: dedup, cluster, global passes      |
| `area:forgetting`    | Ebbinghaus decay and multi-signal importance               |
| `area:temporal`      | Bi-temporal semantics, conflict resolution, prediction     |
| `area:cognitive`     | Phase 5 cognitive pipelines: DreamCycle, provenance        |
| `area:knowledge`     | Knowledge-layer integration (knowledge-base)               |
| `area:cli`           | CLI inspector binary (memory-engine-cli)                   |
| `area:mcp`           | MCP server binary (memory-engine-mcp)                      |
| `area:docs`          | Documentation and narrative docs                           |
| `area:build`         | Build system, CI, tooling, dependencies                    |
| `area:qa`            | Quality assurance, testing infrastructure, super-qa sweeps |
| `area:viz`           | Phase-7 web UI / visualization (#13)                       |

### Additive labels (apply zero or more)

- `severity:*` — `critical`, `high`, `medium`, `low`, `info` (super-qa severity).
- `status:*` — `blocked` (upstream/external dependency), `parked` (no immediate timeline), `needs-design` (design before implementation).
- `priority:*` — `critical`, `high` (convenience labels; the full P0–P4 scale lives in the Projects `Priority` field).
- `super-qa*` — `super-qa` (finding from a `/super-qa` audit), `super-qa:auto-fix`, `super-qa:debated`, `super-qa:security-fallback`.

### Linking sub-issues to an epic

Epics own sub-issues via GitHub's native sub-issue relationship. Use the `addSubIssue` GraphQL mutation with `replaceParent: true` so re-parenting is idempotent (a sub-issue can be moved under a new epic without first detaching it). Resolve the node IDs from owner `dutiona`, repo `memory-engine`:

```graphql
mutation {
  addSubIssue(
    input: {
      issueId: "<EPIC_NODE_ID>"
      subIssueId: "<CHILD_NODE_ID>"
      replaceParent: true
    }
  ) {
    issue {
      number
    }
    subIssue {
      number
    }
  }
}
```

Resolve a node ID with: `gh api graphql -f query='query{repository(owner:"dutiona",name:"memory-engine"){issue(number:<N>){id}}}'`.

### Collateral issues

If you discover a problem unrelated to the issue you are working on (a side finding, an incidental bug, a follow-up), do NOT fold it into the current issue or PR. File a **separate** issue, give it its own `type:*` + `area:*` labels per this contract, and link it to the relevant epic (via `addSubIssue` if it belongs under one). Keep one logical concern per issue.

### Project routing (automatic)

- **Memory Engine — Main** (`users/dutiona/projects/4`): every issue and PR is auto-added on open/reopen/label.
- **Bug & Security Triage** (`users/dutiona/projects/5`): issues labeled `type:bug` or `type:security` are also auto-added.
- **Roadmap** (`users/dutiona/projects/6`): curated — items are added by maintainers, not automatically.

Routing keys off the labels above, so getting `type:*` right is what places an issue on the correct board.

<!-- pm-contract:end -->
