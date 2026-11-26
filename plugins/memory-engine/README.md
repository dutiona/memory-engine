# memory-engine plugin for Claude Code

Captures every Claude Code lifecycle event that [ADR-0020](../../docs/design/adr/0020-harness-neutral-lifecycle-api.md)
maps, queues it in a local outbox, and delivers it to a memory-engine server. Nothing captured is lost to a session
exit, a `claude -p` teardown, an offline machine or a server restart: an event leaves the outbox only when the server
acknowledges it. It also gives the model memory-engine and knowledge-base tools over MCP, with a short skill that
tells it when to use them.

## Install

```sh
claude plugin marketplace add dutiona/memory-engine
claude plugin install memory-engine@memory-engine \
  --config me_url=https://memory.example.net --config kb_url=https://kb.example.net \
  --config dev_root="$(cd ~/dev && pwd -P)"
```

Or, inside Claude Code, `/plugin install memory-engine --marketplace dutiona/memory-engine`. Change the options later
with `claude plugin configure memory-engine@memory-engine`, then restart running sessions: until they end, events go
to whichever URL a session recorded last.

| Option | Meaning |
|---|---|
| `me_url` | The memory-engine server's base URL, `http://` or `https://`, printable ASCII. Until it is first set, events queue locally. Unsetting it later does not withdraw the URL already recorded: to hold delivery, delete `~/.local/state/memory-engine/outbox/endpoint`. No trailing `/`: a URL ending in `/` gives `//mcp` and breaks its MCP server, while the hooks tolerate it. |
| `kb_url` | Optional. The knowledge-base server's base URL, without a trailing `/`. Its server stays unavailable until it is set; an unset URL fails that server alone. |
| `dev_root` | Optional. The machine's dev root: an absolute path in printable ASCII without space, `"`, `%` or `\`. Any other value is ignored. It must be its physical path, as `pwd -P` prints it: a dev root reached through a symlink gives remote-less projects no key, on both channels. |

The plugin ships a `bin/` directory, so it installs in Claude Code only, not through claude.ai or Cowork.

## The token file

Each machine has its own token, in `~/.config/memory-engine/secrets/token`: mode 0600, the token and at most one
line feed. It is written once, when the machine is set up, and is never part of the plugin's configuration. The
helper reads it at send time and hands it to `curl` on standard input, so it reaches no command line and no queued
file. Until the file exists, events queue and every session start shows `memory: no token file`.

## MCP servers

The plugin declares two HTTP MCP servers: `me` (memory-engine, at `<me_url>/mcp`) and `kb` (knowledge-base, at
`<kb_url>/mcp`). Claude Code shows them as `plugin:memory-engine:me` and `plugin:memory-engine:kb`. The skill
`skills/memory/SKILL.md` tells the model when to use them.

The model gets seven tools:

- `mcp__plugin_memory-engine_me__recall`
- `mcp__plugin_memory-engine_me__remember`
- `mcp__plugin_memory-engine_me__forget`
- `mcp__plugin_memory-engine_me__set_level`
- `mcp__plugin_memory-engine_kb__search_index`
- `mcp__plugin_memory-engine_kb__ingest_text`
- `mcp__plugin_memory-engine_kb__ingest_url`

Each MCP connection runs `bin/me-hook headers me` or `bin/me-hook headers kb` (Claude Code's `headersHelper`). The
helper reads the token file at every connection, and Claude Code re-runs it when a tool call gets a 401, so a rotated
token is picked up without restarting the session. `me` receives the token, `ME-Harness`, `ME-Client-Version` and the
project facts of the session's project directory (`CLAUDE_PROJECT_DIR`, which a `cd` does not move), plus the dev root
in a static header; `kb` receives the token only. The helper sends only a token of the right shape (`me_` and 32
letters or digits) and writes nothing. Claude Code's `--debug` output can show the `Authorization` header it prints.

Check the servers with `claude mcp list` from a project directory. Allow rules for the tools are
`mcp__plugin_memory-engine_me` and `mcp__plugin_memory-engine_kb`; the plugin ships none.

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

It needs no argument and no environment, and exits 0; 1 only when it can find no state root (no HOME, and not run from
the stable path).

## Upgrading from 0.1.0

With no session open:

```sh
claude plugin update memory-engine@memory-engine
claude plugin configure memory-engine@memory-engine   # set kb_url; check me_url and dev_root
claude mcp list                                      # from a project: both servers connected
```

`me_url` takes no trailing `/`, and `dev_root` is reset from `pwd -P` if it goes through a symlink. Then remove any hand-written user-scope MCP server for memory-engine
(`claude mcp remove <name> -s user`) and replace allow rules that name its tools, before the next session; otherwise the
model sees every tool twice. Its calls carried no project fact, so what was remembered through it sits in the machine's
unscoped bucket; a session outside any project can raise one with `set_level` to `user`, and a session of the project
can then bring it down with `set_level` to `project` (ADR-0020 §8). A machine not yet updated keeps capturing unchanged.

## Rolling back

Roll back with a later release that removes the servers: `claude plugin disable` would stop capture too. Memories,
forgets and call records stay on the server, and the outbox is untouched.

## Requirements

Claude Code 2.1.332 or later for the MCP servers. POSIX `sh`, git 2.31 or later (`--path-format`), curl 7.55 or later
(`-H @file`). The tests also need `python3` and `pgrep`.

## Tests

```sh
sh plugins/memory-engine/tests/run.sh                     # the shell suite; also under macOS /bin/sh
python3 -I plugins/memory-engine/tests/latency.py         # a capture's p95, against the 50 ms budget
ME_TEST_CLAUDE_TOKEN_FILE=<file> ME_TEST_KB_DIR=<knowledge-base checkout> sh plugins/memory-engine/tests/real-claude.sh
```

`real-claude.sh` runs real `claude -p` sessions under a scratch HOME against a local stub server. Its token file holds
a token from `claude setup-token`; the script never reads your own Claude Code credentials. It also needs
`ME_TEST_KB_DIR`, a knowledge-base checkout whose `.venv` was synced with `uv sync --locked`, and the
`memory-engine-mcp` and `memory-engine-cli` binaries (`cargo build --workspace --locked`; `ME_TEST_ME_BIN` overrides
the path). Its phases M and L run the plugin's MCP servers against a local memory-engine and knowledge-base on
loopback ports.

## Releasing

Bump `version` in `.claude-plugin/plugin.json` and `ME_VERSION` in `bin/me-hook` together; the `version` test checks
that they agree.
