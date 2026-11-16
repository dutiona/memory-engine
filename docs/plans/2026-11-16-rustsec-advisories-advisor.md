# Advisor capability unavailable

Reason: `advisor()` is not exposed to this session (the fleet worker's tool set has no advisor tool), so Step 3a
could not run. The selected gate is **Full**; its advisor-less form ran instead: Codex (headless, persistent
thread) + agy (resume) + the clean-slate subagent (Step 3b). No finding below is attributed to an advisor.

Altitude report SHA-256: 7ad9572577d491fe9392eaab08d656c8bff66ae410ed610070d71aa47dd1c8ae

(Controller metadata: the digest of the final altitude report every fallback reviewer acknowledged in round 4, for
plan SHA-256 cb4ed9008bdb0eb05feb7455efe513c9a6162ad72baee4b21a88164f28b3989b.)

## Findings the fallback reviewers produced (full text in the sibling artifacts)

Codex (`2026-11-16-rustsec-advisories-external-codex.md`):
- Round 1: [HIGH] READY with the clippy red row; [HIGH] full matrix "after every change"; [MEDIUM] item 6's clean
  status cannot pass with `deny.toml` uncommitted.
- Round 2: [HIGH] cadence re-raised; [MEDIUM] deny's expected result after item 5; [MEDIUM] item 10's green grep
  matches its own replacement.
- Round 3: LGTM.
- Round 4 (Delivery 1 pointer, after the archiver refused the self-path): LGTM.

agy (`2026-11-16-rustsec-advisories-external-agy.md`): LGTM in rounds 1, 2, 3 and 4 (round 2 judged the cadence
sufficient).

Subagent (`2026-11-16-rustsec-advisories-subagent-review.md`): round 1, 3 MEDIUM + 6 LOW; round 2, 1 MEDIUM + 5 LOW;
round 3, LGTM; round 4, LGTM.

## Resolution

- Codex [HIGH] READY with the clippy red → declined with rationale: the orchestrator brief explicitly allows READY
  with the one attributed #1032 row ("Your own gate run must be green except for the other ticket's failure ... Say
  in the PR which red row belongs to the sibling ticket, with its output"), and clippy already fails on main, so
  "every other matrix command still passes" cannot cover it. Written into Constraints and Delivery 4. Codex accepted
  this in round 2.
- Codex [HIGH] full matrix after every change (rounds 1 and 2) → partly accepted, then arbitrated: the matrix runs
  after items 5 and 7, at the final HEAD, after every review-fix set and every rebase; the plan states openly that
  the PR is squash-merged, so no intermediate commit reaches `main` alone and per-commit evidence has no consumer.
  agy and the subagent judged this sound; Codex acknowledged it in round 3 ("no new evidence warrants reopening
  it"). Michaël's plan approval covers this reading of AGENTS.md.
- Codex [MEDIUM] item 6 clean status → before/after `git status --porcelain` snapshot comparison.
- Codex [MEDIUM] deny after item 5 → Verification states the intermediate expectation.
- Codex [MEDIUM] item 10 grep → reworded replacement (also the subagent's round-2 MEDIUM).
- Subagent findings → resolved as listed in its artifact's `## Resolution`.

Degradations: advisor unavailable (above); Codex round 1 on gpt-6.1-terra rather than the preferred gpt-6.1-sol
(the preference arrived after round 1; rounds 2-3 ran on gpt-6.1-sol through a private `-m` shim reached via
`codex-headless-round.sh`'s `CODEX_RESOLVE_DIRS` seam, since the script has no model flag).
