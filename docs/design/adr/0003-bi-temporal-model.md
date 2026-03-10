# ADR-0003: Graphiti-inspired 4-Timestamp Model

**Status:** Accepted
**Date:** 2026-03-10

## Context

Agent memory must handle temporal contradictions. A fact that was true yesterday may be false today. The literature shows a clear evolution in how memory systems handle this:

1. **Destructive replacement** -- Overwrite old value. No history. Used by early systems.
2. **Soft deletion with timestamps** -- Mark old facts as expired. Preserves history but conflates system time with real-world time.
3. **Bi-temporal modeling** -- Separate "when the system learned it" from "when it's true in the world." Graphiti (2501.13956) introduced this for knowledge graphs.

The Graphiti paper demonstrated that bi-temporal facts enable temporal queries ("what did the agent know at time T?") and future scheduling ("this fact becomes valid next week"). The Memory Survey (2512.13564) Entry 11 in the Research Journal confirmed: "adopt Graphiti's bi-temporal model."

Temporal contradiction resolution was identified as unsolved across all surveyed systems (Research Journal Entry 5: "Every system struggles with changing facts"). Bi-temporal modeling does not solve contradictions, but it provides the data model to reason about them.

## Decision

Every `Fact` carries 4 timestamps:

| Timestamp   | Axis        | Meaning                                       |
| ----------- | ----------- | --------------------------------------------- |
| `t_created` | System time | When the engine recorded this fact            |
| `t_expired` | System time | When the engine soft-deleted this fact        |
| `t_valid`   | Real-world  | When this fact becomes true in the real world |
| `t_invalid` | Real-world  | When this fact stops being true               |

Deletion is always soft: `t_expired` is set, the row is never removed. This preserves the full audit trail.

`t_valid` and `t_invalid` are optional (`Option<DateTime<Utc>>`). When unset, the fact is assumed to be valid from creation time with no known expiration. The `AddFactOptions` builder allows consumers to set temporal bounds at ingestion time.

Edges also carry `t_created` and `t_expired` for cascade expiry when their source or target fact is expired.

## Consequences

### Positive

- Temporal queries: "what did the agent believe at time T?" is answerable by filtering on system-time axis.
- Future memory: facts with `t_valid` in the future can be ingested now and surface when their date arrives (Phase 3b `drain_due()` API).
- Historical reasoning: expired facts remain queryable for provenance and debugging.
- Conflict resolution has full temporal context: the `ConflictArbiter` receives both facts with all 4 timestamps.

### Negative

- Every fact requires 4 timestamp fields, increasing storage per row.
- Query predicates become more complex. Every search must filter on `t_expired IS NULL` at minimum, and optionally on `t_valid`/`t_invalid` for temporal awareness.
- Consumers must understand the distinction between system time and real-world time to use the model correctly.

### Mitigations

- SQL-level filters for `t_expired IS NULL` are applied automatically by the engine's store layer. Consumers do not write raw SQL.
- `t_valid` and `t_invalid` default to `None`, so consumers who do not need real-world temporal semantics can ignore them entirely.
- Phase 3b adds `resume_context(now)` and `drain_due(now)` APIs that handle temporal filtering transparently.
