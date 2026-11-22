# Plugin store fixtures

Two synthetic plugin stores for `POST /backfill/uploads` (#1117), checked in as SQL and built in memory or on disk by
the tests: `claude-mem.sql`, a claude-mem store, and `claude-memory.sql`, a claude-memory `conversations.db`. Their
`CREATE TABLE` statements reproduce the column lists and types of the real stores' tables, written by hand from
copies; the plugins' full-text tables and triggers are left out, since nothing reads them. Every row is synthetic: the
session and message `uuid`s are random v4, the text is made up, and no value holds a secret. The tests plant every
secret, canary and denylist literal at run time. `plugin_store_fixtures_are_public_safe` fails on a local path or on
any string a redaction rule would change.

The text `NEVER-EXPORTED` marks every value the exporter must never read: `sdk_sessions.user_prompt`,
`observations.files_read` and `content_hash`, every member of `pending_messages`, `projects.key` and `name`,
`sessions.git_branch`, `branches.aggregated_content` and `import_log.file_path`. No export body may hold it.

Event IDs below drop the shared prefix: `p1` of session `eef5665a-…` is `bf.claude-mem.eef5665a-….p1`, `o1` is
`bf.claude-mem.o1`, and a claude-memory `p` or `s` of message `f373ecaf-…` is `bf.claude-memory.f373ecaf-….p` or `.s`.

## `claude-mem.sql`

Sessions (`sdk_sessions`): `eef5665a-…` (memory session `mem-fixture-a`, project `alpha`), `31304ad9-…`
(`mem-fixture-b`, `beta`), and `e4bff46f-…` (`alpha`, no memory session). A distilled row reaches its session through
its memory session; the observation of `mem-fixture-gone` has none.

| Row | What it is | Rule |
|---|---|---|
| `user_prompts` 1 | prompt 1 of `eef5665a-…`, at `.120` | main `prompt` `p1`, seq 120000 |
| `user_prompts` 2 | prompt 2 of `eef5665a-…`, the same millisecond | main `prompt` `p2`, seq 120001 |
| `user_prompts` 3 | prompt 1 of `31304ad9-…` | main `prompt` `p1` |
| `user_prompts` 4 | prompt 2 of `31304ad9-…`, empty | skipped: `empty prompt` |
| `user_prompts` 5 | prompt 1 of `e4bff46f-…`, a session with no memory session | main `prompt` `p1` |
| `observations` 1 | `discovery`, title, subtitle, two facts, narrative, `how-it-works` and `pattern` | `o1`, Episodic, importance 0.6 |
| `observations` 2 | `change`, title, one fact, narrative, `what-changed` | `o2`, Episodic, importance 0.5 |
| `observations` 3 | no title, subtitle, text, narrative or fact | skipped: `empty observation` |
| `observations` 4 | `bugfix`, `facts` not a JSON list, `gotcha` | `o4`, Episodic, importance 0.6, no fact line |
| `observations` 5 | `decision`, `concepts` not a JSON list | `o5`, Episodic, importance 0.5 |
| `observations` 6 | its memory session is gone | skipped: `incomplete row` |
| `session_summaries` 1 | every field | `s1`, Episodic, importance 0.5, six lines |
| `session_summaries` 2 | `request` and `completed` only | `s2`, Episodic, two lines |
| `session_summaries` 3 | every field empty | skipped: `empty summary` |
| `pending_messages` 1 | raw tool I/O | never exported |

Slices: `alpha` holds prompts 1, 2 and 5, observations 1 to 4 and summaries 1 and 3; `beta` prompts 3 and 4,
observation 5 and summary 2; the empty project observation 6. claude-mem keeps no assistant message: no stop.

## `claude-memory.sql`

Projects: `/work/fixture/alpha` and `/work/fixture/beta`. Sessions: `eef5665a-…` (main, `alpha`), `b14d1330-…`
(main, `beta`), `a5b6c7d` (a subagent of `eef5665a-…`, in its project) and `6abf0af7-…` (main, no project). A
session's lines are read by time, then by `id`; every `user` line ends the current turn, and so does the end of the
session.

Session `eef5665a-…`, six turns:

| Turn | Opened by | Lines | Closed by | Stop |
|---|---|---|---|---|
| 1 | message 1, a prompt | 2 (narration), 3 | message 4, a task notification | message 3 |
| 2 | message 4 | 5 | message 6, a prompt | message 5 |
| 3 | message 6 | 7 | message 8, an interrupt marker | none: `interrupted turn` |
| 4 | message 8 | 9 | message 10, a prompt | message 9 |
| 5 | message 10 | none | message 11, a prompt | none |
| 6 | message 11 | 12 (narration), 13, one timestamp, so by `id` | the end of the session | message 13 |

| Row | What it is | Rule |
|---|---|---|
| `messages` 1, 6, 10, 11 | `user` text | main `prompt` `p` |
| `messages` 2, 12 | an `assistant` line that does not end its turn | skipped: `narration` |
| `messages` 3, 5, 9, 13 | a turn's last `assistant` line | main `stop` `completed` `s`: an Episodic fact |
| `messages` 4 | `is_notification` 1 | skipped: `notification` |
| `messages` 7 | the last `assistant` line of a turn an interrupt marker ends | skipped: `interrupted turn` |
| `messages` 8 | `[Request interrupted by user]` | skipped: `interrupt` |
| `messages` 20, 22 | `user` text of the subagent `a5b6c7d` | subagent `prompt` `p` in session `eef5665a-…` |
| `messages` 21, 23 | the subagent's two turns' last lines | subagent `stop` `completed` `s`, event log only |
| `messages` 30 | an `assistant` line before the session's first `user` line | main `stop` `s` of `b14d1330-…` |
| `messages` 31 | `user` text | main `prompt` `p` |
| `messages` 32 | the session's last line | main `stop` `s` |
| `messages` 40 | `user` text of a session with no assistant message | main `prompt` `p` of `6abf0af7-…` |
| `messages` 41 | empty `user` text | skipped: `empty message` |
| `messages` 42 | a time that does not parse | skipped: `incomplete row` |

Slices: `/work/fixture/alpha` holds messages 1 to 13 and 20 to 23 (two sessions), `/work/fixture/beta` messages 30 to
32, the empty project messages 40 to 42.
