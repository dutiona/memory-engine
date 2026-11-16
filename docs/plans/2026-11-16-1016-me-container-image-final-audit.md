# Final audit (post-review): #1016 container image

- Scope: post-review
- Approved plan: `docs/plans/2026-11-16-1016-me-container-image.md`, sha256
  `30e4adfb3868589d28e979ad7c2c617fc61c975db0789071554f53a6a59434d1`
- Audited head: `d5d25e4` (the head that the Full review gate passed in round 2: Codex, Antigravity and Claude
  all LGTM)

| # | Plan item | Status | Commits |
|---|---|---|---|
| 1 | Track `Cargo.lock` | Implemented | `9143ae1` |
| 2 | Gate the lock; update the qa-sweep lock policy | Implemented, extended in review | `b78a6c4`, `da5bb5d` |
| 3 | Boy-scout: duplicate `.gitignore` entries | Implemented, corrected in review | `1f5037b`, `d5d25e4` |
| 4 | Allowlist `.containerignore` | Implemented, tightened in review | `0698d6c`, `c74ad9d` |
| 5 | Multi-stage `Containerfile` | Implemented | `f61d7e0`, `816917a` (header comment) |
| 6 | Boy-scout: nonexistent `async` feature in the install docs | Dropped: already fixed on `main` by #1043 | — |
| 7 | Server image docs | Implemented, extended in review | `8b38fa8`, `816917a` |
| + | CI's default-features build runs `--locked`; the parity test covers the qa-sweep matrix | Added in review | `da5bb5d` |

Counts: 6 Implemented, 0 Modified, 1 Dropped, 0 Incomplete, 1 Added. Divergence: 29% (2 of 7; threshold 30%).
Below the threshold, so no successor plan is needed.

The Added item changes one approved decision. The plan's "Out of scope" list kept `--locked` out of
`.github/workflows/ci.yml`, on the grounds that the local matrix line is the gate. Review showed a hole in that
reasoning: the local line checks the working tree, and an editor's cargo run or an earlier command may already
have rewritten the lock there. A dispatched CI run is then the only check that sees the committed tree. The change
adds no CI dependency (spec D5 holds) and strengthens the approved intent, which is to gate the committed lock.
It does not change that intent, so it is recorded here rather than as a successor plan.

Review-driven variations within approved items:

- Item 2: the qa-sweep skill's copy of the matrix also gets the lock line. The unlocked
  `cargo build --workspace` line is removed from all four matrices, because the locked line now plays its role.
- Item 3: `.gitignore` keeps `/target`, not `/target/`, so a `target` symlink stays ignored.
- Item 4: `**/.worktrees` also excludes a worktree nested under `crates/`. The context probe plants one.
- Items 5 and 7: the build command is `podman build --pull=newer`. Podman's default policy is `missing`, checked
  with `podman build --help`, and it would freeze the floating bases at the first pull. The docs also say Docker
  ignores `.containerignore`.

Rebutted in review: the plan's Resolution cites the Lean review file, which is not in the tree. The approved plan
is digest-bound. The review stays private because it quotes deployment specifics that this public repository
must not carry, and each of its findings is summarized in the plan's Resolution.

Evidence at `d5d25e4`:

- The full local gate matrix passed (rc 0 on every row): 1998 tests passed, 0 failed; `cargo deny check` reported
  `advisories ok, bans ok, licenses ok, sources ok`; both 1.89 builds passed.
- `podman build --pull=newer --no-cache` from a clean tag checkout: rc 0, both `--version`s answer, UID/GID
  10001, the read-only `VACUUM INTO` path gives `ok` and `1`, and the context probe reports `CLEAN`.
