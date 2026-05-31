# Advisor Review — GitHub PM Plan

> Stronger-reviewer pass with full session context. Triages the clean-slate subagent findings, then adds what it missed. Saved verbatim. Resolution appended in Step 3c.

The clean-slate subagent's review is high-quality and I largely concur. Triage, then additions.

## Triage of subagent findings

**B1 [BLOCKER] — Confirmed.** Trace #49 (currently `enhancement`, mapped to `type:feature`): after rename it carries `type:enhancement`; Task 6 applies `--add-label type:feature --remove-label enhancement` (no-op) → both `type:enhancement` AND `type:feature` → 2 `type:*` → INV-GATE-FENCE locks. Fix is per-row conditional:

```bash
extra_remove=""
[ "$type" = "type:feature" ] && extra_remove="--remove-label type:enhancement"
```

**H1 [HIGH] — Confirmed, plus a real spec contradiction in the plan.** The spec places #132 ONLY in "Temporal Reasoning & Prediction"; Task 12 asserts "#132→Cognitive (not Temporal)" — the spec never had #132 in Cognitive. Inherited error from the architecture-first draft. Fix: #132→Temporal (per spec). Author `epics.json` deduplicated upfront with explicit one-line reconciliation: #225→Hook221, #226→Hook221, #132→Temporal.

**H2 [HIGH] — Confirmed.** #224 closed (PR #230); #128 closed (Phase 4a follow-ups ✅). Both in illustrative epic membership, absent from the open fixture. Fix: filter epic members through the open-issue set in `07-epics.sh`. Don't carve out INV-OPEN-ONLY — keep the invariant clean.

**M1 [MEDIUM] — Confirmed, load-bearing.** Provide a concrete jq/shell generator emitting one row per open issue: type pre-filled from explicit labels, area from title `type(area):` hint via regex, phase from `phase-*` label, else `AMBIGUOUS`. Make this Task 6 Step 1's actual content.

**M2 [MEDIUM] — Confirmed.** After each `createProjectV2Field`, query `ProjectV2SingleSelectField.options` and cache `{field_id, option_name→option_id}` in `state.json` with explicit schema. Task 11 reads this; without it `updateProjectV2ItemFieldValue` cannot resolve option IDs.

**M3 [MEDIUM] — Confirmed.** The 11 issues: #49,#158,#57,#225,#50,#51,#52,#164,#165,#166,#226. Step 12 = "close #221" (no issue to add). Reframe as "11 critical-path issues + #221 umbrella"; propagate everywhere.

**M4 [MEDIUM] — Confirmed; understated.** The "if API permits, else UI" hedge must NOT ship. Commit a choice in Task 10: (a) accept default 3-state, drop "In Review"; (b) create a NEW custom single-select "Workflow Stage" with 4 states; (c) UI checklist as explicit mandatory step. Verify live: `gh api graphql -f q='{ __type(name:"UpdateProjectV2FieldInput"){ inputFields{ name } } }'` — if `singleSelectOptions` present, the API can do it.

**L1–L5 — All valid.** L3: `lock.json` = project numbers+node IDs (committed, reproducible); `state.json` = field/option IDs (gitignored, regenerable via field query). L2 (real `restore.sh`, smoke-tested) elevated — safety-first framing depends on rollback actually working.

## What the subagent missed

**A1 [HIGH] — The plan-issue (Task 2) won't be in the TSV.** `/tmp/me_open_issues.json` was harvested before Task 2 creates the plan-issue → TSV omits it → it has only `type:plan` + no `area:*` → `03-verify.sh` fails on the plan-issue itself. Fix: Task 6 Step 1 refreshes the fixture (`gh issue list --state open ... > manifests/open-issues.json`) and the generator reads from there (also catches issues opened during execution).

**A2 [MEDIUM] — `addSubIssue` idempotency unverified.** Plan asserts "idempotent via `replaceParent`" but nothing shows that field exists on `AddSubIssueInput`. Verify: `gh api graphql -f q='{ __type(name:"AddSubIssueInput"){ inputFields{ name } } }'`. If absent, `lib.sh` needs a query-first wrapper (check child's parent before mutating).

**A3 [LOW] — `03-verify.sh` label-set diff noisy intermediate.** Between Task 8 and Task 9 live labels are a superset of the manifest; the `|| echo DRIFT` branch fires without failing (correct) but confuses operators. Split `--type-area-only` (intermediate) vs `--full` (post-delete).

**A4 [LOW] — Renames propagate to closed issues** (node-id stable). Desirable for retrospective queries; add a one-line operator note.

## Strategy verdict

Architecture (declarative manifests + invariants + gated execution) is sound and proportionate. Every finding is tactical — scripted details, not strategy. Don't re-design.

## Recommendation

Address **B1, H1, H2, A1** in the plan body before Step 4. **M1, M2, M4, A2** should also land — the tmux reviewers would rediscover them and burn rounds. Remaining MEDIUM/LOW are polish for the same pass. Verify `AddSubIssueInput` + `UpdateProjectV2FieldInput` live (A2 + M4) — 30 seconds, decides two forks. After fixes, Step 4 still warranted; should converge in 1–2 rounds.

## Resolution

All findings addressed in plan v2. The two forks were verified live before editing:
`UpdateProjectV2FieldInput` HAS `singleSelectOptions` (Status options scriptable) and `AddSubIssueInput` HAS `replaceParent` (idempotent epic linking).

- **B1, H1, H2, M1, M2, M3, M4** — resolved as detailed in the subagent-review Resolution (per-row `type:enhancement` strip; single-parent `epics.json` with #132→Temporal; open-set member filter; concrete TSV generator; field/option-ID cache; 11+#221 reframe; Status options via API).
- **[HIGH] A1** → Task 6 §1 refreshes the open-issue fixture at migration time, capturing the plan-issue (Task 2) + anything opened since; the generator and `verify.sh` run against the live set.
- **[MEDIUM] A2** → `lib.sh::subissue_link` uses `replaceParent:true` (verified present on `AddSubIssueInput`).
- **[LOW] A3** → `03-verify.sh` has `--type-area` (intermediate acceptance) and `--full` (post-delete label-set conformance) modes. **A4** → operator note added: renames propagate to closed issues, so historical `--state closed --label type:*` queries keep working.
- Strategy unchanged (advisor verdict: tactical only). Bash runtime kept for reify/coraly consistency + zero deps; deploy = merge-then-run; `actions/add-to-project` SHA-pin = noted hardening follow-up.
