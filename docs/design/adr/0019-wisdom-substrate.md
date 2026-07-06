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

**Merge ownership:** the maintainer. Gate-approved promotions are committed by the harness tooling; the mandatory human approval (D2, #957) is the review step *before* the commit is made. Manual maintainer edits directly in git are permitted — git blame makes them auditable, and forbidding them would be unenforceable theater — but they are **classified, not invisible**: a manual edit carries a `gate_trace` with `decision: manual` and the maintainer's identity (the maintainer is their own approver). **No agent- or engine-driven write ever bypasses the ADR-0010 gate**, and the loader treats any item carrying neither a gate receipt nor a manual receipt as quarantined (§7). "Projections never silently become the truth they summarize."

**The corpus repository forbids history rewrites.** Force-push, rebase, and squash on the published branch are disabled (`receive.denyNonFastForwards` or host-side branch protection): the audit chain (§6) and the soft-deletion argument (§5) are only as strong as ref immutability. Commit signing is deliberately deferred to #957.

### 2. WisdomItem frontmatter

The OKF base plus the WisdomItem extension keys fixed by #955 deliverable 1:

```yaml
---
# --- OKF base (generic consumers navigate on these) ---
type: wisdom                    # REQUIRED; the one fixed value for this corpus
title: <short display name>
description: <one-line summary — what the index.md entry shows>
tags: [<domain tags>]
timestamp: 2026-07-06T14:00:00Z # OKF semantics: LAST MODIFIED (see §5) — harness-stamped

# --- WisdomItem extension keys (ME semantics; OKF consumers preserve, ignore) ---
schema_version: 1                  # WisdomItem schema revision (evolution = bump + linter migration)
id: 01J9XYZ...                     # STABLE logical identity (ULID), survives moves/renames;
                                   # the path is the OKF concept ID (navigation), `id` is identity
tier: anchors | core | predictions # stability tier (#57, BaseLayer-derived); value == directory name

# Engine-supplied (verbatim from the gate's Allow payload — the #232 contract):
pattern: >                         # the observed regularity this item encodes
  <what was seen, across which episodes>
directive: >                       # the actionable rule derived from the pattern
  <what the agent should do / avoid>
false_positive:                    # known misfire conditions — string OR list of strings;
  - <when NOT to apply it>         # the linter normalizes to list form
provenance:                        # structured — NOT log.md prose (see §4)
  source_fact_ids: [1234, 1301]    # ME fact IDs the pattern consolidates (matches LineageRecord vocabulary)
  store_id: "<engine instance id>" # namespaces the fact IDs (SchemaManager identity)
  kb_refs: ["<knowledge-base item refs>"]  # optional
gate_trace:                        # ref into ME's event log — the decision that admitted this item
  event_id: 55021                  # the gate-evaluation event (ADR-0010 PolicyDecision + trace)
  policy: "<policy name/id>"
  decision: allow | manual         # `manual` = maintainer direct edit (§1); never absent
  payload_digest: sha256:<hex>     # digest of the canonicalized engine payload the approval covered

# Harness-stamped at approval/commit time (the engine cannot know these):
approved_by: <maintainer identity> # the mandatory human approval receipt
promoted_at: 2026-07-06T14:00:00Z  # promotion time — distinct from OKF's timestamp (§5)
operation: 01J9OP...               # idempotency UUID for this revision (§6)
superseded_by: <stable id>         # optional; set on the OLD item when a revision replaces it
retired_at: <ISO 8601>             # optional; set when the item moves to attic/ (§5)
---

<body: the pattern/directive narrative, evidence excerpts, links to related items>
```

`pattern` / `directive` / `false_positive` are the **content triple**: what was observed, what to do about it, and when the rule is known to be wrong. Items where `false_positive` is genuinely unknown omit the key (OKF conformance is permissive; our own linter — see Consequences — decides what is mandatory per tier).

The comment partition above is normative: **engine-supplied fields travel inside the gate's `Allow` payload and are written verbatim; harness-stamped fields are added at approval/commit time** — this is who-writes-what for #232's contract, and it resolves the temporal skew of the engine "predicting" a promotion time it cannot know. This ADR fixes the field *inventory and ownership*; the machine-checkable normative schema (exact types, requiredness per tier and per operation, canonicalization rules for `payload_digest`, ID grammar) ships as a JSON-Schema artifact with #232's implementation PR — sketch here, contract there.

### 3. Directory layout and index convention

Tiers are the top-level directories; OKF `index.md` progressive disclosure applies at every level:

```
wisdom/                       # bundle root (git repo root)
├── index.md                  # bundle-root index; declares okf_version: "0.1"
├── log.md                    # optional freeform history (OKF convention; NOT the audit trail)
├── anchors/                  # tier: anchors — identity-stable, always-loaded candidates
│   ├── index.md
│   └── <topic>.md
├── core/                     # tier: core — established patterns, scenario-loaded
│   ├── index.md
│   └── <topic>/<item>.md     # subdirectories by domain as the corpus grows
├── predictions/              # tier: predictions — provisional, must earn promotion
│   ├── index.md
│   └── <item>.md
└── attic/                    # retired/superseded tombstones — in HEAD, never loaded (§5)
    └── <item>.md
```

- **Two identifiers, two jobs**: the OKF concept ID (path minus `.md`, e.g. `core/rust/verification-gates`) is for *navigation* and changes on `git mv`; the frontmatter `id` (ULID) is *logical identity* and never changes. `superseded_by` links use the stable `id` — a move (demotion) is therefore distinguishable from a replacement (supersession).
- **Tier is stated twice** (directory + `tier:` key) deliberately: the directory drives loading and navigation; the key survives `git mv` history and lets a linter detect drift. The linter treats a mismatch as an error.
- **Tier moves are gate-guarded revisions**: promotion (`predictions/` → `core/`) or demotion is a `git mv` + frontmatter update committed through the same gate path as admission, with a fresh `gate_trace`.
- `index.md` files are **auto-generated** by harness tooling from the frontmatter (`title` + `description`), never hand-maintained — regeneration runs in the same commit as any item change.

### 4. Provenance and gate traces are structured frontmatter, not `log.md`

OKF's `log.md` is deliberately freeform prose — insufficient for replayable audit. The audit trail therefore lives in two structured places: the `provenance`/`gate_trace` frontmatter keys (per-item, queryable by tooling) and git history itself (per-revision). `log.md` may exist as a human-readable digest but carries no load-bearing semantics.

### 5. Temporal semantics — resolving the OKF conflict explicitly

#955's STOP-trigger names this conflict; the resolution decided here:

- **`timestamp` keeps OKF semantics** (last modified) so generic OKF consumers read it correctly.
- **`promoted_at`** (extension key) records when the item passed the gate — the Wisdom analog of `t_created`.
- **Invalidation is supersession, and nothing is ever git-deleted.** A revised item is a new revision committed through the gate; the replaced item gets `superseded_by:` (stable `id` link) plus `retired_at:` and **moves to `attic/`** — it stays in HEAD as a queryable tombstone, so a consumer of the current projection can distinguish "explicitly retired, superseded by X" from "never existed". The supersession graph survives in HEAD, not only in history. (Reviewer finding, 2026-07-06: plain git deletion would erase the negative fact from every HEAD-only consumer.)
- **The claim is scoped honestly: git is a revision store, not a bi-temporal one.** Commit history gives transaction-time-like ordering of the *corpus*, and HEAD is the currently-active projection — but real-world validity semantics (when a pattern was true, when it stopped being true) live in the ME facts referenced by `provenance.fact_ids`, which carry full bi-temporal truth (ADR-0003). The corpus records only the promotion/retirement instants in frontmatter and does not replicate Allen-algebra validity intervals (ADR-0011).

### 6. Git is the revision store; audit symmetry with the event log

Every gate-approved change is **one commit** touching one logical item (plus regenerated indexes). Commit message convention:

```
promote(core): rust/verification-gates

gate-event: 55021
operation: 01J9OP...
policy: <policy name>
```

The mutual reference required by #955 deliverable 4 is a **three-node chain**, because a literal cycle is impossible (each artifact needs the other's ID first):

1. the **gate-evaluation event** is appended to ME's log when `WisdomPolicy::evaluate` returns `Allow` (it exists before any commit);
2. the **commit body cites that event ID and the operation UUID** (above);
3. after the commit, the harness appends a **mirror event** to ME's log — the **authoritative link**, keyed by gate event ID + operation UUID, whose payload carries: repository remote, commit SHA + parent SHA, stable item `id`, operation kind (promote/revise/move/retire), `approved_by`, and the `payload_digest` the approval covered.

Either direction is then walkable: event→commit via the mirror event, commit→event via the message. Identity lives in the **stable keys** (item `id`, gate event ID, operation UUID); the commit SHA is a *locator*, so even a hypothetical history accident degrades the chain to "re-locate" rather than "identity lost" (§1 forbids rewrites outright). The mirror event is an additive event kind following the same envelope pattern as the injection log sketched on #247 (payload-versioned via `UpcasterRegistry`, no schema fork).

**Crash/retry semantics** (the gap between steps 2 and 3): the operation UUID is minted at approval time and stamped in both the commit trailer and the mirror event, making mirroring **idempotent**. The corpus linter's reconciliation pass walks recent commits and re-emits any missing mirror event from the commit trailer (dedup by operation UUID); a gate event with neither commit nor mirror after reconciliation is a **stranded approval** — surfaced to the maintainer, never silently re-promoted. The full state machine is #955 deliverable 4's implementation concern; this ADR fixes the invariants it must satisfy.

### 7. Loader boundary (deliverable 5, scoped here only as an interface)

The harness loader injects tier-appropriate items at session start: `anchors/` unconditionally, `core/`/`predictions/` per scenario, `attic/` never. The loader is an **injection site**; *dose policy belongs to the #247 controller*, and every load is recorded in the injection log.

**The loader read path is a security boundary, and it enforces — it does not trust the write path.** Minimum bar fixed here: an item that fails schema validation, lacks a `gate_trace` with a valid `decision` (`allow` with receipt, or `manual` with maintainer identity), or exceeds size limits is **quarantined** — logged, surfaced to the maintainer, never injected. Git blame attributes a poisoned commit *after the fact*; only the loader can refuse to execute it. Hardening beyond this bar (signature verification, content sanitization depth, secret scanning, mirror-event cross-checking on load) is #957's threat-model scope — this ADR fixes that the enforcement point *is the loader*, not the corpus.

Beyond that, this ADR fixes only what the loader can rely on: stable item IDs, tier directories, auto-generated indexes, and frontmatter it can filter on without parsing bodies.

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
2. **Anchor-tier write friction** — should `anchors/` require a second human ack beyond the standard gate approval (it is identity-defining)? #957's threat model should answer.
3. **Linter home** — corpus repo CI vs harness pre-commit hook vs both.
4. **Concurrent promotions** — merge policy and recovery when two gate evaluations target the same item `id` (the second commit conflicts, stranding its gate event; reconciliation catches it, but the *resolution* policy is undecided).
5. **Canonicalization + digest spec** for `payload_digest`, and whether commits get cryptographically signed — #957 + #232's JSON-Schema artifact own the details.
6. **Schema evolution** — `schema_version` bump rules and the migration story for already-committed items (linter-driven rewrite through the gate vs read-time tolerance).
7. **Source-fact invalidation propagation** — when a fact in `provenance.fact_ids` is later expired/refuted in ME, does the wisdom item get flagged, demoted to `predictions/`, or retired? Needs dogfooding data before choosing (revalidation may be a DreamCycle pass).

## References

- `GoogleCloudPlatform/knowledge-catalog` → `okf/SPEC.md` (v0.1) — field semantics verified 2026-07-06
- ADR-0010 — gate DSL, `gate_trace`, "projections never silently become the truth they summarize"
- ADR-0001 / ADR-0003 — event-sourcing and bi-temporal invariants this ADR mirrors, not duplicates
- #955 (epic: Wisdom substrate), #951 (P2, decision D2), #57 (tier design, BaseLayer-derived)
- Prior art acknowledged in the audit: Anthropic Agent Skills, Memp, Voyager lineage — procedural memory as a first-class, separately-engineered concern
