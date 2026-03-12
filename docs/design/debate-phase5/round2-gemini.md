# Round 2 — Gemini Position

## Q1: Collapse to Two — but simpler than the others think

Strongest: Gemini & Codex. BaseLayer proved fewer steps yield better results. Separating WisdomPromoter as a peer to DreamCycle creates synchronization overhead.

Claude over-engineers by treating "Promotion" as distinct from "Consolidation." In a Rust library, these are passes over the same data.

**Resolution: Two stages, but InsightStream isn't needed either.**
1. **Ingest (Fast)**: Standard `add_fact`. No special "InsightStream" component — just a `FactType::Insight` tag.
2. **Dream (Slow)**: `DreamCycle` trait. The WisdomPromoter logic is simply a closure or strategy provided to `DreamCycle::run()`.

## Q3: Passive API Only

Strongest: Gemini. The engine is a library, not a service. It has no thread to run "background resonance." Active resonance (Claude/Codex) implies a heartbeat the engine doesn't possess.

Claude/Codex conflate capability (finding dormant memories) with execution (triggering the search).

**Resolution**: Expose `sample_dormant(n)`. The consumer calls this in its own idle loop. The engine remains purely reactive.

## Q4: Strict & Complete Provenance

Strongest: Gemini. "Trust requires traceability" (LAM). Claude's "3 representative IDs" is useless for debugging a hallucinated coding rule. Storage cost for `Vec<FactId>` (8 bytes per ID) is negligible compared to embedding vectors (3KB+).

Claude optimizes storage prematurely. Codex's "references" adds schema complexity.

**Resolution**: Mandatory `source_ids: Vec<i64>` on promoted Wisdom facts. If a pattern derives from 1,000 facts, store 1,000 IDs.

## Q6: Signal-Specific Rescoring

Strongest: Gemini. Coding assistants require immediate obedience. If a user says "Don't use unwrap," that negative signal must instantly override 6 months of habit. Symmetric (Claude) or slight asymmetry (Codex) is too slow for safety-critical corrections.

They treat all signals as gradients. Avoidance is often binary.

**Resolution: Hybrid Policy.**
- Correction/Avoidance: Immediate max-importance boost (or "Nuke" to negative).
- Reinforcement: Conservative incremental (+1/-1).

## Q7: Type-Based Ratios

Strongest: Gemini. You cannot compress a Rust API definition (Procedural) by 80% without breaking the code. You can compress a chat log (Episodic) by 80%.

Claude (P75) and Codex (use-case) apply a blanket policy to heterogeneous data types.

**Resolution**: CompressionConfig by FactType.
- Episodic: Target 20% retention (high compression)
- Semantic/Procedural: Target 90% retention (fidelity first)
