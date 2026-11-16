Plan SHA-256: 4a152c778ac9f842f2125e295059c247de399b5470041440cf654dcfb5afe30a

# Altitude report: ADR-0020 plan

## Measurements

Counted with `wc`, `grep -c` and `awk` over the plan file; nothing is estimated unless it says so.

- Plan: `docs/plans/2026-11-16-adr-0020-lifecycle-api.md`, 417 physical lines, 57191 bytes.
- Top-level implementation tasks (`- [ ]` at column 0, outside fences): 2.
- Fenced-code bodies: 19 lines, 1744 bytes, in Verification (the commands of V1, V2, V3 and V7) and in section L (the six-line status-line grammar). None is a file, a validator or a generated payload.
- Table rows: 72 lines, 5882 bytes: the envelope, status, hook-mapping, budget and tool-map tables.
- Constraints (verbatim ticket and spec lines): 6592 bytes. Proposed contract values (sections A to N): 34239 bytes.
- Target files and their measured size today: `docs/design/adr/0020-harness-neutral-lifecycle-api.md` (new, 0 bytes); `docs/design/adr/index.md` (761 bytes, +1 line); `docs/design/adr/0001-event-sourcing.md` (3,160 bytes, +1 line); `AGENTS.md`, `CLAUDE.md`, `GEMINI.md` (one line changed in each).
- Estimate, labelled as one: the finished ADR will be 55 to 70 KB. Basis: ADR-0019, the longest record in the house format, is 19,618 bytes for one decision area; this record covers twelve. The uncertainty is the prose around the values, which does not exist yet.
- Beside the plan, and not part of it: the three lens drafts under `docs/plans/2026-11-16-adr-0020-lifecycle-api-drafts/` (160114 bytes). They are ignored by git, never committed and never reviewed.
- No generated render, no predecessor plan, no successor: this is the first candidate.

## Required inputs

| Input | Review copy | SHA-256 |
|---|---|---|
| The ticket body (dutiona/memory-engine#1014, as fetched) | `.review/issue-1014.md` | 8071abad1ad489ff18e3758e13f0ff303834a258b0ece2388d70b36b4b18c781 |
| The Claude Code hooks reference, fetched 2026-11-16 from `https://code.claude.com/docs/en/hooks.md` | `.review/hooks-reference.md` | d736130c7223da8a552e24dcde194346cfa7f2adaad53d705bc7c9404278668e |

The epic plan and the specification the ticket was cut from are private (they name the deployment) and are
not review inputs. The ticket names the owner's decisions by number only; the plan's Constraints quote the text
of each one it relies on, so a reviewer needs nothing private.

## Signals

- **Size against the product.** The plan is close to the size of the low end of the ADR estimate, and review made it grow: round 1 added precision (segment rules, status classes, the client's lock and ledger), not prose. The self-check "the plan is shorter than the code it describes" holds only narrowly.
- **Duplicated ownership.** Sections A to N and the tool map state values the ADR will state again. That is the signal this checkpoint exists for.
- **What is absent.** No ADR prose, no decision quoted beyond Constraints, no diagram source, no example payload, no second validator. The fenced volume is 19 lines: commands and one grammar.

## Calibration

The deliverable is a decision record. Its work is the decisions, and the ticket leaves about forty of them
open (route paths, header names, grammars, statuses, thresholds, budgets, the fate of 26 tools). Super planning
was confirmed for this ticket so that those values are reviewed before the prose is written, because seventeen
tickets in two public repositories and a shell client build against them in parallel. A plan that named the
sections and left the values to the ADR would move the review to the PR, which is the outcome the planning
tier was chosen to avoid.

The two cause-specific fallbacks do not fit:

- A **design split** would put the values in a design file and reference it from a short plan. That design file would be a draft of the ADR under another name: three copies instead of two.
- **Product extraction** would write the ADR now and plan around it. That is implementing before the plan is approved.

The duplication is also short-lived. `/finish-pr` archives the plan as an issue and deletes the working
copy before merge; from then on the ADR is the only owner of every value, and the archived plan is a record of
what was proposed.

## Disposition

**Proposed override: proceed with the plan as one artifact.** The role has not changed: the plan assigns two
commits, fixes values and names checks; it does not contain the ADR. The override waives nothing: every
requirement, finding, review, audit and approval gate stands.

Each reviewer is asked to acknowledge this override explicitly, or to name the fallback they would choose and
what it would remove from the plan.
