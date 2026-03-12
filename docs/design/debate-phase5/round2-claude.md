# Round 2 — Claude (Opus 4.6) Response

## Q1: I concede — 2 stages is correct

Codex and Gemini are right. My three-trait design fails BaseLayer's simplicity test.

The real question: does WisdomPromoter do something DreamCycle can't? No. The DreamCycle already receives promotion candidates and needs to produce wisdom artifacts. Splitting "detect pattern" from "format output" into separate traits creates an interface boundary with zero implementation diversity — every WisdomPromoter implementation will be called by exactly one DreamCycle implementation. That's an abstraction with one caller. Delete it.

**Resolution**: Two traits.
- `InsightStream`: fast-path input (`fn record(&self, insight: Insight) -> Result<()>`)
- `DreamCycle`: slow-path batch processor (`fn run(&self, engine: &MemoryEngine) -> Result<CycleReport>`)

The DreamCycle internally handles consolidation → pattern detection → formatting → promotion. The `CycleReport` contains the promoted wisdom artifacts, provenance, and rescoring changes. One trait, one entry point, all complexity inside.

## Q3: Gemini's `sample_dormant(n)` is the cleanest answer

I was overcomplicating this. My "reverse HNSW search" proposal is just a query with inverted filters — which means it's a regular engine API, not a trait.

Gemini's framing is precise: the engine exposes `sample_dormant(n: usize, context: &[f32]) -> Vec<Fact>`. The consumer decides when/whether to call it. Autonomous agents call it in their boredom loop. Coding assistants ignore it. No trait needed, no engine-core complexity.

Codex's "associative priming" is the same idea with more implementation detail than needed at this stage. Gemini's API-only approach is simpler.

**Resolution**: Add `sample_dormant(n, context_embedding)` to the engine's public API. Not a trait. Not a core retrieval path. Just a query the consumer can call.

## Q4: Gemini is partially right — provenance should be structural, not summary

I was wrong to dismiss full fact-ID chains. Gemini's argument about "axiomatically pinned" wisdom is compelling: if the source facts decay and the provenance is just "4 facts over 2 weeks," the wisdom becomes unjustified dogma. You can't undo it because you can't find its roots.

But Gemini's `Vec<FactId>` is also too rigid. What if promotion combines facts with different confidence? What if some sources are more representative than others?

**Resolution**: `PromotionProvenance { source_fact_ids: Vec<FactId>, session_count: u32, date_range: (DateTime, DateTime), confidence: f64 }`. Store ALL source fact IDs (they're just UUIDs, storage is cheap), plus the summary metadata Claude/Codex wanted. Best of both: structural traceability for debugging + summary for human approval display.

The key: source facts are soft-deleted (relevance→0), never hard-deleted. The provenance chain remains resolvable indefinitely.

## Q6: Gemini's "nuke avoidance" is too aggressive, but asymmetry is correct

Codex's +3/−1 asymmetry is the right intuition: late-discovered signals (avoidances, corrections) need to rise faster than general-purpose rescoring adjusts. Gemini's "nuke" is too violent — setting importance to 0 in one cycle destroys information. What if the avoidance was context-specific and gets over-generalized?

My symmetric ±2 was too conservative. Corrections ARE higher-signal than positive reinforcement. But they need to accumulate evidence, not detonate in one cycle.

**Resolution**: Asymmetric rescoring with correction-aware targeting.
- General facts: ±2 per cycle, cumulative (my original)
- Correction patterns (fast `t_invalid` after `t_created`): +4 for the correcting fact, −3 for the corrected fact, per cycle
- Hard floor: importance never drops below 1 (preserves reversibility)
- The correcting fact also gets a `correction_source: Option<FactId>` link — structural, not inferred

## Q7: Gemini's per-FactType ratio is the most precise answer

All three of us were talking past each other because we weren't distinguishing fact types. Gemini nailed it:

- Episodic (what happened): compress aggressively (20%). These are the raw experiences that consolidate into patterns.
- Semantic (what is true): compress conservatively (80% retained). API surfaces, project facts, domain knowledge.
- Procedural (how to do things): compress conservatively (80% retained). Build commands, test patterns, workflow steps.

Codex's "5-10% coding / 20% agents" conflates fact type with use case. The correct decomposition is by fact type, and the consumer tunes the ratios.

**Resolution**: `DreamCycleConfig { compression_ratio: HashMap<FactType, f64> }` with defaults: Episodic=0.2, Semantic=0.8, Procedural=0.8. Consumer overrides per use case.
