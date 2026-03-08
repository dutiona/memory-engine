1. **Memory architecture (source of truth vs unified store)**
Final position: use **event-sourced memory as source of truth**, with graph/vector state as **derived, query-optimized projections**. This takes Codex’s durability/replay advantage and directly addresses Gemini’s “temporal lobotomy” concern by preserving chronological causality in the log, not just final embeddings. Claude’s simplification point is still valid: start with one operational data plane if possible (e.g., one DB product), but architecturally keep write semantics append-first and projection-based.  
**Convergence:** all agree memory must support temporal continuity and multi-modal retrieval.  
**Remaining disagreement:** “single unified DB as truth” vs “unified runtime with event-log truth.” I side with the latter for auditability, rollback, and model-evolution safety.

2. **Build strategy (modular monolith vs native multi-service core)**
Final position: start as a **modular monolith** with strict internal boundaries, contracts, and extraction seams; do not pre-split into services. Codex and Claude are right on sequencing and YAGNI. Gemini is right that extraction often fails when boundaries are sloppy, so enforce service-like module contracts from day one (RPC-like interfaces, anti-corruption layers, independent ownership). For performance-critical paths, allow a **native Rust core library early**, but embed it in-process first.  
**Convergence:** avoid premature distributed architecture.  
**Remaining disagreement:** Gemini’s “native-first everywhere” is too costly before real bottlenecks; I adopt “native-first only on profiled hot paths.”

3. **MLX/runtime decision (gateway vs direct engine use)**
Final position: define a **canonical inference API/gateway contract** now, but keep implementation lightweight. Claude is right that mlx-lm’s OpenAI-compatible surface can accelerate early delivery. Codex is right that one canonical contract prevents runtime sprawl. Gemini is right that production-grade guarantees may require Candle/Ratchet or another Rust-native runtime later.  
Operationally: start with one runtime (mlx-lm if on Apple stack), benchmark at quant/model/SLO levels, and switch/add runtime only on measured failures (latency, throughput, reliability, cost).  
**Convergence:** runtime choice should be benchmark-driven.  
**Remaining disagreement:** whether mlx-lm is production-ready by default; I treat it as provisional, not permanent.

4. **Fine-tuning policy (frozen brain vs adaptive updates)**
Final position: **freeze by default, adapt selectively**. Codex’s caution is correct for stability and preventing silent knowledge drift; Gemini’s point is correct that “never adapt” becomes a capability bottleneck. Adopt a tiered policy: prompt/RAG first, PEFT/LoRA for style or narrow behavior second, broader adaptation only after counterfactual evaluation and regression gating. Claude’s Doc-to-LoRA note is promising but should remain controlled Tier-3 experimentation until reliability data exists.  
**Convergence:** no blind continuous full-model tuning.  
**Remaining disagreement:** cadence of small-batch PEFT; I allow it only with explicit offline eval, canary rollout, and rollback-ready checkpoints.

5. **Escalation and execution safety (risk matrix, intent lock, dry-run)**
Final position: combine all three into one protocol: **intent-lock -> risk scoring -> mandatory preconditions -> dry-run/speculative simulation -> gated execution**. Gemini’s intent-locking should be first-class for user trust. Codex’s risk-scored matrix and precondition checks should gate all high-impact actions. Claude’s speculative execution is valuable if strictly bounded and never presented as committed state. Require explainable diffs, blast-radius estimate, and explicit user confirmation for privileged or irreversible steps.  
**Convergence:** dry-run and explicit safeguards are mandatory.  
**Remaining disagreement:** wording varies (“speculative” vs “simulation”), but functionally the model is converging on pre-execution verification plus user-controlled commit.

**CONSENSUS POINTS**
- Start simple operationally; avoid premature service sprawl.
- Use measurable SLOs/benchmarks to drive runtime and architecture changes.
- Safety must include precondition checks and dry-run behavior.
- Fine-tuning should be controlled, evaluated, and rollback-capable.
- Memory must preserve long-horizon context; not just shallow vector recall.

**OPEN DISAGREEMENTS**
- Whether a unified DB should also be the semantic source of truth.
- How early to commit to a Rust-native inference/runtime stack.
- Whether mlx-lm is suitable for production now vs only as an incubation runtime.
- How aggressive continuous PEFT should be in steady state.
