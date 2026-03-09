1. **Backlog placement:** split Phase 3 into **3a hardening** and **3b lifecycle**, but do not block all lifecycle work on all four issues.  
`#5 thread safety` is the real gate for anything multi-client or network-adjacent. `#6 AddFactOptions builder` is cheap API hygiene and should land early because it stabilizes the write surface before lifecycle features depend on it. `#3 ANN index` and `#4 SQL filters` are performance/query improvements, not prerequisites for wake-up/future-memory semantics.  
So: **3a = #5, #6; 3b = wake-up/future memory/unforgettable; 3c = #3, #4**.  
This challenges the “interleave everything” view: interleaving concurrency fixes with new lifecycle semantics is how you bake races into the public model.

2. **KB Protocol timing:** Phase 4 is fine, but **not before observability tooling**. I disagree with Gemini’s bundling of KB Protocol with a full Web UI; that confuses developer tooling with product surface. I also disagree with Claude’s implicit “MCP first, then KB protocol” if that means protocol work waits on a full server.  
The right order is: **internal inspection/debug surface first** (CLI/TUI/minimal local UI), then **MCP/server boundary**, then **narrow KB Protocol**.  
Why: protocol bugs are semantic bugs. Without a way to inspect stored facts, schedules, pins, and projections, you will debug blind. A full Web UI is too expensive; a minimal inspector is high leverage.

3. **Passive vs heartbeat:** neither A nor B as stated.  
Best answer is **(C): keep the core passive, add a scheduling-oriented API, not callback registration**.  
Expose something like `next_due_time()` / `drain_due(now)` / `resume_context(now)` and let the consumer decide whether to poll at session start, run a timer, or ignore timers entirely.  
This challenges A: session-start-only resurfacing is too weak if “future memory” is meant to feel timely. It also challenges B: a `MemoryHeartbeat` trait pushes event-loop assumptions into the library and complicates embedders for little gain. The core should compute due work, not own scheduling.

4. **Unforgettable auto-tagging:** **(A), but not in the storage core.**  
A policy hook such as `UnforgettableClassifier::should_pin(fact, context)` is the right abstraction, with an optional reference heuristic implementation shipped separately. The engine should also support explicit pinning via write options.  
I disagree with Codex only insofar as “cut it” is too strict: the user is asking for a usable system, not a pure database. But hardcoded heuristics in core are wrong because “my birthday is X” may be unforgettable for one app and sensitive/forbidden in another.  
So: **policy plug-in, default heuristics optional, core remains neutral**.

5. **Event-sourced sync feasibility:** it is **tractable**, but only if you stop pretending single-writer semantics survive sync unchanged.  
The core does not need a total redesign, but sync requires explicit multi-writer metadata: stable event IDs, device/actor IDs, causal or hybrid timestamps, idempotent replay, and deterministic conflict rules at projection time.  
If the current model assumes one monotonic writer sequence and in-place truth, sync will force redesign of the event envelope, not necessarily the whole engine.  
So Gemini is right that it becomes log-merging, but wrong if that implies impossibility. It is feasible; it just belongs in a separate sync architecture, not as a casual add-on.
