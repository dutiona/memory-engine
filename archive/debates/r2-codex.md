**1) Memory Architecture**
- **Claude weak point:** “memory writing is the real problem” is true but underspecified; AgeMem-style ops without a hard conflict model will drift into duplicate, stale, and contradictory writes.
- **Codex weak point (mine):** confidence-threshold async promotion assumes confidence is calibrated. It usually isn’t, so bad facts get promoted with high confidence and become sticky.
- **Gemini weak point:** symbolic UID handoff assumes entity resolution is stable across noisy text; it will fragment identities unless you enforce canonicalization and merge policies.
- **Where all agree but may be wrong:** everyone assumes multi-store complexity is worth it from day one.
- **Stronger alternative:** start with **single-writer event-sourced memory log + materialized views** (hot KV, optional graph projection). Make graph fully derived, never source-of-truth.

**2) Build/Architecture Strategy**
- **Claude weak point:** “new workspace, extract patterns” sounds clean but underestimates integration entropy; pattern extraction often recreates bugs without test intent.
- **Codex weak point (mine):** “thin core + ports/adapters” can become architecture theater if domain boundaries are not proven by latency/failure isolation requirements.
- **Gemini weak point:** service-oriented orchestrator is premature; distributed boundaries before observability maturity means debugging hell.
- **Where all agree but may be wrong:** everyone rejects forking but assumes reusable patterns are transferable without behavioral coupling.
- **Stronger alternative:** do a **modular monolith first** with strict package boundaries and contract tests. Split to services only when one module has independent scaling/security/SLA pressure for 2+ quarters.

**3) MLX + Ollama Runtime Split**
- **Claude weak point:** “Ollama for Jetson only” hard-codes deployment policy into architecture; hardware and model support will change faster than this assumption.
- **Codex weak point (mine):** “strict role separation + conformance tests” ignores runtime scheduler realities; role purity collapses during load spikes.
- **Gemini weak point:** “System 1/System 2 bus” is cognitively cute but operationally vague; no hard routing objective or cost/latency optimizer.
- **Where all agree but may be wrong:** dual-runtime by default. That doubles failure modes, model skew, and observability burden.
- **Stronger alternative:** pick **one canonical inference gateway** with pluggable providers behind identical eval harnesses. Add second runtime only after measured unmet SLOs (latency/cost/offline constraints).

**4) No-Fine-Tune / Limited Fine-Tune**
- **Claude weak point:** monthly Jetson ranker training risks low-signal updates and silent regressions from weak labels.
- **Codex weak point (mine):** weekly gated LoRA for retrieval/ranking is cadence-driven, not signal-driven; can overfit to recent incidents.
- **Gemini weak point:** nightly 4090 consolidation assumes stable, high-quality replay data; most memory traces are noisy and policy-contaminated.
- **Where all agree but may be wrong:** “brain stays frozen” may be dogmatic. Some domains require lightweight adaptation for style, tool-use priors, or terminology drift.
- **Stronger alternative:** **offline counterfactual evaluation first, then selective adaptation**: prioritize retrieval + policy tuning; permit small supervised adapters only when frozen baseline repeatedly misses validated tasks.

**5) Escalation / Safety Control Plane**
- **Claude weak point:** timeout-based degradation can turn into silent quality collapse without explicit user-visible confidence and deferred action receipts.
- **Codex weak point (mine):** SLA/fallback/partial-order planning adds complexity before proving basic escalation precision/recall.
- **Gemini weak point:** “hallucinate then rollback” is dangerous in high-stakes flows; rollback cannot undo user trust or side effects once surfaced.
- **Where all agree but may be wrong:** three-tier escalation may be too coarse; risk is multidimensional (impact, reversibility, privacy, compliance, user context).
- **Stronger alternative:** **risk-scored escalation matrix** with mandatory precondition checks, dry-run simulation, and irreversible-action guardrails. Require explicit user confirmation for high-impact branches, not just tier transitions.
