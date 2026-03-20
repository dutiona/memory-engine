# memory-engine — Gemini Instructions

> For Google Gemini CLI.

## What This Is

Embedded Rust library (edition 2024, Rust 1.85+) providing durable long-term memory for AI agents. Single facade type `MemoryEngine` with 5 primitives: Ingest, Query, Consolidate, Forget, Resolve. Zero LLM/network dependencies — consumers inject intelligence via traits (`EmbeddingProvider`, `SummaryGenerator`, `ConflictArbiter`, `PersistenceClassifier`, `Reranker`).

Part of a four-layer cognitive architecture (Knowledge → Memory → Wisdom → Intelligence). This crate is the Memory layer. Companion repos: `knowledge-base` (`~/dev/knowledge-base`, Python MCP server, Knowledge layer), `autonomous-agent-project` (`~/dev/autonomous-agent-project`, research, no code).

## Commands

```bash
cargo build                           # debug build
cargo test                            # all tests
cargo test --all-features             # with async + HNSW + compression
cargo clippy --all-targets            # lint (pedantic + nursery enabled)
cargo fmt --check                     # format check
cargo bench                           # Criterion benchmarks
cargo doc --no-deps --open            # API reference
```

All three checks (test, clippy, fmt --check) must pass before any commit.

Feature flags: `async` (tokio wrapper), `ann` (HNSW vector search), `compress-gzip`, `compress-zstd`.

## Project Structure

Source lives in `src/`. Each module owns its domain:

- `engine.rs` — `MemoryEngine` facade, `EngineConfig`. All public methods take `&self`; thread-safe via `ConnectionPool` + `RwLock`.
- `types.rs` — Core data types: `Event`, `Fact`, `Edge`, `Summary`, `ScopeNode`, enums, option structs.
- `traits.rs` — Consumer-implemented traits. Engine has zero network/LLM deps.
- `error.rs` — `MemoryError` enum with `thiserror`. `Result<T>` alias.
- `store/` — SQLite persistence: schema + migrations, `EventStore`, `FactStore`, `EdgeStore`, `SummaryStore`, `ScopeStore`.
- `search/` — Hybrid retrieval: FTS5 (BM25) + vector (cosine/HNSW) + RRF merge (k=60). `MemoryQuery` fluent builder.
- `graph/` — `MemoryGraph` (petgraph `DiGraph`), loaded from SQLite on startup.
- `consolidation/` — 3-pass pipeline: local dedup → cluster fusion → global integration.
- `forgetting/` — Ebbinghaus decay + multi-signal importance scoring.
- `conflict/` — Bi-temporal conflict resolution via `ConflictArbiter` trait.
- `pool/` — `ConnectionPool`: N readers + 1 writer, `parking_lot::Mutex`.
- `scope/` — `ScopeTree` hierarchical cache. Paths like `"user:michael/project:demo"`.
- `resume/` — 5-tier cognitive boot: pinned → high_importance → due → recent → kb_stubs.
- `bootstrap/` — Parse Claude Code JSONL session logs into historical facts.
- `inspect/` — Debugging APIs: `explain_fact`, `fact_history`, `replay_events`, `dump_state`, `statistics`.
- `async_engine.rs` — `AsyncMemoryEngine` via `tokio::spawn_blocking` (feature-gated).

Tests in `tests/` (6 integration tests), benchmarks in `benches/`, examples in `examples/` (3).

## Conventions

- **No unsafe code** — `unsafe_code = "forbid"` in Cargo.toml lints
- **Error handling** — `thiserror` derivation, `Result<T, MemoryError>` everywhere
- **Soft deletion** — facts are expired (`t_expired` set), never hard-deleted
- **Event-sourced** — append-only event log is source of truth; facts are consumer-derived
- **Trait boundary** — no LLM/network deps in core; all intelligence via consumer traits
- **Commits** — Conventional Commits (`feat:`, `fix:`, `refactor:`), imperative mood, atomic changes
- **Testing** — TDD approach; integration tests use in-memory SQLite via `MemoryEngine::open(":memory:")`

## Deep-Dive Docs

Read only when working on the relevant area:

| Topic                         | File                                     |
| ----------------------------- | ---------------------------------------- |
| Module map                    | `docs/reference/crate-layout.md`         |
| Architecture + threading      | `docs/design/architecture-overview.md`   |
| Design rationale + trade-offs | `docs/design/design-choices.md`          |
| Research basis (15 papers)    | `docs/design/research-basis.md`          |
| ADRs (9 decisions)            | `docs/design/adr/`                       |
| Roadmap + phase status        | `docs/ROADMAP.md`                        |
| Phase 5 design (cognitive)    | `docs/design/plans/`                     |
| Bi-temporal semantics         | `docs/advanced/bi-temporal-semantics.md` |
| Consolidation pipeline        | `docs/advanced/consolidation.md`         |
| Hybrid search tuning          | `docs/advanced/hybrid-search.md`         |
