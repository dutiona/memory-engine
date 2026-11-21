# Claude Code hook fixtures

Real hook inputs, queued by this repository's plugin (`plugins/memory-engine/`) under a real `claude -p` and checked
in as the outbox stores them: `<name>/body` is the hook input byte for byte, `<name>/headers` the ADR-0020 §3
envelope the client sent with it. `tests/lifecycle_endpoint.rs` posts them to `POST /lifecycle/events`; the tests
plant every secret, token and sensitive path at run time, so none is stored here.

Captured on 2026-11-21 with Claude Code 2.1.330 (`capture.sh`), every one live, none derived:

| Fixture | Hook | Kind (ADR-0020 §11) | Actor | Outcome |
|---|---|---|---|---|
| `session-start-startup` | SessionStart, source `startup` | `session_start` | main | |
| `session-start-resume` | SessionStart, source `resume` | `session_start` | main | |
| `session-start-compact` | SessionStart, source `compact` | `session_start` | main | |
| `user-prompt-submit` | UserPromptSubmit | `prompt` | main | |
| `post-tool-use-bash` | PostToolUse, `Bash` | `tool_use` | main | `success` |
| `post-tool-use-read` | PostToolUse, `Read` | `tool_use` | main | `success` |
| `post-tool-use-agent` | PostToolUse, `Agent` | `tool_use` | main | `success` |
| `post-tool-use-toolsearch` | PostToolUse, `ToolSearch` | `tool_use` | main | `success` |
| `post-tool-use-mcp-me-recall` | PostToolUse, `mcp__me__recall` (its result starts with the call line) | `tool_use` | main | `success` |
| `post-tool-use-bash-subagent` | PostToolUse, `Bash`, inside a subagent | `tool_use` | subagent | `success` |
| `post-tool-use-failure-read` | PostToolUseFailure, `Read` of a missing file | `tool_use` | main | `failure` |
| `subagent-start` | SubagentStart | `prompt` | subagent | |
| `subagent-stop` | SubagentStop | `stop` | subagent | `completed` |
| `subagent-stop-internal` | SubagentStop of Claude Code's compaction agent (`agent_type` empty) | `stop` | subagent | `completed` |
| `stop` | Stop | `stop` | main | `completed` |
| `stop-failure` | StopFailure (the API base URL was a closed port) | `stop` | main | `api_error` |
| `post-compact` | PostCompact, trigger `manual` | `compact` | main | |
| `session-end` | SessionEnd | `session_end` | main | |

## Capture again

```sh
cargo build -p memory-engine-mcp --bin memory-engine-mcp
ME_TEST_CLAUDE_TOKEN_FILE=<file> sh capture.sh <out>        # <out> outside the repository
python3 -I normalize.py <out> .                              # writes the fixtures here, then checks them
python3 -I normalize.py --check .                            # before every commit that touches them
```

`capture.sh` runs four sessions in a scratch git project under a scratch HOME: a startup turn (a Bash call, a Read,
a Read of a missing file, a subagent, the scratch server's `recall`), a `--resume` of it, `/compact` on it, and a run
against a closed API port. `normalize.py` changes only these: the scratch HOME becomes `/work/fixture-home`, the rest
of the scratch root `/work/fixture` (literal and in Claude Code's dash-encoded project-directory form), and each
`tool_use_id` a fixed placeholder. Its check fails on the scratch root, `$HOME`, the user name, the host name or the
OAuth token; `fixtures_are_public_safe` fails on a local path or any string a redaction rule would change.
