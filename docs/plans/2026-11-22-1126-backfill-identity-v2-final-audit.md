# Final audit: follow-up group A1 (#1126), plan v2

- Scope: post-review.
- Approved plan: `docs/plans/2026-11-22-1126-backfill-identity-v2.md`, SHA-256
  `4cff7c9059b9a3ab9c4fcd295f3fdfe6b4e657b0404b8b50432921c5220131e0` (approved on #1134).
- Audited head: `94c5430a8b0421c44a78469c4991447957639753`, the head both reviewers passed in round 2 (PR #1141).
- Pre-review audit: `docs/plans/2026-11-22-1126-backfill-identity-v2-implementation-audit.md` (at `607eb3d`), unchanged.

## Items

Items 0-16 stand as the pre-review audit records them (Implemented). Item 17 (the PR) is #1141. Item 18 (review and
`/finish-pr`) is this audit's subject: review converged in two rounds, `/finish-pr` archives the plan on #1134.

## Added since the pre-review audit

| Commit | Kind | Why |
|---|---|---|
| `a41700c` test(cli) | Added (boy-scout, item 6's check) | The plan's grep for `embedding_spaces\|embedding_meta` in both binaries' `src` was not empty: a test seeded the registry by raw SQL and two comments named the tables. The test now records an identity through `add_fact_precomputed`. |
| `14360b1` fix(deps) | Added (unplanned) | `cargo deny check` failed on main once RUSTSEC-2026-0592 was published (`url` 2.5.8). `url` 2.5.9, a patch release; Cargo.lock changed, so the image build ran. |
| `0f5668d` fix(restore) | Added (review fix, Claude round 1, LOW 1) | The final open of `restore_sqlite` goes through the same cleanup as every earlier refusal; a new test pins the path. |
| `94c5430` docs(inspect) | Added (review fix, Claude round 1, LOW 2) | `stream_snapshot`'s doc comment back on `stream_snapshot`. |

## Counts

Implemented 17 (items 0-17), Modified 0, Dropped 0, Incomplete 0, Added 4 (above), of 19 plan items. Divergence
(Modified + Dropped + Added) is 4 of 19: 21%, under the 30% threshold. No item changed approved intent: the four
additions are a boy-scout completion of item 6's own check, a dependency advisory fix, and two review fixes.

## Review

- Roster: agy `gemini-4-hydrogen-medium` (rounds 1 and 2, one conversation) and, in Codex's slot, a fresh Claude
  Sonnet subagent at medium effort (rounds 1 and 2, resumed). Degraded: Codex (out of quota until about 2026-11-25).
- Round 1 at `14360b1`: agy LGTM, every acceptance PASS; Claude 7 LOW: 2 fixed, 5 answered on the PR.
- Round 2 at `94c5430`: both LGTM the delta and accept the answers; one new LOW (a test asserts the refusal text)
  answered: the text is what proves the final open, the path under test, refused.
- agy's first round-2 run completed its review but its cleanup was unproven; after a recorded quiescence check it was
  re-run in a fresh round directory, which completed with verified cleanup.
