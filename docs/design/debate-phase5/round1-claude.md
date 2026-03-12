# Round 1 — Claude (Opus 4.6) Position

## Q1: Pipeline Architecture — Keep three traits, minimize each

The separation IS justified, but for a different reason than the current design assumes. BaseLayer proved that **layering is load-bearing** (3-layer identity 87 > single-layer 83) while **complexity within each layer is NOT** (14-step < 4-step). Our three traits are the load-bearing layers; each trait's contract must be minimal.

- **InsightStream**: Input channel. `fn record_insight(&self, insight: Insight) -> Result<()>`. One method.
- **DreamCycle**: Batch processor. `fn run_cycle(&self, facts: &[Fact]) -> Result<Vec<PromotionCandidate>>`. One method.
- **WisdomPromoter**: Output formatter. `fn promote(&self, candidate: PromotionCandidate) -> Result<WisdomArtifact>`. One method.

This justifies itself because consumers differ: coding assistant runs DreamCycle on session-end and promotes to CLAUDE.md; autonomous agents run DreamCycle every N heartbeats and promote to SOUL.md. Same engine, different implementations.

## Q2: Both needed, sequential not parallel

Consolidation (dedup → cluster → global) = noise reduction. Behavioral compression = understanding creation. They're sequential: you can't extract patterns from noisy duplicates.

Consolidation is engine-internal (pure Rust, no LLM). Behavioral compression is consumer-provided via the DreamCycle trait — the consumer implementation uses LLM intelligence to go from "cluster of similar corrections" to `{pattern, directive, false_positive}`. The trait boundary IS the separation between "what the engine does" and "what the intelligence does."

This maps exactly to BaseLayer: our consolidation = their EXTRACT step; our DreamCycle = their AUTHOR step. Two steps, trait-separated.

## Q3: Resonance — Yes, as an optional trait with trivial implementation

Resonance is just **reverse HNSW search**: given a context embedding, find dormant memories that resonate. Same algorithm, different trigger and filter.

```rust
pub trait ResonanceProvider {
    fn find_resonances(&self, context: &[f32], exclude: &[FactId]) -> Vec<ResonanceMatch>;
}
```

Default implementation: HNSW search on context embedding, filter OUT active-set facts, filter FOR low-relevance (dormant) facts, return top-K. One extra search per interaction. No keyword hackery — we already have embeddings.

For autonomous agents, this is critical (identity emergence requires spontaneous reactivation). For coding assistants, it's a nice-to-have. Optional trait, minimal implementation.

## Q4: Provenance — Summary-level, not full chains

"Detected from 4 facts across 3 sessions over 2 weeks" + the 3 most representative fact IDs (for sampling). This is sufficient for informed human approval AND reversibility.

Full fact-ID chains are LAM's problem, not ours. LAM proves provenance from source text to atom — they need byte-level precision because they target government/defense. We prove provenance from memory to wisdom — human approval is the gate, not cryptographic verification.

If a promoted pattern is wrong, the human deletes it. The source facts still exist (soft-deleted, relevance=0), so the dream-cycle can re-discover and re-propose with evolved context.

## Q5: Keep bi-temporal, steal temporal intent bias

Our bi-temporal model already supports LAM's CHANGED-edge queries:

- "What used to be true?" → `WHERE t_invalid IS NOT NULL AND t_valid < ?`
- "What's current?" → `WHERE t_invalid IS NULL`

CHANGED edges are syntactic sugar over temporal joins — explicit but redundant. For 6-month agents, bi-temporal is strictly superior: continuous evolution, arbitrary point-in-time queries, no LLM dependency for detecting "state transitions" in text.

**Steal this one thing**: temporal intent bias in queries. When query contains "now/current," bias toward `t_invalid IS NULL`. When "previously/used to," bias toward expired facts. This is a query rewriting step, not a storage change.

## Q6: Targeted scanning + capped adjustments

Two-mode rescoring:

1. **Targeted scan** (cheap, O(n) on recent facts): Detect correction pairs (fast `t_invalid` after `t_created`), repeated facts across sessions, facts clustering with already-promoted wisdom
2. **DBSCAN clustering** (expensive, batch): General pattern detection in embedding space

Cap: ±2 per cycle but cumulative. A fact getting +2 three cycles in a row reaches +6 total — the system correctly identifying sustained importance. The cap prevents single-cycle spikes, not gradual accumulation.

Avoidance patterns: scan for facts where the consumer stored a superseding fact shortly after the original (quick correction = high behavioral signal).

## Q7: ~20% is right, but don't hardcode

For coding assistants, the saturation point may be lower (~10%) — corrections and preferences are high-signal, low-volume. For autonomous agent identity, 20% tracks BaseLayer's finding.

Implementation: promote facts where `importance >= P75` of the candidate set (top 25%). The DreamCycle consumer can tune this threshold. The key insight from BaseLayer: **more facts actively hurts** (71.72% with all facts < 71.83% with 20%). Aggressive filtering IS the feature, not a limitation.
