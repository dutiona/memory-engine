# Advisor capability unavailable

Altitude report SHA-256: 73a248717f4c6ae5198da8f98d7400b45ce011d191f87d2aca1274656839e944

(Controller metadata on the unavailable path: the digest of the successor's report under review in round 2.)

- Reason: `advisor()` is not exposed in this session (the fleet worker's tool list has no advisor tool).
- Gate: Full, advisor-less form. Codex's weekly quota is exhausted (resets about 2026-11-18 12:20Z), so the
  external voices are OpenCode (GLM 5.3, `opencode run -m opencode-go/glm-5.3`) and Vibe (Mistral Large 4,
  headless), plus the clean-slate review (`2026-11-17-deferred-embedding-v2-subagent-review.md`): agy
  `claude-opus-5-5-high` answered 429, so a local Opus subagent ran it.
- No finding is attributed to an advisor; each reviewer's findings are in its own artifact.

## Resolution

No advisor findings exist to resolve. The fallback reviewers' findings are resolved in their own artifacts and
in the successor plan.
