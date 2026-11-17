# Advisor capability unavailable

Altitude report SHA-256: aee3488a8f2ed9a949af8aca4d2341659415012ea57df9c1d3c36cc92a171595

Controller metadata, not an advisor verdict. `advisor()` is not exposed to this fleet worker's harness, so Step 3a could
not run. The Full gate drops to its advisor-less form (Codex + agy + clean-slate subagent), and two of those legs were
degraded in turn:

- agy (`gemini-3.8-flash-high`): RESOURCE_EXHAUSTED 429 at round 1 (individual quota, resets in about 76 h); the round's
  receipt is `incomplete` with cleanup verified. Per the fleet rule of 2026-11-17, Codex and the Opus clean-slate
  subagent stay the cross-lineage voices.
- Codex (headless; the config default `gpt-6.1-terra`, accepted by Michaël on 2026-11-17 since the round script cannot
  select `gpt-6.1-sol`): rounds 1 and 2 completed; round 3 hit the account's usage limit. Per the fleet rule, Vibe
  (Mistral Large 4) took over for revisions 3 to 6. OpenCode was not usable (its wrapper reviews a dotfiles commit only).

The reviewers that actually ran, and where their findings are:

| Reviewer | Rounds | Final verdict | Artifact |
|---|---|---|---|
| Clean-slate subagent (Claude Opus 5.5) | 1 to 5 | LGTM on revision 6 | `2026-11-17-1022-project-key-subagent-review.md` |
| Codex | 1, 2 | HIGH/MEDIUM findings, all addressed | `2026-11-17-1022-project-key-external-codex.md` |
| Vibe (Mistral Large 4) | 1 to 3 (on revisions 3, 5, 6) | LGTM on revision 6 | `2026-11-17-1022-project-key-external-vibe.md` |

No finding here is attributed to an advisor.

## Resolution

No advisor findings exist to resolve. Every finding of the reviewers above is resolved in its own artifact's
`## Resolution`; the final revision (plan SHA-256 `dd796ce3cabe0e3b24f7bc1bcfae4989b96dbad6b29417f2b7e1d19181628400`)
holds LGTM from the clean-slate subagent and from Vibe, both bound to the altitude report digest above. Degraded:
Step 3a → advisor-less Full gate (advisor() not exposed); agy → none (quota 429); Codex round 3 onward → Vibe (usage
limit).
