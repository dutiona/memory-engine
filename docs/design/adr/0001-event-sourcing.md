# ADR-0001: Append-only Event Log as Source of Truth

**Status:** Accepted
**Date:** 2026-03-10

## Context

The memory engine stores facts derived from AI agent interactions. These facts are created, updated, expired, consolidated, and forgotten over the engine's lifetime. Several requirements constrain how state changes are recorded:

1. **Audit trail.** AI agent actions must be inspectable after the fact. When a fact is expired or a conflict resolved, operators need to understand what happened and why.
2. **Replay capability.** The storage backend (currently SQLite) may change. The research evaluated SurrealDB, LanceDB, Neo4j, and Qdrant (ROADMAP, Storage Technology Decisions). The engine must not be locked into any single backend.
3. **No surveyed system provides auditability.** Graphiti performs "non-lossy dynamic updates" but has no formal event log. Mem0 overwrites state via CRUD. A-Mem self-organizes without history. None offer replay.
4. **Consolidation is lossy.** The Memory Survey (2512.13564) notes that "semantic summarization is lossy compression -- prioritizes global coherence over local precision." Raw events must survive summarization.

The multi-AI debate (Entry 8, Research Journal) reached consensus: "Event-sourced log as source of truth. Graph is DERIVED, never source of truth."

## Decision

The engine uses an append-only event log (`events` table) as the single source of truth. All state mutations -- fact creation, expiry, conflict resolution, consolidation, forgetting -- are recorded as events before the derived state (facts, edges, summaries) is updated.

Facts are consumer-derived via explicit `add_fact()` calls, not auto-projected from events. The engine does not maintain automatic read-model projections. This keeps the event log simple (no projection rebuild machinery) while preserving the audit trail.

The `Event` type carries: timestamp, event_type (Interaction, ToolCall, MemoryOp, SystemEvent), JSON payload, source identifier, optional session_id, and scope_id.

## Consequences

### Positive

- Any future storage backend can be populated by replaying the event log. Migration to SurrealDB, LanceDB, or a hypothetical successor requires only a new projection, not a data migration.
- Full audit trail for debugging agent behavior. Every fact's provenance is traceable to its source event (`source_event_id` on `Fact`).
- Consolidation and forgetting can be aggressive without data loss. Raw events are never deleted.

### Negative

- Storage grows monotonically. Events accumulate without bound.
- No automatic projections means consumers must explicitly call `add_fact()` to materialize knowledge from events. The engine does not "learn" from events on its own.
- Replay is currently theoretical -- no replay tooling exists yet (planned for Phase 4: `replay_events()` API).

### Mitigations

- Phase 4 plans archival compression (cold storage `.pak` files) and import/export with gzip/zstd for old events.
- The explicit `add_fact()` boundary is intentional: it keeps the engine as a storage/retrieval layer, not an interpretation layer. The consumer (AI agent) decides what constitutes a fact.
