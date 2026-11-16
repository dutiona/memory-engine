Michaël, **one load-bearing replay issue remains**. Confidence: high.

I re-acknowledge the altitude report’s justified override: the plan remains a reviewable outline of the requested ADR, with its duplication bounded by archival.

Altitude report SHA-256: b415d2fdcee00b728ac18348e24476a5d3bf1be0f404cab64cb7d2c41632d34c

- **[HIGH] §12 — Replay can discard an approved Audience restriction.** The revised fold ignores earlier re-filings and replays only `set_level` and explicit `correct` moves. But §12 also allows an approved re-filing to add a machine restriction to a first-release memory containing a literal path. A later replay can reconstruct the original unrestricted Audience, widening visibility. Preserve deliberate approved Audience changes separately from re-derived placement, and add a case that applies a restriction, changes the registry, and replays without widening it.

The round-3 findings are otherwise addressed by the call-context replay rule, the missing-checkout handling, and scanning each artifact before its publication.

REVIEW COMPLETE
## Resolution

Addressed in the round-5 candidate (plan SHA-256 `6334123a9cc4e7874d701834ed6d853629c8698edecb906913626aea57b24db3`).
- [HIGH] replay can discard an approved Audience restriction → §12: every human-confirmed item of an earlier re-filing (approved machine restriction, confirmed legacy-label mapping, hand filing) replays as recorded; only resolution-materializing items are re-derived; C35 adds the restriction-survives-replay case.
