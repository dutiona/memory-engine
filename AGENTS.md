# memory-engine — Agent Instructions

> For OpenAI Codex, Devin, and other autonomous coding agents.

## What This Is

Embedded Rust library (edition 2024, Rust 1.85+) providing durable long-term memory for AI agents. Single facade type `MemoryEngine` with 5 primitives: Ingest, Query, Consolidate, Forget, Resolve. Zero LLM/network dependencies — consumers implement traits (`EmbeddingProvider`, `SummaryGenerator`, `ConflictArbiter`, `PersistenceClassifier`, `Reranker`).

Part of a four-layer cognitive architecture. Companion repos: `knowledge-base` (Python MCP server, Knowledge layer), `autonomous-agent-project` (research, no code).

## Setup

```bash
cargo build --workspace
```

No external services needed — SQLite is bundled via `rusqlite`.

## Verify

```bash
cargo build --workspace               # ALL 3 crates must compile
cargo test --workspace                # ALL 3 crates' tests must pass
cargo clippy --workspace --all-targets # ALL 3 crates must lint-clean (pedantic + nursery)
cargo fmt --check                     # must pass — no reformats
```

Run all four commands after every change. Do not skip any.

**Critical:** The workspace contains 3 crates: `memory-engine` (core lib), `memory-engine-cli`, `memory-engine-mcp`. Changes to `error.rs`, `types.rs`, `traits.rs`, or any public API in the core crate can break the CLI and MCP crates silently if only the root crate is checked. Always use `--workspace`.

## Project Layout

```
src/
  lib.rs              # Crate root, re-exports
  engine.rs           # MemoryEngine facade, EngineConfig
  types.rs            # Core data types (Event, Fact, Edge, Summary, enums)
  traits.rs           # Consumer-implemented traits
  error.rs            # MemoryError enum (thiserror)
  store/              # SQLite persistence (schema, events, facts, edges, summaries, scopes)
  search/             # Hybrid retrieval (FTS5, vector, HNSW, RRF merge)
  graph/              # In-memory petgraph knowledge graph
  consolidation/      # 3-pass pipeline (dedup, cluster, global)
  forgetting/         # Ebbinghaus decay + importance scoring
  conflict/           # Bi-temporal conflict resolution
  pool/               # ConnectionPool (N readers + 1 writer)
  scope/              # Hierarchical scope tree
  resume/             # 5-tier cognitive boot (pinned → importance → due → recent → kb_stubs)
  bootstrap/          # Claude Code JSONL session import
  inspect/            # Debugging APIs (explain, replay, dump, restore, statistics)
  async_engine.rs     # AsyncMemoryEngine (tokio, feature-gated)
tests/                # Integration tests
benches/              # Criterion benchmarks
examples/             # 3 runnable examples
docs/                 # Sphinx narrative documentation
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
- `docs/ROADMAP.md` — phase status, open issues, dependency graph
- `docs/design/adr/` — 9 Architecture Decision Records

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
