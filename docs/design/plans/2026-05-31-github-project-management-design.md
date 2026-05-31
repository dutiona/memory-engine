# memory-engine — GitHub Project Management Design (validated)

**Date:** 2026-05-31
**Status:** Validated by user; ready for implementation planning (`/super-plan`).
**Target home in repo:** `docs/design/plans/2026-05-31-github-project-management.md` (commit as part of implementation).
**Owner/repo:** `dutiona/memory-engine` (personal namespace — shares user-project numbering with `reify` #1–3).

## Goal

Replicate the project-management system deployed in `dutiona/reify` and `Corely-Cycle/coraly-cycle`, **adapted** to memory-engine's nature (a phased Rust library with a cognitive-architecture roadmap). Migrate **open issues only** (96 open at design time); closed issues stay behind. New issues/PRs flow in via auto-add going forward.

## Reference-system findings (Discover)

- **Pattern:** "one firehose + N label-routed lenses." A `.github/workflows/add-to-project.yml` Action (`actions/add-to-project@v1`) on `issues.opened`/`pull_request.opened` feeds a **Main** project (everything) and, label-gated, a **Bug & Security Triage** project; a third **curated strategic** project is manual.
- **Label taxonomy:** shared `type:*` core (identical names + hex across both repos), `area:*` (domain-specific values), cross-cutting `severity:*`/`status:*`/`priority:*`/`super-qa`. Title format `type(area): description` (Conventional-Commits grammar). Hard rule: every issue ≥1 `type:*` + 1 `area:*` (epics may omit `area`).
- **Label/field split (the key lesson):** reify encodes priority/sequencing as **labels**; coraly (newer) moved them to **Projects-v2 custom fields** (`Priority` P0–P4, `Phase`, `Wave`, `Size`, `Iteration`). Severity stayed a label.
- **Views:** 1 BOARD per project (by Status) + 1 TABLE per `area:*` + `Epics` + hygiene views (`Orphans` / `Triage – no type` / `no area`) + QA-severity tables.
- **Agent contract:** the labelling convention is documented in `CLAUDE.md`/`AGENTS.md`/`GEMINI.md` as a hard rule on the agents; epics link children via the GraphQL `addSubIssue` mutation.

## Resolved design decisions

1. **Severity ⊥ Priority** (user's lesson from coraly): high severity in dead code = low priority. → **Severity = label** (intrinsic, set at creation, `gh`-queryable); **Priority = Projects-v2 field** P0–P4 (scheduling, board-managed). Keep `priority:critical`/`priority:high` convenience labels (coraly-style).
2. **Phase over Iteration/Wave/Size:** memory-engine's roadmap IS phase-structured. Keep a `Phase` field; drop Iteration, Wave, Size.
3. **Keep** `type:security`, `type:research`, and `status:*` labels (relevant to a security-sensitive, research-driven repo).
4. **Status = 4-state:** `Todo · In Progress · In Review · Done` (keeps PR-review stage; lean for solo flow).
5. **`type:design` folds into `type:research`** (canonical core = 11 types).
6. **Exactly one `area:*` per issue** (primary workstream; per-area views never overlap; cross-area concerns captured via epic membership).
7. **View setup:** script everything the API supports; deliver a one-time UI checklist for view filters + built-in automations (see API constraints).

## Label taxonomy (final)

### `type:*` — exactly one required (canonical core, colors authoritative)

| Label | Hex | Use |
|---|---|---|
| `type:bug` | `d73a4a` | Something broken |
| `type:feature` | `0e8a16` | New capability |
| `type:enhancement` | `a2eeef` | Improve existing |
| `type:refactor` | `c5def5` | Restructure, no behavior change (incl. perf optimization) |
| `type:test` | `bfd4f2` | Test coverage / infrastructure |
| `type:chore` | `e4e669` | Deps, config, cleanup |
| `type:docs` | `0075ca` | Documentation only |
| `type:plan` | `d4c5f9` | Implementation plan (links to PR) |
| `type:epic` | `3e4b9e` | Umbrella tracking issue with sub-issues |
| `type:research` | `fbca04` | Investigation, design-spec, spike, PoC |
| `type:security` | `e11d48` | Security finding / hardening |

### `area:*` — exactly one required (epics may omit). Colors: distinct-per-area palette (tunable).

| Label | Subsystem / primitive |
|---|---|
| `area:core` | facade, public API, types, traits, error, context-integration policies |
| `area:storage` | Event/Fact/Edge/Summary/Scope stores, SQLite, schema/migration, snapshot, archive (Ingest) |
| `area:retrieval` | FTS5, vector, hybrid RRF, reranker, spreading activation (Query) |
| `area:consolidation` | 3-pass dedup/cluster/global (Consolidate) |
| `area:forgetting` | Ebbinghaus decay, importance scoring (Forget) |
| `area:temporal` | bi-temporal model, conflict arbiter, Allen intervals, prediction, prospective memory (Resolve) |
| `area:cognitive` | DreamCycle, InsightStream, outcome tracking, provenance, identity (Phase 5) |
| `area:knowledge` | KnowledgeBaseConnector, pub/sub, cross-layer linking (Phase 6) |
| `area:cli` | `memory-engine-cli` |
| `area:mcp` | `memory-engine-mcp` |
| `area:docs` | narrative + API documentation |
| `area:build` | CI, build, tooling, benchmarks, eval harness |
| `area:qa` | super-qa findings, quality, refactor sweep |

### Cross-cutting (additive)

| Label | Hex | Meaning |
|---|---|---|
| `severity:critical` | `b60205` | super-qa audit severity (intrinsic) |
| `severity:high` | `d93f0b` | |
| `severity:medium` | `fbca04` | |
| `severity:low` | `0e8a16` | |
| `severity:info` | `c5def5` | |
| `status:blocked` | `e4e669` | Blocked by upstream/external dependency |
| `status:parked` | `d4c5f9` | Valid idea, no immediate timeline |
| `status:needs-design` | `fbca04` | Requires design work before implementation |
| `priority:critical` | `b60205` | Convenience label (full P0–P4 in field) |
| `priority:high` | `d93f0b` | |
| `super-qa` | `9B59B6` | Finding from /super-qa audit |
| `super-qa:auto-fix` | `27AE60` | Mechanically auto-fixable |
| `super-qa:debated` | `0e8a16` | Severity verified/adjusted by multi-model debate |
| `super-qa:security-fallback` | `5319e7` | Security finding from tier-2 pass |

## Projects-v2 fields (on Main + Roadmap)

| Field | Type | Values |
|---|---|---|
| `Status` | single-select | `Todo` · `In Progress` · `In Review` · `Done` |
| `Priority` | single-select | `P0 Critical` · `P1 High` · `P2 Medium` · `P3 Low` · `P4 Deferred` |
| `Phase` | single-select | `Phase 4` · `Phase 5a` · `Phase 5b` · `Phase 5 (indep)` · `Phase 6` · `Phase 7` · `Deferred` |

## The three projects + auto-add routing

| # | Project | Feed | Role |
|---|---|---|---|
| 1 | **Memory Engine — Main** | auto-add all issues/PRs | complete backlog (firehose) |
| 2 | **Memory Engine — Bug & Security Triage** | auto-add `type:bug` ∨ `type:security` | incident/security lens |
| 3 | **Memory Engine — Roadmap** | manual / curated | phase roadmap + 12-issue critical path |

Projects will be created as `users/dutiona/projects/{N}` (next available after reify's #1–3). The auto-add Action references the **post-creation numbers** — create projects first, capture numbers, parameterize the workflow file.

## Views (filters = one-time UI; names/layouts only via clone)

- **Main:** `Daily Kanban` (BOARD by Status) · one TABLE per `area:*` (13) · `Epics` (filter `type:epic`) · `Orphans` (no `type:*` OR no `area:*` — hygiene) · `Super QA` (filter `super-qa`).
- **Triage:** `Bug & Security` (BOARD) · `Bugs` (TABLE) · `Security` (TABLE) · `Triage – no type` · `Triage – no area`.
- **Roadmap:** `Roadmap` (BOARD grouped by `Phase`) · `Critical Path` (the 12-issue shortest chain) · `Blockers` (filter `status:blocked`).

## Epics (`type:epic` umbrellas; children linked via `addSubIssue`)

| Epic | State | Primary area | Members (illustrative; finalize during migration) |
|---|---|---|---|
| Cognitive Pipeline / DreamCycle | in progress (5a critical path) | cognitive | #49 #57 #161 #163 #225 #206 #207 #208 #209 #159 #160 #64 |
| Code-Quality Sweep (super-qa) | in progress (parallel) | qa | #112–#130 #141 #142 #149 #191 #192 #203 #231 #124 #125 |
| #221 Hook Integration | in progress (existing umbrella) | mcp | #224✅ #225 #226 |
| Knowledge Integration (Phase 6) | upcoming | knowledge | #50 #51 #52 #164 #165 #226 #167 #168 #235 |
| Multi-Agent Identity & Access | upcoming | storage/mcp | #158 #166 #14 #36 #38 |
| Retrieval Quality | upcoming | retrieval | #133 #138 #153 #155 #156 #157 #134 #66 #137 |
| Temporal Reasoning & Prediction | upcoming | temporal | #132 #233 #236 #19 |
| Snapshot & Cold-Start Hardening | upcoming | storage | #199 #200 #201 #204 #205 |
| Context-Management Integration | upcoming | core | #210 #211 #212 |

## Migration map (open issues only — 96 total, 18 unlabeled at design time)

| Existing label | open count | → | Action |
|---|---|---|---|
| `bug` | 2 | `type:bug` | rename |
| `enhancement` | 46 | `type:feature` / `type:enhancement` (per ROADMAP "Category") | rename + split |
| `refactoring` | 9 | `type:refactor` | rename |
| `research` | 5 | `type:research` | rename |
| `design` | 5 | `type:research` | fold |
| `documentation` | 5 | `type:docs` | rename |
| `testing` | 1 | `type:test` | rename |
| `performance` | 1 | `type:refactor`/`type:enhancement` + area | fold |
| `quality` | 1 | `type:refactor` + `area:qa` | fold |
| `security` | 1 | `type:security` | rename |
| `super-qa` | 23 | `super-qa` | keep |
| `auto-fix` | — | `super-qa:auto-fix` | rename |
| `phase-5`, `phase-5a`, `phase-5b`, `phase-6` | 22 | `Phase` **field** value | →field, retire labels |
| `deferred` | 9 | `Phase` = `Deferred` | →field, retire label |
| `duplicate`, `invalid`, `wontfix`, `help wanted`, `good first issue`, `question` | — | — | **delete** (gh CLI does not auto-remove default labels) |

- Every open issue gets **exactly one `area:*`** (0 currently have one) — back-fill from the ROADMAP's per-issue placement.
- The **18 unlabeled** open issues get full `type:` + `area:`.
- Migration **fails loudly** if any open issue exits without ≥1 `type:*` and exactly 1 `area:*`.

## Agent-file contract (new section → `CLAUDE.md`, copied to `AGENTS.md` + `GEMINI.md`)

Add an "Issue Labeling Convention" section: title format `type(area): description`; mandatory `type:` + exactly one `area:`; `type:*` mutually exclusive, cross-cutting additive; epic→sub-issue `addSubIssue` GraphQL snippet (owner `dutiona`, repo `memory-engine`); collateral-issue rule; project routing note. Keep the three files in sync (reify keeps them byte-identical; coraly let them drift — avoid that).

## Implementation constraints (bake into the plan)

- **Projects-v2 views are NOT API-writable.** Verified via GraphQL introspection: no `createProjectV2View`/`updateProjectV2View` mutation; `ProjectV2View.filter`/`groupByFields`/`sortByFields` are read-only. `copyProjectV2` + `markProjectV2AsTemplate` DO exist (clone-from-template path). → views + built-in "on-close → Done" automations are a one-time UI pass (documented checklist), everything else scripted.
- **Token:** the auto-add Action needs `MEMORY_ENGINE_PROJECT_TOKEN` (fine-grained PAT or classic with `project` + `repo` scopes); add as a repo secret.
- **Color canonicalization:** use the hex map above (reify-based) as the single source of truth; the script enforces it. Fixes the reify/coraly `type:epic` + severity color drift.
- **Default-label deletion** must be explicit (`gh label delete`).
- **Scriptable via API:** `gh label create/edit/delete`; `createProjectV2`; `createProjectV2Field` (single-select + options); `addProjectV2ItemById`; `updateProjectV2ItemFieldValue`; `addSubIssue`; the `add-to-project.yml` Action file; agent-doc edits.

## Out of scope

- Closed issues (migrate open only).
- Iteration/Wave/Size fields.
- Multi-area labels (strict one primary).
