# ADR-0009: Session Log Bootstrap Design

**Status:** Accepted
**Date:** 2026-03-19
**Phase:** 4a

## Context

The memory engine starts cold -- no historical facts exist until the agent begins interacting. However, Claude Code session logs (JSONL files stored in `~/.claude/projects/`) contain valuable procedural knowledge: bug fixes, architectural decisions, coding conventions, and learnings accumulated over previous sessions.

Research on context adaptation (AWM, APC, Reflexion) demonstrates that success-gated ingestion, workflow extraction, and pre-warming semantics improve agent performance on repeated tasks. The bootstrap pipeline applies these ideas: it parses session logs, classifies outcomes, filters for noteworthy episodes, and extracts facts to seed the memory engine.

Five design decisions required resolution.

## Decisions

### D1: Savepoint transactions for crash safety

Per-session imports are wrapped in `SAVEPOINT bootstrap` / `RELEASE bootstrap`. On any error, `ROLLBACK TO bootstrap` ensures no partial session pollutes the database.

**Alternative considered:** Individual `add_fact()` calls (the standard engine pattern). Rejected because a crash mid-session would leave orphaned facts with no way to distinguish complete vs partial sessions.

**Trade-off:** Savepoints hold the write connection for the full duration of a single session import. Acceptable because bootstrap is a batch operation, not a hot path.

### D2: `last_accessed` backdating alongside `t_created`

Both `t_created` and `last_accessed` on bootstrapped facts are set to the original session timestamp, not to the import time.

**Why both:** `resume_context()` uses `t_created` for the "recent facts" tier. The forgetting policy computes Ebbinghaus decay from `last_accessed`. Only backdating `t_created` would make historical facts appear fresh to the forgetting policy, distorting retention behavior. Both must reflect the original session time for correct decay.

**Trade-off:** Backdated facts may be immediately eligible for forgetting if their decay pushes importance below `min_importance`. This is intentional -- truly old, low-importance episodes should not survive the first prune.

### D3: `SessionExtractor` trait in bootstrap module, not `traits.rs`

The trait lives in `src/bootstrap/extract.rs` and is re-exported from `lib.rs`, rather than being placed in the top-level `src/traits.rs`.

**Rationale:** The four traits in `traits.rs` (`EmbeddingProvider`, `SummaryGenerator`, `ConflictArbiter`, `PersistenceClassifier`) are general-purpose and used across the engine. `SessionExtractor` is domain-specific to the bootstrap pipeline -- it operates on `CandidateEpisode` and `SessionOutcome`, types that only exist in the `bootstrap` module. Placing it in `traits.rs` would create a dependency from the general traits module on bootstrap-specific types.

**Trade-off:** Consumers looking for "all traits to implement" must check both `traits` and `bootstrap::extract`. The crate root re-exports `SessionExtractor` to mitigate discoverability.

### D4: Event-lean approach -- one marker event per session

The pipeline ingests one `SystemEvent` per session (with `source="bootstrap"` and `session_id`), not one event per conversation turn or per extracted fact.

**Rationale:** The event log is the audit trail. A single marker recording "session X was bootstrapped" is sufficient for idempotency and provenance. One event per turn would create O(hundreds) of events per session, bloating the event store with synthetic entries that don't represent real interactions. Facts link back to the marker event via `source_event_id`.

**Trade-off:** Individual facts cannot be traced to their specific source turn via the event log alone. The `metadata` field on each fact carries `session_id` and `category` for finer-grained tracing when needed.

### D5: Forward-compatibility -- outcome and session_id in metadata

Session outcome and session ID are stored in fact `metadata` JSON fields (`session_outcome`, `session_id`), not in dedicated database columns or a separate `bootstrap_sessions` table.

**Rationale:** Adding columns or tables for bootstrap-specific data would require a schema migration and couples the core schema to a single pipeline. Metadata JSON is schemaless and already exists on every fact. When Phase 5 introduces outcome tracking (#63) and co-session edges (#62), the migration path is clear: read metadata, populate the new stores, drop the metadata keys.

**Trade-off:** Querying "all facts from failed sessions" requires JSON extraction in SQL (`json_extract(metadata, '$.session_outcome')`), which is slower than a column index. Acceptable for the current scale; a materialized column can be added later if needed.

## Consequences

**Positive:**

- Cold-start problem is solved: past session logs seed the memory engine with real historical knowledge
- Idempotent by default: re-running bootstrap is safe and produces no duplicates
- Crash-safe: savepoint rollback prevents partial imports
- Correct decay: backdated timestamps ensure Ebbinghaus scoring reflects true fact age
- Extensible: `SessionExtractor` trait allows LLM-powered extraction without engine changes

**Negative:**

- English-only keyword heuristics limit extraction quality for non-English sessions
- Metadata-based storage of outcome/session_id is slower to query than dedicated columns
- Backdated facts may be pruned immediately on first `forget()` call if they are old and low-importance
- Forward-compatibility metadata creates a migration obligation when #63/#62 land

**Neutral:**

- The `Io` error variant added to `MemoryError` has broader utility beyond bootstrap
- `EventFilter.source` field is generally useful for event store queries
