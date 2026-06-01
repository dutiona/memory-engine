# memory-engine

Embedded Rust library providing durable long-term memory for autonomous AI agents. Runs in-process (no external database servers), exposes a single facade type `MemoryEngine`, and has zero LLM/network dependencies — all intelligence is injected by the consumer via traits.

## Why

Part of a **four-layer cognitive architecture** (Knowledge → Memory → Wisdom → Intelligence). This crate materializes the **Memory** layer: internalized facts that decay via Ebbinghaus curves, consolidate into patterns, and (Phase 5) promote to Wisdom. It does not own knowledge (that's `knowledge-base`) or intelligence (that's the model).

Companion repos:

- **knowledge-base** (`~/dev/knowledge-base`): Python MCP server, Knowledge layer. Phase 1 ✅ → Phase 2.
- **autonomous-agent-project** (`~/dev/autonomous-agent-project`): Research repo, no code. Holds the four-layer thesis.

## Commands

```bash
cargo build                           # debug build (root crate only)
cargo build -p memory-engine-cli      # CLI inspector binary
cargo build -p memory-engine-mcp      # MCP server binary
cargo test                            # all tests (root crate only)
cargo test -p memory-engine-cli       # CLI integration tests
cargo test -p memory-engine-mcp       # MCP server tests
cargo test --all-features             # with async + HNSW + compression
cargo clippy --all-targets            # lint (pedantic + nursery)
cargo fmt --check                     # format check
cargo bench                           # Criterion benchmarks
cargo doc --no-deps --open            # API reference
uv run sphinx-build -b html docs docs/_build  # narrative docs (Python 3.12+)
```

Feature flags: `async` (tokio), `ann` (HNSW vector search), `archive` (cold storage .pak files), `compress-gzip`, `compress-zstd`.

**Workspace verification gate** — run before every commit, especially when modifying `error.rs`, `types.rs`, `traits.rs`, `lib.rs`, or any public API:

```bash
cargo build --workspace               # ALL crates compile
cargo test --workspace                # ALL crates' tests pass
cargo clippy --workspace --all-targets # ALL crates lint-clean
```

The workspace contains 3 crates: `memory-engine` (core), `memory-engine-cli`, `memory-engine-mcp`. The CLI and MCP crates consume the core's public API — changes to error variants, type definitions, or trait signatures can break them silently if only the root crate is checked.

## Architecture

Five primitives: **Ingest** (append-only event log) → **Query** (hybrid FTS5 + vector + graph, RRF merge) → **Consolidate** (3-pass: dedup → cluster → global) → **Forget** (Ebbinghaus decay + multi-signal importance) → **Resolve** (bi-temporal conflict arbitration).

Consumer traits — engine delegates all LLM/network ops:

| Trait                   | Purpose                                     |
| ----------------------- | ------------------------------------------- |
| `EmbeddingProvider`     | Compute text embeddings                     |
| `SummaryGenerator`      | Generate cluster summaries + embed them     |
| `ConflictArbiter`       | Decide CRUD action for contradicting facts  |
| `PersistenceClassifier` | Auto-pin unforgettable facts                |
| `Reranker`              | Cross-encoder reranking on top-K candidates |

Read these docs when working on the relevant area:

| Area                                                                       | Doc                                      |
| -------------------------------------------------------------------------- | ---------------------------------------- |
| Module map                                                                 | `docs/reference/crate-layout.md`         |
| Architecture + threading model                                             | `docs/design/architecture-overview.md`   |
| Design rationale + trade-offs                                              | `docs/design/design-choices.md`          |
| Research basis (15 papers)                                                 | `docs/design/research-basis.md`          |
| ADRs (9 decisions)                                                         | `docs/design/adr/`                       |
| Phase history + research→design narrative (⚠️ status → GitHub Projects #6) | `docs/ROADMAP.md`                        |
| Phase 5 design (cognitive)                                                 | `docs/design/plans/`                     |
| Bi-temporal semantics                                                      | `docs/advanced/bi-temporal-semantics.md` |
| Consolidation pipeline                                                     | `docs/advanced/consolidation.md`         |
| Hybrid search tuning                                                       | `docs/advanced/hybrid-search.md`         |
| Schema evolution policy                                                    | `docs/design/schema-evolution-policy.md` |
| CLI inspector usage                                                        | `docs/reference/cli-inspector.md`        |
| MCP server usage                                                           | `docs/reference/mcp-server.md`           |
| Project management (labels/PM)                                             | `docs/reference/project-management.md`   |

## Key Design Decisions

1. **One store, multiple projections** — `fact_type` is a tag, not a partition. All facts share one table, one FTS5 index, one vector space.
2. **Event-sourced** — append-only event log is source of truth. Facts are consumer-derived (explicit `add_fact`), not auto-projected.
3. **Soft deletion** — facts are expired (`t_expired` set), never hard-deleted. Full audit trail for temporal reasoning.
4. **Bi-temporal** — 4 timestamps per fact: `t_created`/`t_expired` (system), `t_valid`/`t_invalid` (real-world). From Graphiti.
5. **`unsafe_code = "forbid"`** — no unsafe code anywhere.
6. **Read-only open path** — `EngineConfig::read_only` opens without write capability. Defense in depth: file existence check + `validate_schema_version()` (read-only) + SQLite `query_only` pragma + Rust-level `try_write()` guard.

## Status

Phase 4a ✅ (inspection APIs, import/export, bootstrap, reranker). Phase 4b in progress: CLI inspector ✅ (`memory-engine-cli`), MCP server ✅ (`memory-engine-mcp`, 10 P0 tools + tiered depth). P1 tools (#95) and P2 tools (#96) pending. Then Phase 5 (cognitive pipelines — DreamCycle, outcome tracking, provenance). See `docs/ROADMAP.md` for open issues and dependency graph.

<!-- pm-contract:start -->

## Issue Labeling Convention

This repository is managed with a label-routed GitHub Projects-v2 system. Every issue you open or triage MUST follow this contract. The canonical label lists live in `scripts/github-pm/manifests/labels.core.json` (cross-repo) and `scripts/github-pm/manifests/labels.area.memory-engine.json` (per-repo); the operator guide is `docs/reference/project-management.md`.

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
