# Project Management

This repository is managed with a reproducible, label-routed **GitHub Projects-v2** system. Issues and PRs are classified by a canonical label taxonomy, then auto-routed onto three project boards. The tooling lives under `scripts/github-pm/` as a desired-state reconciler: target facts are version-controlled manifests; idempotent module scripts converge GitHub to them (`query → diff → apply`), and `03-verify.sh` is the fail-loud acceptance gate.

The agent-facing labelling contract (what every issue MUST carry) is duplicated byte-identically into `CLAUDE.md`, `AGENTS.md`, and `GEMINI.md` under the `<!-- pm-contract:* -->` markers. This page is the operator's reference: the full taxonomy, the project/field schema, the token prerequisite, and the one-time UI steps that the GraphQL API cannot perform.

Plan tracking issue: **#239**.

## Label taxonomy

The label set is split so other repos (`reify`, `coraly`) can adopt the suite by swapping a single per-repo file:

- **Core (cross-repo), 25 labels** — `scripts/github-pm/manifests/labels.core.json`: `type:*` (11), `severity:*` (5), `status:*` (3), `priority:*` (2 convenience), `super-qa*` (4).
- **Area (per-repo), 14 labels** — `scripts/github-pm/manifests/labels.area.memory-engine.json`: `area:*`.

### Hard rule

Every issue carries **exactly one** `type:*` and **exactly one** `area:*` (epics may omit `area:*`). `type:*` labels are mutually exclusive; `severity:*`, `status:*`, `priority:*`, and `super-qa*` are additive. See the `## Issue Labeling Convention` block in `CLAUDE.md` / `AGENTS.md` / `GEMINI.md` for the per-label meanings and the `addSubIssue` epic-linking snippet.

### `type:*` (11, mutually exclusive)

`type:bug`, `type:feature`, `type:enhancement`, `type:refactor`, `type:test`, `type:chore`, `type:docs`, `type:plan`, `type:epic`, `type:research`, `type:security`.

### `area:*` (14, pick one; epics may omit)

`area:core`, `area:storage`, `area:retrieval`, `area:consolidation`, `area:forgetting`, `area:temporal`, `area:cognitive`, `area:knowledge`, `area:cli`, `area:mcp`, `area:docs`, `area:build`, `area:qa`, `area:viz`.

### Additive

- `severity:*` — `critical`, `high`, `medium`, `low`, `info`.
- `status:*` — `blocked`, `parked`, `needs-design`.
- `priority:*` — `critical`, `high` (convenience mirrors of P0/P1; the full P0–P4 scale lives in the `Priority` project field).
- `super-qa*` — `super-qa`, `super-qa:auto-fix`, `super-qa:debated`, `super-qa:security-fallback`.

> **Label renames are association-stable.** Migrations use `gh label edit --name` (node-id-stable), never delete+recreate. Renames propagate to **closed** issues, so historical queries (e.g. `--state closed --label type:enhancement`) keep working.

## Projects-v2 fields

Three custom single-select fields are defined on each project via GraphQL (`updateProjectV2Field` accepts `singleSelectOptions`, so options are API-settable, including the built-in `Status`):

| Field        | Options                                                                                |
| ------------ | -------------------------------------------------------------------------------------- |
| **Status**   | `Todo`, `In Progress`, `In Review`, `Done`                                             |
| **Priority** | `P0 Critical`, `P1 High`, `P2 Medium`, `P3 Low`, `P4 Deferred`                         |
| **Phase**    | `Phase 4`, `Phase 5a`, `Phase 5b`, `Phase 5 (indep)`, `Phase 6`, `Phase 7`, `Deferred` |

## The three projects + routing

| Project                   | URL                        | Auto-add routing                                          |
| ------------------------- | -------------------------- | --------------------------------------------------------- |
| **Memory Engine — Main**  | `users/dutiona/projects/4` | Every issue and PR, on `opened` / `reopened` / `labeled`. |
| **Bug & Security Triage** | `users/dutiona/projects/5` | Issues labeled `type:bug` **or** `type:security` (also).  |
| **Roadmap**               | `users/dutiona/projects/6` | Curated — maintainers add items manually; not automatic.  |

Routing is performed by the `Auto-add to projects` GitHub Action (`.github/workflows/add-to-project.yml`, `actions/add-to-project` pinned to a commit SHA). Because the trigger includes `labeled`, applying `type:bug` / `type:security` to an existing issue routes it to Triage after the fact. Routing keys entirely off labels, so correct `type:*` classification is what places an item on the right board.

## Token prerequisite: `MEMORY_ENGINE_PROJECT_TOKEN`

There is a deliberate **two-token split**:

- **Migration** (one-shot, ~96 open issues relabelled, fields back-filled, epics linked) runs **locally** with the operator's **CLI token** — a classic PAT that already has `repo` + `project` scope. It applies labels. This token is never stored in the repo.
- **Runtime routing** (the Action) uses `MEMORY_ENGINE_PROJECT_TOKEN`, a **fine-grained PAT** scoped for the Action **only**. A PAT is required because the default `GITHUB_TOKEN` cannot write **user-owned** projects.

Required scopes for `MEMORY_ENGINE_PROJECT_TOKEN` (least-privilege — it adds items, never writes labels):

- Account permissions: **Projects: read and write**
- Repository permissions: **Issues: read**, **Pull requests: read**, **Metadata: read** (explicitly **not** Issues: write)

Set it once (paste; never commit):

```bash
gh secret set MEMORY_ENGINE_PROJECT_TOKEN -R dutiona/memory-engine
```

## One-time UI checklist (NOT API-writable)

The Projects-v2 GraphQL schema has **no** `createProjectV2View` / `updateProjectV2View` mutations, and `ProjectV2View.filter` / `groupByFields` / `sortByFields` are read-only. Views and the built-in close→Done automation must be configured once in the web UI. (Field options, including `Status`, are scripted — do **not** redo them here.)

### Per project: built-in automations

In each project's **Workflows** settings, enable:

- **Item added → set `Status` = `Todo`**
- **Issue/PR closed → set `Status` = `Done`**

### Memory Engine — Main (#4) views

- **Daily Kanban** — Board, grouped by `Status`.
- **14 area tables** — one Table view per `area:*` label (one each for `core`, `storage`, `retrieval`, `consolidation`, `forgetting`, `temporal`, `cognitive`, `knowledge`, `cli`, `mcp`, `docs`, `build`, `qa`, `viz`).
- **Epics** — filter `label:type:epic`.
- **Orphans** — filter for items missing a `type:*` **or** missing an `area:*` (hygiene check; should stay empty).
- **Super QA** — filter `label:super-qa`.

### Bug & Security Triage (#5) views

- **Bug & Security** — Board.
- **Bugs** — filter `label:type:bug`.
- **Security** — filter `label:type:security`.
- **Triage – no type** — items missing a `type:*` label.
- **Triage – no area** — items missing an `area:*` label.

### Roadmap (#6) views

- **Roadmap** — Board, grouped by `Phase`.
- **Critical Path** — the 11 critical-path issues + `#221`: `#49`, `#158`, `#57`, `#225`, `#50`, `#51`, `#52`, `#164`, `#165`, `#166`, `#226`, and the `#221` umbrella.
- **Blockers** — filter `label:status:blocked`.

## Verifying

Re-run any reconciler script: it should converge to zero changes and exit 0. The acceptance gate:

```bash
scripts/github-pm/03-verify.sh --full   # exit 0 == label set matches manifest, every issue has type:*+area:*, no phase-*/deferred
```
