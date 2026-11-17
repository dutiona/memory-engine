# Advisor capability unavailable

Altitude report SHA-256: 3e4141ad1e2d9f0a2adef6d689d0d44418b69e0a4fbbf17527c20d38e71b2b1a

(Controller metadata on the unavailable path: the digest of the final report the fallback reviewers acknowledged.
Round 1 used `458a1629…`; every later report was re-acknowledged by Codex and the clean-slate reviewer.)

- Reason: `advisor()` is not exposed in this session (the fleet worker's tool list has no advisor tool).
- Gate: Full. Its advisor-less form runs instead: Codex (headless, persistent thread) + agy (resume, persistent
  conversation) + one clean-slate subagent review (3b, `2026-11-17-deferred-embedding-subagent-review.md`).
- No finding below is attributed to an advisor. The findings each fallback reviewer produced are recorded in
  their own artifacts and in the review state directory, attributed to that reviewer.

## Resolution

No advisor findings exist to resolve (capability unavailable). The fallback reviewers' round-1 findings were
resolved in the plan revision of round 2: the clean-slate subagent's in its own artifact's `## Resolution`,
Codex's (watermark bound to the snapshot, a collision-free sidecar fingerprint, the `fetch_candidate_embeddings`
guard; the shared-limit finding answered by the owner's settled ruling, Open question 12) in the review state.
agy did not run: gemini-3.8-flash-high and claude-opus-5-5-high both answered 429 (quota), recorded as a
degradation. Review closed after round 7 with LGTM from Codex and from the clean-slate reviewer.
