# memory-engine plugin for Claude Code

Captures every Claude Code lifecycle event that [ADR-0020](../../docs/design/adr/0020-harness-neutral-lifecycle-api.md)
maps, queues it in a local outbox, and delivers it to a memory-engine server. Nothing captured is lost to a session
exit, a `claude -p` teardown, an offline machine or a server restart: an event leaves the outbox only when the server
acknowledges it.

## Install

```sh
claude plugin marketplace add dutiona/memory-engine
claude plugin install memory-engine@memory-engine \
  --config me_url=https://memory.example.net --config dev_root=/home/you/dev
```

Or, inside Claude Code, `/plugin install memory-engine --marketplace dutiona/memory-engine`. Change the options later
with `claude plugin configure memory-engine@memory-engine`, then restart running sessions: until they end, events go
to whichever URL a session recorded last.

| Option | Meaning |
|---|---|
| `me_url` | The server's base URL, `http://` or `https://`, printable ASCII. Until it is set, events queue locally. |
| `dev_root` | Optional. The machine's dev root: an absolute path in printable ASCII without space, `"`, `%` or `\`. Any other value is ignored. |

The plugin ships a `bin/` directory, so it installs in Claude Code only, not through claude.ai or Cowork.

## The token file

Each machine has its own token, in `~/.config/memory-engine/secrets/token`: mode 0600, the token and at most one
line feed. It is written once, when the machine is set up, and is never part of the plugin's configuration. The
helper reads it at send time and hands it to `curl` on standard input, so it reaches no command line and no queued
file. Until the file exists, events queue and every session start shows `memory: no token file`.

## The outbox

Everything lives under `~/.local/state/memory-engine/`, directories 0700 and files 0600:

| Path | Content |
|---|---|
| `bin/me-hook` | A copy of the helper, refreshed by every hook. The watcher runs it. |
| `outbox/<id>/body`, `outbox/<id>/headers` | One queued event: the hook input byte for byte, and the curl header file holding its envelope |
| `outbox/rejected/<id>/` | An event the server refused, kept for ever |
| `outbox/endpoint` | The server URL the hooks recorded |
| `outbox/seq/`, `outbox/tmp/`, `outbox/lock/` | Seq reservations, files being written, the flusher's lock |

`<id>` is `<epoch>-<seq>-<pid>`, so the queue lists in delivery order. Queued bodies are not redacted (the server
redacts): they stay on the machine, like Claude Code's own transcripts.

A flush sends the oldest event first and reacts to each answer as ADR-0020 §3 says: a 204 removes the event; 400, 409,
413 and 422 move it to `rejected/` and the flush goes on; anything else ends the flush and keeps the queue. To replay a
parked event after a server fix, move it back: `mv ~/.local/state/memory-engine/outbox/rejected/<id> ~/.local/state/memory-engine/outbox/`.

Every hook starts a flush in the background. That flush may or may not outlive a `claude -p` run (both were observed);
whatever it does not deliver stays queued and goes with the next hook or with the watcher. Requests go straight to the server, never through a proxy, so
the token reaches nothing else.

## The watcher

A per-machine timer, installed by my-dotfiles, delivers what a machine queued while no session was open:

```sh
/bin/sh ~/.local/state/memory-engine/bin/me-hook flush
```

It needs no argument and no environment, and exits 0 (1 only when it finds no state directory).

## Requirements

POSIX `sh`, git 2.31 or later (`--path-format`), curl 7.55 or later (`-H @file`). The tests also need `python3` and
`pgrep`.

## Tests

```sh
sh plugins/memory-engine/tests/run.sh                     # the shell suite; also under macOS /bin/sh
python3 -I plugins/memory-engine/tests/latency.py         # a capture's p95, against the 50 ms budget
ME_TEST_CLAUDE_TOKEN_FILE=<file> sh plugins/memory-engine/tests/real-claude.sh
```

`real-claude.sh` runs real `claude -p` sessions under a scratch HOME against a local stub server. Its token file holds
a token from `claude setup-token`; the script never reads your own Claude Code credentials.

## Releasing

Bump `version` in `.claude-plugin/plugin.json` and `ME_VERSION` in `bin/me-hook` together; the `version` test checks
that they agree.
