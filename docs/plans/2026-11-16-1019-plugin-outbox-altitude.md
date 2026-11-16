Plan SHA-256: 25aca0c4b3f4a49f44c7b1dd70df79a83915f0f98e2a70e07e85cb4537f44fad

# Altitude checkpoint — 2026-11-16-1019-plugin-outbox

- Plan: `docs/plans/2026-11-16-1019-plugin-outbox.md`, 675 physical lines (`wc -l`), 65,091 bytes (`wc -c`),
  10 top-level implementation tasks (`grep -c '^- \[ \]'`; the plan has no fenced blocks, so no literal is miscounted).
- Literal/data volume: no fenced code blocks (`grep -n '^```'` finds none). Inline literals are commands and exact
  one-line contract texts: the hook handler JSON (D2), the curl request (D8), the two SessionStart lines (D9), the
  marketplace/manifest field values (item 1), and test fixture values. None is a complete canonical file, validator,
  generator or transcript; `hooks.json`, `me-hook` and the tests are described by rule, not reproduced.
- Target files (all new; no existing surface is modified except three docs): `.claude-plugin/marketplace.json`,
  `plugins/memory-engine/{.claude-plugin/plugin.json, hooks/hooks.json, bin/me-hook, tests/run.sh, tests/fake-curl,
  tests/stub_server.py, tests/real-claude.sh, tests/latency.py, README.md}`, plus small edits to `AGENTS.md`,
  `CLAUDE.md`, `GEMINI.md`, `README.md`, and, per open questions 7 and 8, one or two sentences in
  `docs/design/adr/0020-harness-neutral-lifecycle-api.md` in their own commit. Estimated new surface (estimate, not measured): `me-hook` 300–450 lines of sh;
  `run.sh` 600–900 lines; `stub_server.py` ~80; `fake-curl` ~50; `real-claude.sh` ~150; `latency.py` ~60; README
  ~120. Total ≈ 1,400–1,800 lines, so the plan (675 lines) is well under the code it describes.
- Regeneration: revision 11 of the same plan (not a successor); revision 11 only removes two machine-local paths from Operational steps 1 and 2, which the plan-issue archiver refuses; revision 10 `1013f249f1e534125e8142d4c9b29cc44e27ffb427037aa5509d95754942e077` is kept as `.review/last-reviewed-plan.md`, delta `.review/plan-delta-r11.diff`; revision 10 stated the per-machine-endpoint residual (D5 step 2, README) and moves two endpoint cases to item 5; revision 9 `726e670757f1e2c2b98a35aa6706bc73c36eba40da166b6f2691b659fb789ce9`; revision 9 resolved Codex round 8 (the flusher re-reads the endpoint before each request); revision 8 `b73b9a2ceca38acad1438128728dc019168e0360043e013b82d7eae63c044546`; revision 8 resolved Codex round 7 (endpoint removed on a failed update; D7's `.git`-named layouts stated) and the subagent's N8/N9 (real-failure fixtures; Q8 wording); revision 7 `a2aa2b8b9eca156b1a93258862a83af604c67057e0e8d7233a3d046f6fcc0af8`; revision 7 resolved the clean-slate re-review of revision 6 (side duties best effort and the stable copy after publication, park by full target name, D7 case order, G6's script option, stale text); revision 6 `2c708536421d882960be03ef1b89719d75aaa18f30e4b5fd2e8bba3340904d08`; revision 6 reworded open questions 7 and 8; revision 5 `7fc9a85042a055f5a4d5570c1969f0a057e8e6a18bbf8b9a72d027fc3285d34a`. Earlier: revisions 1 `376f2795a13ccaca9861b73d8e2888ec32ee208a64de1d3b4edc693debbbbd7c`, 2 `ac2e10b72a66f56365d2f5ba11b0e24ac071b67b3ea6783e45da393cbe50c8f8`, 3 `b64037f714dbeba57626a45b03bbf21cba3b741f103d4607491d5f954e990284`, 4 `b3475a32f626832e9e987d6a554707ffa00bd67cedf9ca71577d704df5e08dac` ; revision 5 reverts revision 4's counter to ADR-0020 §12's literal seq rule, rewrites D7's Root rule, and moves two ADR tensions to Michaël as Q7 and Q8. Growth since revision 1 (+173 lines, 502 → 675), all from resolving review findings; no new product surface, no literal blocks.
- Required inputs (copied into the review tree's `.review/`):
  - `issue-1019-body.md` — the issue body (acceptance criteria), source
    `~/.local/state/agent-work/2026-11-02-me-kb-claude-code/fleet/artifacts/memory-engine-1019/scratch/issue-1019-body.md`,
    SHA-256 `8092686912ff900cbd4157a75923a473cd54a0948680549d183eb1ecef6500a7`.
  - `planning-context.md` — the sources list, neighbouring-ticket boundaries and probe facts given to the lens
    drafters, source `~/.local/state/agent-work/2026-11-02-me-kb-claude-code/fleet/artifacts/memory-engine-1019/scratch/planning-context.md`,
    SHA-256 `ab74c25f1407e0f08bc9ed24284f4bf4478480a301cc7ce671aef70be843fb12`.
  - Claude Code documentation saved 2026-11-16 (Claude Code 2.1.325), copied to `.review/cc-docs/` from
    `~/.local/state/agent-work/2026-11-02-me-kb-claude-code/fleet/artifacts/memory-engine-1019/scratch/docs/`:
    `hooks.md` `d736130c7223da8a552e24dcde194346cfa7f2adaad53d705bc7c9404278668e`,
    `plugins_manifest-reference.md` `f5d88a52b3bf6ef52b249dc230df025d4915701459a6947375e87740ffc75ffc`,
    `plugin-marketplaces.md` `09ca4f45324f12bfae726823b14c7b3c5bdd8f31261afe74c93691d2d21b4e1c`,
    `plugins_cli-reference.md` `8d24a5282d3f8ffb48c70f094abe0f7dd404de5836b610ce53f996c6363201d9`,
    `plugins_loading.md` `4bb33ca142523286f1ae9d0fea67cd234bd94376041a959108444187e71b45cd`.
    (`planning-context.md` names their scratch paths; inside the review tree read them under `.review/cc-docs/`.)
  - ADR-0020 is at HEAD in the review tree (`docs/design/adr/0020-harness-neutral-lifecycle-api.md`, SHA-256
    `5118e73d627496d3da88312fd96b8d315a35734ae9388c3def62edb8f62e5d79`); not copied.
- Calibration: this is an implementation plan for one new component (a shell client, its manifest and tests) with a
  binding external contract. Its Design section (D1–D9) fixes the decisions the implementer cannot infer (argv,
  on-disk layout, lock protocol, encoding rule, request line) by rule and exact one-liners; it does not duplicate
  ADR-0020's text beyond the verbatim Constraints, which the plan structure requires. No duplicated validator,
  generator, frozen manifest or mutation catalogue. Planning maintenance is small relative to implementation.
- Disposition: **proceed**.
