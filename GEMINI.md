# memory-engine — Gemini Instructions

> For Google Gemini CLI.

## What This Is

Embedded Rust library (edition 2024, **Rust 1.88+** — the workspace uses let-chains) providing durable long-term memory for AI agents. Single facade type `MemoryEngine` (async-native; DB methods are `async fn` over an `Arc<dyn StorageBackend>`, so `tokio` is non-optional) with 5 primitives: Ingest, Query, Consolidate, Forget, Resolve. Zero LLM/network dependencies — consumers inject intelligence via traits (`EmbeddingProvider`, `SummaryGenerator`, `ConflictArbiter`, `PersistenceClassifier`, `Reranker`).

Part of a four-layer cognitive architecture (Knowledge → Memory → Wisdom → Intelligence). This crate is the Memory layer. Companion repos: `knowledge-base` (`~/dev/knowledge-base`, Python MCP server, Knowledge layer), `autonomous-agent-project` (`~/dev/autonomous-agent-project`, research, no code).

## Commands

```bash
cargo build                           # debug build (root crate only)
cargo test                            # all tests (root crate only)
cargo test --all-features             # with HNSW + archive + compression + test-util
cargo clippy --all-targets            # lint (pedantic + nursery enabled)
cargo fmt --check                     # format check
cargo bench                           # Criterion benchmarks
cargo doc --no-deps --open            # API reference
```

**Verification gate — the CI contract** (`.github/workflows/ci.yml`); run before every commit. These are the _exact_ commands CI runs. A local pass that diverges — weaker features, narrower scope, or **piped output** — is a **false pass**:

```bash
cargo fmt --all --check                                                # Format
cargo clippy --workspace --all-targets --all-features -- -D warnings   # Clippy (deny warnings)
cargo build --workspace                                                # Build (default features)
cargo build --workspace --all-features                                 # Build (all features)
cargo test  --workspace --all-features                                 # Test — --all-features is mandatory, or ann/archive/eval tests never run
export RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links"
cargo doc --no-deps -p memory-engine                                   # Docs, default features (core crate only)
cargo doc --no-deps -p memory-engine --all-features                    # Docs, all features
cargo deny check                                                       # Supply-chain (advisories/licenses/bans/sources)
```

CI also runs an **MSRV** job — `cargo +1.88 build --workspace --tests --examples` (default _and_ all-features); reproduce it if you touch let-chains or edition-sensitive code.

The workspace contains **4 crates**: `memory-engine` (core lib), `memory-engine-cli`, `memory-engine-mcp`, `memory-engine-embed`. Changes to `error.rs`, `types/`, `traits.rs`, or any public API in the core crate can silently break the CLI, MCP, and embed crates if only the root crate is checked. Always use `--workspace`.

**Verification traps** — never pipe a cargo gate through `head`/`tail` (truncation + dropped exit code → false green; `grep` doesn't truncate but also masks the exit code); `clippy --all-features` compiles tests but does not run them; `cargo build` green ≠ tests green for file moves / dark `[[test]]` targets; triage findings against current `main`, not the issue snapshot (a "magic constant" may be an intentional sentinel — `git log -S` first).

Feature flags: `backend-sqlite` (default, in-process SQLite), `backend-postgres` (`PgBackend` skeleton, gated tests), `ann` (HNSW vector search), `archive` (cold storage .pak files), `compress-gzip`, `compress-zstd`, `test-util` (cross-crate test-only hooks). At least one backend feature must be enabled. There is **no** `async` feature — `tokio` is non-optional (#702).

## Project Structure

Virtual workspace (no root package); crates live under `memory-engine/`. The core library is at **`memory-engine/lib/memory-engine/src/…`**, not `src/…` (the #814 bin/lib reorg). Each module owns its domain:

- `engine/` — `MemoryEngine` facade, `EngineConfig`, and bi-temporal conflict resolution (was `engine.rs` + `conflict/`). Async-native; DB methods `.await` an `Arc<dyn StorageBackend>`.
- `types/` — Core data types: `Event`, `Fact`, `Edge`, `Summary`, `ScopeNode`, enums, option structs (split into submodules).
- `traits.rs` — Consumer-implemented traits. Engine has zero network/LLM deps.
- `error.rs` — `MemoryError` enum with `thiserror`. `Result<T>` alias.
- `store/` — Original SQLite stores: schema + migrations, `EventStore`, `FactStore`, `EdgeStore`, `SummaryStore`, `ScopeStore`.
- `storage/` — #628 pluggable `StorageBackend` trait family: `sqlite/` (live) + `postgres/` (skeleton, `backend-postgres`).
- `search/` — Hybrid retrieval: FTS5 (BM25) + vector (cosine/HNSW) + RRF merge (k=60). `MemoryQuery` fluent builder.
- `graph/` — `MemoryGraph` (petgraph `DiGraph`), loaded from SQLite on startup.
- `consolidation/` — 3-pass pipeline: local dedup → cluster fusion → global integration.
- `forgetting/` — Ebbinghaus decay + multi-signal importance scoring.
- `pool/` — `ConnectionPool`: N readers + 1 writer.
- `scope/` — `ScopeTree` hierarchical cache. Paths like `"user:michael/project:demo"`.
- `resume/` — 4-tier cognitive boot: pinned → high_importance → due → recent.
- `bootstrap/` — Parse Claude Code JSONL session logs into historical facts.
- `inspect/` — Debugging APIs: `explain_fact`, `fact_history`, `replay_events`, `dump_state`, `statistics`.

Sibling crates: `lib/embed/` (`memory-engine-embed` — HTTP embedding + `HttpDeltaProposer`), `bin/cli/`, `bin/mcp/`. Workspace integration tests in `tests/`, benchmarks in `benches/`, examples in `examples/`, fuzz targets in `fuzz/` (detached, nightly).

## Conventions

- **No unsafe code** — `unsafe_code = "forbid"` in Cargo.toml lints
- **Error handling** — `thiserror` derivation, `Result<T, MemoryError>` everywhere
- **Soft deletion** — facts are expired (`t_expired` set), never hard-deleted
- **Event-sourced** — append-only event log is source of truth; facts are consumer-derived
- **Trait boundary** — no LLM/network deps in core; all intelligence via consumer traits
- **Commits** — Conventional Commits (`feat:`, `fix:`, `refactor:`), imperative mood, atomic changes
- **Testing** — TDD approach; integration tests use in-memory SQLite via `MemoryEngine::builder(embed_dim).build()`

## Deep-Dive Docs

Read only when working on the relevant area:

| Topic                                             | File                                     |
| ------------------------------------------------- | ---------------------------------------- |
| Module map                                        | `docs/reference/crate-layout.md`         |
| Architecture + threading                          | `docs/design/architecture-overview.md`   |
| Design rationale + trade-offs                     | `docs/design/design-choices.md`          |
| Research basis (15 papers)                        | `docs/design/research-basis.md`          |
| ADRs (9 decisions)                                | `docs/design/adr/`                       |
| Phase history (frozen; live status → Projects #6) | `docs/ROADMAP.md`                        |
| Phase 5 design (cognitive)                        | `docs/advanced/dream-cycle.md`           |
| Bi-temporal semantics                             | `docs/advanced/bi-temporal-semantics.md` |
| Consolidation pipeline                            | `docs/advanced/consolidation.md`         |
| Hybrid search tuning                              | `docs/advanced/hybrid-search.md`         |

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
