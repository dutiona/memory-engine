# Issue Authoring for Implementation Agents

Convention for writing GitHub issues that implementation agents (and humans) can
execute **without re-deriving roadmap context or silently taking decisions that
are not theirs**. Introduced by the 2026-07-06 roadmap restructure; the three
program roots (P1 [#950], P2 [#951], P3 [#952]) require it for all sub-issues.

## Why this exists

Most implementation work on this repo is offloaded to agents that are strong at
local reasoning and weak at recovering high-level intent from a terse issue
body. The two failure modes this convention prevents:

1. **Silent decision-taking** — the agent hits an ambiguity, picks an option to
   keep moving, and buries a load-bearing choice in an implementation detail.
2. **False blockage** — the agent treats every open dependency as a
   start-blocker and stalls, or asks for permission it already has.

The cure is not more prose; it is a fixed anatomy that makes explicit *which
decisions are already taken*, *which are the agent's to make*, and *which must
be escalated*.

## The anatomy

Every issue intended for agent execution carries these sections, in order.
Epics carry them too (scoped to the epic's own deliverable — its body and
sequencing — not to each child's).

### 1. `## Context` — why this exists

What problem the issue solves, what it will show or enable, and where it sits
in the program (parent epic, track). One paragraph is usually enough. If the
issue exists because of an audit finding, ADR, or experiment outcome, link it.

### 2. `## Decisions already locked — do not re-litigate`

Bullet list of the decisions the implementer must treat as fixed, each with its
source (ADR number, epic decision log, dated maintainer comment). An agent that
disagrees with a locked decision comments on the issue and escalates; it does
not "improve" the design in the PR.

### 3. `## Deliverable` — with acceptance criteria and verification commands

What done looks like, mechanically checkable. Acceptance criteria include the
exact verification commands (the CI-exact gate from `CLAUDE.md` applies always
and does not need repeating; list only what is *additional* — a new benchmark,
a mutation test, a grep that must return empty).

### 4. `## Sequencing` — gates-start vs gates-merge

Name what this issue blocks and what blocks it, and for each dependency say
which kind it is:

- **gates start** — do not begin before this exists (typically: a decision, a
  schema, an upstream API).
- **gates merge** — begin freely; the PR cannot land before this exists
  (typically: a budget, a benchmark, a review artifact).

This distinction is load-bearing: treating merge-gates as start-blockers is the
single largest source of false stalls.

### 5. `## STOP-and-escalate triggers`

Concrete conditions under which the agent stops and reports instead of
deciding. Good triggers are observable ("the measured p95 delta exceeds 2×
baseline", "a named type from this body does not exist on current main"), not
vibes ("if something seems wrong"). Everything *not* listed here and not locked
in §2 is the implementer's call — that is the permission structure, stated
positively.

## Skeleton

```markdown
## Context
<why, what it shows, parent/track, source links>

## Decisions already locked — do not re-litigate
- <decision> (source: ADR-00NN / #NNN decision log / maintainer comment YYYY-MM-DD)

## Deliverable
<what done looks like>
Acceptance criteria:
- [ ] <criterion> — verify: `<command>`

## Sequencing
- Gates start: <dep> (why)
- Gates merge: <dep> (why)
- Blocks: <issues>

## STOP-and-escalate triggers
- <observable condition> → comment on this issue and stop.
```

## Reader contract (for the implementing agent)

- If an issue lacks a `STOP-and-escalate triggers` section, **add one** (as a
  comment proposing it) before starting work.
- Verify every named file, type, and issue reference against **current main**
  before building on it — issue bodies go stale across reorgs (this repo's
  verification trap #5). A stale reference is an escalation, not a guess.
- Amendment comments tagged `**[Audit amendment — YYYY-MM-DD]**` or
  `**[Restructure gating — YYYY-MM-DD]**` override the body where they
  conflict. Read the comments before the body's checkboxes.
- The labeling contract, verification gate, and traps in `CLAUDE.md` apply to
  every issue regardless of what the issue body says.
