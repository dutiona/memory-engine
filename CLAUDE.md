# memory-engine

Embedded Rust library providing durable long-term memory for autonomous AI agents. Runs in-process (no external database servers), exposes a single facade type `MemoryEngine`, and has zero LLM/network dependencies — all intelligence is injected by the consumer via traits.

## Why

Part of a **four-layer cognitive architecture** (Knowledge → Memory → Wisdom → Intelligence). This crate materializes the **Memory** layer: internalized facts that decay via Ebbinghaus curves, consolidate into patterns, and (Phase 5) promote to Wisdom. It does not own knowledge (that's `knowledge-base`) or intelligence (that's the model).

Companion repos:

- **knowledge-base** (`~/dev/knowledge-base`): Python MCP server, Knowledge layer. Phase 1 ✅ → Phase 2.
- **autonomous-agent-project** (`~/dev/autonomous-agent-project`): Research repo, no code. Holds the four-layer thesis.

## Commands

```bash
cargo build                           # debug build
cargo build -p memory-engine-cli      # CLI inspector binary
cargo build -p memory-engine-mcp      # MCP server binary
cargo test                            # all tests
cargo test -p memory-engine-cli       # CLI integration tests
cargo test -p memory-engine-mcp       # MCP server tests
cargo test --all-features             # with async + HNSW + compression
cargo clippy --all-targets            # lint (pedantic + nursery)
cargo fmt --check                     # format check
cargo bench                           # Criterion benchmarks
cargo doc --no-deps --open            # API reference
uv run sphinx-build -b html docs docs/_build  # narrative docs (Python 3.12+)
```

Feature flags: `async` (tokio), `ann` (HNSW vector search), `compress-gzip`, `compress-zstd`.

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

| Area                           | Doc                                      |
| ------------------------------ | ---------------------------------------- |
| Module map                     | `docs/reference/crate-layout.md`         |
| Architecture + threading model | `docs/design/architecture-overview.md`   |
| Design rationale + trade-offs  | `docs/design/design-choices.md`          |
| Research basis (15 papers)     | `docs/design/research-basis.md`          |
| ADRs (9 decisions)             | `docs/design/adr/`                       |
| Roadmap + phase status         | `docs/ROADMAP.md`                        |
| Phase 5 design (cognitive)     | `docs/design/plans/`                     |
| Bi-temporal semantics          | `docs/advanced/bi-temporal-semantics.md` |
| Consolidation pipeline         | `docs/advanced/consolidation.md`         |
| Hybrid search tuning           | `docs/advanced/hybrid-search.md`         |
| CLI inspector usage            | `docs/reference/cli-inspector.md`        |
| MCP server usage               | `docs/reference/mcp-server.md`           |

## Key Design Decisions

1. **One store, multiple projections** — `fact_type` is a tag, not a partition. All facts share one table, one FTS5 index, one vector space.
2. **Event-sourced** — append-only event log is source of truth. Facts are consumer-derived (explicit `add_fact`), not auto-projected.
3. **Soft deletion** — facts are expired (`t_expired` set), never hard-deleted. Full audit trail for temporal reasoning.
4. **Bi-temporal** — 4 timestamps per fact: `t_created`/`t_expired` (system), `t_valid`/`t_invalid` (real-world). From Graphiti.
5. **`unsafe_code = "forbid"`** — no unsafe code anywhere.
6. **Read-only open path** — `EngineConfig::read_only` opens without write capability. Defense in depth: file existence check + `validate_schema_version()` (read-only) + SQLite `query_only` pragma + Rust-level `try_write()` guard.

## Status

Phase 4a ✅ (inspection APIs, import/export, bootstrap, reranker). Phase 4b in progress: CLI inspector ✅ (`memory-engine-cli`), MCP server ✅ (`memory-engine-mcp`, 10 P0 tools + tiered depth). P1 tools (#95) and P2 tools (#96) pending. Then Phase 5 (cognitive pipelines — DreamCycle, outcome tracking, provenance). See `docs/ROADMAP.md` for open issues and dependency graph.
