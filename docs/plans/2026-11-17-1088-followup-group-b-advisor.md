# Advisor capability unavailable

- Reason: `advisor()` is not exposed in this session (a fleet-worker subagent: the tool list carries no advisor
  built-in).
- Gate: Full. Its advisor-less form is Codex + agy + one clean-slate subagent (debate-defaults), which is what this
  run dispatched: the clean-slate subagent (Step 3b, `-subagent-review.md`) and the persistent external reviewers of
  Step 4 (Codex headless, agy `gemini-3.8-flash-high`). Any external voice that fails is recorded as a degradation
  in the Resolution below and in the worker report.
- No advisor findings exist. Findings below are attributed to the reviewer that produced them.

Altitude report SHA-256: 017b6e7018fdd3b0e259765b53983f6493dcc8e3db55a8a612bacd824ea13080

## Resolution

Advisor unavailable (see above); no advisor findings to resolve. External voices that actually ran, round 1, on plan
7f1e3bae…a2d9e / altitude 017b6e70…3080:

- Codex (gate-named): degraded. `codex exec` refused at turn start: "You've hit your usage limit … try again at 12:20 PM".
- agy `gemini-3.8-flash-high` (gate-named): degraded. RESOURCE_EXHAUSTED (429) "Individual quota reached … Resets in 71h35m".
- Replacements per the lane rules for a Codex outage: OpenCode `opencode/hy4-flash-free` (best effort) and Vibe
  (`mistral-large-4`, read-only tool set `read_file` + `grep`, review copy verified unchanged). Both LGTM with refinements.
  Their findings and dispositions (applied in revision 2):
  - [OpenCode MEDIUM] probe predicate under-specified → the rule now states it exactly (stepped probe, `SqliteFailure`
    with extended code 1; anything else propagates the original error).
  - [OpenCode MEDIUM] corrupt-index injection unverified on bundled SQLite → the test checks its own injection first
    (raw MATCH fails with an extended code other than 1).
  - [OpenCode LOW] assert each malformed string's raw code is 1 → added to the characterization test.
  - [OpenCode LOW] table header/separator rows → skipped explicitly. [OpenCode LOW] derived cells verbatim → stated.
  - [OpenCode LOW] `.containerignore` vs `git archive` → accepted as "Surfaced, not done": podman applies
    `.containerignore` inside the tar context (I think; not measured).
  - [OpenCode LOW] "No source names me_backend_postgres" too broad → scoped to me-test-support.
  - [Vibe LOW] unused-dependency warning may not fire → measured on cargo 1.100.0; item 6 says so.
  - [Vibe LOW] B2 targets `ScopeStore::insert`'s doc → applied. [Vibe LOW] probe is stepped / no-rows means valid → in the rule.
  - [Vibe LOW] fuzz `expect`s wording → aligned. [Vibe LOW] `lexical_malformed_query_is_empty_not_error` location →
    named (sqlite/search_index.rs; the crate-wide test run selects it).
  - [Vibe LOW] duplicated `### Bootstrap` in api.md → "Surfaced, not done" (follow-up), with the other api.md drift.
  - [Vibe LOW] counts only the implementer can measure → the implementer's evidence, as the plan already states.

Altitude report SHA-256: 0267ae88f8b54011fb88fc3cd56af47cc26bfc81fbbad31e6f1c50c0017b9b4a

## Rounds 2 and 3 (controller metadata; advisor still unavailable)

- Round 2 on revision 2 (2e4e2b39…): Vibe LGTM with LOWs; OpenCode's first attempt returned nothing (its globs left
  the working directory and were auto-rejected); its one allowed retry reviewed revision 3 against revision 1 and gave
  LGTM, no findings. Clean-slate subagent LGTM with four LOWs, all applied in revision 3.
- Round 3 on revision 3 (0925754c…7313): subagent LGTM (two cosmetic LOWs), Vibe LGTM (one LOW), OpenCode LGTM
  (retry above). Arbitration of the remaining LOWs is in the subagent review's "Resolution (round 3)". Converged.
- Actual external roster: opencode (`opencode/hy4-flash-free`), vibe (`mistral-large-4`). Degraded: codex (usage
  limit until 12:20Z), agy (`gemini-3.8-flash-high` 429, ~71h).

Altitude report SHA-256: 1c7769ef9d21e1123af14973e8eb5ac9ea8903d40bf52e89857093d42897b260
