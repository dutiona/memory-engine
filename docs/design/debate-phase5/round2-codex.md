1. **Q1: Two stages wins. Strongest: Advisor B.**  
`WisdomPromoter` is not a pipeline stage; it is one possible outcome of `DreamCycle`. Splitting it into a third trait adds an extra abstraction boundary without adding scheduling or ownership clarity.  
Claude is wrong because “one method per trait” is still over-factoring if one trait is just a sub-step. Gemini is close, but `compress_behavior(Vec<Fact>) -> Option<Wisdom>` risks baking one promotion product too directly into the public seam.  
**Resolution:** keep **2 traits**: `InsightCapture` and `DreamCycle`. Inside `DreamCycle`, model promotion/compression as pluggable policies or strategies, not public stages.

2. **Q3: Keep resonance out of the engine core. Strongest: Advisor C.**  
Passive activation changes retrieval semantics and makes relevance harder to reason about, test, and tune. BaseLayer says default path should stay explicit.  
Claude is wrong because an “optional trait” still blesses resonance as a first-class engine concern too early. Codex is better than Claude, but even bounded priming in-core creates hidden coupling between consolidation and recall.  
**Resolution:** no automatic resonance in core. Expose a **read-only API** such as `sample_dormant` or `priming_candidates(query, n)`. Consumers may opt in and merge candidates explicitly.

3. **Q4: Hybrid provenance wins. Strongest: Advisor B.**  
You need enough metadata for audit/debugging, but forcing `Vec<FactId>` into every summary makes summaries heavy, brittle, and over-coupled to source granularity.  
Claude is wrong because representative IDs plus counts are too weak for debugging regressions. Gemini is wrong because strict full lineage everywhere is an expensive default and punishes compaction.  
**Resolution:** mandatory provenance envelope on every derived artifact: `count`, `time_window`, `method/version`, `confidence`, `3-5 representative FactId`s, and a **lineage key**. Full `Vec<FactId>` lives in a sidecar lineage table, loaded only when needed.

4. **Q6: Conservative general rescoring with explicit quarantine. Strongest: Advisor A.**  
A symmetric rule is the cleanest default and easiest to explain. Most memory systems fail from unstable score dynamics, not from under-reacting.  
Codex is wrong because asymmetric `+3/-1` adds extra knobs and can create runaway promotion for late noisy signals. Gemini is right about “nuke avoidance,” but a general “nuke” concept is too blunt unless restricted to invalid/poisoned facts.  
**Resolution:** default **symmetric ±2** per cycle with cumulative cap. Add a separate **quarantine/suppress path** for explicit contradictions, toxicity, or corruption. Do not mix quarantine with ordinary rescoring.

5. **Q7: Type-aware compression wins. Strongest: Advisor C.**  
Compression is not one scalar. Episodic traces compress differently from semantic or procedural memory.  
Claude is wrong because a single P75 threshold over mixed candidates ignores memory type. Codex is wrong because use-case percentages are too application-specific for engine policy.  
**Resolution:** parameterize by **FactType family**, with coarse defaults only: e.g. episodic `0.2`, semantic/procedural `0.8`. Within each type bucket, use a percentile threshold such as P75 to select candidates. This stays simple and respects structure.
