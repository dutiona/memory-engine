Michaël, **the delta resolves the by-ID authority gap and improves the subset proof, but load-bearing issues remain**. Confidence: high.

I re-acknowledge the altitude report’s justified override: the expanded outline remains the assigned review artifact, with duplication bounded by plan archival.

Altitude report SHA-256: f7aabe4bd08a6e4b34de2eb16a17c2712e6278c1f076a3fed116dd629a5d39e7

- **[HIGH] §8, §11 — Rejected remote inputs are confused with rejected keys.** ADR-0020 ignores a local-path or `file:` remote, then falls back to the dev-relative project root. §8 instead lists such a remote among inputs that stay “no Locator.” A session with a local remote and a valid dev-relative root could therefore move from its first-release project to its bucket. Distinguish an ignored remote from a rejected final key and add that fallback to C34.

- **[HIGH] §8, §12 — A merged project can become unreachable before its memories move.** `merge` redirects the absorbed ID for resolution immediately, while its memories remain in the absorbed Home until a separately approved re-filing. §5 resolves calls to the survivor and reads only that Home plus `user`, so a session formerly in the absorbed project loses access during the interval. Define whether redirect and filing are atomic, or how the absorbed Home remains reachable without widening access; test reads before and after approval in C23.

- **[HIGH] §9 — The revised path rule still silently changes references.** C27 explicitly maps paths from both the main tree and a linked worktree to the same token. Those files can differ because the checkouts are on different branches or contain different edits. Calling the result “the same repository path” describes the substitution but does not preserve which checkout the author referenced. Preserve that distinction or leave an ambiguous reference literal and flagged.

- **[HIGH] §12 — Replay can discard an intentional `set_level`.** The procedure re-resolves capture context into a candidate filing, with precedence specified only for explicit moves from `correct`. A fact deliberately moved from project to user by `set_level` can be re-filed back to the project on replay. Define the ordering and precedence of all logged Filing decisions, including level changes, and add an idempotent replay case.

- **[MEDIUM] §9 — Literal-token escaping is not unambiguous.** A user-authored `${HOME}` is stored as `$${HOME}`, but the plan does not define escaping for an incoming literal `$${HOME}`. Without an invertible encoding or separate token provenance, those inputs can collide or be decoded incorrectly. Add round-trip cases for both strings and repeated dollar signs.

- **[MEDIUM] Verification — The revised privacy scan still has an execution gap.** The instructions say to run `privacy` on `git log --format=%B origin/main..HEAD`, but the function accepts filenames, not a command or standard-input specification. Passing that expression as arguments makes `grep` try to open files named `git`, `log`, and so on. Specify capturing the log to a scratch file before scanning it, and require a nonzero result to block publication.

The decision section now presents Michaël with the automatic-alias conflict and its consequences; that choice remains for him. The compatibility boundary, role/session preview, by-ID case, and inclusion of the fuzz gate address the other round-1 points at plan level.

REVIEW COMPLETE
## Resolution

Addressed in the round-3 candidate (plan SHA-256 `dbaa23f25a3040aa8e228d5adc9d0858e0991d36adcbeeabe5c2b53e8f9ff612`).
- [HIGH] rejected remote vs rejected key → §8 keeps ADR-0020's key order: a local or `file:` remote is ignored as an input and the next rule applies; only a final rejected key is "no Locator"; C22.
- [HIGH] merged project unreachable before its memories move → §8: redirect and Filing decisions apply in one approved transaction; nothing changes before it; C23.
- [HIGH] path rule changes references → §9: `${PROJECT_ROOT}` has exactly one prefix per session (its own checkout, else ADR-0020's project root); a path into another checkout is never remapped (falls to `${DEV_ROOT}`, `${HOME}` or "other"); C27.
- [HIGH] replay discards `set_level` → §12: a memory's filing is a fold of its re-resolved initial placement and every later logged Filing decision, each replayed as its recorded intent; C35.
- [MEDIUM] escaping not invertible → §9: a run of n `$` before a token name is stored as n + 1, a server token has one; C30 covers `${HOME}` and `$${HOME}`.
- [MEDIUM] P1 on `git log` → the commit messages are saved to a file first; any nonzero P1 result blocks the push or post.
