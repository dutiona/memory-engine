# Round 1 — Gemini Position

## Q1: Pipeline Architecture (Collapse to Two)

Collapse into InsightCollector (Fast Path) and DreamCycle (Slow Path). WisdomPromoter should be a logic strategy within the DreamCycle, not a standalone architectural peer.

BaseLayer's success with 4 steps proves that distinct architectural boundaries add friction.

- Use Case A (Coding): Needs immediate capture of user corrections (InsightCollector) and nightly cleanup (DreamCycle).
- Use Case B (Agents): Needs the same topology. Separating "Promotion" from "Consolidation" creates synchronization headaches. Merge them: The DreamCycle trait iterates over recent facts, dedups them (consolidation), and then checks for patterns (promotion) in the same pass.

## Q2: Behavioral Compression vs. Fact Consolidation

Distinct operations, executed sequentially.

- Fact Consolidation is geometric/semantic (noise reduction). It reduces 10 "I failed to compile" logs into 1 summary.
- Behavioral Compression is logical/inference (understanding). It derives "I should always use --release for this project" from that summary.

You must run consolidation first to clean the input for the behavioral compression trait. For a Rust library, the DreamCycle trait should expose a `compress_behavior(Vec<Fact>) -> Option<Wisdom>` hook. This relies on the consumer's intelligence (LLM) to bridge the gap, keeping the engine deterministic.

## Q3: Resonance / Passive Activation

Reject passive activation in the engine core. VividnessMem's "random surfacing" is valuable for agent "aliveness" (Case B) but distracting for coding assistants (Case A).

Solution: Expose a lightweight `sample_dormant(n)` API.
- The consumer (Autonomous Agent) runs a "boredom loop" that calls this API to simulate resonance.
- The consumer (Coding Assistant) ignores it. Keep the engine reactive; let the application layer drive the "ghost in the machine."

## Q4: Provenance for Promoted Wisdom

Mandatory strict provenance (`Vec<FactId>`). LAM proves that trust requires traceability.

- Case A: If the engine suggests a coding pattern, the user needs to click "Why?" and see the 3 previous compile errors that caused this rule.
- Case B: Agent identity is unstable without roots. If an agent "believes X," it must know which conversations formed that belief. Summary-level attribution ("detected from interactions last week") is insufficient for debugging or undoing bad wisdom. If the source facts are forgotten/expired, the wisdom becomes "axiomatically pinned."

## Q5: Explicit CHANGED Edges vs. Bi-temporal Model

Stick to Bi-temporal. Switching to LAM's explicit CHANGED edges creates graph bloat and schema migration nightmares for a marginal queryability gain.

- Reasoning: You already have `t_invalid`. A "change" is simply Fact A (`t_invalid = T1`) and Fact B (`t_valid = T1`) sharing a semantic link.
- Implementation: Phase 4's `explain_fact` API can virtually reconstruct the "Change Edge" by querying the history. Don't materialize edges you can compute from timestamps.

## Q6: Retrospective Importance Rescoring

Aggressive on Avoidance, Conservative on Promotion.

- Avoidance (Negative Feedback): If a user says "Don't do that," the DreamCycle should retroactively nuke the importance of the behavior that caused it and boost the correction. BaseLayer identifies this as the highest-value signal.
- General Rescoring: Cap at ±2. Volatility in memory weights leads to schizophrenic agent behavior (Case B). Stability is preferred over rapid adaptation for long-term identity.

## Q7: The 20% Compression Saturation

Apply strictly to Episodic Memory; Reject for Procedural Knowledge. BaseLayer's 20% rule applies to narrative (Case B). It does not apply to reference (Case A).

- Coding: You cannot compress a library's API surface by 80% without hallucinations.
- Agents: You can and should compress 6 months of chat logs by 80%. Policy: The DreamCycle trait should accept a `target_compression_ratio` per FactType. Set Episodic to 0.2, Semantic/Procedural to 0.8.
