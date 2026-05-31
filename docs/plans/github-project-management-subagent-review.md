# Clean-Slate Subagent Review — GitHub Project Management Plan

> Reviewer: fresh `general-purpose` subagent, zero prior context, read the plan + spec + ROADMAP + fixture from disk. Saved verbatim. Resolution appended in Step 3c.

**Verdict: REQUEST CHANGES.** One BLOCKER that breaks the acceptance gate on ~22 issues, plus two HIGH correctness issues in the epic layer. The architecture (desired-state reconciler, additive→gate→delete ordering, INV-RENAME) is sound and the empirically-verified-facts section is unusually disciplined — most assertions I spot-checked hold. But several scripted steps are under-specified or contain a real logic bug, and the plan diverges from its own spec in the epic membership without flagging it.

Structural completeness: **Documentation**, **Testing** (justified cargo-N/A), and **Verification** (V1–V13) are all present. ✓

---

## [BLOCKER] B1 — `--remove-label enhancement` is a no-op after the Task 5 rename; produces two `type:*` labels and fails the gate

Task 5 renames the label `enhancement` → `type:enhancement` via `gh label edit` (node-stable, preserves all 46 associations — correct per INV-RENAME). **After that rename, no label named `enhancement` exists.**

Task 6's `02-migrate-issues.sh` then runs, for every issue:

```bash
gh issue edit "$n" --add-label "$type" --add-label "$area" \
  --remove-label enhancement --remove-label design --remove-label performance --remove-label quality
```

For the ~22 issues reclassified to `type:feature`, `--remove-label enhancement` matches nothing (the label was renamed away). Those issues **retain `type:enhancement` (from the rename) AND gain `type:feature`** → two `type:*` labels → `03-verify.sh`'s `.t != 1` check fails for that whole subset. The gate stays red, fencing Tasks 9/10+ (INV-GATE-FENCE), blocking the entire downstream pipeline.

**Fix:** the token to remove for the feature subset is `type:enhancement`, conditional on the row's resolved type (remove `type:enhancement` only when `type == type:feature`). The current blanket remove list cannot express that — the loop needs per-row logic (e.g. `[ "$type" = "type:feature" ] && extra="--remove-label type:enhancement"`).

---

## [HIGH] H1 — `epics.json` must be authored de-duplicated, but the plan only declares "winners" and contradicts the spec

The design spec places **#225 in both Cognitive _and_ Hook221**, and **#226 in both Hook221 _and_ Knowledge**. GitHub sub-issues form a tree (single parent), and Task 12 Step 3's `uniq -d` guard will **refuse to run** if any child appears twice. Task 12 Step 1 names the winners but **never instructs removing #225 from the Cognitive member list and #226 from the Knowledge member list** in the manifest. Unless silently done during authoring, the guard trips.

Worse, the plan asserts "**#132→Cognitive (not Temporal)**" — but the spec places #132 **only** in Temporal, never in Cognitive. Unexplained divergence. **Fix:** author `epics.json` deduplicated from the start; add a reconciliation note mapping each contested issue to its single parent with a one-line rationale, reconciled against the spec.

## [HIGH] H2 — Closed/nonexistent issues in epic membership violate INV-OPEN-ONLY

The spec's Hook221 epic lists **#224** (ROADMAP marks ✅ done; absent from the 96-open fixture), and the Code-Quality epic's `112-130` range includes **#128**, which does not exist in the open set. `07-epics.sh` calls `addSubIssue` on every member → operates on a closed issue (#224) and a nonexistent one (#128), contradicting INV-OPEN-ONLY. **Fix:** filter epic members through the open-issue set before `addSubIssue`, OR explicitly carve an exception in INV-OPEN-ONLY for epic linkage and state closed children are intentionally linked.

---

## [MEDIUM] M1 — `issue-map.tsv` generation is asserted, not specified (the core data artifact has no generator)

The TSV drives the entire migration but **no generator is provided**. Several rules depend on "ROADMAP category"/"ROADMAP placement," and the ROADMAP has no machine-readable per-issue category column — it's prose tables. The `AMBIGUOUS` guard only catches the enhancement-split column; nothing guards a wrong `area:*` or a missed phase. **Fix:** specify the generator (a documented manual procedure with a checked-in jq skeleton that emits one row per fixture issue, area pre-filled where the title `type(area):` hint exists, everything else `AMBIGUOUS`), so the artifact is reproducible.

## [MEDIUM] M2 — `05-projects-fields.sh` never shows field/option creation, yet Task 11 depends on captured IDs

The Task 10 sketch creates the 3 projects but the `createProjectV2Field` calls for Priority/Phase, the option creation, and "capture field+option IDs into state.json" are a trailing comment. Setting a single-select value via `updateProjectV2ItemFieldValue` requires the field node ID and the specific `singleSelectOptionId`; resolving those from names needs an explicit query. **Fix:** write the field-creation mutations + an option-ID resolver (query field → map option name → id → cache), and define the exact `state.json` schema Task 11 reads.

## [MEDIUM] M3 — "12 critical-path issues" is 11 issues + a milestone, and is never enumerated

The ROADMAP's "Shortest path (12 issues)" has **11 numbered issues** (#49,#158,#57,#225,#50,#51,#52,#164,#165,#166,#226 — all open) plus step 12 = "Close #221 umbrella" (no issue number). The plan says "12 critical-path issues" but never lists them. V8/Task 11 verification ("Roadmap = 12 + epics") is off by one. **Fix:** enumerate the 11 explicitly (manifest field/TSV flag); treat #221 as the 12th roadmap item if the count must read 12.

## [MEDIUM] M4 — `In Progress` is already a default Status option; only `In Review` needs adding

Built-in Projects-v2 `Status` ships Todo/In Progress/Done. Re-adding `In Progress` would error or duplicate. Also `createProjectV2` may create projects with no Status field depending on template — handle both cases. **Fix:** query existing Status options first, diff, add only `In Review`.

---

## [LOW] findings

- **L1 — Views-not-writable claim confirmed.** No `createProjectV2View`/`updateProjectV2View`; filter/groupByFields/sortByFields read-only. UI-checklist fallback is correct. `copyProjectV2`/`markProjectV2AsTemplate` exist but plan doesn't lean on them. Validated.
- **L2 — `restore.sh` is sketched, not written.** The recovery path for a botched rename (highest-consequence op) is never exercised. Make it real + smoke-test.
- **L3 — `state.json` vs `projects.lock.json` boundary underspecified.** Task 10 `tee`s the same content to both; one is gitignored "runtime," one committed. Task 11 reads load-bearing field/option IDs from the gitignored `state.json` (a fresh checkout won't have it). Decide reproducible (regenerate) vs captured (commit).
- **L4 — Token handling correct.** Active CLI token is a classic PAT (`gho_`, scopes project,repo). Workflow correctly requires a separate fine-grained `MEMORY_ENGINE_PROJECT_TOKEN`. No secret committed. Good.
- **L5 — Canary selection non-deterministic.** `head -2 | tail -1` depends on TSV sort order; may not be representative.

## Over-engineering assessment

The reconciler + manifest split is **justified, not premature** for 96 issues + destructive deletes + the cross-repo reuse goal. The additive→backup→dry-run→canary→gate→delete ordering and seven invariants are proportionate. Would not cut it. Only friction without payoff: the `state.json`/`lock.json` duplication (L3).

## Bottom line

Fix **B1** (gate-breaking, mechanical), reconcile **H1/H2** (author `epics.json` deduped + open-filtered, reconcile #132 against spec), make **M1/M2** concrete (TSV generator + field/option-ID resolution) before executing. Remaining MEDIUM/LOW are polish. The safety architecture and empirical grounding are strong; defects are in scripted details, not strategy.

## Resolution

All findings addressed in plan v2 (full re-write of `2026-05-31-github-project-management.md`).

- **[BLOCKER] B1** → Task 6 §4: strip `type:enhancement` whenever target type ≠ `type:enhancement` (generalized beyond the review's "only when type:feature" — also fixes former-`enhancement` issues mapped to `type:refactor`/`type:research`, which would otherwise keep a stale 2nd `type:*`). Deterministic canary on #49 added (§5).
- **[HIGH] H1** → `epics.json` authored single-parent up front: #225→#221, #226→#221, **#132→Temporal** (per spec; the draft's "→Cognitive" was an inherited error). Reconciliation rationale documented in Task 12 §1.
- **[HIGH] H2** → `07-epics.sh` filters members through the live open set; #224/#128 (verified CLOSED) are skipped with a log note. INV-OPEN-ONLY kept clean (no carve-out).
- **[MEDIUM] M1** → concrete `gen-issue-map.sh` generator (label/title-derived type+area+phase, else `AMBIGUOUS`) + human review gate. No longer prose.
- **[MEDIUM] M2** → Task 10 creates Priority/Phase fields, sets Status options, and queries+caches `{field_id, option_name→id}` into `state.json` with an explicit schema that Task 11 reads.
- **[MEDIUM] M3** → enumerated the 11 critical-path issues (#49 #158 #57 #225 #50 #51 #52 #164 #165 #166 #226); reframed "12" as "11 + #221 umbrella" everywhere (V7).
- **[MEDIUM] M4** → verified live: `UpdateProjectV2FieldInput` HAS `singleSelectOptions` → Status "In Review" set via API (`updateProjectV2Field`); UI is now only a contingency fallback, not the primary path.
- **[LOW] L1** confirmed (no change). **L2** → real `restore.sh` + smoke-test (Task 4 §3). **L3** → boundary fixed: `projects.lock.json` (numbers+node IDs, committed) vs `state.json` (field/option IDs, gitignored, regenerable). **L4** noted (classic CLI PAT vs fine-grained Action PAT). **L5** → deterministic canary (#49).
