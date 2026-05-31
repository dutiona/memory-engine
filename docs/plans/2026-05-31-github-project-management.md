# GitHub Project Management System — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up a reproducible, label-routed GitHub Projects-v2 management system for `dutiona/memory-engine` — canonical label taxonomy, three project boards with custom fields, an auto-add workflow, all **open** issues migrated into the scheme, nine single-parent epics, and an agent-file labelling contract.

**Architecture:** A **desired-state reconciler**. Target facts live in version-controlled manifests under `scripts/github-pm/` (single source of truth); thin, idempotent module scripts converge GitHub to them via `gh` + GraphQL (query → diff → apply; re-run ⇒ zero changes). A fail-loud `verify.sh` is the acceptance gate and the test surface. Execution follows a strict **backup → dry-run → canary → gate → delete** order; every destructive step is fenced behind the green gate. The core taxonomy is split from the per-repo `area:*` manifest so `reify`/`coraly` can adopt the suite by swapping one file.

**Tech Stack:** `gh` CLI 2.45.0 (`label`, `issue`, `project`, `secret`, `api graphql`), GitHub GraphQL Projects-v2 mutations, `jq`, Bash (matches reify/coraly's gh-native convention — zero new runtime deps), `actions/add-to-project@v1`. **No Rust touched.**

**Spec (ground truth):** `docs/design/plans/2026-05-31-github-project-management-design.md` (committed in Task 1 from `/home/mroynard/.claude/plans/memory-engine-github-pm-design.md`).

**Worktree:** `worktree-chore+github-project-management` — **already created; you are in it.**

---

## Empirically verified facts (this session — do not re-assume)

- `gh` 2.45.0; active CLI token is a **classic PAT** with `project` + `repo` scopes → drives all migration scripts as user `dutiona`. No new token needed for migration.
- GraphQL mutations **present**: `createProjectV2`, `createProjectV2Field`, `updateProjectV2Field` (**`singleSelectOptions` IS an input** → field options are API-settable, incl. the built-in `Status` field), `addProjectV2ItemById`, `updateProjectV2ItemFieldValue`, `deleteProjectV2Item`, `addSubIssue` (**has `replaceParent` → idempotent re-parent**), `removeSubIssue`, `copyProjectV2`, `markProjectV2AsTemplate`.
- GraphQL mutations **absent**: `createProjectV2View`, `updateProjectV2View` (input types do not exist). `ProjectV2View.filter`/`groupByFields`/`sortByFields` read-only ⇒ **view filters/grouping + built-in close→Done automation are a one-time UI pass** (Task 15). Everything else is scriptable — including `Status` options.
- `gh label create --force` upserts (idempotent). `gh label edit <old> --name <new>` is **node-id-stable** → preserves every issue↔label association (incl. on **closed** issues — historical queries like `--state closed --label type:enhancement` keep working). delete+create orphans associations. (INV-RENAME)
- `.github/workflows/` does **not** exist yet.
- Repo state at last harvest: **96 open issues**; **23 existing labels** (9 GitHub defaults + 14 ad-hoc); **18 open issues unlabeled**; **0 issues carry any `area:*`**; `enhancement` on 46. **The fixture is a snapshot — refresh it at migration time** (Task 6) so issues opened since (incl. the plan-issue) are covered.
- `#224`, `#128` are **CLOSED** (verified) — excluded from migration + epic membership (INV-OPEN-ONLY).
- `CLAUDE.md`/`AGENTS.md`/`GEMINI.md` are **not** byte-identical (5791/4226/5279 B), no labelling section.
- New projects → `users/dutiona/projects/#4,#5,#6` (reify holds #1–3) — but **#N can drift**; capture from the create response, never hard-code.

## Invariants (asserted by every script)

- **INV-OPEN-ONLY** — migration + epic-linking operate only on the **open** issue set (refreshed live); closed issues are read for the deletion-safety audit only and never mutated.
- **INV-RENAME** — 1:1 label migrations use `gh label edit --name` only; an in-use label is never delete+recreated.
- **INV-ADDITIVE-FIRST** — all target labels created before any old label is removed/deleted.
- **INV-GATE-FENCE** — no `gh label delete` and no project creation runs until `verify.sh --type-area` exits 0.
- **INV-CAPTURE** — project numbers/node-IDs + field/option IDs are read from API responses into `state.json`/`projects.lock.json`; never guessed.
- **INV-SECRET-FIRST** — the workflow file is committed only after `MEMORY_ENGINE_PROJECT_TOKEN` exists and passes a write pre-flight.
- **INV-IDEMPOTENT** — every script re-run produces zero changes and exits 0.

---

## File Structure

```
memory-engine/
├── .github/workflows/add-to-project.yml      # GENERATED (Task 13) — GitHub metadata
├── docs/
│   ├── design/plans/2026-05-31-github-project-management-design.md   # validated spec (Task 1)
│   ├── plans/2026-05-31-github-project-management.md                 # THIS plan
│   └── reference/project-management.md        # operator guide + UI checklist (Task 15)
└── scripts/github-pm/                         # operator tooling — portable across repos
    ├── README.md
    ├── manifests/
    │   ├── labels.core.json                   # CROSS-REPO: type/severity/status/priority/super-qa (25)
    │   ├── labels.area.memory-engine.json     # PER-REPO: area:* (14)
    │   ├── projects.json                      # project titles + field schema + option colors
    │   ├── projects.lock.json                 # GENERATED+COMMITTED: #4/#5/#6 numbers + node IDs
    │   ├── open-issues.json                   # GENERATED: live open-issue fixture (refreshed Task 6)
    │   ├── issue-map.tsv                       # GENERATED+REVIEWED: issue→type,area,phase,priority,epic,reason
    │   └── epics.json                         # 9 epics → {title,area,state,existing_issue,members[]} (single-parent)
    ├── lib.sh                                 # throttle/retry/upsert/rename/field/subissue helpers
    ├── 00-backup.sh   01-sync-labels.sh   02-migrate-issues.sh   03-verify.sh
    ├── 04-delete-labels.sh   05-projects-fields.sh   06-add-items-fields.sh
    ├── 07-epics.sh   08-retire-phase-labels.sh   12-render-workflow.sh
    ├── gen-issue-map.sh                       # emits issue-map.tsv from open-issues.json (Task 6)
    ├── restore.sh                             # rollback from backups/<ts>/
    ├── state.json                             # GENERATED+GITIGNORED: numbers + field/option IDs (regenerable)
    └── backups/                               # GITIGNORED snapshots
```

**`state.json` vs `projects.lock.json` (resolves the boundary):** `projects.lock.json` holds only project **numbers + node IDs** — committed, the durable record the workflow renders from. `state.json` is the **full runtime cache** (lock data + every field ID + option-name→option-ID map) — gitignored, fully regenerable by re-querying the projects. A fresh checkout regenerates `state.json` via `05-projects-fields.sh` (idempotent: it finds existing projects/fields and re-caches their IDs).

---

## Canonical label manifest (source of truth)

`labels.core.json` — **25 core labels** (cross-repo):

| name                         | color    | description                                  |
| ---------------------------- | -------- | -------------------------------------------- |
| `type:bug`                   | `d73a4a` | Something broken                             |
| `type:feature`               | `0e8a16` | New capability                               |
| `type:enhancement`           | `a2eeef` | Improve existing functionality               |
| `type:refactor`              | `c5def5` | Restructure, no behavior change (incl. perf) |
| `type:test`                  | `bfd4f2` | Test coverage or infrastructure              |
| `type:chore`                 | `e4e669` | Deps, config, cleanup                        |
| `type:docs`                  | `0075ca` | Documentation only                           |
| `type:plan`                  | `d4c5f9` | Implementation plan (links to PR)            |
| `type:epic`                  | `3e4b9e` | Umbrella tracking issue with sub-issues      |
| `type:research`              | `fbca04` | Investigation, design-spec, spike, PoC       |
| `type:security`              | `e11d48` | Security finding or hardening                |
| `severity:critical`          | `b60205` | super-qa severity: critical                  |
| `severity:high`              | `d93f0b` | super-qa severity: high                      |
| `severity:medium`            | `fbca04` | super-qa severity: medium                    |
| `severity:low`               | `0e8a16` | super-qa severity: low                       |
| `severity:info`              | `c5def5` | super-qa severity: info                      |
| `status:blocked`             | `e4e669` | Blocked by upstream/external dependency      |
| `status:parked`              | `d4c5f9` | Valid idea, no immediate timeline            |
| `status:needs-design`        | `fbca04` | Requires design before implementation        |
| `priority:critical`          | `b60205` | Convenience label (full P0–P4 in field)      |
| `priority:high`              | `d93f0b` | Convenience label                            |
| `super-qa`                   | `9B59B6` | Finding from /super-qa audit                 |
| `super-qa:auto-fix`          | `27AE60` | Mechanically auto-fixable                    |
| `super-qa:debated`           | `0e8a16` | Severity verified by multi-model debate      |
| `super-qa:security-fallback` | `5319e7` | Security finding from tier-2 pass            |

`labels.area.memory-engine.json` — **14 areas** (colors tunable):

`area:core`=`7B68EE`, `area:storage`=`006B75`, `area:retrieval`=`1D76DB`, `area:consolidation`=`5319E7`, `area:forgetting`=`E36209`, `area:temporal`=`0052CC`, `area:cognitive`=`8250DF`, `area:knowledge`=`1A7F37`, `area:cli`=`B60205`, `area:mcp`=`FBCA04`, `area:docs`=`0075CA`, `area:build`=`CFD3D7`, `area:qa`=`BFD4F2`, `area:viz`=`D876E3` (Phase-7 web UI, #13 — new vs the original 13-area spec so #13 has an honest home).

---

## Migration mapping (deterministic rules → `issue-map.tsv`)

`issue-map.tsv` columns: `issue⟶type⟶area⟶phase⟶priority⟶epic⟶reason`. It is **generated by `gen-issue-map.sh`** (Task 6) from the refreshed `open-issues.json`, then **human-reviewed** before any mutation. The generator pre-fills what is machine-derivable and writes `AMBIGUOUS` everywhere it cannot decide; the migration script refuses to run while any `AMBIGUOUS` remains.

### type:\* (precedence top→bottom; first match wins)

| Signal                                                                            | → `type:*`                  |
| --------------------------------------------------------------------------------- | --------------------------- |
| title `Plan:`/`Plan archive:`                                                     | `type:plan`                 |
| existing umbrella (#221) or title `Umbrella:`/`[epic]`                            | `type:epic`                 |
| existing `bug` label, ROADMAP `correctness`/`bug`, title `fix:`                   | `type:bug`                  |
| existing `security` label or ROADMAP `security`                                   | `type:security`             |
| existing `testing` label or ROADMAP `testing`                                     | `type:test`                 |
| existing `documentation` label (non-Plan) or ROADMAP `docs`                       | `type:docs`                 |
| existing `refactoring`/`quality`/`performance` label or ROADMAP `refactor`/`perf` | `type:refactor`             |
| existing `research`/`design` label or ROADMAP `research`/`design`                 | `type:research`             |
| title `feat(`/`feat:` or ROADMAP `feat`                                           | `type:feature`              |
| residual "improve existing"                                                       | `type:enhancement`          |
| no signal                                                                         | `AMBIGUOUS` (fails dry-run) |

`super-qa` is **additive** — it never satisfies the type requirement.

### area:\* (exactly one; epics may omit) — derive from title `type(area):` hint, else ROADMAP placement (rules table). Ambiguous cross-area picks flagged for reviewer: #226→mcp, #204→storage, #134→forgetting, #66/#137→retrieval, #158→storage, #13→viz.

### 18 unlabeled — pre-filled proposal (reviewer confirms)

`#236`→feature/temporal · `#235`→feature/knowledge · `#233`→feature/temporal · `#232`→feature/cognitive · `#226`→feature/mcp · `#225`→feature/mcp · `#221`→epic/mcp · `#183`→plan/docs · `#157`→research/retrieval · `#156`→feature/forgetting · `#155`→feature/retrieval · `#154`→feature/forgetting · `#153`→feature/retrieval · `#19`→research/core · `#17`→feature/consolidation · `#15`→feature/storage · `#14`→feature/storage · `#13`→feature/viz.

### Phase + Priority field values

`phase-5a`→`Phase 5a`; `phase-5b`→`Phase 5b`; `phase-5`→`Phase 5a/5b/5 (indep)` per ROADMAP; `phase-6`→`Phase 6`; `deferred`→`Deferred`; #199–205/#203→`Phase 4`; #13→`Phase 7`; super-qa sweep→unset. Priority: the **11 critical-path issues + #221 umbrella** → P0/P1; super-qa info/low → P3/P4; else unset. `priority:critical`/`priority:high` mirror P0/P1.

**Critical-path set (enumerated — ROADMAP "Shortest path"):** the **11 issues** `#49 #158 #57 #225 #50 #51 #52 #164 #165 #166 #226`, plus step 12 = _close #221 umbrella_ (not a separate issue). Roadmap project = these 11 + #221 + the 9 epics.

---

## Phase A — Scaffolding & Backup (non-destructive)

### Task 1: Commit the spec + scaffold tooling

**Files:** Create `docs/design/plans/2026-05-31-github-project-management-design.md`, `scripts/github-pm/README.md`; modify `.gitignore`.

- [ ] **Step 1:** `cp /home/mroynard/.claude/plans/memory-engine-github-pm-design.md docs/design/plans/2026-05-31-github-project-management-design.md`
- [ ] **Step 2:** `printf '\n# github-pm runtime\nscripts/github-pm/state.json\nscripts/github-pm/backups/\nscripts/github-pm/manifests/open-issues.json\n' >> .gitignore`
- [ ] **Step 3:** Write `scripts/github-pm/README.md` (run order 00→13, the seven invariants, `restore.sh`, cross-repo recipe).
- [ ] **Step 4: Commit** `chore(pm): scaffold github-pm tooling + commit validated design spec`

### Task 2: Bootstrap `type:plan`, publish the plan issue

- [ ] **Step 1:** `gh auth status` (account dutiona, scopes project+repo); `gh repo view dutiona/memory-engine --json nameWithOwner -q .nameWithOwner`.
- [ ] **Step 2:** `gh label create "type:plan" -R dutiona/memory-engine --color d4c5f9 --description "Implementation plan (links to PR)" --force`
- [ ] **Step 3:** `PLAN_ISSUE=$(gh issue create -R dutiona/memory-engine --title "plan(build): GitHub project-management system rollout" --body-file docs/plans/2026-05-31-github-project-management.md --label type:plan | grep -oE '[0-9]+$')` — record `PLAN_ISSUE`. (Its `area:build` is applied by Task 6 because the **refreshed** fixture includes it — see A1 fix.)

### Task 3: Write `lib.sh`

**Files:** Create `scripts/github-pm/lib.sh`.

- [ ] **Step 1:** Helpers (sourced everywhere):

```bash
#!/usr/bin/env bash
set -euo pipefail
PM_OWNER="${PM_OWNER:-dutiona}"; PM_REPO="${PM_REPO:-memory-engine}"; SLUG="$PM_OWNER/$PM_REPO"
THROTTLE="${GH_PM_THROTTLE:-0.25}"
gh_throttle(){ sleep "$THROTTLE"; }
gh_ratecheck(){ local r; r=$(gh api rate_limit --jq '.resources.graphql.remaining' 2>/dev/null||echo 9999); [ "$r" -lt 100 ]&&{ echo "rate low ($r); sleep 60"; sleep 60; }||true; }

# CORRECT GraphQL invocation (fixes Codex C1 + C3). Two hazards this avoids:
#   C1: `gh api graphql` requires the document in a field named `query` — `-f q=...` would send a
#       VARIABLE named `q` and NO query. C3: typed array variables (e.g. singleSelectOptions) cannot
#       be passed via `-f`/`-F` (sent as a string → type-validation failure); they must travel inside a
#       JSON `variables` object. So: build {query,variables} with jq and pipe via `--input -`.
# Usage: gql '<query>' '<variables-json>' [extra gh args e.g. --jq '...']
gql(){ local q="$1" vars="$2"; shift 2; gh_throttle; jq -nc --arg q "$q" --argjson v "$vars" '{query:$q,variables:$v}' | gh api graphql --input - "$@"; }

label_upsert(){ gh_ratecheck; gh_throttle; gh label create "$1" -R "$SLUG" -c "$2" -d "$3" --force; }
label_rename(){ gh label list -R "$SLUG" --json name --jq '.[].name'|grep -qx "$1"||{ echo "skip rename $1 (absent)"; return 0; }; gh_throttle; gh label edit "$1" -R "$SLUG" --name "$2" --color "$3"; }
owner_id(){ gql 'query($l:String!){user(login:$l){id}}' "$(jq -nc --arg l "$PM_OWNER" '{l:$l}')" --jq '.data.user.id'; }
issue_node(){ gh issue view "$1" -R "$SLUG" --json id --jq .id; }

# Idempotent item-add with PAGINATION (fixes Codex C5: Main will hold 105+ items, so first:100 misses
# existing items past page 1 → duplicate adds). Page until found, else add.
project_item_add(){ local pid="$1" cid="$2" after=null page found
  while :; do
    page=$(gql 'query($p:ID!,$a:String){node(id:$p){... on ProjectV2{items(first:100,after:$a){pageInfo{hasNextPage endCursor} nodes{id content{... on Issue{id} ... on PullRequest{id}}}}}}}' "$(jq -nc --arg p "$pid" --argjson a "$after" '{p:$p,a:$a}')")
    found=$(echo "$page"|jq -r --arg c "$cid" '.data.node.items.nodes[]|select(.content.id==$c)|.id'|head -1)
    [ -n "$found" ]&&{ echo "$found"; return 0; }
    [ "$(echo "$page"|jq -r '.data.node.items.pageInfo.hasNextPage')" = true ]||break
    after=$(echo "$page"|jq '.data.node.items.pageInfo.endCursor')   # quoted JSON string for next $a
  done
  gql 'mutation($p:ID!,$c:ID!){addProjectV2ItemById(input:{projectId:$p,contentId:$c}){item{id}}}' "$(jq -nc --arg p "$pid" --arg c "$cid" '{p:$p,c:$c}')" --jq '.data.addProjectV2ItemById.item.id'; }

# Idempotent epic link: replaceParent:true (verified present on AddSubIssueInput via introspection this session).
subissue_link(){ gql 'mutation($p:ID!,$c:ID!){addSubIssue(input:{issueId:$p,subIssueId:$c,replaceParent:true}){issue{number}}}' "$(jq -nc --arg p "$1" --arg c "$2" '{p:$p,c:$c}')" >/dev/null; }
```

- [ ] **Step 2:** `bash -n scripts/github-pm/lib.sh`. **Commit.**

### Task 4: `00-backup.sh` + real `restore.sh` (smoke-tested)

**Files:** Create `00-backup.sh`, `restore.sh`.

- [ ] **Step 1:** `00-backup.sh` snapshots → `backups/<ts>/`: `labels.json` (name,color,description), `open-issues.json` (open: number,title,labels), `all-issue-labels.json` (state:all: number,state,labels), `projects-before.json` (user projectsV2). Symlink `backups/latest`.
- [ ] **Step 2:** Write a **working** `restore.sh` (not a sketch): recreate/rename labels to match `labels.json`; for each open issue, set its label set back from `open-issues.json` (`gh issue edit --add-label`/`--remove-label` to converge); `removeSubIssue` for links recorded in `state.json.epic_links[]` (Task 12's `07-epics.sh` appends each `{parent,child}` link there as it creates it — fixes Codex C9, so epic rollback is real); list run-created projects for manual `deleteProjectV2 --confirm`.
- [ ] **Step 3: Smoke-test restore** — create a throwaway label `zzz-restore-test`, run a fake "rename" to `zzz-renamed`, run `restore.sh --labels-only`, confirm `zzz-restore-test` is back and `zzz-renamed` gone; delete `zzz-restore-test`.
- [ ] **Step 4:** Run `00-backup.sh`; confirm 4 artifacts in `backups/latest/`. **Commit** scripts.

---

## Phase B — Labels (additive → migrate → gated delete)

### Task 5: Manifests + `01-sync-labels.sh` (additive + node-stable renames; NO deletes)

**Files:** Create `manifests/labels.core.json`, `manifests/labels.area.memory-engine.json`, `01-sync-labels.sh`.

- [ ] **Step 1:** Write both manifests as `[{name,color,description}]` per the tables.
- [ ] **Step 2:** `01-sync-labels.sh`:

```bash
#!/usr/bin/env bash
source "$(dirname "$0")/lib.sh"; MD="$(dirname "$0")/manifests"
# 1. Renames FIRST — node-id-stable (INV-RENAME). enhancement→type:enhancement keeps all 46 assocs;
#    the SPLIT to type:feature/refactor/etc. happens per-issue in Task 6 (strips type:enhancement there).
label_rename bug           "type:bug"          d73a4a
label_rename documentation "type:docs"         0075ca
label_rename refactoring   "type:refactor"     c5def5
label_rename research      "type:research"     fbca04
label_rename testing       "type:test"         bfd4f2
label_rename security      "type:security"     e11d48
label_rename auto-fix      "super-qa:auto-fix" 27AE60
label_rename enhancement   "type:enhancement"  a2eeef
# 2. Upsert the full canonical set (idempotent; fixes colors incl. super-qa).
for f in labels.core.json labels.area.memory-engine.json; do
  jq -c '.[]' "$MD/$f" | while read -r l; do
    label_upsert "$(jq -r .name <<<"$l")" "$(jq -r .color <<<"$l")" "$(jq -r .description <<<"$l")"
  done
done
# design/performance/quality (folded per-issue) + 6 defaults are deleted in Task 9, gate-fenced.
```

- [ ] **Step 3:** Run; list labels; confirm additive + renames only (no deletions yet). **Commit.**

### Task 6: Refresh fixture, generate + review `issue-map.tsv`, write `02-migrate-issues.sh`

**Files:** Create `gen-issue-map.sh`, `manifests/issue-map.tsv`, `02-migrate-issues.sh`; generate `manifests/open-issues.json`.

- [ ] **Step 1 (A1 fix — refresh the live fixture, incl. the plan-issue + anything opened since):**

```bash
gh issue list -R dutiona/memory-engine --state open --limit 500 \
  --json number,title,labels > scripts/github-pm/manifests/open-issues.json
jq 'length' scripts/github-pm/manifests/open-issues.json   # expect ≥ 97 (96 + plan-issue)
```

- [ ] **Step 2 (M1 fix — concrete generator):** `gen-issue-map.sh` emits one row per open issue, pre-filling what is derivable, else `AMBIGUOUS`:

```bash
#!/usr/bin/env bash
source "$(dirname "$0")/lib.sh"; F="$(dirname "$0")/manifests/open-issues.json"
printf 'issue\ttype\tarea\tphase\tpriority\tepic\treason\n'
jq -r '.[] | [.number, .title, ([.labels[].name]|join(","))] | @tsv' "$F" | while IFS=$'\t' read -r n title labels; do
  t=""; reason=""
  ct=$(printf '%s' "$labels" | tr ',' '\n' | grep -m1 '^type:' || true)   # canonical type:* from a Task-5 rename
  # 1. Title overrides (BOTH legacy `Plan:` and conventional `plan(scope):`/`plan:`; same for umbrellas).
  #    Fixes Codex C8: the plan-issue title is `plan(build): ...`, not `Plan:`.
  case "$title" in
    "Plan:"*|"Plan archive:"*|plan\(*|"plan:"*) t="type:plan"; reason="title-plan";;
    "Umbrella:"*|*[Uu]mbrella*) t="type:epic"; reason="title-umbrella";;
  esac
  # 2. Existing canonical type:* (authoritative post-rename — fixes Gemini G1; enhancement falls through to split).
  if [ -z "$t" ]; then case "$ct" in type:enhancement) ;; type:*) t="$ct"; reason="existing-type";; esac; fi
  # 3. Legacy (pre-rename) names — in case the generator is run before Task 5.
  if [ -z "$t" ]; then case ",$labels," in
    *,bug,*) t="type:bug"; reason="label-bug";;
    *,security,*) t="type:security"; reason="label-security";;
    *,testing,*) t="type:test"; reason="label-test";;
    *,documentation,*) t="type:docs"; reason="label-docs";;
    *,refactoring,*|*,quality,*|*,performance,*) t="type:refactor"; reason="label-refactor";;
    *,research,*|*,design,*) t="type:research"; reason="label-research/design";;
  esac; fi
  # 4. enhancement split + brand-new issues: conventional title prefix; keep enhancement if no signal; else AMBIGUOUS.
  if [ -z "$t" ] || [ "$ct" = type:enhancement ]; then case "$title" in
    feat\(*|"feat:"*) t="type:feature"; reason="title-feat";;
    fix\(*|"fix:"*) t="type:bug"; reason="title-fix";;
    refactor\(*|perf*) t="type:refactor"; reason="title-refactor";;
    *) [ "$ct" = type:enhancement ] && { t="type:enhancement"; reason="keep-enhancement"; };;
  esac; fi
  [ -z "$t" ] && { t="AMBIGUOUS"; reason="needs-human"; }
  a=$(printf '%s' "$title" | sed -nE 's/^[a-z]+\(([a-z]+)\):.*/\1/p')   # area from title type(area): hint
  case "$a" in core|storage|retrieval|consolidation|forgetting|temporal|cognitive|knowledge|cli|mcp|docs|build|qa|viz) area="area:$a";; *) area="AMBIGUOUS";; esac
  phase=""; case ",$labels," in *,phase-5a,*) phase="Phase 5a";; *,phase-5b,*) phase="Phase 5b";; *,phase-5,*) phase="Phase 5 (indep)";; *,phase-6,*) phase="Phase 6";; *,deferred,*) phase="Deferred";; esac
  printf '%s\t%s\t%s\t%s\t\t\t%s\n' "$n" "$t" "$area" "$phase" "$reason"
done
```

Run it: `bash scripts/github-pm/gen-issue-map.sh > scripts/github-pm/manifests/issue-map.tsv`.

- [ ] **Step 3 (HUMAN REVIEW GATE):** Resolve every `AMBIGUOUS` (type **and** area) using the rule tables + the 18-unlabeled pre-fill above; confirm the flagged cross-area picks. The migration refuses to run while any `AMBIGUOUS` remains.
- [ ] **Step 4:** `02-migrate-issues.sh` (B1 fix — strip `type:enhancement` whenever target ≠ `type:enhancement`; no-op when absent, so it also corrects former-enhancements mapped to refactor/research):

```bash
#!/usr/bin/env bash
source "$(dirname "$0")/lib.sh"; MAP="$(dirname "$0")/manifests/issue-map.tsv"
grep -qP '\tAMBIGUOUS(\t|$)' "$MAP" && { echo "ABORT: AMBIGUOUS rows remain"; exit 1; }
tail -n +2 "$MAP" | while IFS=$'\t' read -r n type area phase prio epic reason; do
  case "$type" in type:*) ;; *) echo "ABORT #$n bad type '$type'"; exit 1;; esac
  [ "$type" = "type:epic" ] || case "$area" in area:*) ;; *) echo "ABORT #$n bad area '$area'"; exit 1;; esac
  # Reconcile to EXACTLY {type, area} (fixes Gemini G2 + idempotency G4/G7): strip ANY other type:*/area:*
  # the issue carries — including a rename-derived type that differs from the mapped one (the canonical
  # case: a Plan: issue renamed documentation->type:docs but mapped to type:plan). Not just type:enhancement.
  rm=()
  for l in $(gh issue view "$n" -R "$SLUG" --json labels --jq '.labels[].name'); do
    case "$l" in
      type:*) [ "$l" = "$type" ] || rm+=(--remove-label "$l");;
      area:*) { [ "$type" = "type:epic" ] || [ "$l" = "$area" ]; } || rm+=(--remove-label "$l");;
    esac
  done
  rm+=(--remove-label design --remove-label performance --remove-label quality)
  add=(--add-label "$type"); [ "$type" = "type:epic" ] || add+=(--add-label "$area")
  gh_ratecheck; gh_throttle
  gh issue edit "$n" -R "$SLUG" "${add[@]}" "${rm[@]}"
done
```

- [ ] **Step 5 (L5 fix — deterministic canary):** pick a fixed representative issue (#49 — formerly `enhancement`, maps to `type:feature`/`area:cognitive`); run the loop body for #49 only, hand-verify `gh issue view 49 --json labels` shows exactly `type:feature`+`area:cognitive` (no `type:enhancement`), then run the full script. **Commit** generator + reviewed TSV + migration script.

### Task 7: Confirm the `enhancement` split inside the TSV

- [ ] **Step 1:** Assert no `AMBIGUOUS`; every formerly-`enhancement` row resolves to a `type:*` with a `reason`. Spot-check 5 rows vs `gh issue view <n>`. (This task gates Task 6 Step 5 via the `grep AMBIGUOUS` guard.)

### Task 8: `03-verify.sh` (fail-loud gate; A3 fix — two modes)

**Files:** Create `03-verify.sh`.

- [ ] **Step 1:**

```bash
#!/usr/bin/env bash
source "$(dirname "$0")/lib.sh"; MD="$(dirname "$0")/manifests"; MODE="${1:---type-area}"; fail=0
gh issue list -R "$SLUG" --state open --limit 500 --json number,labels | jq -e '
  [ .[] | {n:.number, t:([.labels[].name|select(startswith("type:"))]|length),
           a:([.labels[].name|select(startswith("area:"))]|length),
           epic:([.labels[].name]|index("type:epic"))}
    | select(.t!=1 or (.epic==null and .a!=1) or (.epic!=null and .a>1)) ] as $bad
  | if ($bad|length)>0 then ("FAIL \($bad|length):\n"+($bad|map("  #\(.n) types=\(.t) areas=\(.a)")|join("\n"))|halt_error(1))
    else "OK: type+area on all open issues" end' || fail=1
if [ "$MODE" = "--full" ]; then   # post-delete: live label set must EQUAL the manifest
  diff <(gh label list -R "$SLUG" --limit 300 --json name --jq '[.[].name]|sort|.[]') \
       <(jq -r '.[].name' "$MD"/labels.core.json "$MD"/labels.area.memory-engine.json | sort) \
    && echo "LABELS OK" || fail=1
  for L in duplicate invalid wontfix "help wanted" "good first issue" question phase-5 phase-5a phase-5b phase-6 deferred; do
    gh label list -R "$SLUG" --json name --jq '.[].name'|grep -qx "$L" && { echo "FAIL: '$L' still present"; fail=1; }
  done
fi
exit $fail
```

- [ ] **Step 2:** `bash scripts/github-pm/03-verify.sh --type-area` → **exit 0** (constraint #3). **Commit.**

### Task 9: `04-delete-labels.sh` (gate-fenced + usage-checked on OPEN+CLOSED)

**Files:** Create `04-delete-labels.sh`.

- [ ] **Step 1:**

```bash
#!/usr/bin/env bash
source "$(dirname "$0")/lib.sh"
bash "$(dirname "$0")/03-verify.sh" --type-area || { echo "GATE RED — refusing to delete"; exit 1; }
TARGETS=(duplicate invalid wontfix "help wanted" "good first issue" question design performance quality)
for L in "${TARGETS[@]}"; do
  cnt=$(gh issue list -R "$SLUG" --state all --label "$L" --limit 1000 --json number --jq 'length' 2>/dev/null||echo 0)
  [ "$cnt" -gt 0 ] && { echo "WARN '$L' still on $cnt issue(s) — NOT deleting"; continue; }
  gh_throttle; gh label delete "$L" -R "$SLUG" --yes || true; echo "deleted $L"
done
```

- [ ] **Step 2:** Run; confirm targets gone, nothing in-use deleted. **Commit.** (`phase-*`/`deferred` retained → Task 17.)

---

## Phase C — Projects, fields, items

### Task 10: `05-projects-fields.sh` (M2 + M4 fixes — concrete fields, options via API, ID cache)

**Files:** Create `manifests/projects.json`, `05-projects-fields.sh`; generate `projects.lock.json` + `state.json`.

- [ ] **Step 1:** `projects.json`:

```json
{
  "projects": [
    {
      "key": "main",
      "title": "Memory Engine — Main",
      "fields": ["Status", "Priority", "Phase"]
    },
    {
      "key": "triage",
      "title": "Memory Engine — Bug & Security Triage",
      "fields": ["Status", "Priority"]
    },
    {
      "key": "roadmap",
      "title": "Memory Engine — Roadmap",
      "fields": ["Status", "Priority", "Phase"]
    }
  ],
  "options": {
    "Status": [
      ["Todo", "GRAY"],
      ["In Progress", "YELLOW"],
      ["In Review", "BLUE"],
      ["Done", "GREEN"]
    ],
    "Priority": [
      ["P0 Critical", "RED"],
      ["P1 High", "ORANGE"],
      ["P2 Medium", "YELLOW"],
      ["P3 Low", "GREEN"],
      ["P4 Deferred", "GRAY"]
    ],
    "Phase": [
      ["Phase 4", "GRAY"],
      ["Phase 5a", "BLUE"],
      ["Phase 5b", "BLUE"],
      ["Phase 5 (indep)", "PURPLE"],
      ["Phase 6", "GREEN"],
      ["Phase 7", "PINK"],
      ["Deferred", "GRAY"]
    ]
  }
}
```

- [ ] **Step 2:** `05-projects-fields.sh` — idempotent create-or-get by title; set `Status` options via `updateProjectV2Field(singleSelectOptions)` (verified input; the built-in field is editable this way); create `Priority`/`Phase` via `createProjectV2Field`; **query each field's options and cache `{field_id, options:{name→id}}` into `state.json`**; write project numbers+IDs to both `projects.lock.json` (committed) and `state.json` (gitignored):

```bash
#!/usr/bin/env bash
source "$(dirname "$0")/lib.sh"; MD="$(dirname "$0")/manifests"; ST="$(dirname "$0")/state.json"; OID=$(owner_id)
create_or_get(){ local e; e=$(gql 'query($l:String!){user(login:$l){projectsV2(first:50){nodes{number title id}}}}' "$(jq -nc --arg l "$PM_OWNER" '{l:$l}')" --jq ".data.user.projectsV2.nodes[]|select(.title==\"$1\")|\"\(.number) \(.id)\""|head -1); [ -n "$e" ]&&{ echo "$e"; return; }; gql 'mutation($o:ID!,$t:String!){createProjectV2(input:{ownerId:$o,title:$t}){projectV2{number id}}}' "$(jq -nc --arg o "$OID" --arg t "$1" '{o:$o,t:$t}')" --jq '.data.createProjectV2.projectV2|"\(.number) \(.id)"'; }
field_id(){ gql 'query($p:ID!){node(id:$p){... on ProjectV2{fields(first:50){nodes{... on ProjectV2SingleSelectField{id name}}}}}}' "$(jq -nc --arg p "$1" '{p:$p}')" --jq ".data.node.fields.nodes[]?|select(.name==\"$2\")|.id"; }
# Desired options as JSON, MERGING any existing option's id by name (fixes Codex C4: replacing a field's
# options WITHOUT echoing back existing ids recreates them and DROPS items' current Status assignments).
desired_opts(){ local pid="$1" f="$2" ex
  ex=$(gql 'query($p:ID!){node(id:$p){... on ProjectV2{fields(first:50){nodes{... on ProjectV2SingleSelectField{name options{id name}}}}}}}' "$(jq -nc --arg p "$pid" '{p:$p}')" --jq "[.data.node.fields.nodes[]?|select(.name==\"$f\")|.options[]?]" 2>/dev/null)
  jq -c --argjson ex "${ex:-[]}" --arg f "$f" '[ .options[$f][] as $o | ($ex|map(select(.name==$o[0]))[0]) as $m | {name:$o[0],color:$o[1],description:""} + (if $m then {id:$m.id} else {} end) ]' "$MD/projects.json"; }
set_status_opts(){ local fid; fid=$(field_id "$1" Status); gql 'mutation($f:ID!,$o:[ProjectV2SingleSelectFieldOptionInput!]!){updateProjectV2Field(input:{fieldId:$f,singleSelectOptions:$o}){projectV2Field{... on ProjectV2SingleSelectField{id}}}}' "$(jq -nc --arg f "$fid" --argjson o "$(desired_opts "$1" Status)" '{f:$f,o:$o}')" >/dev/null; }
create_select(){ local fid; fid=$(field_id "$1" "$2"); [ -n "$fid" ] && return 0; gql 'mutation($p:ID!,$n:String!,$o:[ProjectV2SingleSelectFieldOptionInput!]!){createProjectV2Field(input:{projectId:$p,dataType:SINGLE_SELECT,name:$n,singleSelectOptions:$o}){projectV2Field{... on ProjectV2SingleSelectField{id}}}}' "$(jq -nc --arg p "$1" --arg n "$2" --argjson o "$(desired_opts "$1" "$2")" '{p:$p,n:$n,o:$o}')" >/dev/null; }
read MN MI < <(create_or_get "Memory Engine — Main")
read TN TI < <(create_or_get "Memory Engine — Bug & Security Triage")
read RN RI < <(create_or_get "Memory Engine — Roadmap")
for PID in "$MI" "$RI"; do set_status_opts "$PID"; create_select "$PID" Priority; create_select "$PID" Phase; done
set_status_opts "$TI"; create_select "$TI" Priority
# projects.lock.json: numbers + node IDs (committed, reproducible from the create response).
jq -n --arg mn "$MN" --arg mi "$MI" --arg tn "$TN" --arg ti "$TI" --arg rn "$RN" --arg ri "$RI" \
  '{main:{number:$mn,id:$mi},triage:{number:$tn,id:$ti},roadmap:{number:$rn,id:$ri}}' > "$MD/projects.lock.json"
# state.json (fixes Codex C2 — concrete, ALL 3 projects): lock + per-project field/option-id maps. Gitignored,
# regenerable (re-run this script). Task 11 reads .<project>.fields.<Field>.{id,options.<value-name>}.
fmap(){ gql 'query($p:ID!){node(id:$p){... on ProjectV2{fields(first:50){nodes{... on ProjectV2SingleSelectField{id name options{id name}}}}}}}' "$(jq -nc --arg p "$1" '{p:$p}')" --jq '[.data.node.fields.nodes[]?|select(.name)|{(.name):{id:.id,options:(reduce (.options[]?) as $o ({};.[$o.name]=$o.id))}}]|add'; }
jq -n --slurpfile lk "$MD/projects.lock.json" --argjson m "$(fmap "$MI")" --argjson t "$(fmap "$TI")" --argjson r "$(fmap "$RI")" \
  '$lk[0] * {main:{fields:$m},triage:{fields:$t},roadmap:{fields:$r}}' > "$ST"
```

- [ ] **Step 3:** Run; verify `projects.lock.json` has 3 numbers; `gh project field-list <MN> --owner dutiona` shows Status(4 opts incl. In Review)/Priority/Phase. If `set_status_opts` errors on the built-in field, fall back to the Task 15 UI step for Status options only (contingency, not expected).
- [ ] **Step 4: Commit** `projects.json` + `projects.lock.json` + script.

### Task 11: `06-add-items-fields.sh`

**Files:** Create `06-add-items-fields.sh`.

- [ ] **Step 1 — add items + record item ids** (fixes Codex R2-2 — concrete map). `project_item_add` _returns_ the item id; capture it into an `issue⟶item_id` map TSV per project so Step 2 can look it up. E.g. for Main:
  ```bash
  jq -r '.[].number' manifests/open-issues.json | while read -r n; do
    iid=$(project_item_add "$MI" "$(issue_node "$n")"); printf '%s\t%s\n' "$n" "$iid"
  done > manifests/main-items.tsv
  ```
  Targets: **all** open issues → Main; `type:bug`∨`type:security` → Triage; the **11 critical-path issues + #221 + the 9 epics** → Roadmap (each into its own `<project>-items.tsv`).
- [ ] **Step 2 — set Priority/Phase** (fixes Codex C6 — concrete shape + lookup + unset handling). For each `issue-map.tsv` row with a non-empty `phase`/`priority` (empty = legitimately unset → skip), for the project(s) the issue belongs to: get `iid` from the project's `<project>-items.tsv` map (Step 1); read `fid=.<project>.fields.<Field>.id` and `oid=.<project>.fields.<Field>.options["<value>"]` from `state.json`; **fail loud if `oid` is null** (a value-name typo, never silent). Then `set_field <project_id> "$iid" "$fid" "$oid"`:
  ```bash
  set_field(){ # project_id item_id field_id option_id
    gql 'mutation($p:ID!,$i:ID!,$f:ID!,$o:String!){updateProjectV2ItemFieldValue(input:{projectId:$p,itemId:$i,fieldId:$f,value:{singleSelectOptionId:$o}}){projectV2Item{id}}}' "$(jq -nc --arg p "$1" --arg i "$2" --arg f "$3" --arg o "$4" '{p:$p,i:$i,f:$f,o:$o}')" >/dev/null; }
  ```
  `updateProjectV2ItemFieldValue` is last-write-wins ⇒ idempotent.
- [ ] **Step 3:** Verify Main items ≥ 97; Triage = count(bug∨security); Roadmap = 11 + #221 + epics. **Commit.**

### Task 12: `epics.json` (single-parent, open-filtered) + `07-epics.sh` (H1/H2/A2 fixes)

**Files:** Create `manifests/epics.json`, `07-epics.sh`.

- [ ] **Step 1 (H1 fix — deduplicated single-parent tree; reconciled to spec):** Author `epics.json`. Contested issues resolved to ONE parent with rationale: **#225→#221** (it's a #221 hook sub-issue), **#226→#221** (same), **#132→Temporal** (per spec — #132 is `FactType::Prediction`, a temporal capability; the earlier "→Cognitive" was a draft error). 9 epics; #221 reuses `existing_issue:221`; the other 8 are new `type:epic` issues.
- [ ] **Step 2 (H2 fix — filter members through the open set):** `07-epics.sh` loads `manifests/open-issues.json` into a set; for each epic: resolve-or-create the umbrella (label `type:epic`+area, add to Main+Roadmap), then for each member **that is in the open set**, `subissue_link` (uses `replaceParent:true` — A2, idempotent). Members not open (e.g. #224, #128) are **skipped with a logged note** — INV-OPEN-ONLY preserved. Each successful `subissue_link` appends `{parent,child}` to `state.json.epic_links[]` so `restore.sh` can `removeSubIssue` them (C9 rollback path).
- [ ] **Step 3:** Single-parent guard: `jq -r '.epics[].members[]' manifests/epics.json | sort | uniq -d` → **empty**. Cross-ref: every member ∈ open set or logged-skipped.
- [ ] **Step 4:** Run; verify each epic's sub-issue list. **Commit** `epics.json` + script.

---

## Phase D — Workflow, agent contract, docs

### Task 13: Secret + `12-render-workflow.sh` (INV-SECRET-FIRST)

- [ ] **Step 1:** Create `MEMORY_ENGINE_PROJECT_TOKEN` — **a fine-grained PAT for the Action only.** Two-token split (clarifies Gemini G3): the 96-issue label **migration** runs locally with your **CLI token** (classic PAT, already has `repo` write — it applies labels). This PAT is consumed **only by the `add-to-project` Action at runtime**, which _adds items to projects_ and never writes labels. Scopes: account `Projects: read and write`; repo `Issues: read`, `Pull requests: read`, `Metadata: read` — **not** Issues write (a PAT is required because `GITHUB_TOKEN` can't write user-owned projects). `gh secret set MEMORY_ENGINE_PROJECT_TOKEN -R dutiona/memory-engine` (paste; never commit).
- [ ] **Step 2: Token pre-flight** — with the token, `addProjectV2ItemById` then `deleteProjectV2Item` a throwaway item on Main. Must succeed before committing the workflow.
- [ ] **Step 3 (Codex C7 / Gemini G5 — SHA-pin in the shipped workflow, not a follow-up):** `12-render-workflow.sh` resolves the action tag to a full commit SHA, then emits `.github/workflows/add-to-project.yml` with that pinned SHA + literal project numbers from `projects.lock.json`:

```bash
SHA=$(gh api repos/actions/add-to-project/commits/v1 --jq '.sha')   # resolve tag v1 -> immutable commit SHA
```

```yaml
name: Auto-add to projects
on:
  issues: { types: [opened, reopened] }
  pull_request: { types: [opened, reopened] }
jobs:
  add-to-main:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/add-to-project@<SHA> # rendered: pinned to the v1 commit SHA (supply-chain hardening)
        with:
          project-url: https://github.com/users/dutiona/projects/<MAIN_NUM>
          github-token: ${{ secrets.MEMORY_ENGINE_PROJECT_TOKEN }}
  add-to-triage:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/add-to-project@<SHA> # pinned to the v1 commit SHA
        with:
          project-url: https://github.com/users/dutiona/projects/<TRIAGE_NUM>
          github-token: ${{ secrets.MEMORY_ENGINE_PROJECT_TOKEN }}
          labeled: type:bug, type:security
          label-operator: OR
```

- [ ] **Step 4:** `python3 -c 'import yaml,sys;yaml.safe_load(open(sys.argv[1]))' .github/workflows/add-to-project.yml`. **Commit** workflow + render script.

### Task 14: Agent-file labelling contract (×3, byte-identical block)

- [ ] **Step 1:** Author the "Issue Labeling Convention" block (title grammar `type(area): description`; exactly one `type:`+`area:` (epics may omit area); exclusivity; enumerated value lists; `addSubIssue` snippet w/ `replaceParent:true`; collateral-issue rule; routing note). Wrap in `<!-- pm-contract:start -->`/`<!-- pm-contract:end -->`.
- [ ] **Step 2:** Insert byte-identically into `CLAUDE.md`/`AGENTS.md`/`GEMINI.md` at a consistent anchor.
- [ ] **Step 3:** `for f in CLAUDE.md AGENTS.md GEMINI.md; do awk '/pm-contract:start/{f=1} f; /pm-contract:end/{f=0}' "$f"|sha256sum; done` → 3 equal hashes. **Commit.**

### Task 15: Operator docs + one-time UI checklist

- [ ] **Step 1:** `docs/reference/project-management.md`: taxonomy (link manifests), 3 fields, 3 projects + routing, `MEMORY_ENGINE_PROJECT_TOKEN` prerequisite, and the **one-time UI checklist** (now smaller — `Status` options are scripted): built-in **close→Done** automation per project; **Main views** `Daily Kanban` (Board by Status) + 14 `area:*` Tables + `Epics` (filter `type:epic`) + `Orphans` (no `type:*` OR no `area:*`) + `Super QA` (filter `super-qa`); **Triage views** `Bug & Security` (Board)+`Bugs`+`Security`+`Triage – no type`+`Triage – no area`; **Roadmap views** `Roadmap` (Board by `Phase`)+`Critical Path` (the 11 issues + #221)+`Blockers` (filter `status:blocked`). Note: renames propagate to closed issues (historical queries keep working — A4).
- [ ] **Step 2:** Add "## Project Management" note to `docs/ROADMAP.md` (link `PLAN_ISSUE` + reference page); link the page from `CLAUDE.md`'s doc table. **Commit.**

---

## Phase E — Back-fill retirement & ship

### Task 16: Confirm Phase-field coverage (gates Task 17)

- [ ] **Step 1:** Assert every issue carrying a `phase-*`/`deferred` label now has a non-empty `Phase` value on its Main item (query items; list any gaps; must be empty).

### Task 17: `08-retire-phase-labels.sh`

- [ ] **Step 1:** After Task 16 passes, delete `phase-5 phase-5a phase-5b phase-6 deferred`. Re-run `03-verify.sh --full` → exit 0 (label set == manifest; no `phase-*`/`deferred`). **Commit.**

### Task 18: PR, review, merge

- [ ] **Step 1:** Push branch; `gh pr create -R dutiona/memory-engine --base main --title "chore(build): GitHub project-management system" --body "Implements the validated PM design. Plan: #$PLAN_ISSUE. Includes verify.sh gate output + UI checklist."`
- [ ] **Step 2:** Run **`/super-review`**; address severity-gated findings; re-run until clean.
- [ ] **Step 3: Squash-merge** (repo rule). Rebase first if conflicts.
- [ ] **Step 4: Post-merge** — execute the Task 15 UI checklist (views + close→Done; Status options only if the API path failed). Confirm `gh workflow view "Auto-add to projects"` active; open a throwaway issue → confirm it lands in Main → close → confirm it moves to Done.

---

## Documentation

- Validated spec at `docs/design/plans/2026-05-31-github-project-management-design.md` (Task 1).
- Agent-file contract — marked block, byte-identical in the three files (Task 14).
- `docs/reference/project-management.md` — operator guide + UI checklist + token prerequisite (Task 15); linked from `CLAUDE.md` + `docs/ROADMAP.md`.
- `scripts/github-pm/README.md` — run order, invariants, restore, cross-repo reuse.
- **Known-not-addressed:** broader `CLAUDE/AGENTS/GEMINI.md` drift — only the contract block is synced; full reconciliation is a separate chore.

## Testing

**N/A for `cargo` — justified:** zero Rust touched (label/project config, a workflow YAML, Bash, Markdown). `cargo build/test/clippy/fmt` exercise nothing here; the CLAUDE.md gate protects `error.rs/types.rs/traits.rs/lib.rs`/public API — none touched.
**Executable test surface:** `03-verify.sh` (fail-loud type+area + label-set conformance); **idempotency** (re-run 01/02/05/06/07 → zero changes); **canary** (#49, Task 6 Step 5); **restore smoke-test** (Task 4 Step 3); **YAML validity** + post-merge throwaway-issue auto-add (Task 18 Step 4).

## Verification

| #   | Check                                                     | Command                                           | Pass            |
| --- | --------------------------------------------------------- | ------------------------------------------------- | --------------- |
| V1  | type+area on all open issues                              | `bash scripts/github-pm/03-verify.sh --type-area` | exit 0          |
| V2  | label set == manifest (post-delete)                       | `bash scripts/github-pm/03-verify.sh --full`      | exit 0          |
| V3  | 6 defaults + design/perf/quality gone                     | grep `gh label list`                              | absent          |
| V4  | no `phase-*`/`deferred` (post Task 17)                    | grep `gh label list`                              | absent          |
| V5  | 3 projects, numbers captured                              | `cat manifests/projects.lock.json`                | 3 numbers       |
| V6  | Status(4 incl. In Review)/Priority/Phase on Main+Roadmap  | `gh project field-list <n> --owner dutiona`       | options match   |
| V7  | Main ≥ 97; Triage = bug∨security; Roadmap = 11+#221+epics | count items                                       | matches         |
| V8  | epics single-parent + linked (open members only)          | `uniq -d` empty; query sub-issues                 | members present |
| V9  | workflow YAML valid + numbers == lock                     | `yaml.safe_load`; grep                            | parses; equal   |
| V10 | secret exists before workflow committed                   | `gh secret list`                                  | present         |
| V11 | agent-doc block identical ×3                              | sha256 of marked block                            | 3 equal         |
| V12 | idempotency                                               | re-run scripts                                    | zero changes    |
| V13 | restore.sh works                                          | Task 4 smoke-test                                 | label restored  |
| V14 | cargo N/A                                                 | —                                                 | justified       |

---

## Optional PR-split (if smaller PRs preferred)

Default: one PR. If incremental: **PR-1** Tasks 1–9+14 (labels + migration + agent contract — self-sustaining core); **PR-2** Tasks 10–13,15 (projects + fields + auto-add + docs); **PR-3** Tasks 16–17 + 11/12 (field back-fill + epics + phase-label retirement). Each independently shippable.

## Risk register (top 5)

| Risk                                                           | Mitigation (task)                                                                |
| -------------------------------------------------------------- | -------------------------------------------------------------------------------- |
| `enhancement` split / stale `type:enhancement` (gate-breaking) | strip `type:enhancement` when target ≠ enhancement (Task 6 §4) + canary #49 (§5) |
| delete+recreate orphans associations                           | INV-RENAME — `gh label edit` only (Task 5/lib.sh)                                |
| project number ≠ #4 (reify drift)                              | query-first + capture to lock (INV-CAPTURE, Task 10)                             |
| auto-add fails (wrong token)                                   | fine-grained PAT + pre-flight write test + INV-SECRET-FIRST (Task 13)            |
| epic membership hits closed/nonexistent issues                 | filter members through live open set (Task 12 §2)                                |

## Self-Review

- **Spec coverage:** labels (T5), 3 projects+fields incl. API-set Status options (T10), auto-add+secret (T13), open-only migration + fail-loud gate (T6/T8), enhancement-split + design→research fold + phase→field (T6/7/16/17), single-parent open-filtered epics (T12), agent contract (T14), one-area discipline (T8), UI checklist (T15), git workflow (T2/T18). ✅
- **Findings addressed:** B1 (per-row `type:enhancement` strip, generalized), H1 (#132→Temporal, deduped epics.json), H2 (open-filter members), A1 (refresh fixture), M1 (concrete `gen-issue-map.sh`), M2 (field/option-ID cache + schema), M3 (11 issues enumerated + #221), M4 (Status options via API, verified), A2 (`replaceParent:true`), L2 (real restore + smoke-test), L3 (lock vs state boundary), A3 (verify two modes), L5 (deterministic canary), A4 (closed-issue rename note). ✅
- **Placeholder scan:** mapping rules + manifests + scripts concrete; `issue-map.tsv` has a real generator + human gate. ✅
- **Type consistency:** `state.json`/`projects.lock.json`, `lib.sh` names (`label_rename`, `project_item_add`, `subissue_link`), field/option naming consistent. ✅
- **Open decisions surfaced:** Bash vs Python (chose Bash for reify/coraly consistency + zero deps); deploy = merge-then-run; `actions/add-to-project` SHA-pin = noted follow-up.
