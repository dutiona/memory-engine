# ADR-0012: Decay Exemption for Knowledge-Shaped Fact Types

**Status:** Accepted
**Date:** 2026-06-12

## Context

ADR-0006 scores every fact with the same 4-signal importance formula, using a 69-day Ebbinghaus half-life default transplanted from Memori. That default applied to **all** `FactType`s — including `Semantic` (declarative assertions) and `Procedural` (validated procedures).

This is the category error the four-layer architecture exists to prevent: a paper's findings do not become less true after 69 days. In the layer mapping, Semantic facts are knowledge-shaped (Knowledge layer: persistence via supersession) and Procedural facts are wisdom-shaped (Wisdom layer: evidence-gated revision). Applying cognitive decay to them contradicts the thesis in the engine's own defaults — the 2026-06 program review flagged this as its top coherence finding (coherence:C3), and with the repository public since 2026-06-12, any reviewer reading `traits.rs` alongside the paper finds the contradiction in minutes.

## Decision

`ForgetPolicy` gains a `decay_exempt_types: HashSet<FactType>` field, **default `{Semantic, Procedural}`**.

**Content predicate** (what makes a type exempt): a fact type belongs in the set iff its facts' truth value is independent of time-since-encoding. Declarative assertions and validated procedures qualify; episodic facts are time-indexed experience records and decay by design.

Enforcement at two points:

1. `compute_importance`: recency stays `1.0` for exempt types — age does not degrade a knowledge-shaped fact's score, so materialized importance stays honest for downstream consumers.
2. `prune`: exempt types bypass the expiry filter entirely (same mechanism as `is_pinned`). This is the hard guarantee — no weight/threshold configuration can expire them. They still count in `facts_evaluated` and still get scores materialized.

**Escape hatch:** an explicit `half_life_overrides` entry for a type wins over the exemption and re-enables finite-half-life decay (`ForgetPolicy::is_decay_exempt` encodes the rule). This preserves ADR-0006's documented `Procedural=365` example as valid configuration and keeps the eval suites able to exercise decay mechanics on any type.

Lifecycle for exempt facts: supersession (`t_expired` via conflict resolution) and evidence-gated revision — never decay.

## Alternatives Considered

- **M→K promotion edge** — route knowledge-shaped facts out of the memory engine into the knowledge layer. More faithful to the four-layer story, but cross-system plumbing; deferred to the harness-integration thin slice. The exemption is compatible with adding promotion later.
- **`f64::INFINITY` half-life override** — keeps facts inside the decay framework with a degenerate constant. Rejected as leaky: under non-default weights/thresholds an "infinite half-life" fact can still fall below `min_importance` and expire.

## Consequences

- Default behavior change (pre-1.0): Semantic and Procedural facts persist indefinitely unless superseded, conflict-resolved, or explicitly opted back into decay.
- Eval/conformance suites updated: decay-mechanics scenarios now use `Episodic` subjects; the pinned=false conformance test exercises the override-wins escape hatch.
- Episodic facts remain on the 69-day default until session-backfill telemetry fits a measured constant (#139/#140) — the transplanted value now governs only the type it can defensibly govern.
- Storage growth for exempt types is bounded by supersession and consolidation, not decay.
