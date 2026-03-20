# memory-engine — Agent Instructions

> For OpenAI Codex, Devin, and other autonomous coding agents.

## What This Is

Embedded Rust library (edition 2024, Rust 1.85+) providing durable long-term memory for AI agents. Single facade type `MemoryEngine` with 5 primitives: Ingest, Query, Consolidate, Forget, Resolve. Zero LLM/network dependencies — consumers implement traits (`EmbeddingProvider`, `SummaryGenerator`, `ConflictArbiter`, `PersistenceClassifier`, `Reranker`).

Part of a four-layer cognitive architecture. Companion repos: `knowledge-base` (Python MCP server, Knowledge layer), `autonomous-agent-project` (research, no code).

## Setup

```bash
cargo build
```

No external services needed — SQLite is bundled via `rusqlite`.

## Verify

```bash
cargo test --all-features             # must pass before any PR
cargo clippy --all-targets            # must pass — zero warnings (pedantic + nursery)
cargo fmt --check                     # must pass — no reformats
```

Run all three commands after every change. Do not skip any.

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
