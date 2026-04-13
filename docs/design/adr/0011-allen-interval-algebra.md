# ADR-0011: Allen Interval Algebra Module for Bi-Temporal Consistency

**Status:** Proposed
**Date:** 2026-04-13
**Gap ID:** ME-P0-B
**Phase:** 5 (Cognitive Pipelines) → enables structural temporal consistency
**Related:** ADR-0003 (bi-temporal model), ADR-0010 (Wisdom Revision Gate DSL)

## Context

ADR-0003 established the bi-temporal model: every `Fact` carries `t_created`/`t_expired` (system time) and `t_valid`/`t_invalid` (real-world validity). Today the engine treats these timestamps as scalars used for filtering (e.g., `list_due` in `src/store/facts.rs:397`, conflict resolution in `src/conflict/`). What it does **not** do is reason about *interval relations* between facts. Concretely:

- Two facts claiming different values for the same `(scope_id, fact_type, subject)` tuple over **overlapping** validity intervals are silently coexistent. The conflict path only fires when the consumer hands the engine a pair via `resolve_conflict()`.
- A fact stream for a given predicate may have **gaps** (missing intervals) that signal incomplete ingestion. The engine has no way to detect this.
- `PromotionProvenance` lineage chains (Phase 5a, ADR-0010 follow-up) can in principle form **cycles** if a fact's ancestry loops back on itself. There is no detector.

The April 2026 landscape review highlighted Semantica (Hawksight-AI) as the closest existing system that addresses this: Semantica's Temporal Intelligence Stack (v0.4.0) ships an Allen Interval Algebra module with the 13 deterministic relations used as a temporal consistency engine over its bi-temporal knowledge graph. The April 2026 design refinement note in `~/dev/autonomous-agent-project/raw/docs/summaries/02-system-design.md §11.2` states the case:

> Adding Allen algebra primitives as a new crate-level module would unlock: (a) **overlap detection** — two facts claiming different values for the same (scope_id, fact_type, subject, predicate) tuple over overlapping intervals flag as supersession candidates; (b) **gap detection** — missing intervals in a fact stream for a given predicate signal incomplete ingestion; (c) **cycle detection** in provenance chains — a fact whose ancestry loops back violates supersession integrity. The module is deterministic, LLM-free, and cheap (the 13-relation table is constant-time lookup given two intervals).

Allen 1983 (Communications of the ACM, "Maintaining Knowledge about Temporal Intervals") proves that any qualitative relation between two intervals on a totally ordered time line falls into exactly one of 13 classes: seven base relations and six converses.

| # | Relation | Mnemonic | Inverse |
|---|----------|----------|---------|
| 1 | `before(a, b)` | `a` ends strictly before `b` starts | `after` |
| 2 | `meets(a, b)` | `a.end == b.start` | `met_by` |
| 3 | `overlaps(a, b)` | `a.start < b.start < a.end < b.end` | `overlapped_by` |
| 4 | `during(a, b)` | `a.start > b.start ∧ a.end < b.end` | `contains` |
| 5 | `starts(a, b)` | `a.start == b.start ∧ a.end < b.end` | `started_by` |
| 6 | `finishes(a, b)` | `a.start > b.start ∧ a.end == b.end` | `finished_by` |
| 7 | `equals(a, b)` | `a.start == b.start ∧ a.end == b.end` | (self-inverse) |

Plus the six converses: `after`, `met_by`, `overlapped_by`, `contains`, `started_by`, `finished_by`. Total 13. The classification is **complete** (every interval pair has a relation), **disjoint** (exactly one), and **constant-time** to compute given two `(start, end)` pairs.

This is a structural addition that the existing bi-temporal substrate has been quietly ready for since Phase 3b. Nothing else in the engine needs to change for the module to land.

## Decision

Add a new module `src/temporal/allen.rs` exposing the 13 Allen relations as a deterministic, LLM-free, constant-time consistency primitive over `Fact` validity intervals. The module is consumed by (a) a new `consistency::` checker that runs on demand, (b) future ADR-0010 (Wisdom Revision Gate DSL) leaves, and (c) the prospective-memory event-based predicate DSL (ME-P1-E, ADR-0013).

### Surface (sketch)

```rust
// crate::temporal::allen

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AllenRelation {
    Before, After,
    Meets, MetBy,
    Overlaps, OverlappedBy,
    During, Contains,
    Starts, StartedBy,
    Finishes, FinishedBy,
    Equals,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval {
    pub start: DateTime<Utc>,
    pub end: Option<DateTime<Utc>>,  // None = open-ended (still valid)
}

impl Interval {
    /// Build from a `Fact`'s validity window. Open-ended `t_invalid` is
    /// treated as `+∞` for the purposes of relation classification.
    pub fn from_fact(fact: &Fact) -> Option<Self> {
        Some(Self { start: fact.t_valid?, end: fact.t_invalid })
    }

    pub fn relate(&self, other: &Self) -> AllenRelation { /* O(1) match */ }

    pub fn overlaps_with(&self, other: &Self) -> bool {
        matches!(
            self.relate(other),
            AllenRelation::Overlaps | AllenRelation::OverlappedBy
            | AllenRelation::During | AllenRelation::Contains
            | AllenRelation::Starts | AllenRelation::StartedBy
            | AllenRelation::Finishes | AllenRelation::FinishedBy
            | AllenRelation::Equals
        )
    }
}
```

Plus three consistency checkers built on top:

```rust
// crate::temporal::consistency

pub struct OverlapReport {
    pub fact_a: i64,
    pub fact_b: i64,
    pub relation: AllenRelation,
    pub conflict_key: ConflictKey,  // (scope_id, fact_type, subject)
}

pub fn detect_overlaps(
    store: &FactStore,
    scope_ids: &[i64],
) -> Result<Vec<OverlapReport>>;

pub fn detect_gaps(
    store: &FactStore,
    predicate: PredicateRef,
    horizon: Interval,
) -> Result<Vec<Interval>>;

pub fn detect_lineage_cycles(
    lineage: &LineageTable,  // Phase 5a
) -> Result<Vec<Vec<i64>>>;
```

### Properties

- **Deterministic.** Given two `Interval` values the relation is a pure function. No clocks, no RNG, no I/O.
- **LLM-free.** ADR-0004 invariant intact — `temporal::allen` is in `core` and has no consumer-trait callouts.
- **Constant-time per pair.** Single `match` on six comparisons. The full O(N²) overlap detector is a separate `consistency::` concern, gated behind a scope predicate so consumers control the cost.
- **`unsafe_code = "forbid"` compliant.** Pure arithmetic on `chrono::DateTime`.
- **Open-ended intervals first-class.** `t_invalid IS NULL` (the dominant case for active facts) is handled as `end = +∞`, not as an error.

### What this unlocks

| Use case | Today | After ADR-0011 |
| --- | --- | --- |
| Two facts claim conflicting values for the same key over overlapping windows | Silent coexistence until consumer detects | `detect_overlaps` flags as supersession candidate; consumer's `ConflictArbiter` decides |
| A fact stream has gaps | Invisible | `detect_gaps` returns missing intervals for an operator dashboard |
| Phase 5a `LineageTable` forms a cycle | Promotion provenance lies | `detect_lineage_cycles` errors before the bad promotion lands |
| ADR-0010 policy needs a temporal leaf | No vocabulary | `Expr::IntervalRelation { other, relation }` becomes well-defined |
| ME-P1-E event-based prospective memory | No deterministic predicate language | Allen relations + `entity_id` matching gives a complete deterministic core |

### Out of scope

- Composition/transitivity tables (Allen's 13×13 inference table). Useful for constraint propagation but not needed for the three immediate use cases. Defer to a follow-up ADR if/when constraint propagation is required.
- Probabilistic temporal reasoning (Vilain & Kautz 1986 extensions). Not needed for deterministic memory consistency.
- LLM-based temporal natural-language parsing (Semantica's `TemporalQueryRewriter`). That belongs in the consumer.

## Consequences

### Positive

- **Closes a structural blind spot.** Bi-temporal facts have existed since Phase 3b and the engine could not introspect interval relations between them. This was a latent gap waiting to be named.
- **Cheap.** ~150 LOC for the relation match + ~300 LOC for the three consistency checkers + tests. No dependencies (uses only `chrono`, already in `Cargo.toml`).
- **Deterministic and replayable.** Consistent with ADR-0001's "any future storage backend can replay the event log" property.
- **Composable substrate.** ADR-0010 leaves and ADR-0013 (event-based prospective memory) both consume `AllenRelation`. Three open ADRs share one primitive.
- **Paper #3 sub-claim.** "memory-engine ships a deterministic temporal consistency layer over bi-temporal facts" becomes a defensible architectural property to cite. Semantica is the only surveyed parallel and it is a Python knowledge-graph framework, not an embedded memory engine.

### Negative

- **O(N²) overlap detection in the naïve checker.** Mitigated by scope-bounding and conflict-key indexing, but operators must understand the cost. Not a runtime path — invoked on demand from CLI/MCP.
- **Edge-case ambiguity at ms boundaries.** `meets` (a.end == b.start) needs a documented convention for inclusive/exclusive endpoints. Proposed: ends are exclusive (`[start, end)`), matching how `list_due` already treats `t_invalid > now`.
- **Open-ended intervals.** `+∞` semantics need careful test coverage. The sketched `Interval::end: Option<DateTime<Utc>>` keeps the `Option` honest in the type system.

### Mitigations

- Add a 26-case property test (each base relation × each input ordering) using `proptest` (already a dev-dep). The Allen 13-relation table is small enough to exhaustively test classification with handwritten fixtures plus property tests for inverse identities.
- Document the half-open `[start, end)` convention in `temporal::allen` doctests and link from ADR-0003.
- Bench `detect_overlaps` against a 100K-fact scope to validate the O(N²) cost in the README — operators should know what they are paying for.

### Open questions

1. **Where does `consistency::` live?** As a sub-module of `temporal::` or as a sibling crate-level module? Sibling reads better (it consumes Allen, doesn't define it). Defer to implementation PR.
2. **Do we expose the Allen composition table?** Not in v1. Watch for a use case before committing to ~700 LOC of static data.
3. **Should the relation be a method on `Fact` directly?** No — keep it on `Interval` to preserve the LLM-free, store-free purity of the algebra. `Fact::interval()` is the bridge.
4. **`equals` granularity.** `chrono::DateTime<Utc>` is nanosecond-precision; SQLite stores RFC3339 strings (microsecond precision in our schema). Document the rounding rule and consider a `Interval::eq_at(precision)` helper.

## References

- Allen, J. F. (1983). "Maintaining Knowledge about Temporal Intervals." *Communications of the ACM*, 26(11), 832–843.
- `~/dev/autonomous-agent-project/raw/docs/summaries/02-system-design.md` §11.2 (Allen Interval Algebra as a Deterministic Temporal Consistency Module)
- `~/dev/autonomous-agent-project/raw/docs/summaries/04-results-and-roadmap.md` §11.1 (ME-P0-B gap statement)
- `~/dev/autonomous-agent-project/raw/landscape/32-memory-knowledge-landscape-april-week2-2026.md` (Semantica architectural steal)
- Semantica (Hawksight-AI) — <https://github.com/Hawksight-AI/semantica>
- Vilain & Kautz (1986) — composition table extensions, deferred reading
- ADR-0003 (bi-temporal model) — invariant this ADR builds on
- ADR-0001 (event sourcing), ADR-0004 (trait-based extensibility) — invariants preserved
