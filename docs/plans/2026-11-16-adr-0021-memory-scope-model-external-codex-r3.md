Michaël, **the delta resolves most round-2 findings, but two replay and path cases still need rules**. Confidence: high.

I re-acknowledge the altitude report’s justified override: the expanded outline remains the assigned design-review artifact, and the plan is to be archived after implementation.

Altitude report SHA-256: 2808691b58d1a90a1629368905d497e562f32eb06c1902861c2d3970d449cd85

- **[HIGH] §12 — Replaying `set_level(project)` can move a memory to the wrong project.** ADR-0020 defines `project` as the *calling session’s* project. A user-level memory originally captured in P can be moved to Q by a caller in Q. The new replay rule records the intent as “the memory’s resolved project,” which can put it back in P. Record and re-resolve the calling context or the actual destination of each level change; test this sequence and replay after a Locator correction.

- **[HIGH] §9, C27 — Missing checkout facts can still silently remap a worktree path.** When the client does not send the checkout top level, the main tree becomes `${PROJECT_ROOT}`. C27 consequently stores the linked-worktree path as `${PROJECT_ROOT}/.worktrees/w/src/a.rs`. On recall from a different checkout, that expands beneath the reader’s checkout, which may contain a different worktree or none. Treat the checkout identity as unknown in this case and preserve or flag the ambiguous path; add a cross-checkout recall to C27.

- **[MEDIUM] Verification — P1 requires artifacts before they exist.** The new instruction runs the scan on both the PR body and KB comment “before each push or post,” but the comment is prepared in item 9, after the PR and review. Because missing files return an error and any nonzero result blocks the action, an earlier push can be blocked by an artifact not yet created. Scan each artifact before its own publication, while scanning the branch and commit messages before each push.

The local-remote fallback, atomic merge, literal-token escaping, and saved commit-message scan address the other round-2 findings at plan level. The revised decision section gives Michaël the choice and its consequences.

REVIEW COMPLETE
## Resolution

Addressed in the round-4 candidate (plan SHA-256 `6135c0c810e39fd720fbe4d45d686525920ab46cbff03f80f4c8223eecd37a33`).
- [HIGH] replaying `set_level(project)` → §12: its target is re-resolved from its own call record's Capture context (the calling session's project); re-filings are re-derived, never replayed; C35 adds P → user (from P) → project (from Q) → Q, and a re-filing followed by a `correct`.
- [HIGH] missing checkout fact → §9: no checkout fact, no `${PROJECT_ROOT}` (paths fall to `${DEV_ROOT}`); a reader without the fact gets the token literal and flagged; C27.
- [MEDIUM] P1 sequencing → branch files and commit messages before each push; the PR body before item 6; the KB comment before item 9.
