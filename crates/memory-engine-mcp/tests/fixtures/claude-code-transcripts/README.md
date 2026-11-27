# Claude Code transcript fixtures

Synthetic Claude Code transcripts for `POST /backfill/uploads` (#1026), written by hand in the shapes the reader
reads by field name (Claude Code 2.1.296 and 2.1.310): one project directory, `-work-fixture-p`, as Claude Code
encodes `/work/fixture/p`, holding two session files and one subagent file. The text is synthetic, the `uuid`s are random
v4, the message and tool IDs are placeholders, and no line holds a secret: the tests plant every secret, token,
sensitive path and denylist literal at run time. `tests/fixtures/claude-code-memory/` holds the native memory files
(`MEMORY.md`, one file per memory type, one with a file-name date, one with a frontmatter date, two with one body, and
one frontmatter-only stub). `fixtures_are_public_safe` fails on a local path or any string a redaction rule would
change.

## `-work-fixture-p/52c6c7b7-c890-43ec-81a5-a1cf99d3cc84.jsonl`

Session `52c6c7b7-…`, main agent. "Call" marks an `assistant` `tool_use` block, whose record is made at its result.

| Line | Entry | Rule |
|---|---|---|
| 1 | `file-history-snapshot` | skipped: `file-history-snapshot` |
| 2 | `user`, string content, 2.1.296 | `prompt` |
| 3 | `assistant` `msg_fixture01`, a thinking block | skipped: `thinking` |
| 4 | `assistant` `msg_fixture01`, text, no `stop_reason` | nothing: its message ends in `tool_use` |
| 5 | `assistant` `msg_fixture01`, call `Bash` | the call of line 6 |
| 6 | `tool_result` of line 5 | `tool_use` `Bash`, `success` |
| 7 | call `Read` of a missing file | the call of line 8 |
| 8 | `tool_result` with `is_error`, a string `toolUseResult` | `tool_use` `Read`, `failure` |
| 9 | call `Read` of `.env` | the call of line 10 |
| 10 | `tool_result` of line 9 | `tool_use` `Read`, a sensitive-path stub |
| 11 | call `mcp__me__recall` | the call of line 12 |
| 12 | `tool_result` opening with `call 518b327349710550` | `tool_use`, `success`, its call line read |
| 13 | `assistant` `msg_fixture05`, text, no `stop_reason` | the first part of line 14's message |
| 14 | `assistant` `msg_fixture05`, text, `end_turn` | `stop` `completed`, lines 13 and 14 joined: an Episodic fact |
| 15 | `system`, `informational` | skipped: `system` |
| 16 | `attachment` | skipped: `attachment` |
| 17 | `progress` | skipped: `progress` |
| 18 | `user`, a text block | `prompt` |
| 19 | call `Bash` `cargo test --release` | the call of line 20 |
| 20 | `tool_result`, `toolUseResult.interrupted` | `tool_use` `Bash`, `interrupted` |
| 21 | `user` `[Request interrupted by user for tool use]` | skipped: `interrupt` |
| 22 | `user`, string content | `prompt`; line 18's turn had no completed message: `incomplete_turn` |
| 23 | call `Grep` | the call of line 24 |
| 24 | `tool_result` of line 23 | `tool_use` `Grep`, `success`; the turn ends in `tool_use` |
| 25 | `user`, 2.1.310, 43 minutes after line 24 | `prompt`; line 22's turn: `incomplete_turn`; the gap opens a segment |
| 26 | `assistant` with `isApiErrorMessage`, model `<synthetic>` | `stop` `api_error` |
| 27 | `user` | `prompt` |
| 28 | `assistant` `msg_fixture08`, a thinking block | skipped: `thinking` |
| 29 | `assistant` `msg_fixture08`, text, `end_turn` | `stop` `completed`: an Episodic fact |
| 30 | `user` with `isMeta` | skipped: `meta` |
| 31 | `system` `compact_boundary`, trigger `manual` | skipped: `system`; the trigger of line 32 |
| 32 | `user` with `isCompactSummary` | `compact`, trigger `manual`: an Episodic fact |
| 33 | `assistant`, model `<synthetic>`, `No response requested.` | skipped: `synthetic` |
| 34 | `user`, an image block and a text block | `prompt` of the text; skipped: `image` |
| 35 | `assistant`, text, `end_turn` | `stop` `completed`: an Episodic fact |
| 36 | line 2's entry under session `23373885-…` and another `cwd` | `prompt` with line 2's event ID and digest: a duplicate; its turn in that session: `incomplete_turn` |
| 37 | `tool_result` naming no call of the file | skipped: `orphan tool_result` |
| 38 | `last-prompt` | skipped: `last-prompt` |
| 39 | `summary` | skipped: `unknown type` |
| 40 | `queue-operation` | skipped: `queue-operation` |
| 41 | a line cut short | malformed |
| 42 | `user` without `uuid` | skipped: `no uuid, sessionId or timestamp` |
| 43 | `user` at 11:00:00.100 | `prompt`, in the second of the subagent file's line 1 |
| 44 | call `Task` | the call of line 45 |
| 45 | `tool_result` of line 44 | `tool_use` `Task`, `success` |
| 46 | `assistant`, text, `end_turn` | `stop` `completed`: an Episodic fact |
| 47 | `user` | `prompt` |
| 48 | call `Bash` `cargo fmt`, no result in the file | `pending`; line 47's turn: `incomplete_turn` (a copy taken mid-turn) |

## `-work-fixture-p/52c6c7b7-c890-43ec-81a5-a1cf99d3cc84/subagents/agent-a3c91f2.jsonl`

Subagent `a3c91f2` of the same session: every line has `isSidechain` and the parent's `sessionId`.

| Line | Entry | Rule |
|---|---|---|
| 1 | `user` at 11:00:00.250 | `prompt`, subagent, in full |
| 2 | call `Read` | the call of line 3 |
| 3 | `tool_result` of line 2 | `tool_use` `Read`, `success`, subagent |
| 4 | `assistant`, text, `end_turn` | `stop` `completed`, subagent: event log only |

## `-work-fixture-p/e3b7a1c4-5d2f-4a86-9c1e-7f40b2d8a915.jsonl`

Session `e3b7a1c4-…`, main agent, resumed (#1173). Lines 6 to 10 replay lines 1 to 5 the way a resume writes them: the
same `uuid`, `timestamp`, `sessionId` and message content, with a new `cwd`, `gitBranch` and `version`, a `slug`, the
assistant entries' other `usage`, and line 8's `toolUseResult.stdout` emptied. A replay is read once, at its first
occurrence.

| Line | Entry | Rule |
|---|---|---|
| 1 | `user` `Run the fixture tests.`, 2.1.296 | `prompt` |
| 2 | `assistant` `msg_fixture21`, call `Bash` `cargo test` | the call of line 3 |
| 3 | `tool_result` of line 2, `toolUseResult.stdout` `test result: ok. 3 passed` | `tool_use` `Bash`, `success` |
| 4 | `assistant` `msg_fixture22`, text, no `stop_reason` | the first part of line 5's message |
| 5 | `assistant` `msg_fixture22`, text, `end_turn` | `stop` `completed`, lines 4 and 5 joined |
| 6–10 | lines 1–5 replayed, 2.1.310; line 8's `toolUseResult.stdout` is `""` | skipped: `replayed entry` |
| 11 | `user` `Commit the change.`, 2.1.310, an hour after line 1 | `prompt` |
| 12 | `assistant` `msg_fixture23`, text, `end_turn` | `stop` `completed` |
