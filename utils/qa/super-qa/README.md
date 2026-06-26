# super-qa archive

Versioned record of `/super-qa` codebase-audit rounds, kept so future rounds can
**regenerate, diff, and compute stats** over time. Each round is a dated dir under
`runs/`; the canonical machine-readable artifact is `consolidated.json`.

## Layout

```
utils/qa/super-qa/
├── README.md                       # this file
├── scripts/
│   ├── stats.py                    # stats over any run's consolidated.json   (reusable)
│   ├── diff.py                     # heuristic diff between two runs           (reusable)
│   ├── gen_manifest.py             # consolidated.json -> GitHub issue manifest (as-run; dupe-map is run-specific)
│   └── file_issues.sh              # manifest -> GitHub issues (idempotent; reuses utils/scripts/github-pm/lib.sh)
└── runs/
    ├── 2026-03-22/                 # prior round (findings only)
    └── 2026-06-01/                 # this round
        ├── consolidated.json       # CANONICAL: every finding, full fields  <- diff/stats target
        ├── findings.md             # Phase 1 + narrative + verified critical-path
        ├── phase2-detail.md        # all highs, category/module buckets, refactoring backlog
        ├── issue_manifest.json     # exact issue specs filed (bodies, labels, parent links, dupe map)
        ├── issue_state.json        # manifest key -> GitHub issue #  (finding -> issue traceability)
        ├── raw-workflow-output.json.gz  # raw 577 pre-dedup agent output (full reproducibility)
        └── stats.md                # generated snapshot of this run's stats
```

## Re-run (every 3–5 months, or after major change)

1. Run the skill: `/super-qa` (full 3-phase) — language detection, hotspot ranking,
   85-agent deep-dive, consolidation. It produces a fresh `consolidated.json`.
2. Drop it under `runs/<YYYY-MM-DD>/consolidated.json` and the human reports beside it.
3. Stats + diff against the previous round (below).
4. File issues (optional): regenerate the manifest and run the filer.

## Stats

```bash
python3 scripts/stats.py runs/2026-06-01/consolidated.json --out runs/2026-06-01/stats.md
```

Severity × auto-fixable, category, module, area tables + provenance (verified /
security-fallback / auto-fixable counts).

## Diff (longitudinal)

```bash
python3 scripts/diff.py runs/2026-03-22/consolidated.json runs/2026-06-01/consolidated.json --out delta.md
```

Reports **added / removed(fixed) / persisted / severity-changed**. Matching is
**fuzzy** (same source file + title-token Jaccard) because finding IDs and line
numbers are not stable across runs — treat counts as directional. `--jaccard`
tunes strictness. (The 2026-03-22 round predates `consolidated.json`; it only has
`findings.md`, so the first machine diff is possible from 2026-06-01 onward.)

## Filing pipeline (how the GitHub issue tree was built)

`gen_manifest.py` turns `consolidated.json` into `issue_manifest.json`:

- one `area:*` epic per area; a master epic parenting all area-epics + the auto-fix
  epic + the prior-run epic; per-severity auto-fix issues; per-severity "Index"
  observer issues; one issue per non-autofixable blocker/critical/high/medium;
  low/info grouped per area.
- a **curated dupe map** links findings already filed by a prior round to their
  existing issue instead of re-filing — this map is **run-specific**, re-curate it
  each round (the auto-heuristic under-matches; manual review of candidates wins).

`file_issues.sh` (idempotent, resumable; `DRY=1` to preview) creates/links via the
repo's own `utils/scripts/github-pm/lib.sh` (`subissue_link` = native `addSubIssue`,
query-first so reruns are no-ops). State lives in `issue_state.json`
(`key -> issue#`); override inputs with `RUN_DIR` / `MANIFEST` / `STATE` env.
Phases: `labels | epics | findings | dupes | observers | relabel | all`.

## Labelling scheme

Every issue: `super-qa` + `severity:{blocker|critical|high|medium|low|info}` +
`area:*` + `type:{bug|security|test|docs|refactor|chore|epic}`, plus
`super-qa:auto-fix` and/or `super-qa:security-fallback` where applicable.
`severity:blocker` was added to the repo label set in this round (the scheme
otherwise tops out at critical).

## Methodology (what makes the output trustworthy)

An 85-agent fan-out surfaces _candidates_ cheaply but over-classifies severity and
occasionally hallucinates cross-refs. The discipline that turns candidates into
findings:

1. **Source-verify the top tier** (blocker/critical + consequential highs) against
   the actual code — stronger than multi-model debate, which is skipped when the
   compiler/source already settles a fact.
2. **Caveat the long tail** — every non-source-verified issue body carries a
   review-before-action header; verified ones carry a ✅ note.
3. **Dedupe against history** before filing (curated, not keyword).
4. **File idempotently** so reruns never duplicate.
