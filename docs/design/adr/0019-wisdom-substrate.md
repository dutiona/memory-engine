# ADR-0019: Wisdom Substrate — Git-Versioned, OKF-Informed, Gate-Guarded Corpus

**Status:** Proposed
**Date:** 2026-07-06
**Parent:** epic #955 (deliverable 1), program P2 #951 (decision D2, locked 2026-07-06)
**Related:** ADR-0010 (revision gate DSL — the only write path), ADR-0001 (event sourcing), ADR-0003 (bi-temporal model), #232 (gate DSL implementation), #247 (context-assembly controller — the loader's dose policy), #957 (threat model)

## Context

The 2026-07-06 audit's sharpest finding: **Wisdom is a layer by convention, not architecture** — flat markdown (CLAUDE.md, skills, memory files) with no schema, no store, no revision mechanism. That is the exact criticism the four-layer thesis levels at competitors. Meanwhile the pipeline *into* Wisdom exists: `DreamCycle` produces promotion candidates (shipped), and ADR-0010 designs the gate that arbitrates them. What is missing is the destination.

Decision **D2** (P2 #951, locked) fixes the shape: a **git-versioned, schema'd markdown corpus**, format **OKF-informed** (Google's Open Knowledge Format, `GoogleCloudPlatform/knowledge-catalog` → `okf/`, SPEC v0.1). A dedicated wisdom-engine is a *later graduation*, only if file conventions calcify. This ADR is the schema and layout decision that D2 delegates; it gates the start of #232 (the gate's `Allow` payload contract *is* the WisdomItem defined here).

### What OKF actually provides (verified against `okf/SPEC.md`, 2026-07-06)

- **Frontmatter base:** only `type` is required; `title`, `description`, `resource`, `tags`, `timestamp` (ISO 8601, last-modified) are recommended. **Arbitrary producer-defined extension keys are explicitly legal**; consumers MUST preserve unknown keys and MUST NOT reject unknown fields.
- **Concept ID** = file path minus `.md`. Reserved filenames: `index.md` (per-directory progressive-disclosure listing; frontmatter permitted only on the bundle-root one, where `okf_version` may be declared) and `log.md` (freeform, date-grouped prose history).
- **Links:** untyped markdown links; bundle-root-relative form recommended. Consumers tolerate broken links.
- **Deliberate non-goals:** no type taxonomy, no per-item versioning/supersession, no structured provenance. `log.md` is prose by design.

The consequence that shapes this ADR: OKF gives us a legitimate, precedented *skeleton* (frontmatter + directory + index conventions, git-as-distribution), but every piece of rigor a Wisdom corpus needs — tiers, gate traces, structured provenance, supersession, bi-temporal validity — must live in **extension frontmatter keys**, not in OKF's own devices. That is not a fork: extension keys are exactly where OKF says domain semantics belong.

## Decision

### 1. Corpus location and ownership

The corpus is a **standalone git repository owned by the harness** (the consumer), not by memory-engine and not a subdirectory of any engine repo. Default path is a harness configuration key (proposed: `~/.claude/wisdom/`); the engine never learns the path — it hands `Allow` payloads to the consumer, which ships the bytes (ADR-0010's "what stays out of the engine" invariant, preserved verbatim).

**Merge ownership:** the maintainer. Gate-approved promotions are committed by the harness tooling; the mandatory human approval (D2, #957) is the review step *before* the commit is made. Manual maintainer edits directly in git are permitted — git blame makes them auditable, and forbidding them would be unenforceable theater — but **no agent- or engine-driven write ever bypasses the ADR-0010 gate.** "Projections never silently become the truth they summarize."

### 2. WisdomItem frontmatter

The OKF base plus the WisdomItem extension keys fixed by #955 deliverable 1:

```yaml
---
# --- OKF base (generic consumers navigate on these) ---
type: wisdom                    # REQUIRED; the one fixed value for this corpus
title: <short display name>
description: <one-line summary — what the index.md entry shows>
tags: [<domain tags>]
timestamp: 2026-07-06T14:00:00Z # OKF semantics: LAST MODIFIED (see §5)

# --- WisdomItem extension keys (ME semantics; OKF consumers preserve, ignore) ---
tier: anchor | core | prediction   # stability tier (#57, BaseLayer-derived)
pattern: >                         # the observed regularity this item encodes
  <what was seen, across which episodes>
directive: >                       # the actionable rule derived from the pattern
  <what the agent should do / avoid>
false_positive: >                  # known conditions under which the directive misfires
  <when NOT to apply it>           # (optional; list form allowed when several are known)
provenance:                        # structured — NOT log.md prose (see §4)
  fact_ids: [1234, 1301]           # ME fact IDs the pattern consolidates
  kb_refs: ["<knowledge-base item refs>"]  # optional
gate_trace:                        # ref into ME's event log — the decision that admitted this item
  event_id: 55021                  # the gate-evaluation event (ADR-0010 PolicyDecision + trace)
  policy: "<policy name/id>"
  decision: allow
promoted_at: 2026-07-06T14:00:00Z  # promotion time — distinct from OKF's timestamp (§5)
superseded_by: <item-id>           # optional; set on the OLD item when a revision replaces it
---

<body: the pattern/directive narrative, evidence excerpts, links to related items>
```

`pattern` / `directive` / `false_positive` are the **content triple**: what was observed, what to do about it, and when the rule is known to be wrong. Items where one of the three is genuinely absent omit the key (OKF conformance is permissive; our own linter — see Consequences — decides what is mandatory per tier).

### 3. Directory layout and index convention

Tiers are the top-level directories; OKF `index.md` progressive disclosure applies at every level:

```
wisdom/                       # bundle root (git repo root)
├── index.md                  # bundle-root index; declares okf_version: "0.1"
├── log.md                    # optional freeform history (OKF convention; NOT the audit trail)
├── anchors/                  # tier: anchor — identity-stable, always-loaded candidates
│   ├── index.md
│   └── <topic>.md
├── core/                     # tier: core — established patterns, scenario-loaded
│   ├── index.md
│   └── <topic>/<item>.md     # subdirectories by domain as the corpus grows
└── predictions/              # tier: prediction — provisional, must earn promotion
    ├── index.md
    └── <item>.md
```

- **Item ID** = OKF concept ID = path minus `.md` (`core/rust/verification-gates`).
- **Tier is stated twice** (directory + `tier:` key) deliberately: the directory drives loading and navigation; the key survives `git mv` history and lets a linter detect drift. The linter treats a mismatch as an error.
- **Tier moves are gate-guarded revisions**: promotion (`predictions/` → `core/`) or demotion is a `git mv` + frontmatter update committed through the same gate path as admission, with a fresh `gate_trace`.
- `index.md` files are **auto-generated** by harness tooling from the frontmatter (`title` + `description`), never hand-maintained — regeneration runs in the same commit as any item change.

### 4. Provenance and gate traces are structured frontmatter, not `log.md`

OKF's `log.md` is deliberately freeform prose — insufficient for replayable audit. The audit trail therefore lives in two structured places: the `provenance`/`gate_trace` frontmatter keys (per-item, queryable by tooling) and git history itself (per-revision). `log.md` may exist as a human-readable digest but carries no load-bearing semantics.

### 5. Temporal semantics — resolving the OKF conflict explicitly

#955's STOP-trigger names this conflict; the resolution decided here:

- **`timestamp` keeps OKF semantics** (last modified) so generic OKF consumers read it correctly.
- **`promoted_at`** (extension key) records when the item passed the gate — the Wisdom analog of `t_created`.
- **Invalidation is supersession, not deletion**: a revised item is a new file (or new content at the same ID, committed through the gate); the replaced item either gets `superseded_by:` and moves under `predictions/` (if demoted) or is deleted *in git* — where deletion is soft by construction, since git history retains it. This mirrors ADR-0003's soft-deletion philosophy with git as the temporal store: **the corpus HEAD is the "currently valid" projection; history is the bi-temporal record.**
- Full Allen-algebra-style validity intervals (ADR-0011) are *not* replicated in frontmatter — the ME facts referenced by `provenance.fact_ids` already carry bi-temporal truth; the corpus does not duplicate it.

### 6. Git is the revision store; audit symmetry with the event log

Every gate-approved change is **one commit** touching one logical item (plus regenerated indexes). Commit message convention:

```
promote(core): rust/verification-gates

gate-event: 55021
policy: <policy name>
```

The mutual reference required by #955 deliverable 4 is a **three-node chain**, because a literal cycle is impossible (each artifact needs the other's ID first):

1. the **gate-evaluation event** is appended to ME's log when `WisdomPolicy::evaluate` returns `Allow` (it exists before any commit);
2. the **commit body cites that event ID** (above);
3. after the commit, the harness appends a **mirror event** to ME's log recording the commit SHA + item ID.

Either direction is then walkable: event→commit via the mirror event, commit→event via the message. The mirror event is an additive event kind following the same envelope pattern as the injection log sketched on #247 (payload-versioned via `UpcasterRegistry`, no schema fork).

### 7. Loader boundary (deliverable 5, scoped here only as an interface)

The harness loader injects tier-appropriate items at session start: `anchors/` unconditionally, `core/`/`predictions/` per scenario. The loader is an **injection site**; *dose policy belongs to the #247 controller*, and every load is recorded in the injection log. This ADR fixes only what the loader can rely on: stable item IDs, tier directories, auto-generated indexes, and frontmatter it can filter on without parsing bodies.

## Consequences

### Positive

- **The undefined layer gets a definition.** Memory→Wisdom promotion becomes auditable end-to-end: fact → DreamCycle candidate → gate evaluate (`gate_trace`) → human approve → git commit → mirror event → next-session injection (#955 deliverable 6's test is expressible against this ADR alone).
- **Zero new engine surface.** The engine's contract stays "return `Allow` with a payload"; the corpus is consumer-owned files. No new crates, no storage changes, no LLM anywhere near the engine (ADR-0004 intact).
- **Free tooling.** Diff review, blame, revert, bisect, PR-style approval — the entire revision-control problem is delegated to git rather than reinvented (the thesis's own argument for why a dedicated wisdom-engine is premature).
- **Generic navigability.** An OKF consumer that knows nothing of memory-engine can still walk the corpus (`type` + `index.md` + links) — the alignment check of #955 deliverable 2 is mechanically testable.

### Negative

- **Schema enforcement needs a linter.** OKF validates nothing beyond non-empty `type`; tier/provenance/gate_trace integrity requires a corpus-side CI check (frontmatter schema + tier/directory match + `gate_trace.event_id` well-formedness). This linter lives with the corpus tooling in the harness, not in memory-engine.
- **Two-repo coordination.** The gate trace lives in ME's DB, the item in a separate git repo; the mirror-event chain (§6) is the stitch, and a broken stitch (commit without mirror event) is detectable but not preventable transactionally. Accepted: the failure mode is an audit gap flagged by the linter, not silent corruption.
- **Frontmatter grows organically.** Extension keys will accrete (confidence scores, outcome counters, injection stats). Accepted per #951's risk register: start OKF-minimal + the #955 field list, revise after P1-T1 dogfooding produces real promotions.

### Open questions (for PR review, not silently decided)

1. **Exact default path** — `~/.claude/wisdom/` vs a path inside the existing harness dotfiles repo. Leaning standalone repo (separate history, separate access control).
2. **`false_positive` cardinality** — scalar prose now, list-of-conditions later? Proposed: allow both, linter normalizes.
3. **Anchor-tier write friction** — should `anchors/` require a second human ack beyond the standard gate approval (it is identity-defining)? #957's threat model should answer.
4. **Linter home** — corpus repo CI vs harness pre-commit hook vs both.

## References

- `GoogleCloudPlatform/knowledge-catalog` → `okf/SPEC.md` (v0.1) — field semantics verified 2026-07-06
- ADR-0010 — gate DSL, `gate_trace`, "projections never silently become the truth they summarize"
- ADR-0001 / ADR-0003 — event-sourcing and bi-temporal invariants this ADR mirrors, not duplicates
- #955 (epic: Wisdom substrate), #951 (P2, decision D2), #57 (tier design, BaseLayer-derived)
- Prior art acknowledged in the audit: Anthropic Agent Skills, Memp, Voyager lineage — procedural memory as a first-class, separately-engineered concern
