# ADR-0010: Wisdom Revision Gate DSL

**Status:** Proposed
**Date:** 2026-04-13
**Gap ID:** ME-P0-A
**Phase:** 5 (Cognitive Pipelines) → enables Wisdom-layer promotion
**Related:** ADR-0001 (event sourcing), ADR-0004 (trait-based extensibility), ADR-0008 (materialized importance)

## Context

Phase 5a ships the substrate for the consolidation → wisdom pipeline (`PromotionProvenance`, `LineageTable`, `InsightStream`, `DreamCycle` trait contract, `sample_dormant`). What is still missing is the **revision gate**: the predicate that decides *when* a candidate insight crosses out of the Memory store and becomes durable procedural knowledge in the Wisdom layer (the consumer's prompts, skills, or fine-tuned weights).

Two things drive the need for a dedicated DSL rather than ad-hoc Rust closures:

1. **Auditability.** Wisdom-layer promotion is the most consequential mutation in the engine — a bad promotion contaminates the consumer's identity. Predicates must be inspectable, testable, replayable, and serializable into the event log next to the `PromotionProvenance` they justify. Free-form closures are none of those things.
2. **Vocabulary convergence.** The April 2026 landscape review surfaced a parallel vocabulary in Papr's Python SDK (`papr-pythonSDK/cookbook/ai_sales_intelligence.py`) for declarative graph-mutation policies. Their decorator set decouples *graph structure specification* from *resolution behavior* and gives first-class boolean logic plus a narrow LLM call-out (`Auto(prompt)`) that fires only at constraint-evaluation time. This is exactly the shape we need, in a different language.

The April 2026 design refinement note in `~/dev/autonomous-agent-project/raw/docs/summaries/02-system-design.md §11.3` summarizes the borrowing:

> Papr (papr2026sdk) provides a vocabulary for declarative specification of the Wisdom-layer revision gate that we were missing. Papr's Python decorators (`@upsert`, `@lookup`, `@resolve(on_miss="error")`, `@constraint(when=And(Or(...),Not(...)), set={"flagged": True, "summary": Auto("Summarize in 1-2 sentences")})`) decouple graph-structure specification from resolution behavior and enable first-class boolean logic with `Auto(prompt)` callouts to LLMs at constraint-firing time.

The canonical invariant adopted from r/Rag 1sgvvig OP (April 2026) is:

> **"Projections never silently become the truth they summarize."**

In our setting that means: a `CycleReport` from `DreamCycle` cannot silently mutate Wisdom-layer state. Every promotion must traverse a declared, auditable gate.

### Why not just a Rust closure?

| Concern | Closure | DSL |
| --- | --- | --- |
| Replay against historical events | breaks if the closure code changes | DSL is data, persists in event log |
| Inspect why a promotion fired | opaque | predicate is structured AST |
| Test in isolation | requires building a fake engine | evaluate against a `Fact` snapshot |
| Compose AND/OR/NOT cleanly | nested closures get unreadable | first-class operators |
| Delegate one branch to LLM | every closure that does this leaks the consumer's LLM dependency into the engine call site | `Auto(prompt)` becomes a single typed seam |
| Serialize over MCP for operator review | impossible | trivial |

### Why borrow Papr's vocabulary specifically?

Papr is the only surveyed system that (a) ships a typed declarative DSL for graph-mutation policies, (b) keeps LLM calls behind a narrow `Auto()` seam, and (c) supports compound boolean logic at the predicate level. Hermes Agent's `agentskills.io` writes back markdown skills via LLM-in-the-loop with no formal gate. Mem0/Graphiti use imperative CRUD. Papr is the closest existing prior art to what we need, and aligning vocabulary lowers the cost of cross-system literacy for paper #3 readers.

## Decision

Introduce a new crate-level module `wisdom::policy` exposing a typed Rust DSL for Wisdom-layer promotion predicates. The DSL borrows Papr's vocabulary but re-types it for Rust's static guarantees and for the `LLM-free engine + consumer trait` boundary established in ADR-0004.

### Surface (sketch)

```rust
// crate::wisdom::policy

pub struct WisdomPolicy {
    pub structure: WisdomSchema,            // node/edge shape this policy applies to
    pub promotion_predicate: Expr,          // the gate
    pub conflict_strategy: Resolution,      // upsert / lookup / resolve_or_error
    pub llm_callout: Option<ConsumerTraitCallback>,  // Auto(prompt) analog
}

pub enum Resolution {
    Upsert,                       // @upsert
    Lookup,                       // @lookup (no creation)
    ResolveOrError,               // @resolve(on_miss="error")
}

pub enum Expr {
    // Leaves
    AccessCount { gte: u32 },
    ImportanceScore { gte: f64 },
    HasOutcome { kind: OutcomeKind, gte: u32 },
    HasProvenance { min_sources: u32 },
    FieldEquals { field: FieldRef, value: Value },
    FactType(FactType),
    InScope(ScopePath),
    // Combinators
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
    // Narrow consumer-trait seam
    Auto(ConsumerTraitCallback),  // see below
}

pub struct ConsumerTraitCallback {
    pub prompt: String,
    // The engine never owns an LLM. At gate evaluation time the engine
    // hands `(fact, prompt)` to the consumer-supplied trait and receives
    // back a typed verdict — never a free-form string spliced into state.
    pub returns: AutoVerdict,
}

pub enum AutoVerdict {
    Bool,           // gate continues / aborts
    Score(f64),     // numeric, compared against a sibling threshold
    EnumChoice(&'static [&'static str]),
}
```

### Evaluation contract

`WisdomPolicy::evaluate(&self, fact: &Fact, ctx: &PolicyContext) -> PolicyDecision`

- Pure function over `(Fact, PolicyContext)` for the deterministic branches.
- Auto-leaves are routed through a new narrow consumer trait `WisdomAutoEvaluator` (see ME-P2-G) — the engine itself never imports an LLM client.
- `PolicyDecision` carries a `gate_trace: Vec<EvalStep>` so that every promotion event log entry can be paired with the exact predicate evaluation that justified it. This is the auditability requirement made concrete.

### Wiring into Phase 5a

`DreamCycle::run()` already produces a `CycleReport` with promotion candidates. The change is:

1. The default `DreamCycle` impl gains an optional `WisdomPolicy` parameter.
2. For each promotion candidate, the policy is evaluated; the verdict (with its `gate_trace`) is stored on the existing `PromotionProvenance` row.
3. Only candidates with `Allow` verdicts mutate Wisdom-layer state via the (consumer-owned) Wisdom sink.

This change is additive — `DreamCycle` without a policy is the current behavior. Existing tests pass unchanged.

### Persistence

`Expr` is `Serialize + Deserialize` (serde). Promotion provenance gains a `gate_predicate: Option<serde_json::Value>` column. Replaying the event log + promotion table against an old engine version is a deterministic operation: the predicate is data, not code.

### What stays out of the engine

- The LLM. `Auto(prompt)` is a seam, not an implementation.
- The Wisdom sink. Whether the consumer's Wisdom layer is a system prompt, a skill module, or a fine-tuning dataset is none of the engine's business — `WisdomPolicy` returns `Allow`, the consumer ships the bytes.
- Soft-gating of *retrieval*. Predicates govern promotion, not query results. Retrieval-time policy is a separate (and simpler) story.

## Consequences

### Positive

- **Audit trail completeness.** Every Wisdom promotion is paired with the structured predicate that justified it. ADR-0001's "no surveyed system provides auditability" claim now extends to the Wisdom layer.
- **Replayable promotions.** Predicates are serialized data; replaying the event log produces identical Wisdom mutations on a fresh engine. This unblocks paper #3 §System Design's "deterministic replay" claim for the cognitive pipeline.
- **LLM-free engine purity preserved.** The narrow `Auto` seam keeps every LLM call behind the consumer trait boundary. The engine still has zero LLM dependencies (ADR-0004 invariant intact).
- **Cross-system vocabulary alignment.** Readers familiar with Papr can read our policies without relearning. The mapping is documented in `docs/reference/wisdom-policy-dsl.md` (to be added with the implementation PR).
- **The canonical invariant becomes enforceable.** "Projections never silently become the truth they summarize" is operationalized: the only path from a `CycleReport` to Wisdom is through `WisdomPolicy::evaluate`.

### Negative

- **New surface area.** A DSL with combinators, leaves, and a serialization story is a non-trivial module. Estimated ~600–900 LOC in `wisdom::policy` plus tests.
- **Schema migration.** Promotion provenance gains a nullable column. Backwards compatible but requires v6 migration.
- **Trait churn.** `WisdomAutoEvaluator` is a new consumer-facing trait (gated behind an `Auto`-using policy — consumers who never use `Auto` never need to implement it). Coordination with ME-P2-G.

### Mitigations

- Implement in two passes: (1) deterministic-only DSL (no `Auto`), shipped first; (2) `Auto` seam + `WisdomAutoEvaluator` trait shipped after ME-P2-G is sequenced.
- Provide a `WisdomPolicy::is_pure(&self) -> bool` predicate so consumers can statically reject policies that would invoke `Auto` if they have not provided an evaluator.
- Mirror the Papr cookbook examples in our doctest suite so the vocabulary alignment is checkable.

### Open questions

1. **Boolean operator arity.** Papr uses variadic `And/Or`. Should ours use `Vec<Expr>` (proposed above) or fixed-arity `Box<Expr>` pairs? `Vec<Expr>` reads better and serializes to flat JSON arrays, but `Box<Expr>` makes structural pattern matching exhaustive.
2. **Where does the policy library live?** A consumer-curated set of named policies (`identity_anchors`, `procedural_skills`, `retracted_beliefs`) shipped in the engine vs. a `WisdomPolicyRegistry` consumer trait. Defer until first real consumer needs it.
3. **Coupling to Allen algebra (ADR-0011).** Should `Expr` gain a temporal leaf like `IntervalRelation { other: FactRef, relation: AllenRelation }`? Probably yes once ADR-0011 lands; flagged as a follow-up.
4. **Auto-verdict trust.** If a consumer's `Auto` evaluator is non-deterministic, replay determinism is lost for that policy branch. Document clearly; consider a `policy_replay_mode` flag on `EngineConfig` that errors when a replay encounters an `Auto` leaf without a deterministic-verdict cache.

## References

- `~/dev/autonomous-agent-project/raw/docs/summaries/02-system-design.md` §11.3 (Papr Schema-Policy DSL for the Wisdom Revision Gate)
- `~/dev/autonomous-agent-project/raw/docs/summaries/04-results-and-roadmap.md` §11.1 (ME-P0-A gap statement)
- `~/dev/autonomous-agent-project/raw/landscape/32-memory-knowledge-landscape-april-week2-2026.md` (architectural steals section)
- Papr Python SDK — <https://github.com/Papr-ai/papr-pythonSDK>
- Papr cookbook example — <https://github.com/Papr-ai/papr-pythonSDK/blob/main/cookbook/ai_sales_intelligence.py>
- r/Rag 1sgvvig OP (April 2026) — origin of "projections never silently become the truth they summarize"
- ADR-0001 (event sourcing), ADR-0004 (trait-based extensibility) — invariants this ADR must preserve
