**Positions**

1. **Event schema forward-compat**
Add `origin_node_id` and `sequence_id` now. Claude is underestimating how expensive causal metadata becomes once events are persisted and copied around. If you ever want replication, merge, audit, or deterministic replay across nodes, a per-origin monotonic `sequence_id` is foundational.
Add `timestamp` only as non-authoritative metadata now, not as a correctness primitive. Wall clocks are useful for debugging and UX, but they should not decide ordering when clocks skew. So Gemini/Codex are right on direction, but the timestamp must be explicitly advisory.

2. **Phase 4**
Do **inspection tooling + MCP first**. KB Protocol should wait.
Freezing an external protocol before you have strong observability is backwards. You will discover missing concepts only after debugging real memory traces, replay issues, consolidation artifacts, and due-item behavior. MCP is fine in Phase 4 because it is a pragmatic integration surface, not a deep architectural commitment.

3. **Web UI timing**
Defer it. Gemini is right that it is luxury, but I’d go further: it should not be on the critical roadmap at all.
A graph UI is seductive and mostly non-essential at this stage. If inspection needs visualization, generate exportable artifacts first. A UI built before the engine’s operational model settles becomes maintenance drag.

4. **KB Protocol vs inspection-first**
Inspection-first, decisively.
Protocols calcify mistakes. Observability exposes them. For an event-sourced memory system, the first external “product” should be the ability to inspect event history, replay decisions, explain consolidation, and debug forgetting/conflict resolution.

**Blind Spots**

- **Snapshotting and compaction**: event sourcing without a replay/compaction strategy turns into slow startup and painful operations.
- **Determinism guarantees**: you need crisp rules for replay, merge, clock skew, and idempotency. Otherwise future sync is fragile.
- **Schema evolution discipline**: not just fields, but versioning policy, migration rules, and backwards-compat testing.
- **Evaluation harness**: no one mentioned regression suites for retrieval quality, consolidation correctness, forgetting behavior, and latency. “Feels good” is not enough.
- **Import/export and backup**: local-first systems live or die by portability and recoverability.
- **Explainability**: users will need “why is this fact active/conflicted/forgotten/due?” as a first-class API, not just raw search.

**Proposed Roadmap**

**Phase 3a: Core hardening**
- Thread safety
- `AddFactOptions` builder
- Event envelope v2: `origin_node_id`, `sequence_id`, optional advisory `created_at`, schema version
- Idempotent append semantics
Rationale: fix the substrate before adding more behavior.

**Phase 3b: Temporal memory**
- `next_due_time()`, `drain_due(now)`, `resume_context(now)`
- Future Memory
- Unforgettable flag + trait hook
Rationale: these features fit together around time, resurfacing, and working-state reconstruction.

**Phase 4: Operability and correctness**
- Inspection CLI/library
- Replay and explain APIs
- Snapshot/compaction support
- Benchmarks and regression corpus for retrieval/consolidation/forgetting
Rationale: make the engine understandable and testable before widening the surface area.

**Phase 5: Integration**
- MCP server
- Import/export format
- Backup/restore workflows
Rationale: expose the system safely once internals are inspectable.

**Phase 6: Experimental federation**
- KB Protocol behind an explicit experimental flag
- One reference adapter, not a broad standard
Rationale: protocol design should follow usage evidence, not precede it.

**Separate track, not core roadmap**
- Auth
- SaaS Sync
- Web UI

That keeps `memory-engine` a strong local-first core instead of prematurely turning it into a platform.
