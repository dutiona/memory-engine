# Implementation audit, pre-review: #1028 plan

- Scope: pre-review.
- Approved plan: `docs/plans/2026-11-26-plugin-connect-http-mcp.md`, SHA-256
  `091d579025096c15228173b45f49f4aa7e800430b862febeba8cb83ba06b1620`, approved on its plan issue #1165.
- Implementation audited: the branch `feat/1028-plugin-connect` at `bee0241` (rebased on origin/main `d4cf7c6`).

| Item | Status | Where |
|---|---|---|
| 0. Worktree, plan issue, approval | Implemented | plan issue #1165; approval marker by the owner |
| 1. A local ME and KB for the real run | Modified | folded into item 6 (`tests/real_mcp.py`, `tests/stub_embedder.py`) and `bee0241` |
| 2. Channel-neutral fact derivation | Implemented | `refactor(plugin): make the project-fact derivation channel-neutral` |
| 3. Absolute directories only; physical non-git root | Implemented | `fix(plugin): send a non-git project root as its physical path` |
| 4. `me-hook headers me\|kb` | Implemented | `feat(plugin): add me-hook headers, the helper of the MCP channel` |
| 5. Declare the servers | Implemented | `feat(plugin): declare the me and kb HTTP MCP servers` |
| 6. The real run through the plugin | Implemented | `test(plugin): run claude -p against a local ME and KB …` |
| 7. ADR-0020 and the reference | Implemented | `docs(adr): require a physical dev root and an absolute helper line …` |
| 8. The memory skill | Implemented | `feat(plugin): add the memory skill` |
| 9. README and AGENTS.md | Implemented | `docs(plugin): document the MCP servers, the skill, the upgrade and the rollback` |
| 10. Release 0.2.0 | Implemented | `chore(plugin): release 0.2.0` |
| 11. PR, review, merge | In progress | the PR, `/super-review` Full, `/finish-pr` |
| Added: ADR-0021 note | Added | `docs(adr): record the self-asserted project key as an accepted consequence (ADR-0021)`, per the owner's ruling at approval ("Accept, record it") |

Counts: 10 Implemented, 1 Modified, 0 Dropped, 0 Incomplete (item 11 is the remaining lifecycle), 1 Added.
Divergence: (Modified 1 + Dropped 0 + Added 1) / 12 = 17%, under the 30% boundary. Approved intent is unchanged.

## Item 1, modified (intent preserved)

- The bring-up lives in a Python driver, `tests/real_mcp.py`, which `real-claude.sh` calls, instead of shell functions
  inside `real-claude.sh`: it owns the server processes across restarts and rotations, the streaming session and the
  token draws. Same seams, same checks.
- The one-off, uncommitted `claude -p --mcp-config` handshake probe was not run: the host's command guard refuses an
  ad-hoc command that hands the Claude test-token file to an interpreter, and the committed harness is the sanctioned
  path for that token. Its two questions are answered by phase M instead: the init events show both servers connected
  with their seven tools on Claude Code 2.1.332 (the `server/discover` then `initialize` handshake works against both
  stateless servers), and the info line records that, launched from a logical path through a symlink with `PWD` set to
  it, the hooks saw the physical project directory. So `CLAUDE_PROJECT_DIR` is physical, item 3 moves no existing hook
  key for a launch through a symlink, and the README needs no conditional upgrade note (Decision 1's contingency).
- The four bring-up checks: "401 without a token" and "a rotated-out token gets 401, the current one does not" are the
  `auth` check (`bee0241`); "tools/list is exactly …" is the `tools` check on the init events; "a restart keeps the
  ports and the stores" is the `restart` check (recall and search find what was filed before the restart).
- Order: items 2 to 5 landed before the real run, because the workspace build it needs was running; the real run still
  came before the ADR, the skill and the docs, as the plan's risk order wants.

## Evidence at this head (pre-review)

- `dash` and `bash plugins/memory-engine/tests/run.sh`: `# 85 ok, 0 not ok` each.
- `claude plugin validate --strict plugins/memory-engine && claude plugin validate --strict .`: both pass.
- `real-claude.sh`, all phases (before `bee0241`): `# 23 checks, 0 not ok`.
- Mutations shown red: a hostile `git status` call in `facts` (`hostile_git_config`); an unquoted here-document
  delimiter (`helper_command`); the `me` helper fed `/dev/null` (phase M: key, restart, cd and resume checks red).
- The full AGENTS.md matrix runs at the final head; its results go to the PR body.
- `latency.py` could not be judged: the host's load average was about 47 during the run, and the unchanged baseline
  measured p95 78.9 ms against the 50 ms budget in the same minute. It is re-run in a quieter moment before READY.
