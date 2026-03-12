1. **Pipeline architecture**  
Collapse to **two operational stages**, not three subsystems:  
`Insight capture` and `Dream-cycle promotion`.  
`WisdomPromoter` should remain a trait, but as an implementation detail used by DreamCycle, not a separately orchestrated pipeline. `InsightStream` is justified because it captures voluntary, high-signal observations at ingest time. This keeps Use case A simple and gives Use case B an explicit path for self-observation without adding another full runtime loop.

2. **Behavioral compression vs fact consolidation**  
You need **both**. They solve different problems.  
Fact consolidation reduces duplicate/noisy observations. Behavioral compression extracts durable patterns such as “prefers X,” “avoids Y,” “corrects itself after Z.” The interaction should be one-way: consolidate facts first, then run behavioral extraction on the cleaner substrate. This is fully compatible with zero-LLM design because the library only defines the trait and lifecycle; consumers can supply heuristic, symbolic, embedding-based, or LLM-backed implementations.

3. **Resonance / passive activation**  
Worth it, but only as a **bounded optional mechanism**.  
For Use case A, query-driven retrieval is usually enough. For Use case B, passive activation matters because identity and relationship formation depend on old patterns resurfacing without explicit asks. Do not build a free-floating resonance engine. Add a cheap “associative priming” pass: recent salient entities, emotions, corrections, or partners can inject a few extra candidate memories into retrieval. Top-k only, low frequency, opt-in.

4. **Provenance for promoted wisdom**  
Yes, promoted wisdom should carry provenance, but **summary-level plus references**, not full byte-level chains.  
Store: supporting fact IDs, support count, time window, confidence, and promotion method/version. Example: “Derived from 4 facts across 16 days.” This is enough for audit, rollback, and trust calibration. Full chains are too heavy for the wisdom layer; the underlying event/fact store already preserves the detailed trail.

5. **CHANGED edges vs bi-temporal model**  
Keep the bi-temporal core. Add **derived CHANGE relations as an index/view**, not as the primary model.  
For 6-month agents, queryability of belief evolution matters more than raw storage efficiency, but you do not need to abandon the current design. Generate explicit `changed_from/changed_to` links during consolidation or indexing. That gives Use case B the “what did I used to believe?” affordance while preserving the elegance of the current event-sourced, bi-temporal base.

6. **Retrospective importance rescoring**  
Be **targeted and asymmetric**.  
Do not rely on clustering alone; avoidance, correction, and contradiction signals are too sparse and too important. Dream-cycle should explicitly scan for: repeated corrections, reversals, abandoned intentions, partner-specific adjustments, and recurring conflict-resolution patterns. Use bounded updates, but stronger on positive revaluation than decay: e.g. `+3/-1` per cycle with a hard ceiling. That keeps stability while letting late-discovered identity signals surface.

7. **20% compression saturation**  
Do **not** treat 20% as a universal rule.  
For Use case B, 20% is a good **upper bound for promotion candidates** because behavioral fidelity matters more than exact episodic recall. For Use case A, behavioral wisdom is a smaller slice; most value remains in exact corrections, APIs, preferences, and task-local procedural facts. So: promote roughly top **20% for agent pairs**, top **5-10% for coding assistants**, and keep raw facts retrievable outside wisdom either way.
