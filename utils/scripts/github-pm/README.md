# github-pm — GitHub Projects-v2 desired-state reconciler

Reproducible, label-routed GitHub Projects-v2 management for `dutiona/memory-engine`.
Target facts live in version-controlled **manifests** (`manifests/`, the single source of
truth); thin, idempotent module scripts converge GitHub to them via `gh` + GraphQL
(query → diff → apply; a re-run produces **zero changes**). `03-verify.sh` is the
fail-loud acceptance gate.

Execution follows a strict **backup → dry-run → canary → gate → delete** order; every
destructive step is fenced behind the green gate.

## Layout

```
utils/scripts/github-pm/
├── README.md                                # this file
├── lib.sh                                   # throttle/retry/upsert/rename/field/subissue helpers (sourced everywhere)
├── manifests/
│   ├── labels.core.json                     # CROSS-REPO: type/severity/status/priority/super-qa (25)
│   ├── labels.area.memory-engine.json       # PER-REPO: area:* (14)
│   ├── projects.json                        # project titles + field schema + option colors
│   ├── projects.lock.json                   # GENERATED+COMMITTED: #4/#5/#6 numbers + node IDs
│   ├── open-issues.json                     # GENERATED+GITIGNORED: live open-issue fixture (refreshed in 02)
│   ├── issue-map.tsv                         # GENERATED+REVIEWED: issue→type,area,phase,priority,epic,reason
│   └── epics.json                           # 9 single-parent epics → {title,area,state,existing_issue,members[]}
├── 00-backup.sh   01-sync-labels.sh   02-migrate-issues.sh   03-verify.sh
├── 04-delete-labels.sh   05-projects-fields.sh   06-add-items-fields.sh
├── 07-epics.sh   08-retire-phase-labels.sh   12-render-workflow.sh
├── gen-issue-map.sh                          # emits issue-map.tsv from open-issues.json
├── restore.sh                                # rollback from backups/<ts>/
├── state.json                                # GENERATED+GITIGNORED: numbers + field/option IDs (regenerable)
└── backups/                                  # GITIGNORED snapshots
```

## Run order (00 → 12)

Source `lib.sh` is loaded by each script. Run from the repo root. Stop at the gate
(`03-verify.sh`) before any destructive step.

| #   | Script                      | Phase        | Notes                                                                                                                                                                                                     |
| --- | --------------------------- | ------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 00  | `00-backup.sh`              | A — backup   | Snapshots labels, open issues, all-issue labels, projects → `backups/<ts>/`; symlinks `backups/latest`. **Non-destructive.**                                                                              |
| —   | `gen-issue-map.sh`          | B — labels   | Refresh `open-issues.json`, then emit `issue-map.tsv`. Pre-fills the machine-derivable columns, writes `AMBIGUOUS` elsewhere. **HUMAN-REVIEW GATE: resolve every `AMBIGUOUS` before 02.**                 |
| 01  | `01-sync-labels.sh`         | B — labels   | Node-id-stable renames FIRST (INV-RENAME), then upsert the full canonical set. **Additive only — no deletes.**                                                                                            |
| 02  | `02-migrate-issues.sh`      | B — labels   | Reconcile each open issue to exactly `{type, area}`. Refuses to run while any `AMBIGUOUS` row remains. Canary #49 first.                                                                                  |
| 03  | `03-verify.sh`              | B — gate     | **The gate.** `--type-area` (default): every open issue has exactly one `type:*` + one `area:*` (epics may omit area). `--full` (post-delete): live label set == manifest. Exit 0 == green.               |
| 04  | `04-delete-labels.sh`       | B — labels   | **Gate-fenced** (re-runs `03-verify.sh --type-area`, refuses on red). Deletes 6 defaults + design/performance/quality, but only if usage on OPEN+CLOSED is zero.                                          |
| 05  | `05-projects-fields.sh`     | C — projects | Create-or-get 3 projects by title; set `Status` options + create `Priority`/`Phase` via GraphQL; cache field/option IDs → `state.json`; write numbers+node IDs → `projects.lock.json`.                    |
| 06  | `06-add-items-fields.sh`    | C — items    | Add items (all open → Main; bug∨security → Triage; 11 critical-path + #221 + 9 epics → Roadmap); record `issue→item_id` TSVs; set Priority/Phase from `issue-map.tsv`.                                    |
| 07  | `07-epics.sh`               | C — epics    | Resolve-or-create each umbrella; link open members via query-first `subissue_link` (`replaceParent:true`). Records `{child,priorParent}` rollback entries to `state.json.epic_links[]` only on `changed`. |
| 08  | `08-retire-phase-labels.sh` | E — ship     | After Phase-field coverage confirmed: delete `phase-5 phase-5a phase-5b phase-6 deferred`; re-run `03-verify.sh --full` → exit 0.                                                                         |
| 12  | `12-render-workflow.sh`     | D — workflow | Resolve `actions/add-to-project@v1` tag to a commit SHA; emit `.github/workflows/add-to-project.yml` SHA-pinned with literal project numbers from `projects.lock.json`. **INV-SECRET-FIRST.**             |

> The numbering matches the plan's task IDs; `02-migrate-issues.sh` consumes the
> `gen-issue-map.sh` output, and `06`/`07` consume `state.json` produced by `05`.

## The seven invariants (asserted by every script)

- **INV-OPEN-ONLY** — migration + epic-linking operate only on the **open** issue set
  (refreshed live); closed issues are read for the deletion-safety audit only and never
  mutated.
- **INV-RENAME** — 1:1 label migrations use `gh label edit --name` only (node-id-stable,
  preserves every issue↔label association incl. on closed issues); an in-use label is
  never delete+recreated.
- **INV-ADDITIVE-FIRST** — all target labels are created before any old label is
  removed/deleted.
- **INV-GATE-FENCE** — no `gh label delete` and no project creation runs until
  `03-verify.sh --type-area` exits 0.
- **INV-CAPTURE** — project numbers/node-IDs + field/option IDs are read from API
  responses into `state.json` / `projects.lock.json`; never guessed or hard-coded.
- **INV-SECRET-FIRST** — the workflow file is committed only after
  `MEMORY_ENGINE_PROJECT_TOKEN` exists and passes a write pre-flight.
- **INV-IDEMPOTENT** — every script re-run produces zero changes and exits 0.

## Rollback — `restore.sh`

Reverses changes from a backup snapshot in `backups/<ts>/` (default `backups/latest`).

```bash
bash utils/scripts/github-pm/restore.sh                 # full rollback from backups/latest
bash utils/scripts/github-pm/restore.sh --labels-only   # labels only (used by the Task 4 smoke-test)
```

What it restores:

- **Labels** — recreate/rename labels to match the snapshot's `labels.json`.
- **Issue label sets** — for each open issue, converge its label set back to the
  snapshot's `open-issues.json` (`gh issue edit --add-label` / `--remove-label`).
- **Epic links** — `removeSubIssue` for every link recorded in
  `state.json.epic_links[]` (each entry written by `07-epics.sh` only on an actual
  `changed`), re-attaching the child to its original parent under `replaceParent`.
- **Projects** — run-created projects are listed for manual
  `deleteProjectV2 --confirm` (intentionally not auto-deleted).

`state.json` and `backups/` are gitignored; `projects.lock.json` is committed (the
durable record the workflow renders from).

## Cross-repo reuse recipe

The core taxonomy is split from the per-repo `area:*` manifest so another repo
(e.g. `reify`, `coraly`) can adopt the suite by swapping one file. To reuse:

1. **Copy the directory** `utils/scripts/github-pm/` into the target repo.
2. **Swap `labels.area.<repo>.json`** — replace `labels.area.memory-engine.json` with
   a per-repo `area:*` manifest reflecting that repo's subsystems
   (`labels.core.json` is cross-repo — keep it byte-identical).
3. **Regenerate `issue-map.tsv`** — run `gen-issue-map.sh` against the new repo's
   refreshed `open-issues.json`, then human-review every `AMBIGUOUS`.
4. **Re-author `epics.json`** — the umbrellas + single-parent membership are repo-specific.
5. **Set `PM_OWNER` / `PM_REPO`** — every script reads these from the environment
   (defaults `dutiona` / `memory-engine`):

   ```bash
   export PM_OWNER=dutiona PM_REPO=reify
   ```

6. Run the suite in order from `00`; `projects.lock.json` / `state.json` regenerate
   for the new repo.
