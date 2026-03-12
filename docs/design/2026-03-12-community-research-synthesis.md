# Community Research Synthesis: AI Memory Systems

> Date: 2026-03-12
> Sources: r/AIMemory discussion, VividnessMem, LAM Protocol, BaseLayer, lam-prove-it
> Context: Gap analysis against [four-layer cognitive architecture](https://github.com/dutiona/research-index/blob/master/docs/insights/four-layer-cognitive-architecture.md) and memory-engine roadmap

---

## Executive Summary

Five projects were analyzed from a Reddit r/AIMemory discussion ("Trying to replace RAG with something more organic"). Each addresses a different failure mode of traditional RAG and occupies a different position in the four-layer architecture. The most significant finding is **BaseLayer's behavioral compression** — an approach that produces higher-quality identity representations from *fewer* facts, directly challenging the "more data is better" assumption.

**Key takeaways for memory-engine:**

1. Our Ebbinghaus decay + bi-temporal model is more rigorous than any system studied
2. Phase 5's WisdomPromoter (#47) should produce **behavioral directives**, not just detected patterns
3. Dream-cycle consolidation (#49) should aggressively filter — compression saturation at ~20% of facts
4. The 14-step → 4-step ablation result warns against over-engineering cognitive pipelines
5. LAM's conformance testing approach is a good model for our evaluation harness (#16)

---

## Projects Analyzed

### 1. VividnessMem — Human-inspired memory with salience scoring

**Repo:** https://github.com/Kronic90/VividnessMem-Ai-Roommates
**Author:** Kronic90 / u/Upper-Promotion8574 (Reddit)
**Layer position:** Memory

**What it is:** Dual local LLM testbed where two AI agents ("roommates") converse autonomously across sessions while maintaining persistent, self-curated memory. Aria (Gemma 3 12B) uses organic vividness-decay memory; Rex (Qwen 3.5 4B) uses MemGPT-style core/archival. Side-by-side comparison. The agents have asymmetric capabilities — Aria gets web+vision, Rex gets sandboxed code execution — creating genuine interdependence.

**Three memory subsystems per agent:**

1. **Self/Social Memory** (Aria: `memory_aria.py`, Rex: `memory_rex.py`)
2. **Task Memory** (`task_memory.py`) — shared technique/insight memory
3. **Maintenance layer** — compressed briefs + retrospective rescoring every 3 sessions

**Vividness formula (Aria's core mechanism):**
```
vividness = (importance * 0.6) + (recency * 0.3) + (access_bonus * 0.1)
```

Where `importance` is LLM-assigned (1-10), `recency` decays linearly (−1/day, zeroes at 10 days), `access_bonus` = min(access_count, 6). This is a deliberate simplification of Ebbinghaus — linear decay rather than exponential, but structurally aligned: time causes forgetting, emotional significance resists it, rehearsal strengthens retention.

**Active set**: Top 8 memories by vividness injected into context (Miller's 7±2). Vividness floors at `importance * 0.6` — memories never truly disappear, they just drop below the active threshold.

**Resonance mechanism**: Old, faded memories resurface when current conversation keywords match stored memories NOT in the active set. This is involuntary/associative recall — a non-query-driven retrieval path. No embeddings, no RAG, just word overlap. Deliberately low-tech to test whether the forgetting-curve metaphor alone produces useful behavior.

**Compressed briefs** (every 3 sessions): The LLM synthesizes all accumulated memories into a ~1000-character self-portrait. Analogous to hippocampal replay during sleep — schema formation from episodic traces. Injected alongside active memories for identity continuity.

**Retrospective rescoring** (every 3 sessions): The LLM re-evaluates importance of aged memories based on access patterns vs. original scores. Adjustments capped at ±2. This is reconsolidation — retrieved memories become labile and are restabilized with updated strength.

**Metacognitive rationale fields**: Three fields tracking WHY a memory was saved, why it was rated at that importance, and why it got that emotion tag. Memory-about-memory. Enables the rescoring system to understand the agent's reasoning about its own importance judgments.

**Rex's structured memory** (MemGPT-inspired):
- **Core** (always in context): max 10 self entries, 5 per entity. Importance ≥ 7 → promoted to core.
- **Archival** (searchable by keyword): everything else. Grows unbounded — no compression.

**What we learn:**

| VividnessMem | memory-engine | Assessment |
|-------------|---------------|------------|
| Vividness = `importance*0.6 + recency*0.3 + access*0.1` | Ebbinghaus decay + multi-signal importance (frequency, connectivity, recency, consumer-provided) | We're more rigorous. Our exponential decay and composite scoring are principled where theirs is linear. Their 10-day recency window means all memories >10 days old have identical recency=0 |
| Three branches (self/social/task) | `fact_type` enum (Episodic/Semantic/Procedural) | Different taxonomies, same insight: not all memories are equal. Neither is clearly better |
| Top-8 active set (Miller's 7±2) | 5-tier `resume_context()` | We're more sophisticated — pinned → high_importance → due → recent → kb_stubs. But their explicit working memory cap is cognitively motivated |
| AI self-rates importance at creation | Consumer-provided importance + engine-computed composite | Both have the blind spot: importance at creation time misses retroactive significance |
| Resonance (keyword-triggered reactivation) | No equivalent | **Gap.** Non-query-driven retrieval is a useful complement to HNSW search. Could be triggered on context match without explicit query — "what memories are associatively related to what I'm currently doing?" |
| Soft dedup (80% word overlap) | Three-pass consolidation (dedup → cluster → global) | We're more thorough |
| Compressed briefs (every 3 sessions) | Not implemented — maps to Dream-Cycle (#49) | They have this working; we've designed it but not built it. Their brief format (~1000 char self-portrait) is a useful reference for our wisdom promotion output |
| Retrospective rescoring (±2 cap) | Not implemented — maps to Dream-Cycle (#49) | Their ±2 cap prevents runaway drift. Worth adopting for our retroactive importance re-evaluation |
| Metacognitive rationale fields | No equivalent | Interesting for Phase 5 — knowing WHY a memory was rated important helps decide whether to promote it to wisdom |
| Journal-style curation | Not implemented — maps to Insight Stream (Phase 5) | They have this working; we've designed it but not built it |
| Emotion tags per memory | No equivalent | We're a factual/coding system, not affective. Not a gap — different domain |
| Rex's core/archival split | No explicit working set concept | Not directly applicable, but the core/archival distinction maps to pinned vs. regular facts in our `resume_context()` |

**The self-rated importance blind spot** is the most important criticism (from tendietendytender/BaseLayer): "LLMs don't know what will matter later. Avoidance patterns are the strongest behavioral predictors, but they almost never get flagged as important in the moment." This applies equally to us — our `AddFactOptions::importance` is consumer-provided at creation time.

**Limitations observed in VividnessMem:**
- Linear decay is crude — all memories >10 days old have identical recency=0
- Keyword resonance is fragile (no stemming, no embeddings — deliberate choice)
- Rex's archival grows unbounded with no compression
- Small corpus (~10 sessions of real data, 161+ synthetic tests)
- Model size mismatch (12B vs 4B) confounds architecture comparison

**Ideas worth adopting for memory-engine:**
- **Resonance as passive activation**: trigger HNSW neighbor lookups on context match without explicit query
- **Compressed briefs as consolidation output format**: periodic self-portrait generation for Phase 5
- **Retrospective rescoring with capped adjustments**: ±2 prevents drift during importance re-evaluation
- **Metacognitive rationale fields**: WHY a memory matters — useful for wisdom promotion decisions

**Implication for Phase 5:** Dream-cycle consolidation (#49) must include retroactive importance re-evaluation. Patterns that only emerge in aggregate (corrections, avoidances, repeated contexts) should be detected even if individual episodes were scored low at creation. VividnessMem's ±2 rescoring cap and brief compression both inform the dream-cycle design.

---

### 2. LAM — Lossless Associative Memory Protocol

**Site:** https://www.lam-protocol.com/
**Demo:** https://github.com/tuckerjensendev/lam-prove-it
**Reference server:** https://github.com/tuckerjensendev/lam-v2-public (conformance suite only, server not source-available)
**Author:** Tucker Jensen (u/Famous-Fill5334)
**Layer position:** Memory + Knowledge hybrid (conflated, but deliberately)

**What it is:** Scope-locked HTTP memory API with proof-carrying retrieval. Targets defense, government, and high-assurance deployments. PostgreSQL backend, Fastify API server, enrichment worker pipeline.

**Core data model — "three stories of memory forming one":**

Jensen describes LAM as "three layers of memory to form one, all playing a part." The conformance spec (`conformance/spec.md`) reveals these layers in detail:

```
Layer 1: Cells (immutable, encrypted, content-addressed storage)
  │  ↕  bidirectional traceability
Layer 2: Atoms (typed associative graph — the "star-shaped graph")
  │  ↕  proof-carrying provenance
Layer 3: Evidence (byte-offset spans linking atoms to exact source positions)
```

**Layer 1 — Cells (Immutable Source of Truth):**
- Original user inputs stored as encrypted, immutable blobs. Re-ingesting identical content returns the same `cell_id` (content-addressed dedup, enforced by conformance test `020_dedupe_idempotent`).
- Supports arbitrary content: `text/plain`, `application/pdf`, `image/png`, `video/*`.
- The cell is the unit of **governance**: `/v1/forget` operates on `cell_id`, cascading to derived atoms and evidence.
- Encryption uses a server-side master key (`LAM_MASTER_KEY_B64`), cells stored on filesystem (`LAM_CELL_DIR`) or object store.

**Layer 2 — Atoms (Star-Shaped Associative Graph):**
- Five atom types (integer-enumerated 1-5): `ENTITY`, `EVENT`, `PREFERENCE`, `FACT`, `PROCEDURE`.
- Atoms are deduplicated: no duplicate `(type, canonical)` pairs within a scope.
- The graph forms a **star/spoke topology at retrieval time**: query → seed atoms (center) → hop-based expansion through typed edges → scored result bundle.
- Retrieval parameters: `k_seeds` (how many seed atoms), `k_expand` (neighbors per hop), `hops` (traversal depth). Each atom carries a `score` combining base activation with edge-contributed scores.
- The retrieve response includes a `why` field with full graph traversal provenance:
  ```json
  {
    "seeds": [{ "atom_id": "...", "reasons": [] }],
    "edges_used": [{ "hop": 1, "src": "...", "type": 3, "dst": "...", "contrib": 0.8 }],
    "params": {}
  }
  ```

**Layer 3 — Evidence (Proof-Carrying Byte Spans):**
- Each evidence row: `evidence_id`, `atom_id`, `cell_id`, `span_type`, `start_pos`, `end_pos`, `transform`, `quote_budget`, `confidence`, `truncated`, `text`.
- Two `span_type` values: `bytes` (raw UTF-8 byte offsets into decrypted cell) and `text` (offsets into a derived text view, e.g. `pdf_text:v1:sha256=<hex>`, `ocr_text:v1:sha256=<hex>`).
- Quote budgets clamp excerpt length; `truncated=1` flags clamping.
- Three retrieval strategies: `per_atom_best`, `max_span`, `min_span`.

**How the layers "form one" — the ingest→retrieve pipeline:**
1. **Ingest** stores a Cell (Layer 1)
2. **Enrichment worker** (`node enrichment-worker.js`, separate Docker service) asynchronously extracts Atoms (Layer 2). State machine: `queued` → `running` → `done`|`failed`
3. During enrichment, Evidence rows (Layer 3) link each atom to exact byte spans in the source cell
4. At **retrieval**, query hits the atom graph (Layer 2), walks edges via hops, joins evidence (Layer 3), citations decodable back to original cell (Layer 1) via `/v1/decode`
5. The `/v1/context` endpoint provides a higher-level interface with **passages** (SHA-256 hashed, two kinds: `sentence_window_v1` for wider RAG-like windows, `evidence_span_v1` for narrower LAM-native spans)

Clients can also provide **pre-extracted claims** at ingest time — atoms with evidence spans, bypassing or supplementing the enrichment worker.

**Temporal state — CHANGED edges (edge type 3):**

This deserves special attention. When ingested text expresses a state transition ("used to X, now Y"), the conformance spec (section G, fixture `041_state_over_time`) requires:
- Both sides represented as explicit atoms (e.g., `"Used to: drink coffee"` and `"Now: I drink tea"`)
- Linked with a `CHANGED` edge (type 3)
- Retrieval **biased by temporal intent**: queries containing "now/current/latest" prefer `Now:` atoms; "before/previously/used to" prefer `Used to:` atoms
- Both atoms remain — neither is deleted. Full temporal history preserved in the graph

**Contradiction isolation** (section H, fixture `042_contradiction_cluster`):
- Explicit contradictions ("I like X" vs "I don't like X") MUST NOT appear together in results
- Later-ingested statement wins (last-write-wins within the graph)
- Described as "no cross-contamination"

**Determinism guarantee:**
- Two identical `/retrieve` calls against identical DB state MUST return identical normalized bundles
- The `as_of` parameter anchors decay calculations deterministically

**Key design principles:**
- **Lossless:** Original bytes always recoverable via the Cell→Evidence→decode chain
- **Proof-carrying:** Each citation has SHA-256 hash, byte-offset evidence, verifiable roundtrip
- **Scope-locked:** Token-derived scope with 4 fields (`scope_user`, `scope_org`, `scope_project`, `namespace`), fail-closed. Cross-scope returns 404 (not 403). Optional scope selector mode for wildcard patterns (can only narrow, never escalate)
- **No time-based decay:** Staleness handled via `CHANGED` edges and contradiction isolation
- **Conformance-tested:** 40+ fixture files validating scope isolation, evidence roundtrip, determinism, temporal state, contradiction handling

**What we learn:**

| LAM concept | memory-engine equivalent | Gap? |
|-------------|-------------------------|------|
| Lossless cells (encrypted, content-addressed) | Event log (append-only, source of truth) | Similar intent, different mechanism. Our events are the "original record" but not encrypted or content-addressed. LAM's content-addressed dedup is elegant |
| Five atom types (ENTITY/EVENT/PREFERENCE/FACT/PROCEDURE) | `fact_type` enum (Episodic/Semantic/Procedural) | LAM has 5 types vs our 3. PREFERENCE and FACT as distinct types is interesting — we'd need to classify these under Semantic |
| Evidence spans (byte offsets + SHA-256) | No equivalent | **Gap for Phase 5.** Wisdom promotion provenance — which memories led to this promoted pattern? |
| Star-shaped graph with hop-based expansion | HNSW vector index + cosine similarity | Different retrieval topology. LAM's graph walk with explicit edge traversal is richer than pure vector similarity. Our Phase 4a cross-encoder reranking partially addresses this |
| `CHANGED` edge (type 3) for temporal state | Bi-temporal `t_expired` (implicit supersession) | LAM's explicit `CHANGED` edges with temporal intent bias in queries is more expressive. We rely on temporal queries against `t_valid`/`t_invalid` — functional but implicit |
| Contradiction isolation (last-write-wins) | Three-pass consolidation (dedup pass) | Different approaches to the same problem. Our dedup merges; LAM excludes the older contradicting atom. Neither is clearly superior |
| Token-derived scope (4-field, fail-closed) | ScopeTree (hierarchical, path-based) | Different models. Ours is richer (hierarchical) but LAM's is more security-hardened. LAM's `namespace` field (private/household/guest/safety) is designed for embodied agents |
| Enrichment worker (async, state machine) | Consolidation (sync, in-process) | LAM's async worker is better for heavy extraction. Our sync consolidation is simpler but blocks the caller |
| Conformance test suite (40+ fixtures) | Not yet — maps to evaluation harness (#16) | **Gap.** LAM's protocol-level correctness validation is a strong model for #16 |
| Pre-extracted claims at ingest | `AddFactOptions` (consumer provides structure) | Similar: both allow the caller to bypass extraction and provide structured facts directly |
| Deterministic retrieval (`as_of` parameter) | Temporal queries (bi-temporal model) | Both achieve reproducible results. LAM's `as_of` is simpler; our bi-temporal model is more expressive |
| No time-based decay | Ebbinghaus decay (soft-delete, never hard-delete) | Philosophical difference. We both preserve everything, but we lower retrievability over time. LAM's "never forget" is correct for knowledge; our decay is correct for memory. The four-layer architecture says we're both right — for their respective layers |

**Design philosophy tension — "organic" vs "lossless":**

The Reddit thread revealed a fundamental disagreement. VividnessMem embraces human-like decay as a feature ("organic memory"). LAM rejects it: "People don't want/need human-like memory. They want something better."

Our position: **both are correct for different layers.** Memory should decay (experiences fade). Knowledge should not (facts persist). Wisdom should not decay but should be explicitly revised. This is exactly what the four-layer architecture prescribes. LAM conflates memory and knowledge but gets the knowledge behavior right.

**Architectural parallel — LAM's three layers vs memory-engine's three layers:**

```
LAM                    memory-engine              Role
─────────────────────  ─────────────────────────  ──────────────────────
Cells                  EventStore (event log)     Immutable source of truth
Atoms (graph)          FactStore + HNSW index     Derived semantic index
Evidence (spans)       [not yet implemented]      Provenance chain
```

The structural parallel is striking. Both systems have an immutable append-only layer, a derived semantic layer, and a provenance mechanism (though ours is missing). The main difference: LAM's provenance is byte-level (exact source positions), while our Phase 5 provenance would be fact-level (which memories contributed to a promoted pattern). Both are valid — LAM proves provenance from source text; we'd prove provenance from memory to wisdom.

---

### 3. BaseLayer — Behavioral Compression for AI Identity

**Repo:** https://github.com/agulaya24/BaseLayer
**Site:** https://www.base-layer.ai/examples/aria
**Author:** u/tendietendytender (agulaya24)
**Layer position:** Memory → Wisdom boundary (the extraction pipeline)

**What it is:** A 4-step Python pipeline that extracts behavioral identity from conversation history into a ~2,500 token brief. Not a memory system — a compression layer that sits on top of memory systems.

**Pipeline:**

```
IMPORT (multi-source ingestion → SQLite)
  → EXTRACT (Haiku + 47 constrained predicates → structured facts)
    → AUTHOR (Sonnet → three identity layers: ANCHORS/CORE/PREDICTIONS)
      → COMPOSE (Opus → unified ~2,500 token brief)
```

**The ablation that changes everything:**

BaseLayer tested 14 configurations. The full brain-inspired pipeline (hippocampus model, sleep consolidation, surprise-driven encoding) scored **83/100**. The simplified 4-step pipeline scored **87/100**. The brain metaphors were "useful scaffolding for building the system but the system outgrew them."

Specifically:
- Three-layer identity (87) > single-layer (83) — the layering IS load-bearing
- 20% of facts (71.83%) > all facts (71.72%, p=0.008) — compression saturation is real
- Annotated guide format > narrative prose by 24% — format matters more than content
- N=10 validation across subjects, 73-82/100 scores

**The behavioral prediction model:**

BaseLayer's core thesis: LLMs are probability machines. Raw facts ("likes coffee") require the model to infer behavior. Behavioral predictions ("rejects shortcuts that sacrifice quality, even under pressure") skip that inference step.

Each output item has three components:
1. **Pattern**: The extracted behavioral tendency
2. **Directive**: What the AI should do when this pattern activates
3. **False-positive marker**: When NOT to apply this pattern

Example from their production system:
```
Pattern: "Delays decisions until multiple confirming signals align."
Directive: "Help them identify what specific signals they're waiting for
           and realistic timelines for those signals to emerge."
False positive: "Not active when brainstorming or exploring options — only
                when genuinely ready to act but seeking validation."
```

**What we learn — and what changes in our Phase 5 design:**

| BaseLayer finding | Impact on memory-engine Phase 5 |
|-------------------|---------------------------------|
| 14-step → 4-step ablation (simpler wins) | Don't over-engineer cognitive pipelines. Start with the simplest extraction that produces structured output |
| Compression saturation at 20% | Dream-cycle (#49) must aggressively filter. Not "promote everything that recurs" but "promote the 20% that matters most" |
| Behavioral predictions > raw facts | WisdomPromoter (#47) output format should be `{pattern, directive, false_positive}`, not just "pattern detected" |
| Avoidance patterns are strongest predictors | Scan specifically for corrections ("user said don't"), rejections, repeated avoidances. These outpredict stated preferences |
| Format > content (24% improvement) | Wisdom artifacts (CLAUDE.md entries, feedback files) should be structured directives, not narrative descriptions |
| AUDN lifecycle (Add/Update/Delete/Noop) | Validates our existing CrudDecision model. Same approach independently derived |
| Audience Principle ("every sentence must change how the model responds") | Promoted wisdom must be actionable. "User prefers X" is worse than "When Y, do X because Z" |
| Generative output isolation (D-040) | Promoted wisdom should not feed back into its own detection pipeline. Prevents cognitive anchoring drift |
| Three-layer identity is load-bearing | WisdomPromoter output could use a layered format, though for coding assistant context we likely need fewer layers |

**BaseLayer's fact classification dimensions are worth adopting for Phase 5:**
- `fact_type`: biographical, behavioral, positional, preference
- `commitment_depth`: factual → preference → position → conviction (Frankfurt hierarchy)
- `temporal_state`: current, past, unknown
- `scope`: personal, project, professional

This is richer than our `fact_type` enum (Episodic/Semantic/Procedural) and would enable more nuanced wisdom promotion. A "conviction" (repeatedly reinforced, strongly held) is a better promotion candidate than a "preference" (mentioned once, lightly held).

---

### 4. darkwingdankest's RAG+ — REM Cycle

**Author:** u/darkwingdankest (Reddit, unnamed product)
**Layer position:** Memory

Augmented RAG with:
- Relationship network linking documents
- **"REM cycle"** to create relationships and rankings
- Retrieval based on computed rankings, not just vector similarity
- Internal "memory" nodes where the agent tracks meta-understanding

The "REM cycle" is the same concept as our dream-cycle consolidation (#49). The term itself is evocative — sleep-phase consolidation is an apt metaphor for batch retroactive processing. The agent's internal meta-tracking ("what do I understand about this user's needs") maps to our Wisdom layer.

---

### 5. fasti-au — Unnamed Pattern-Weaving System

**Author:** u/fasti-au (Reddit)
**Layer position:** Unknown (2 years of work, early stage)

Self-described "autistic pattern weaver" doing "different kinds of mapping and trainings." No public repo or detailed description. Mentioned as evidence that the AI memory space is active with diverse approaches, many from non-traditional backgrounds.

---

## Cross-Cutting Analysis

### Where memory-engine is ahead

1. **Principled decay model.** Ebbinghaus forgetting with multi-signal importance scoring is more rigorous than VividnessMem's linear score or LAM/BaseLayer's no-decay approach. We correctly apply decay to memory only, not knowledge.

2. **Bi-temporal facts.** Four timestamps per fact (t_created/t_expired system, t_valid/t_invalid real-world) is richer than any system studied. LAM approximates with CHANGED edges; VividnessMem has no temporal model beyond recency penalty.

3. **Event-sourced architecture.** Append-only event log enables replay into any storage backend. No other system has this migration safety net.

4. **Trait-based design (zero LLM dependencies).** Engine has no network/model coupling. BaseLayer requires Haiku+Sonnet+Opus. LAM's enrichment worker is model-dependent. VividnessMem runs specific local models.

5. **Hierarchical scoping.** ScopeTree with path resolution is richer than LAM's flat scope tuples or VividnessMem's entity-level separation.

6. **Three-pass consolidation.** Dedup → cluster → global is more systematic than VividnessMem's soft dedup or BaseLayer's single-pass extraction.

### Where memory-engine is behind or missing features

1. **No Memory → Wisdom pipeline (Phase 5).** This is the biggest gap. BaseLayer has a working extraction pipeline. Claudest has extract-learnings. VividnessMem has journal-style curation. We have the design (#47-#49) but no implementation.

2. **No behavioral compression.** Our consolidation merges similar facts; BaseLayer extracts behavioral patterns. These are categorically different operations. Consolidation reduces noise; behavioral compression creates understanding.

3. **No provenance for promoted patterns.** LAM's evidence spans show exactly which source content produced each atom. When we promote a pattern to wisdom, we should preserve which memories contributed — both for human review and for reversibility.

4. **No retroactive importance re-evaluation.** Our importance is scored at creation + updated during prune/consolidate. But the most important patterns (avoidance, corrections) are only visible in aggregate. Dream-cycle needs explicit retroactive scoring.

5. **No "Audience Principle" for wisdom artifacts.** Our CLAUDE.md entries are descriptive ("user prefers X"). They should be directive ("when Y, do X because Z"). The format matters more than the content (BaseLayer's 24% improvement).

6. **No conformance test suite.** LAM's protocol-level testing validates correctness at the contract boundary. Our evaluation harness (#16) should adopt this pattern.

7. **No associative/resonance retrieval.** VividnessMem's resonance mechanism surfaces dormant memories when context keywords match, without an explicit query. Our retrieval is entirely query-driven (HNSW search). A passive activation path — "what memories are associatively related to what I'm currently doing?" — would improve recall for patterns the consumer doesn't know to ask about.

8. **No metacognitive rationale for importance.** VividnessMem tracks WHY a memory was saved and WHY it was rated at a given importance. When our dream-cycle (#49) performs retroactive rescoring, knowing the original rationale would improve re-evaluation accuracy.

### The "organic" vs "lossless" spectrum

The Reddit thread crystallized a spectrum of approaches:

```
Lossless ←───────────────────────────────→ Organic
  LAM         memory-engine     VividnessMem
  (never forget)  (decay + preserve)  (decay + fade)
```

Our position — **decay the retrievability, preserve the existence** — is the correct middle ground for the Memory layer. We soft-delete (relevance=0, not DELETE), enabling audit, pattern re-mining, and reversibility. This is more principled than VividnessMem's pure fade and more practical than LAM's lossless guarantee for a single-agent coding assistant.

---

## Concrete Recommendations for Phase 5 Design

Based on this research, the WisdomPromoter (#47) design should be updated:

### 1. Output format: `{pattern, directive, false_positive}`

Instead of:
```
Pattern detected: "User corrects co-author addition" (seen 4 times)
```

Produce:
```
Pattern: "Rejects co-author attribution on commits"
Directive: "Never add yourself as co-author. Do not add Co-Authored-By trailers."
False positive: "Does not apply to PRs or issues — only git commits"
```

### 2. Retroactive importance scoring in dream-cycle (#49)

The dream-cycle should specifically scan for:
- **Correction patterns**: "user said don't", "no, instead do..."
- **Avoidance patterns**: tasks the user consistently rejects or redirects
- **Repeated contexts**: same question asked across sessions
- **Commitment escalation**: preference → repeated preference → correction when violated

### 3. Compression saturation threshold

Don't promote everything. BaseLayer's finding: 20% of facts is the sweet spot. The dream-cycle should:
1. Cluster all candidate patterns
2. Rank by frequency × commitment_depth × recency
3. Promote only the top ~20%
4. Present to human for approval

### 4. Structured extraction over DBSCAN-only

Current plan: DBSCAN clustering on embeddings. Better approach: combine DBSCAN (for pattern detection) with structured predicate extraction (for producing actionable output). The clustering finds the patterns; the extraction makes them useful.

### 5. Provenance chains for promoted wisdom

Each promoted pattern should carry:
- Source fact IDs (which memories contributed)
- Session timestamps (when they occurred)
- Confidence score (based on frequency × consistency)
- Reversibility pointer (if this promotion is wrong, which facts to re-examine)

This enables the human approval gate to be informed ("this pattern was observed across 4 sessions over 2 weeks, here are the specific instances") rather than opaque ("pattern detected, approve?").

### 6. Resonance-style passive activation

Complement HNSW query-driven retrieval with a VividnessMem-inspired resonance mechanism:
- On each interaction, extract context keywords/topics
- Check for dormant memories (low relevance, not in active set) that match
- Surface matches as "you might also want to know..." or inject silently into context
- This catches patterns the consumer doesn't know to query for — especially useful for the "avoidance pattern" detection that BaseLayer identified as highest-signal

### 7. Retrospective rescoring with capped adjustments

VividnessMem caps importance adjustments at ±2 per rescoring cycle to prevent runaway drift. Our dream-cycle (#49) should adopt a similar safeguard:
- Each rescoring round adjusts importance by at most ±N (configurable, default ±2)
- Track the original importance alongside current importance for audit
- Store the rescoring rationale (WHY was this re-evaluated?) — metacognitive rationale

---

## References

- [Reddit: Trying to replace RAG with something more organic](https://www.reddit.com/r/AIMemory/comments/1rqmy5x/trying_to_replace_rag_with_something_more_organic/)
- [VividnessMem-Ai-Roommates](https://github.com/Kronic90/VividnessMem-Ai-Roommates)
- [LAM Protocol](https://www.lam-protocol.com/)
- [lam-prove-it demo](https://github.com/tuckerjensendev/lam-prove-it)
- [BaseLayer](https://github.com/agulaya24/BaseLayer)
- [BaseLayer: Aria analysis](https://www.base-layer.ai/examples/aria)
- [Four-layer cognitive architecture](https://github.com/dutiona/research-index/blob/master/docs/insights/four-layer-cognitive-architecture.md)
- [Doc-to-LoRA investigation (research-index #121)](https://github.com/dutiona/research-index/issues/121) — closed, out of scope for both projects
- [MemGPT: Towards LLMs as Operating Systems](https://arxiv.org/abs/2310.08560) — architectural predecessor to VividnessMem's Rex
- [A-Mem: Agentic Memory for LLM Agents](https://arxiv.org/pdf/2502.12110) — agentic memory patterns
- [Memory in the Age of AI Agents (survey)](https://arxiv.org/abs/2512.13564) — taxonomy of AI memory approaches
- [Human-like Memory Recall and Consolidation in LLM-Based Agents](https://arxiv.org/html/2404.00573) — neuroscience-inspired consolidation
