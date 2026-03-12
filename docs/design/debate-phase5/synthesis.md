# Phase 5 Cognitive Pipeline — Debate Synthesis

> Date: 2026-03-12
> Participants: Claude (Opus 4.6), Codex (o4-mini), Gemini (2.5 Pro)
> Rounds: 2 (convergence achieved, Round 3 unnecessary)
> Context: Community research synthesis + four-layer cognitive architecture

---

## Final Positions (Consensus + Resolved Disagreements)

### Q1: Pipeline Architecture → **Two Traits**

**Consensus (3:0 after R2).** WisdomPromoter is not a separate concern — it's an output of DreamCycle.

```rust
// Fast path: captures high-value observations from the intelligence layer
pub trait InsightStream {
    fn record(&self, insight: Insight) -> Result<()>;
}

// Slow path: periodic batch consolidation + pattern detection + promotion
pub trait DreamCycle {
    fn run(&self, engine: &MemoryEngine) -> Result<CycleReport>;
}
```

DreamCycle internally handles: consolidation → pattern detection → behavioral compression → formatting → promotion. Promotion/compression are pluggable **strategies** inside the trait implementation, not separate public traits.

**Rationale**: BaseLayer proved layering is load-bearing but complexity within layers is not. Two traits = two scheduling concerns (real-time vs batch). WisdomPromoter would be a third trait with one caller — an abstraction without implementation diversity.

---

### Q2: Behavioral Compression vs Fact Consolidation → **Both, Sequential**

**Consensus (3:0).** Two categorically different operations:
1. **Consolidation** (engine-internal, pure Rust): dedup → cluster → merge. Noise reduction.
2. **Behavioral compression** (consumer-provided via DreamCycle): pattern extraction → `{pattern, directive, false_positive}`. Understanding creation.

Consolidation runs first, producing clean input for behavioral compression. The DreamCycle trait bridges the gap — the engine provides consolidated facts, the consumer's implementation uses whatever intelligence (LLM, heuristic, symbolic) it needs.

---

### Q3: Resonance / Passive Activation → **API Only, No Engine Core**

**Consensus (3:0 after R2).** Passive activation changes retrieval semantics and makes relevance harder to test. Keep the engine reactive.

```rust
// Public API, not a trait. Consumer calls when/if it wants resonance.
impl MemoryEngine {
    pub fn sample_dormant(&self, n: usize, context: &[f32]) -> Result<Vec<Fact>>;
}
```

- Autonomous agents call this in their "boredom loop" to simulate resonance
- Coding assistants ignore it
- No hidden coupling between consolidation and recall
- Implementation: HNSW search on context embedding, filter for low-relevance facts, return top-N

**Rationale (Gemini)**: "Keep the engine reactive; let the application layer drive the ghost in the machine."

---

### Q4: Provenance for Promoted Wisdom → **Hybrid: Envelope + Sidecar Lineage**

**Resolved (Codex R2 proposal, adopted by all).**

Every promoted wisdom artifact carries a mandatory **provenance envelope**:
```rust
struct PromotionProvenance {
    source_count: u32,                        // how many facts contributed
    session_count: u32,                       // across how many sessions
    date_range: (DateTime<Utc>, DateTime<Utc>), // earliest to latest
    confidence: f64,                          // frequency × consistency
    method_version: String,                   // which DreamCycle impl version
    representative_ids: Vec<FactId>,          // 3-5 most representative
    lineage_key: LineageId,                   // foreign key to full lineage
}
```

Full `Vec<FactId>` lives in a **sidecar lineage table**, loaded on demand when the human clicks "Why?" or when debugging bad promotions. This balances:
- Gemini's requirement for structural traceability (full chain exists)
- Claude/Codex's concern about lightweight default display (envelope is small)
- Reversibility: source facts are soft-deleted (relevance→0), never hard-deleted. Lineage chains remain resolvable indefinitely.

---

### Q5: Temporal State → **Keep Bi-temporal, Compute Change Views**

**Consensus (3:0).** No CHANGED edges. Bi-temporal model already supports LAM's queries:
- "What's current?" → `WHERE t_invalid IS NULL`
- "What used to be true?" → `WHERE t_invalid IS NOT NULL`

Two enhancements to steal from LAM:
1. **Temporal intent bias** (Claude): query rewriting that detects "now/current/previously" and biases toward current or expired facts
2. **Virtual change reconstruction** (Gemini): an `explain_fact` API that computes the temporal history of a fact on demand, without materializing edges

**Rationale**: Don't materialize what you can compute from timestamps. Bi-temporal is strictly more expressive for 6-month autonomous agents (continuous evolution, arbitrary point-in-time queries).

---

### Q6: Retrospective Importance Rescoring → **Symmetric ±2 + Quarantine Path**

**Resolved (Codex R2 reversal, strongest final position).**

Two separate mechanisms:

1. **General rescoring**: symmetric ±2 per cycle, cumulative. A fact that deserves +6 gets there over 3 cycles, not in a single spike. Stability prevents "schizophrenic agent behavior" (Gemini) over 6-month runs.

2. **Quarantine/suppress path**: explicit corrections, contradictions, and toxic facts get a separate treatment — not rescoring but **quarantine**. A quarantined fact drops out of retrieval immediately but remains in storage for pattern mining and audit.

**Targeted scanning** (all three agree): DreamCycle scans for:
- Correction pairs (fast `t_invalid` after `t_created`)
- Repeated facts across sessions
- Facts clustering with already-promoted wisdom
- Avoidance patterns (supersession shortly after creation)

**Rationale (Codex R2)**: "Most memory systems fail from unstable score dynamics, not from under-reacting. Don't mix quarantine with ordinary rescoring."

---

### Q7: Compression Saturation → **Per-FactType Ratios**

**Consensus (3:0 after R2).** Gemini's decomposition is the most precise.

```rust
struct DreamCycleConfig {
    compression_ratios: HashMap<FactType, f64>,
    // Defaults:
    // Episodic:   0.2  (compress aggressively — raw experiences consolidate into patterns)
    // Semantic:   0.8  (retain most — API surfaces, project facts, domain knowledge)
    // Procedural: 0.8  (retain most — build commands, test patterns, workflow steps)
}
```

Within each FactType bucket, use a percentile threshold (P75 of importance within that type) to select promotion candidates (Codex R2 addition).

**Rationale**: "You cannot compress a library's API surface by 80% without hallucinations" (Gemini). BaseLayer's 20% finding applies to episodic/narrative memory, not procedural knowledge.

---

## Architecture Summary

```
                    ┌─────────────────────────────┐
                    │    Consumer Application      │
                    │  (Coding Assistant / Agent)   │
                    └─────┬──────────┬─────────────┘
                          │          │
              record()    │          │  run()
                          │          │
                    ┌─────▼──┐  ┌───▼──────────────┐
                    │Insight │  │   DreamCycle       │
                    │Stream  │  │                    │
                    │(trait) │  │ 1. Consolidation   │
                    │        │  │    (engine-internal)│
                    │ Fast   │  │ 2. Pattern detect  │
                    │ path   │  │    (DBSCAN+targeted)│
                    │        │  │ 3. Behavioral      │
                    │        │  │    compression      │
                    └────────┘  │    (consumer impl)  │
                          │     │ 4. Promotion        │
                          │     │    (with provenance) │
                          │     │ 5. Rescoring         │
                          │     │    (±2 + quarantine)  │
                          │     └───────┬──────────────┘
                          │             │
                    ┌─────▼─────────────▼──────┐
                    │      MemoryEngine         │
                    │                           │
                    │  EventStore (immutable)    │
                    │  FactStore (bi-temporal)   │
                    │  HNSW (vector search)      │
                    │  ScopeTree (isolation)     │
                    │  Ebbinghaus (decay)        │
                    │  LineageTable (provenance) │
                    │                           │
                    │  + sample_dormant() API    │
                    └───────────────────────────┘
```

---

## Design Decisions Summary

| Decision | Resolution | BaseLayer Check |
|----------|-----------|-----------------|
| Pipeline: 3 traits → 2 | InsightStream + DreamCycle | ✓ Simpler. One less abstraction boundary |
| Consolidation + compression | Sequential, not parallel | ✓ Two steps, clear ownership |
| Resonance | API only, not engine core | ✓ No hidden complexity |
| Provenance | Envelope + sidecar lineage | ✓ Lightweight default, full chain on demand |
| Temporal model | Bi-temporal + query rewriting | ✓ No new storage structures |
| Rescoring | Symmetric ±2 + quarantine | ✓ One simple rule + one escape hatch |
| Compression | Per-FactType ratios | ✓ Respects domain structure |

---

## Unresolved Tensions (Gemini R2 Dissent)

Three questions where Gemini held firm against the Claude+Codex consensus. These are genuine design tradeoffs for the implementor to resolve:

### 1. InsightStream: Trait or FactType tag?

- **Claude/Codex**: InsightStream should be a separate trait — it represents a distinct scheduling concern (real-time vs batch)
- **Gemini**: No trait needed. Just tag insights as `FactType::Insight` via standard `add_fact()`. One fewer abstraction.

**My (Claude) assessment**: Gemini's point is valid for simple cases, but a trait gives consumers a semantic contract: "this is a high-value observation that should resist decay." A FactType tag achieves the same if importance scoring handles the rest. **Lean toward Gemini** — start with `FactType::Insight`, add the trait only if consumers need richer semantics.

### 2. Provenance: Full `Vec<i64>` or sidecar table?

- **Claude/Codex**: Sidecar lineage table (full chain on demand, envelope for display)
- **Gemini**: Inline `Vec<i64>` directly on wisdom facts. "8 bytes per ID is negligible."

**My assessment**: Gemini is right about storage cost being negligible. But inline `Vec<i64>` complicates the `Fact` struct for ALL facts when only promoted wisdom facts need provenance. **Lean toward Codex's sidecar** — separate concerns, load on demand.

### 3. Correction rescoring: Nuke or quarantine?

- **Claude/Codex**: Symmetric ±2 + separate quarantine path for contradictions
- **Gemini**: Immediate nuke for corrections ("avoidance is binary")

**My assessment**: Gemini's "nuke" is correct for explicit corrections ("don't use unwrap") but dangerous for ambiguous signals. A user redirecting a conversation is not the same as a user issuing a correction. **Resolution**: explicit corrections (user says "don't," "stop," "no") → quarantine + boost correction fact. Implicit avoidance (user redirects) → standard ±2 rescoring. The DreamCycle consumer classifies the signal type.

---

## What This Means for Implementation

### Phase 5a (Minimum Viable Cognitive Pipeline):
1. Define `InsightStream` trait (one method)
2. Define `DreamCycle` trait (one method, returns `CycleReport`)
3. Define `PromotionProvenance` struct + `LineageTable` in SQLite
4. Implement default `DreamCycle` with consolidation + DBSCAN pattern detection
5. Add `sample_dormant()` to engine public API
6. Add `DreamCycleConfig` with per-FactType compression ratios

### Phase 5b (Behavioral Intelligence):
7. Add targeted scanning (correction pairs, avoidance patterns)
8. Implement quarantine/suppress path for contradictions
9. Add temporal intent bias to query rewriting
10. Add `explain_fact()` virtual change reconstruction API

### Deferred (not in Phase 5):
- `compress_behavior()` hook on DreamCycle (depends on consumer LLM integration)
- Metacognitive rationale fields (interesting but not load-bearing)
- Emotion tags (wrong domain — we're factual, not affective)
