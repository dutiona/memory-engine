# Backfill

How a machine's past history enters the memory service: machine by machine, from copies, under a curated allowlist per
machine, each file seen as a dry run and reviewed before its write. The route is `POST /backfill/uploads`
([MCP server](reference/mcp-server.md#backfill-uploads)); this page is the procedure around it.

## What goes where

The allowlist of a host splits its copy in two:

- **Narrative, to memory-engine, through the route**: Claude Code session transcripts
  (`projects/<project-dir>/<session>.jsonl`), their subagent files (`projects/<project-dir>/<session>/subagents/agent-<id>.jsonl`)
  and the native auto-memory files of each project (`projects/<project-dir>/memory/*.md`).
- **Durable text artifacts, to knowledge-base ingest**: documents, specifications, notes kept as files. They never go
  through this route.

The claude-mem and claude-memory plugin stores have no reader yet; they wait for their own ticket.

Never run `memory-engine-cli bootstrap` on the service store. It is ADR-0009's offline importer for an embedded store:
it extracts by keywords, embeds inline and writes one marker per session, a second import mechanism beside the route,
and it needs the store lock the running server holds.

## Before a host

1. **A verified backup of the store**: a backup taken now, opened read-only (`memory-engine-cli --db <backup> status`)
   to check it reads.
2. **The host's token line.** The machine an upload is filed under is the one its token names, so each source host
   uploads with its own token: a host that still runs keeps its own, and a retired host gets a line for its run, removed
   after it. Mint the line as [Transport and authentication](reference/mcp-server.md#transport-and-authentication)
   shows, and restart the server, which reads the token file at start only.
3. **The author denylist.** Set `ME_REDACT_DENYLIST_FILE` in the server's unit to a mounted file of literals, one per
   line (the names, hosts and paths the history may hold), and restart. Without it every upload answers 503. Check the
   first dry run's `denylist_literals`: it is the number of literals the server loaded.
4. **The allowlist**, from the copy, never from the host's live directory.

## Per file

### The dry run

Send the file with `ME-Backfill-Mode: dry-run`. The token is read from its file into a header on standard input, never
put on a command line; every `ME-Project-*` value and `ME-Backfill-Source` is percent-encoded as ADR-0020 §3 says (each
byte outside 0x21 to 0x7E, and `%`, `"` and `\`, as `%HH`):

```sh
printf 'Authorization: Bearer %s\n' "$(cat <token-file>)" | curl -sS -H @- \
  -H 'ME-Harness: claude-code' \
  -H 'ME-Backfill-Kind: transcript' \
  -H 'ME-Backfill-Mode: dry-run' \
  -H 'ME-Backfill-Source: projects/<project-dir>/<session>.jsonl' \
  -H 'ME-Client-Version: <driver-version>' \
  -H 'ME-Project-Root: <project-root>' \
  -H 'ME-Project-Dev-Root: <dev-root>' \
  -H 'ME-Project-Origin-Url: <origin-url>' \
  --data-binary @<copy>/projects/<project-dir>/<session>.jsonl \
  https://<memory-host>/backfill/uploads > <report-dir>/<session>.dry.json
```

A memory file is `ME-Backfill-Kind: memory-file` and carries the copy's modification time, which dates a memory with
no frontmatter date and no date in its name: `-H "ME-Backfill-Mtime: $(date -u -r <file> +%Y-%m-%dT%H:%M:%SZ)"` with
GNU `date`.

**The project facts.** One set per upload, from the harvest's `projects.manifest.tsv`: the row whose `project_root` is
the longest prefix, at a `/` boundary, of the transcript's first `cwd` (for a memory file, of its project's), sends
that row's facts. No row sends none, and the upload is filed under the machine's bucket, which the dry run's
`project.home` shows. A `cwd` that moves during the session keeps the upload's project.

### The review

Read the dry run's report (`jq . <session>.dry.json`) before any write:

- **`project`**: the Home and key you expect for this file; any `dropped` fact explained.
- **`denylist_literals`**: the count you loaded.
- **`census`**: the lines, the malformed ones, what was skipped by reason, and the `incomplete_turn` count.
- **`sessions`**: each session's first and last time and its stored events; a `live_since` means the plugin captured
  that session live from then, and only what came before imports.
- **`events`**: stored, duplicates (a re-upload, or a forked session's repeated entries), `live_overlap`, `pending`
  calls, refusals by reason, and the counts by kind and actor.
- **`tool_events` and `stubs`**: the tools called, and every call that touched a sensitive path, kept as a stub.
- **`redaction`**: what was found, the stubs, the placeholders by rule; read the `facts.items[].preview` texts for
  anything redaction missed.
- **`facts`**: what the write would create, reinforce or find blocked, each with its Home, type and date. An item with
  `hint: set_level` is a `user` memory filed at its project: after the write, a session raises it with `set_level`.
- **`collisions`**: empty. A collision is a key held with other content; it stops the file.

### The write

Send the same request with `ME-Backfill-Mode: write` and `ME-Backfill-Preview` set to the dry run's `preview`
(`jq -r .preview <session>.dry.json`). A write lands only the effects the dry run showed: its report is the dry run's
but for `mode` and `committed`. Compare the two and keep both.

| Answer | Then |
|---|---|
| 200 | A dry run: review it. A write: done |
| 400 | Fix the header the error names |
| 401 | The token: its line, or the wrong host's token |
| 408, 503 | Retry. A 503 naming `ME_REDACT_DENYLIST_FILE` needs the configuration fixed and a restart first |
| 409 | Stop the file and look at its `collisions` |
| 412 | The file, the denylist or the store changed since the dry run: run a new dry run and review it again |
| 413 | Stop the file: it is over 256 MiB and is refused whole |
| 422 | Stop the file and look: the reader could not read it, or its import failed |

Uploads go one at a time; a second one meanwhile answers 503. Memories are queued for the embedding drain, never
embedded by the upload, so an upload works with the embedder down: full-text `recall` finds the memories at once and
`status` shows them waiting. While a large backlog drains, live memories wait behind it for their vectors; pace the
writes by the dry runs' fact counts.

## Undo

- **One fact**: `forget` it from a session of its project.
- **A whole host**: `memory-engine-cli erase --machine <host>`, with the unit stopped. It takes every row the host's
  uploads wrote, beside its live captures: events, facts, queue rows, activities, receipts and segments.

No source is deleted before its ingestion and a verified backup taken after it.

## Accepted residuals

- A synthetic user entry the transcript does not mark (neither `isMeta` nor an interrupt marker) imports as a prompt,
  in the event log only.
- A resume within 30 minutes stays in one segment, where live capture would have split it.
- An `erase` of the first machine takes a memory a second machine's copy reinforced.
- A tombstone matches the text as stored, so a text forgotten under one denylist and re-imported under another is not
  blocked.
- A subagent file uploaded long after its parent's can find the parent's segment ended and import as late events:
  stored, with no segment change (ADR-0020 §5, rule 1).
