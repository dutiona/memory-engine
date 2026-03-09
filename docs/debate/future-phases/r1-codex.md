**Recommendation**

Phase the roadmap by staying strict about what belongs to the local memory engine versus what is product/infrastructure around it.

**Phase 3: Cognitive Reactivation**
Group: `Wake-up Procedure` + `Future Memory` + `Unforgettable Memories`

- `Wake-up Procedure` is the right Phase 3 anchor. It turns the current store into an actual agent memory system by rebuilding working context, not just searching history. It exploits what Phase 1 and 2 already built: bi-temporal facts, importance heuristics, graph connectivity, forgetting.
- `Future Memory` belongs here because it is only valuable if something actively surfaces it. Without wake-up, future-dated facts are just inert rows. Dependency: wake-up scheduler/checkpoint logic.
- `Unforgettable Memories` also belongs here, but only the manual flag. It is a simple policy override on forgetting/decay and directly affects what `resume_context()` loads. Dependency: forgetting pipeline.  
  Cut for now: auto-tagging. That is not a storage problem; it is an application-policy or ML/classifier problem. Keep it trait- or hook-based later.

Why this grouping: these three together define “what should be in the agent’s head when it wakes up.” That is a coherent core-library milestone.

**Phase 4: External Knowledge Boundary**
Group: `KB Protocol`

- `KB Protocol` should be Phase 4, and probably mostly a separate companion crate plus protocol spec, not deep core surgery.
- It naturally builds on Phase 3 because wake-up/query recipes can reference external KB handles for deeper retrieval.
- Dependency: stable identity for memories, summaries, citations, retry/error semantics.  
  Not dependent on auth or sync if the protocol is capability-based and tolerant of unreachable peers.
- Essential, but only if kept narrow: summary/insight references out, raw content pointers back, graceful degradation in failure cases. Do not let this become distributed query execution.

Challenge: don’t mix KB protocol with sync. KB is loose federation of knowledge sources; sync is replication of your own source of truth. Different failure modes, different guarantees.

**Phase 5: Tooling / Operator Surface**
Group: `Web UI`

- `Web UI` should come after the reactivation and protocol APIs stabilize. Otherwise you will freeze unstable concepts into UX.
- Dependency: stable graph/query/edit/import/export APIs; ideally event-log inspection endpoints.
- Essential as a companion project, not as core engine work. The engine does not need WASM concerns in its design center.
- Trade-off: large-graph rendering is a serious project by itself. Start with search/filter/timeline/event-log views before “full graph exploration.”

**Deferred / Separate Project**
Group: `Auth` + `SaaS Sync`

- `Auth` should be deferred unless you are committing to multi-user or hosted operation. For a local-first single-process library, auth in core is mostly abstraction debt.
- `SaaS Sync` is a separate project or at least a later major initiative. It is not just “add networking”; it changes the consistency model.
- Dependencies: device identity, key management, replication format, event-log merge semantics, conflict policy across devices, rollback/versioning. It likely needs event-level replication rather than state diffing if append-only log is truly the source of truth.
- If you do sync, `Auth` becomes a service-layer concern, not necessarily a memory-engine concern.

**Net**

- Phase 3: Wake-up, Future, Unforgettable(manual only)
- Phase 4: KB Protocol
- Phase 5: Web UI
- Deferred/separate: Auth, SaaS Sync

This keeps the core library focused on memory semantics first, integration second, product/platform concerns last.
