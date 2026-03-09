# Round 3 — Claude: Final Positions

### 1. Scheduling API Surface

**Codex's `next_due_time()` + `drain_due(now)` + `resume_context(now)` wins.**

Gemini's `poll_temporal_events(now) -> Vec<TemporalEffect>` is too abstract — what is a `TemporalEffect`? It introduces a new concept for something that's just "facts that are now relevant."

My Round 2 position (`resume_context()` alone) was too minimal. Codex is right that consumers need different granularities:

- `resume_context(now)` — "rebuild my full working context" (session start)
- `drain_due(now)` — "give me just the newly-due future memories" (periodic check)
- `next_due_time()` — "when should I check again?" (scheduling hint)

The third one is key. Without it, consumers either poll wastefully or miss events. `next_due_time()` returns `Option<DateTime>` — the earliest `t_valid` among future-dated facts — so the consumer can set a precise timer. Zero overhead, maximum scheduling flexibility.

### 2. Phase 4 Content

**Codex's ordering is correct, but the "inspection tooling" shouldn't be a phase-level item.**

The right decomposition:

- Phase 4a: **MCP server adapter** — this is the primary network interface. It's what makes the engine usable by any AI agent framework. The MCP protocol already defines tool semantics that map 1:1 to our API (`ingest`, `add_fact`, `query`, `resume_context`, `consolidate`, `forget`).
- Phase 4b: **KB Protocol** — a `KnowledgeBaseConnector` trait + a reference implementation for research-index. The MCP server can expose KB-linked facts with "knowledge unavailable" annotations when the KB is unreachable.

Inspection tooling (CLI/TUI) is not a phase — it's a development aid that can ship incrementally alongside any phase. A `memory-engine-cli` crate with `inspect`, `dump`, `query` subcommands is a weekend project, not a milestone.

Gemini is right that KB Protocol is important but wrong to dismiss inspection tooling entirely — debugging blind is painful. The solution is: CLI inspector ships with Phase 3, not as a formal phase item.

### 3. Event Schema Forward-Compatibility for Sync

**Gemini is half-right. Add `origin_node_id` to events in Phase 3a. Defer the rest.**

The argument for: if we bake a single-writer event format now and sync needs multi-writer metadata later, every existing database needs a migration. Adding `origin_node_id: String` (defaulting to `"local"`) costs nothing and future-proofs the schema.

The argument against `sequence_id` and causal timestamps now: premature. We don't know the sync protocol yet. Vector clocks, Lamport timestamps, and hybrid logical clocks have different metadata requirements. Committing to one now might be wrong.

Compromise: add `origin_node_id` to the event envelope in Phase 3a (it's a one-column schema migration). Leave `sequence_id` and causal ordering to the sync project when it's designed.

## Final Roadmap

```
Phase 3a: "Kernel Hardening"
  - #5 Thread safety (Send + Sync wrapper)
  - #6 AddFactOptions builder (with unforgettable: bool flag)
  - Event schema: add origin_node_id field
  - CLI inspector (memory-engine-cli: inspect, dump, query)

Phase 3b: "Agent Lifecycle"
  - Unforgettable flag in schema + PersistenceClassifier trait
  - Materialized importance score (persisted, updated on access/consolidate)
  - Future memory: query filter on t_valid <= now
  - resume_context(now) + drain_due(now) + next_due_time()
  - Scoping mechanism for context filtering

Phase 4: "Knowledge Integration"
  - 4a: MCP server adapter (expose engine as MCP tool server)
  - 4b: KnowledgeBaseConnector trait + KB protocol spec
  - Reference implementation: research-index bridge crate
  - Graceful degradation (KnowledgeRef with retry semantics)

Phase 5: "Visualization"
  - Web UI as separate workspace member (memory-engine-viz)
  - WASM + Rust graph rendering
  - Scope filtering, fact editing, import/export
  - Event log timeline view

Deferred / Separate Projects:
  - Auth (deployment layer concern)
  - SaaS Sync (separate product: sync server + CRDT merge + key management)
  - #3 ANN index (benchmark-triggered)
  - #4 SQL-level filters (benchmark-triggered)
  - Hierarchical summarization
  - Async API wrapper
```
