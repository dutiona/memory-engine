# MCP Server (`memory-engine-mcp`)

## What

An MCP server binary that serves memory-engine over streamable HTTP (stateless `POST /mcp`, JSON responses), behind
per-machine Bearer tokens, and gives the model four tools: `recall`, `remember`, `forget` and `set_level` (ADR-0020 §8). It also
receives each Claude Code session's events on `POST /lifecycle/events` ([Lifecycle events](#lifecycle-events)), and a
machine's past history, one copied file at a time, on `POST /backfill/uploads` ([Backfill uploads](#backfill-uploads)). Part of
the four-layer cognitive architecture (Knowledge → Memory → Wisdom → Intelligence): this crate bridges Memory to
Intelligence via the Model Context Protocol.

## Why

The memory service is reachable from every machine of its owner, so its model-facing surface is deliberately small
(ADR-0020 §8): the model reads, writes and retracts memories, and nothing it can call prunes, deletes, dumps or writes a
path. Maintenance (pruning, erasure, reversing a forget, export, consolidation, inspection) is `memory-engine-cli`'s, run by an operator; captured
session events arrive through the lifecycle routes, not through tools. The server validates every input at the
transport boundary: the engine trusts its callers, the server does not.

## How

### Installation

```bash
cargo build -p memory-engine-mcp --release
```

### Usage

```bash
# With TOML config
memory-engine-mcp --listen 127.0.0.1:<port> --token-file /path/to/tokens --user alice --config /path/to/mcp.toml

# With CLI args
memory-engine-mcp --listen 127.0.0.1:<port> --token-file /path/to/tokens --user alice \
  --db-path /path/to/memory.db --embed-url http://localhost:11434/v1/embeddings --embed-model nomic-embed-text \
  --embed-query-instruction ''   # nomic is symmetric: opt out of ME's default query instruction

# With env vars
MEMORY_MCP_LISTEN=127.0.0.1:<port> MEMORY_MCP_TOKEN_FILE=/path/to/tokens MEMORY_MCP_USER=alice \
  MEMORY_MCP_DB_PATH=/path/to/memory.db MEMORY_MCP_EMBED_URL=... memory-engine-mcp
```

| Flag | Environment variable | Meaning |
|---|---|---|
| `--listen` (required) | `MEMORY_MCP_LISTEN` | Socket address to serve on |
| `--token-file` (required) | `MEMORY_MCP_TOKEN_FILE` | The token-hash file (below) |
| `--embed-api-key-file` | `MEMORY_MCP_EMBED_API_KEY_FILE` | A file holding the embedder API key alone, one trailing newline allowed; mount it read-only, like the token file. The one key source when set: beside `--embed-api-key` / `MEMORY_MCP_EMBED_API_KEY` or `api_key` the server refuses to start, naming both settings. A missing, unreadable or empty file refuses to start, naming its path and never its contents. The key is held like `api_key`: never logged |
| `--user` (required) | `MEMORY_MCP_USER` | The owner: the user level is `user:<user>` (ADR-0020 §7). A lowercase DNS label |
| `--allowed-host` (repeatable) | `MEMORY_MCP_ALLOWED_HOSTS` (comma list, no spaces) | A `Host` value served besides loopback |
| `--backup-dir` | `MEMORY_MCP_BACKUP_DIR` | Overrides `engine.backup_dir` |
| `--embed-dim` | `MEMORY_MCP_EMBED_DIM` | Overrides `engine.embed_dim` (lets a blank store start without a TOML file) |
| `--embed-url`, `--embed-model`, `--embed-api-key`, `--embed-provider` | `MEMORY_MCP_EMBED_URL`, `_MODEL`, `_API_KEY`, `_PROVIDER` | Override the `[embedding]` keys of the same meaning |
| `--embed-query-instruction` | `MEMORY_MCP_EMBED_QUERY_INSTRUCTION` | Overrides `query_instruction`; `''` for a symmetric model |
| `--embed-artifact-sha256`, `--embed-pooling`, `--embed-normalization`, `--embed-max-input-tokens` | `MEMORY_MCP_EMBED_ARTIFACT_SHA256`, `_POOLING`, `_NORMALIZATION`, `_MAX_INPUT_TOKENS` | Override the served-artifact keys (below) |
| — | `RUST_LOG` | The log filter, on stderr. Unset: WARN for every crate. A bare level (`RUST_LOG=debug`) applies to every crate, dependencies included (hyper, rusqlite, tokio). A crate directive (`RUST_LOG=memory_engine_mcp=debug`) raises that crate only; every other crate stays at WARN |

### Configuration

```toml
[engine]
db_path = "/path/to/memory.db"
embed_dim = 384  # Optional — probed from existing DB if omitted
backup_dir = "/path/to/backups"  # Optional — an existing directory; a migration first writes <db>.v<N>.bak there

[embedding]
endpoint = "http://localhost:11434/v1/embeddings"
model = "nomic-embed-text"
api_key = "sk-..."   # Optional — for authenticated endpoints; or --embed-api-key-file, never both
provider = "ollama"  # Serving backend, e.g. llama.cpp | ollama | tei | openai (default: ollama). Free-form; feeds the fingerprint.
dimensions = 384     # Native model dimension (validated against the raw response)
query_instruction = ""  # nomic is symmetric: no query prefix (absent = ME's pinned instruction)
timeout_secs = 30
```

CLI flags and environment variables override TOML values field-by-field (e.g., `--embed-api-key` overrides `api_key` in TOML while keeping `endpoint` and `model` from TOML; `--embed-provider` overrides `provider`). The merge and its checks run before the store opens, so a configuration error creates no database and migrates nothing. Any embedder setting given without both a URL and a model is refused, never silently dropped.

#### Query instruction

Queries are prefixed with ME's pinned instruction (ADR-0015 §6), `Instruct: Given a search query, retrieve relevant memory facts.\nQuery: `, unless `query_instruction` (or `--embed-query-instruction`) says otherwise; documents are never prefixed. **A symmetric model must opt out** with `query_instruction = ""`, or its queries carry the Qwen prefix. Changing the instruction re-embeds nothing.

#### The pinned identity (ADR-0015 §5)

The deployed identity declares every field, because a blank store records whatever its first embedding write declares:

```toml
[engine]
db_path = "/path/to/memory.db"
embed_dim = 1024

[embedding]
endpoint = "http://127.0.0.1:8080/v1/embeddings"
model = "Qwen/Qwen3-Embedding-0.6B"
provider = "llama.cpp"
dimensions = 1024
artifact_sha256 = "06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439"
pooling = "last"
normalization = "l2"
max_input_tokens = 2048
```

The four served-artifact keys (`artifact_sha256`, `pooling`, `normalization`, `max_input_tokens`) are all set or all absent; `artifact_sha256` is 64 lowercase hex digits. They are declared, never probed: the deployment checks the served file's SHA-256 at install. `max_input_tokens` is also
the limit a longer memory is chunked by ([Embedding queue](#embedding-queue)).

#### Asymmetric models (TEI / Qwen)

Models like `Qwen/Qwen3-Embedding-0.6B` take a query-only instruction prefix and support Matryoshka (MRL) truncation. `recall` embeds its query with `embed_query` (prefix applied); the embedding queue embeds what `remember` stored
prefix-free.

```toml
[engine]
db_path = "/path/to/memory.db"
embed_dim = 256       # The STORED dimension — must equal mrl_dim below

[embedding]
endpoint = "http://localhost:8080/v1/embeddings"
model = "Qwen/Qwen3-Embedding-0.6B"
provider = "tei"
dimensions = 1024     # NATIVE dim the model emits (validated against the raw response)
mrl_dim = 256         # Truncate + L2-renormalize to this; must equal engine embed_dim
```

`mrl_dim` is the stored (post-truncation) dimension and **must equal** the engine's `embed_dim` — the server rejects a mismatch at startup. `dimensions` stays the native pre-truncation length.

#### Embedding-identity check (fail-fast)

On startup the server verifies the configured provider's identity (`model`, `provider`, `dim`, MRL, element type and the served artifact) against the one the store was created with, and **refuses to start on a mismatch** (#614, #1017). This prevents the silent-corruption case where a different model — even at the same dimension, or the same model served from another file — embeds queries into an incompatible vector space. A fresh store with nothing embedded yet has no recorded identity and accepts any provider; the identity is stamped on the first embedding write.

An absent served artifact matches only another absent one, so a store recorded before schema v15 (or without the served keys) refuses a configuration that declares them, and the reverse. The recoveries are to drop the declaration and keep serving the recorded identity, or to start from a blank store. Reconstruction under the new identity is the normative path (ADR-0015 §4) but has no CLI or MCP surface yet (#689). A refused start leaves the store as it was: the check reads it read-only, before anything migrates it, so either binary still opens it (#1065). A store of another dimension is likewise refused before it migrates.

### Transport and authentication

The server answers MCP only as rmcp's streamable-HTTP service on `POST /mcp`, in stateless mode with JSON responses:
every call is an independent request, so an idle client or a server restart needs no session recovery.

Every request must carry exactly one `Authorization: Bearer <token>` header whose token belongs to a machine in the
token-hash file. The file's format, the token's shape and the fail-closed rules are those of
[ADR-0020 §4](../design/adr/0020-harness-neutral-lifecycle-api.md#4-machine-tokens): one `<machine> <sha256-hex>`
line per machine, read at startup only, and any malformed or repeated line refuses to start. Mint a line without
putting the token on a command line or hashing a stray line feed:

```bash
printf '%s %s\n' laptop "$(printf '%s' "$token" | sha256sum | cut -d' ' -f1)" >> tokens
```

| Status | When |
|---|---|
| 401 | Any request, on any method or path, without exactly one valid token |
| 403 | A valid token, but a `Host` that is not admitted |
| 405 | `GET` or `DELETE` on `/mcp` (stateless mode serves `POST` only) |
| 406, 415 | A `POST` without `Accept: application/json, text/event-stream` or `Content-Type: application/json` |

The `Host` check admits loopback plus each `--allowed-host` value; an entry without a port admits any port, and `*`,
`@` and spaces are refused. The calling machine, taken from the token, is written to `metadata.machine` on every
fact `remember` stores; a client cannot set it.

The Claude Code plugin sends the token through its `headersHelper` (ADR-0020 §12); never put a token in `.mcp.json`
or on a command line. A manual check reads the header from standard input:

```bash
printf 'Authorization: Bearer %s\n' "$(cat /path/to/token)" | curl -sS -H @- \
  -H 'Accept: application/json, text/event-stream' -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' https://memory.example/mcp
```

### Shutdown and health

SIGTERM or SIGINT stops accepting connections, gives open connections 5 s to finish, waits for every tool call,
lifecycle event and backfill upload already started, stops the embedding drain and the idle-segment sweep, flushes the `<db>.snapshot` sidecar and exits 0. The drain and the sweep stop at the
same fence as the tool calls: a settle or a sweep in progress commits first and none starts after it. A lifecycle event that
arrives once the stop has begun is answered 503 before it touches the store, and its client retries; one already
being applied runs to its commit, even if its client hung up. An embed call still in
flight is abandoned, its item untouched and retried at the next start; the process never waits for that call. A stop timeout shorter than the slowest call
becomes a kill, and the next start rebuilds the projections from the database. There is no unauthenticated route, so
a liveness probe expects 401:
`curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:<port>/mcp` prints `401`.

**Store lock.** For its whole life the server holds an exclusive advisory lock on `<db>.store-lock` beside the
database (ADR-0020 §8), taken after every configuration check and before the store opens or migrates. A second server
on the same store, or a server started while a maintenance command holds the lock, exits 1 naming the lock file.
Every writable `memory-engine-cli` command takes the same lock or refuses, so maintenance runs with the server
stopped; read-only commands, `export` included, run beside it
([CLI reference](cli-inspector.md#store-lock)).

### Project facts and the project key

Every request names its project through five optional headers (ADR-0020 §3), the same on the MCP route and on the
lifecycle routes: `ME-Project-Root`, `ME-Project-Dev-Root`, `ME-Project-Upstream-Url`, `ME-Project-Origin-Url` and
`ME-Project-Branch`. A client percent-encodes every byte outside printable ASCII (0x21 to 0x7E) and each of `%`, `"`
and `\`; the server decodes every `%HH` in either hex case, so two clients that encode a path differently name one
project. The dev root is plain ASCII and sent as configured, never decoded.

**A bad fact is dropped, never the call**: it is named in the call record's `project.dropped` with its reason, and the
call goes on without it.

This page holds the key rules canonically; `KEY_RULES_VERSION` in `src/project.rs` names their version, and any change
to what follows increments it. **Rules, version 1.**

Decoding, per header (`ME-Project-Root` → `root`, `-Dev-Root` → `dev_root`, `-Upstream-Url` → `upstream_url`,
`-Origin-Url` → `origin_url`, `-Branch` → `branch`); the first failing rule drops the fact with its reason:

1. No header: no fact, no flag.
2. Two or more headers of that name: `repeated`.
3. Empty value: `empty`.
4. Over 4,096 bytes: `too_long`.
5. A byte outside 0x21 to 0x7E: `raw_byte`.
6. Dev root only: not starting with `/`, or holding `%`, `"` or `\`: `not_plain_absolute`; otherwise taken as sent,
   never decoded.
7. A `%` not followed by two hex digits: `bad_escape`; each `%HH`, either case, decodes exactly once to its byte.
8. Not UTF-8: `not_utf8`.
9. Any control character (C0, DEL, C1): `control`.
10. Both URLs then lose their userinfo, in two steps: when `://` occurs, the authority after its first occurrence (up to
    the next `/`) loses everything through its last `@`; then, when a `:` exists and the text before the first one
    holds no `/`, that text loses everything through its last `@`. This removes at least what the plugin's client
    removes.
11. A scheme-form URL that still holds an `@` after step 10 and gives no remote key had its userinfo hidden from the
    strip by a client that did not encode `%`: an encoded `/` in the password, or an encoded separating `@`
    (`https://user:pa%2Fss@host/x`). It is dropped as `unstripped_userinfo` instead of stored. The rule reads only the
    decoded value, so every encoding of one URL gives one outcome, and it changes no key: a dropped URL gave none. An
    `@` in a path that gives a key is kept (`https://github.com/example/a@b`), and a keyless URL with an `@` in its
    path is dropped under the same name. Residual, for such a client only: when the userinfo's text before its first
    encoded `/` reads as `host`, `host:` or `host:<digits>` (`https://to%2Fken@host/x`, `https://user:%2Fss@host/x`,
    `https://user:12%2F34@host/x`), the URL gives a key, so the URL, the key and its scope node hold the secret; the
    same holds for an scp-form remote whose user name holds an encoded `/` (`to%2Fken@host:x`, stored, keyless).
12. A fact that holds a machine token (`me_` plus 32 base62 characters), or a URL whose remote key would hold one (a
    host is lowercased), is dropped as `machine_token` before the key is derived, so no stored provenance or key
    carries one (#1024); the key comes from the facts that remain.

Remote key, tried on `upstream_url` then `origin_url`; a URL that gives none is ignored, and the next rule applies:

- R1. A `file:` prefix in any case, or a remote-helper prefix `[A-Za-z][A-Za-z0-9+.-]*::`: none.
- R2. Scheme form (`[A-Za-z][A-Za-z0-9+.-]*://`): the authority runs to the first `/`; the host is the authority after
  its last `@`; a host starting with `[` runs through the first `]` (no `]`: none); otherwise it ends at its first `:`.
  After the host only nothing, or `:` and zero or more ASCII digits (the port, dropped), may follow; else none.
- R3. scp form: a `:` exists and the text before the first one holds no `/`. The host starts after the last `@` before
  it, bracket-aware (`[...]` must be followed by `:`); the path follows the `:` that ends the host.
- R4. Anything else is a local path: none.
- R5. Path: trim `/` at both ends, drop one trailing `.git` (lowercase only: `.GIT` stays), trim trailing `/` again.
  Empty host or path: none.
- R6. Key: the host lowercased in ASCII only, `/`, the path with its case kept. No URL-level decoding.

`git@github.com:example/project.git` and `https://github.com/example/project` are both `github.com/example/project`.

Path key, when no remote gives one; it needs both the root and the dev root:

- P1. Trim trailing `/` from the dev root (`/` becomes empty); the root must start with it followed by `/`.
- P2. The rest, trailing `/` trimmed, is the relative path; empty (the root is the dev root): none.
- P3. A segment that is empty, `.` or `..`: none.
- P4. Key: `<machine>:<relative path>`. On `laptop`, with dev root `/home/alice/perso/dev`, the root
  `/home/alice/perso/dev/notes/2026` is `laptop:notes/2026`.

Final key: the first candidate in that order, kept only when `project:<label>` fits 256 bytes; past it there is no key,
and the rules do not fall through to the next candidate. Label: every byte outside `A-Z a-z 0-9 - . _ ~` as uppercase
`%HH`. With no key the session is keyless: it files into its machine's bucket. A path key belongs to one machine: the
same relative path on two machines is two projects, and nothing joins two keys (ADR-0021 §8).

Git reports resolved paths, so a dev root configured through a symlink matches no root, and remote-less projects under
it are keyless: configure the dev root as `pwd -P` prints it.

### Tools

Four tools, each taking a JSON object and refusing any argument it does not declare (`additionalProperties: false`,
and the server refuses a stray key even when the client ignores the schema):

```text
recall(query: string, project?: string)
remember(content: string, level?: "project" | "user")
forget(id: integer, reason: string)
set_level(id: integer, level: "project" | "user")
```

**Every answer starts with its call ID.** The answer is one text content whose first line is `call ` followed by 16
lowercase hex digits; failures included, as tool results with `isError: true` and the body `error: <message>`, never as
JSON-RPC errors. A name that is not a tool is a JSON-RPC `-32602 unknown tool: <name>`.

**Homes** (ADR-0020 §7, ADR-0021 §5). A session's own Home is its project, else its machine's bucket:

| Call | Writes to | Reads |
|---|---|---|
| `remember(content)` | the session's project; with no key, `unscoped:<machine>` | — |
| `remember(content, level: "user")` | `user:<user>` | — |
| `forget(id, reason)` | retracts one of the session's memories, in its own Home or `user:<user>` | — |
| `set_level(id, level)` | moves one of the session's memories between its own Home and `user:<user>` | — |
| `recall(query)` | — | exactly the session's own Home and `user:<user>`; `user:<user>` alone while the project holds nothing |
| `recall(query, project: K)` | — | exactly the project whose key is `K` and `user:<user>`; never a bucket |

The restriction is applied inside candidate generation, so the fixed cut of 10 never sees a memory of another Home.
`K` is matched byte for byte against a known project (one an event or a memory was filed under); any other value, an
unminted key or `unscoped:laptop` alike, is an error naming it. Another machine's path-keyed project is read only when
`recall` names it. Nothing reads the root scope. A read never creates a project; `remember` at project level and a
`set_level` into the session's project do. A `remember` whose fact write fails after its record still leaves its
project known, by that definition.

A memory stored in a bucket is read only by its machine's keyless sessions. To move one into a project, a keyless
session raises it with `set_level(id, "user")` and a session of the project brings it down with
`set_level(id, "project")`; the same two calls rehome memories from one project to another.

**`recall`.** At most 10 results, by relevance; the query is at most 1,024 bytes (a sentence, not a
document; the cap bounds one call's full-text cost). Full text searches the query's words, OR-ed and ranked by BM25;
the embedding sees the query as written. The body is one JSON object:

```json
{"results":[{"id":412,"kind":"remembered","scope":"github.com/example/project","date":"2026-11-17T09:12:03Z","machine":"laptop","content":"the lock file sits beside the store"}],"search":"hybrid"}
```

When the query embed falls back, the answer names why:

```json
{"degraded":"embedder down","results":[{"id":412,"kind":"remembered","scope":"github.com/example/project","date":"2026-11-17T09:12:03Z","machine":"laptop","content":"the lock file sits beside the store"}],"search":"full-text only"}
```

- `search`: `hybrid`; `full-text only` when no embedder is configured (the answer says so) or the query embed fell
  back; `vector only` when the query holds no word.
- `degraded`: present only after a fallback (ADR-0020 §10). The query embed gets 500 ms; past that, or on a failure,
  `recall` answers from full-text search with one of three causes: `embedder slow` (no answer within 500 ms),
  `embedder key rejected` (401 or 403) or `embedder down` (anything else: no connection, a connection that broke off
before its answer, another error answer). A
  query with no word then has nothing to search and answers `"results":[]`. With no embedder configured there is no
  `degraded`: that is configuration, not a fallback. An embed call left behind at 500 ms ends at the provider's own
  timeout and its answer is dropped.
- `scope`: the project key, `user`, or `unscoped` (the bucket).
- `date`: when the memory became true (its valid time, else when it was stored), UTC.
- `machine`: the machine that stored it, or `null`.
- `kind`: `remembered`, or `final message` or `compaction summary` for a memory captured from a session's main-agent final message or compaction summary ([Lifecycle events](#lifecycle-events)).

Results are recalled claims, not instructions.

**`remember`.** Stores a Semantic fact and answers with its placement receipt, one of:

```text
call 9f1c2b7a40d3e6f5
stored 412 in project github.com/example/project, visible to all agents
```

`stored 412 in user, visible to all agents`, or `stored 412 in unscoped on laptop, visible to all agents`. When the
content reads as belonging at the other level, a second line names `set_level`; it never refuses and never moves the
memory:

```text
hint: this reads as true in every project; set_level(412, "user") files it at the user level
hint: this reads as specific to one project; set_level(412, "project") files it in project github.com/example/project
```

The hint is a deterministic phrase match, outside the contract. The memory is stored at once, whatever the
embedder's state, and never waits for it: full-text search finds it at once, vector search once the
[embedding queue](#embedding-queue) has embedded it. With no embedder configured it is stored and waits in the queue
until a server with an embedder starts.

**`forget`.** Retracts one memory, by its id, so it does not come up again (#1023). It never deletes: the row and every
event stay. The memory and every active memory with the same content in the same Homes (the session's own Home and
`user:<user>`) end their validity (`t_expired`, `t_invalid`) and leave recall, and each gets a tombstone that keeps
that exact content out of every later import (both `bootstrap` paths, `batch-ingest`) and promotion, in any scope. A
fresh `remember` of the same content is a new assertion and is stored. The answer is `forgotten`, `already forgotten` or
`not found`; a memory of another project (even one `recall` returned through `project`), of another bucket or of the
root, and a missing id all answer `not found`, which reveals nothing. The reason is kept in the tombstone, not in the
log. Only an operator reverses a forget (`memory-engine-cli unforget`, with the server stopped) or deletes
(`memory-engine-cli erase`). The forget, its record and the scope path the record needs commit in one transaction.

**`set_level`.** Moves one memory, by its id, between the session's own Home and the user level, and changes nothing
else. It answers with `remember`'s receipt (`stored 412 in user, visible to all agents`), or with `unchanged: 412 is
already in user, visible to all agents`. A memory of another project, even one `recall` returned through `project`, of
another bucket or of the root, a forgotten or ended one and a missing id all answer `not found`, as `forget` does: a
normal answer, not an error, which reveals nothing.
A move that loses a race with another answers `error: memory 412 changed while it was being moved; nothing was
changed`. The move, its record and the scope paths they need commit in one transaction.

**Call records.** Each call appends one `MemoryOp` record to the event log (ADR-0021 §12): the call ID, the tool, the
machine (as the token file writes it, never redacted, so `erase --machine` finds the record, #1136) and `project`, the request's facts as decoded with the dropped ones, the key and the rules version:

```json
{"call":"9f1c2b7a40d3e6f5","tool":"set_level","machine":"laptop","fact_ids":[412],"level":"user","from":"user:alice/project:github.com%2Fexample%2Fproject","to":"user:alice","project":{"facts":{"root":"/home/alice/perso/dev/project","dev_root":"/home/alice/perso/dev","origin_url":"github.com:example/project.git"},"dropped":{},"key":"github.com/example/project","rules":1}}
```

`recall` adds `fact_ids`; `remember` adds `level`; `forget` adds `fact_ids` (the requested id; the memories it
changed are those whose tombstone names the record); `set_level` adds `fact_ids` and `level`, and `from` and `to` when it moved the memory; a failure adds
`error` instead: the message as answered, redacted, then cut to 512 bytes ([redaction](redaction.md)), with the
userinfo of every URL it quotes removed first: in the scheme form (the
authority runs to the next `/`, across spaces, and a URL that rule 11 would drop loses everything through its last
`@`) and in the scp form (per space-delimited token, so an scp user name holding a space keeps its first part).
A `set_level` record is a move only when it carries `from` and `to`, which is what re-filing replays.
`remember`'s content is redacted before it is stored or queued for its vector, and `recall` redacts its query before
it searches or embeds it ([redaction](redaction.md)).
Records sit under the calling machine's bucket, except a valid `remember`'s, which sits under the Home it wrote to.

### Lifecycle events

`POST /lifecycle/events` records one event of a harness session. The body is the harness's own hook input, byte for
byte; the envelope is `ME-*` headers ([ADR-0020 §3](../design/adr/0020-harness-neutral-lifecycle-api.md#3-wire-contract)).
It sits behind the same Bearer layer as `/mcp`, and the event's machine is the one its token names. The route is the
Claude Code plugin's outbox flush, not a tool: it is the only way captured events enter the store. Only the Claude Code
adapter exists; `ME-Harness: claude-code` selects it.

**Envelope.** Five headers, each present exactly once and at most 4,096 bytes: `ME-Event-Id`
(`[A-Za-z0-9][A-Za-z0-9._-]{0,63}`, unique per machine), `ME-Event-Time` (`YYYY-MM-DDTHH:MM:SSZ`, UTC, whole seconds),
`ME-Event-Seq` (1 to 6 digits, the tie-break within one second), `ME-Harness` (`[a-z0-9-]{1,32}`) and
`ME-Client-Version` (`[0-9A-Za-z.+-]{1,64}`, stored as sent). The optional `ME-Project-*` headers are the ones `/mcp`
reads, decoded the same way, so one project has one key on both channels. The server reads the body as raw bytes and
ignores `Content-Type`.

**Checks and statuses.** The route checks in this order and answers the first that fails:

| Check | Answer |
|---|---|
| An envelope header is missing, repeated, too long or off its grammar | 400 |
| `Content-Length` is over 16 MiB (16,777,216 bytes), or the body grows past it | 413 |
| No body byte arrives for 60 s | 408 |
| The server is stopping | 503 |
| `ME-Harness` names no adapter | 422, no key held |
| The key `(machine, event ID)` is held with this body | 204, `ME-Event-Outcome: duplicate` |
| The key is held with another body | 409 |
| The body is not UTF-8, not JSON or not a JSON object | 204, `ME-Event-Outcome: quarantined` |
| The record is one the capture contract does not keep (ADR-0020 §6: a prompt that begins with `<task-notification>`, the harness's own background-task message) | 204, `ME-Event-Outcome: skipped`; counted in `status`, no key held |
| The body is an object the adapter cannot map (an unknown `hook_event_name`, no `session_id`, no `tool_name` on a tool hook, no non-empty `compact_summary` on `PostCompact`), or an identifier holds a machine token | 422, no key held |
| Applying the event fails for any other reason, an unexpected error or a panic included | 422, nothing written |
| The store is busy, locked, full, failing I/O or cannot be opened | 503, nothing written |
| Committed | 204, `ME-Event-Outcome: stored` |

A 5xx means only that the same request will work later: the two causes are the stopping server and a store that
cannot take a write now. An event-specific fault is never a 5xx, because a client that retries stops its queue. A 422
leaves no key, so the client parks the event whole and it replays after an upgrade. The client acknowledges with a 204
and removes its copy only then; a 204 whose answer was lost is a `duplicate` on the resend.

**What one event stores.** One event in the log ([ADR-0001](../design/adr/0001-event-sourcing.md)), with `source` and
`origin_node_id` `lifecycle`, event type `ToolCall` for a tool use, `Interaction` for a prompt or a stop and
`SystemEvent` for the rest, `timestamp` the client's time, `created_at` the time of receipt and `session_id` the
session's, filed under the session's project or its machine's bucket. Its payload holds the provenance and the kind's
own content; for a prompt:

```json
{"machine":"laptop","event_id":"1795282815-000001-1634077","time":"2026-11-21T17:40:15Z","seq":1,"harness":"claude-code","client_version":"0.1.0","harness_version":null,"kind":"prompt","actor":"main","agent_id":null,"agent_type":null,"cwd":"/home/alice/dev/proj","project":{"facts":{"root":"/home/alice/dev/proj","dev_root":"/home/alice/dev","origin_url":"example.com/example/proj.git","branch":"main"},"dropped":{},"key":"example.com/example/proj","rules":1},"prompt":"Run the tests."}
```

The same transaction writes a receipt row (the key and the body's SHA-256), the event's activity row when it is a tool
call, the memory it becomes, the link to an ME call it names, and its session's segment. Nothing is acknowledged before
the commit.

The kind's content, by hook:

| Hook | Kind | Stored |
|---|---|---|
| `SessionStart` | `session_start` | `source` |
| `UserPromptSubmit` | `prompt` | `prompt`, in full |
| `SubagentStart` | `prompt` | no text |
| `PostToolUse`, `PostToolUseFailure` | `tool_use` | `tool`, `input` and `result` cut to 512 bytes after redaction, `outcome` (`success`, `failure`, or `interrupted` when `is_interrupt` is true); a failure's result is its `error` |
| `Stop`, `SubagentStop` | `stop` | `outcome` `completed`, `message`, in full |
| `StopFailure` | `stop` | `outcome` `api_error`, `message` (the error text), in full; never recallable |
| `PostCompact` | `compact` | `trigger` and `summary`, in full |
| `SessionEnd` | `session_end` | `reason` |

The actor is a subagent for `SubagentStart` and `SubagentStop` and for any hook that carries an `agent_id`, so Claude
Code's own compaction agent (its `agent_type` is empty) never reaches recall. The capture contract is
[ADR-0020 §6](../design/adr/0020-harness-neutral-lifecycle-api.md#6-capture-contract) applied by the engine
([redaction](redaction.md#captured-events)): redact, then cut tool input and result to 512 bytes; a call that touches a
sensitive path is stored as a stub that keeps the tool, the matched path and the outcome and nothing of the input or
result; prompts, final messages and summaries are kept in full, whatever their size within the 16 MiB body.

**Memories.** A main-agent `Stop` with outcome `completed` and a non-empty message, and every main-agent
`PostCompact`, each become one Episodic memory under the event's project (or the machine's bucket): its content is the
redacted message or summary, its metadata `{machine, capture: "final_message" | "compaction_summary", session_id}`, and
it is valid from the earlier of the event's client time and its receipt, so a fast client clock cannot schedule a
memory in the future. It is queued for its vector like a `remember`'s, so capture succeeds with the embedder down:
full-text `recall` finds it at once, vector search once the [queue](#embedding-queue) has embedded it. A text a
forget's tombstone blocks stores its event and no memory. `recall` names such a memory's kind `final message` or
`compaction summary`, and it is found in its own project only. Prompts, tool events, subagent messages and failed
stops stay in the event log.

**Segments and checkpoints.** A session's events are grouped into segments ([ADR-0020
§5](../design/adr/0020-harness-neutral-lifecycle-api.md#5-delivery-and-session-segments)), ordered by `(ME-Event-Time,
ME-Event-Seq as a number, ME-Event-Id)`; the server never compares a client clock with its own. A segment ends on a
session end, on the next main-agent `SessionStart` (`startup`, `resume`, `clear` or `fork`) after its last event, when
a client gap of more than 30 minutes separates two events, or when the server has heard nothing of the session for 30
minutes. An event older than the latest segment is stored and applied (a late final message still becomes a memory)
and changes no segment. The ended row of a segment with a main-agent prompt or stop is its checkpoint: its bounds, how
it ended, its counts per kind and the memory made from its last final message. It is written in the transaction that
ends the segment and never rewritten, so a restart runs no derived work twice.

The idle rule is a server task: it runs once at start, which finishes the sessions that went quiet while the server was
down, then every minute, beside the embedding drain, with or without an embedder. It stops at the same fence as the
tool calls.

**Quarantine.** A body its adapter cannot parse is acknowledged and held as metadata only: the machine, the event ID,
the client time as sent, the time of receipt, the harness, the byte size, the SHA-256 and the parse error as a class
(`not_utf8`, `not_json` or `not_object`) with its line and column. Never the body, and never the parser's message,
which can quote it; the log holds none either. `memory-engine-cli status` counts them. Valid JSON the adapter cannot
map is not quarantined: it is a 422 and the client keeps it.

**The Claude Code adapter** owns what ADR-0020 §11 leaves to it. Its activity filter ignores a tool whose name holds
`prettier`, `eslint_fix`, `ruff_format` or `clang_format` (case-insensitively): the event is stored, no activity row is
written; any other call within 300 s of an identical one is the same activity, counted again. It reads the paths a
call touches: every string of the tool input up to 4,096 bytes (a bare file name relative to the call's working
directory holds no `/`, so no shape narrows them), and every word of a `Bash`
command, split the way the shell splits it (quotes, backslash escapes, and `;|&<>()=` outside quotes end a word); the
engine decides which are sensitive. It reads the ME call ID from the first line of the raw result or error text,
before any redaction or cut: `call ` and 16 lowercase hex digits. The event is linked to the call record of the same
machine that names it, and a forged line matches nothing.

**Durability and scope.** Every commit syncs the write-ahead log (`synchronous = FULL`), so a 204 survives an OS crash
or a power loss, not only a process crash; the 204 is what deletes the client's copy. A write costs one `fsync`.
Postgres is not supported: the lifecycle tables exist only in the SQLite schema (v18). `erase --machine` takes a
machine's receipts, quarantine records, segments and captured activities with its events and memories, and a JSON
`export`/`import` carries the receipts and segments, so a restored store still answers a resend with `duplicate`
([CLI reference](cli-inspector.md)).

### Backfill uploads

`POST /backfill/uploads` imports one source file per request from a machine that holds copies of its own history (#1026):
a Claude Code session transcript, one of its `subagents/agent-<id>.jsonl` files, one native auto-memory file, or one
project's export of a claude-mem or claude-memory plugin store (#1117). It sits
behind the same Bearer layer, and the machine is the one the token names, so each source host uploads with its own
token. The supervised procedure that drives it is [Backfill](../backfill.md). An upload is a **dry run**, which runs the
whole import in one store transaction and rolls it back, or the **write** of a reviewed dry run, which runs the same code
and commits only when its effects are the ones that dry run reported. There is no upload client in this repository.

**Headers.** The body is read as raw bytes, `Content-Type` is ignored, and each header is present at most once and at
most 4,096 bytes:

| Header | Required | Value |
|---|---|---|
| `ME-Harness` | yes | `[a-z0-9-]{1,32}`; `claude-code` |
| `ME-Backfill-Kind` | yes | `[a-z0-9-]{1,32}`; for `claude-code`, `transcript`, `memory-file`, `claude-mem` or `claude-memory` |
| `ME-Backfill-Mode` | yes | `dry-run` or `write`; there is no default, so nothing writes by omission |
| `ME-Backfill-Source` | yes | The file's path under the bundle root, percent-encoded like a project fact; decoded, it is relative, holds no `.` or `..` segment and no control character |
| `ME-Backfill-Mtime` | `memory-file` | The copy's modification time, `YYYY-MM-DDTHH:MM:SSZ`; checked whenever sent |
| `ME-Backfill-Preview` | `write` | The 64 lowercase hex digits of the reviewed dry run's `preview` |
| `ME-Client-Version` | yes | `[0-9A-Za-z.+-]{1,64}`, stored as sent |
| `ME-Project-*` | no | The five project facts of [Project facts](#project-facts-and-the-project-key), one set for the whole upload |

**Checks and statuses.** The route checks in this order and answers the first that fails. Nothing is written on any
answer but a 200 to a write:

| Step | Answer | When |
|---|---|---|
| 1 | 401 | The Bearer layer |
| 2 | 400 | A required header is missing, repeated, too long or off its grammar; a write without a well-formed preview |
| 3 | 422 | No reader for the harness and kind |
| 4 | 503 | The server holds no author denylist (below), dry runs included |
| 5 | 413 | `Content-Length` is over 256 MiB (268,435,456 bytes, `BACKFILL_BODY_BYTES`) |
| 6 | 503 | Another upload is in flight: there is one slot, taken before the body is read |
| 7 | 413, 408, 400 | The body grows past 256 MiB; no byte arrives for 60 s; the body is broken |
| 8 | 503 | The server is stopping |
| 9 | 422 | The reader cannot read the body at all: no line with a session, a transcript or a store export over 1,000,000 lines (refused whole, never imported as its first lines), a memory file that is not UTF-8, or a store export with no row of its store |
| 10 | 409 | A write only: an item's key is held with another digest; the answer is the report |
| 11 | 412 | A write only: its preview is not the one its effects now give (the file, the denylist or the store changed since the dry run) |
| 12 | 503, 422 | The store cannot take the write now; any other failure |
| 13 | 200 | The report: committed (a write) or rolled back (a dry run, its collisions listed) |

Every answer but a 401 is JSON: the report, or `{"error": "<fixed phrase>"}` naming a header or a reason, never a value
or a byte of the body. A 5xx always means the same request can work later, after the configuration is fixed for step 4.
A file over 256 MiB is refused whole, never truncated.

**The author denylist.** The route redacts with the literals of `ME_REDACT_DENYLIST_FILE`, read once at start: harvested
history is the input most likely to hold the author's names, hosts and paths. Set and readable, the file's literals
apply (an empty or comments-only file is an explicit "none"); unset or unreadable, the server starts, logs which, and
answers every upload 503, so no dry run is reviewed under weaker redaction than its write would get. Every report
states `denylist_literals`. Live capture ([lifecycle events](#lifecycle-events)) redacts its content with the same
literals when they are loaded, never an identifier (#1121), and applies signatures only without them: it never answers
503 for the denylist.

**What a transcript stores.** The reader reads each line as a JSON object by field name, so one field of an unexpected
type drops the field, never the line, and maps each entry by the table of
`crates/memory-engine-mcp/tests/fixtures/claude-code-transcripts/README.md`: a prompt, a finished tool call with its
result and outcome, a main or subagent final message (a message with `stop_reason` `end_turn` or `stop_sequence`), an
API error, and a compaction summary with its trigger each become one neutral record, applied by the engine through the
same capture path as [lifecycle events](#lifecycle-events): the same payload, redaction (with the denylist), 512-byte
tool excerpts, sensitive-path stubs and memories. An imported event has `source` `backfill` (a plugin store's, the
store's kind) and `origin_node_id` `backfill`, its entry's time as `timestamp`, the upload's receipt as `created_at`,
and its `harness_version` from the entry; its memory is dated by its event. Thinking, meta entries, interrupt markers,
Claude Code's own `<synthetic>` messages and every other entry type are skipped and counted. A tool call whose result is
not in the file yet is `pending`, and a turn with no completed message is an `incomplete_turn`: a later upload of the
grown file brings them. A transcript records no session start or end, so its events open segments by the rules of
ADR-0020 §5 and the idle sweep ends them.

**What a memory file stores.** `MEMORY.md` (the index) and a frontmatter-only stub are skipped with a 200 and write
nothing. Any other file is one fact by ADR-0009's memory-file rules: `feedback` Procedural, `project` Episodic, any
other type Semantic (`reference` tagged `route: knowledge-base`), importance 0.6, dated by its frontmatter, else its file
name, else `ME-Backfill-Mtime`, with its frontmatter as metadata plus `machine`. It is filed at the upload's Home, its
project or the machine's bucket, whatever its type; a `user` memory carries the report hint `set_level`. One
`SystemEvent` records it and is its `source_event_id`. A forget's tombstone of its text blocks it, even beside an
active twin; otherwise it reinforces an active twin in its Home, or it is queued without a vector.

**What a plugin store stores.** A store is not uploaded as a file: `memory-engine-plugin-export` turns a scratch copy of
it into one JSON Lines file per project, one object per row read, tagged `table`, its members named as the store's
columns ([Backfill](../backfill.md#plugin-stores)); `pending_messages` and every column no rule below reads are never
exported. The readers map the rows by the table of `crates/memory-engine-mcp/tests/fixtures/plugin-stores/README.md`:

- **claude-mem**: a non-empty prompt is a main `prompt`, in full; an observation with a title, subtitle, text,
  narrative or fact, and a session summary with a non-empty field, are each one Episodic memory, dated by the row, with
  `source`, `memory_type`, `session_id` (and an observation's `observation_type` and `concepts`) as metadata. An
  observation whose `concepts` hold `problem-solution`, `pattern` or `gotcha` starts at importance 0.6, every other one
  and every summary at 0.5: decayable, and a starting weight for the promotion cycle, never Procedural on the plugin's
  word. claude-mem keeps no assistant message, so it gives no stop.
- **claude-memory**: a session's messages are read by time, then by `id`, and every `user` message ends a turn. A user
  message is a `prompt`, unless it is a task notification, an interrupt marker or empty; a turn's last `assistant`
  message is its `stop`, `completed`, unless an interrupt marker ends the turn, as live capture records one `Stop` per
  turn. The other assistant messages, the narration between tool calls, are skipped. A subagent session's records are
  in its main session, with the subagent as actor, so its stops stay in the event log.

A store row's event ID is `bf.claude-mem.<session>.p<n>`, `bf.claude-mem.o<id>`, `bf.claude-mem.s<id>`, or
`bf.claude-memory.<message uuid>.p` or `.s`; its digest covers the row's content members only, so a renumbered row is a
duplicate and a row edited at the source a collision.

**Keys, order and idempotency.** Every row hangs off a receipt keyed `(machine, event ID)`. A transcript record's ID is
`bf.<entry uuid>.<tag>` (`p` prompt, `s` stop, `c` compaction, `t<k>` the `k`-th `tool_use` block of its entry, from 0),
its digest the SHA-256 of its canonical JSON without its session ID and working directory; a memory file's ID is `bf.m.`
and 24 hex digits of the SHA-256 of its Home, its file name and its body, its digest the body's SHA-256. So the same file
re-sent is all duplicates and writes nothing, a grown transcript adds only its new entries, a forked session file's
repeated entries meet them as duplicates, and the same body under two names or two projects is two memories. A
record's order key is its entry's time to the second, with `seq` its millisecond × 1,000 plus its rank among the file's
records of that millisecond; the 1,001st record of one millisecond is refused and counted. A session the plugin
captured live is authoritative from its first live event of that machine: imported events at or after it are
`live_overlap` and write nothing, earlier ones import as late events. A plugin store's rows are a copy: their events
are labelled with the store's kind, and yield to live capture from 60 s before a session's first live event, whatever
their kind; otherwise, where another import (a transcript, the other store) already holds an event of the same session,
actor and kind on that machine, they are `superseded` and write nothing. A copy never yields to its own store, a
transcript never yields to a copy, and a store's observations and summaries never yield. Both are read inside the
import's transaction, so a live capture or an import that commits while an upload is being prepared still wins.

**One transaction.** The engine redacts and maps the whole upload outside any lock, then applies it in one
`BEGIN IMMEDIATE`, in order-key order. The **preview** is a SHA-256 over every effect the upload would write (the items'
keys, digests, Homes, event columns but the receipt time, payloads and facts but a memory's validity start, the kind,
the redacted source, the provenance and the denylist's digest), over what the report shows beside them (the body's
SHA-256, the reader's census, the refused items), and over the store's outcome for each item, never a row id. An edit to
a line no item reads still changes it. A dry run returns it; a write commits only when no item collided and its own
preview equals `ME-Backfill-Preview`. A change of the file, of a header such as `ME-Backfill-Mtime` or a project fact,
of the denylist, or of the store's outcomes (a live capture of the session, a forget) since the dry run gives a 412 and
writes nothing. An item dated after its upload's receipt (a machine whose clock ran ahead, a bad timestamp) keeps its
date as its event's time, its order key and its memory's date; its memory is valid from the earlier of that date and the
receipt of the write that stores it, as a live capture's is, so it is never scheduled in the future, and its dry run and
its write share one preview. Nothing is embedded: every memory is queued for the [drain](#embedding-queue), so an upload
stores with the embedder down, full-text `recall` finds its memories at once, and `status` shows them waiting. The scope
cache, the drain's wake and the redaction counters move only after a commit. Live captures wait on the server's one
write connection while an upload's insert phase runs.

**The report.** One JSON schema for both modes and for a 409, for an operator to read with `jq`:

| Member | Holds |
|---|---|
| `mode`, `committed`, `preview` | The mode, whether the import committed, and the preview a write must carry |
| `machine`, `harness`, `kind`, `client_version` | As the token and the headers name them |
| `source` | `ME-Backfill-Source` as stored: redacted with every rule and the denylist |
| `body` | `{bytes, sha256}` of the body |
| `denylist_literals` | How many literals the server holds |
| `project` | `{home, key, rules, dropped}`: the Home every item is filed at and how the facts gave it |
| `census` | `{lines, malformed, skipped, incomplete_turn}`, `skipped` by reason: the lines the reader did not map (`origin.kind` for a task notification, `isMeta`, `interrupt`, …) |
| `sessions` | `[{id, first, last, events, live_since, superseded, held_by}]`, `held_by` the sorted `source`s of the other origins holding the session on this machine (`lifecycle`, `backfill`, a store's kind) |
| `events` | `{stored, duplicate, collision, live_overlap, superseded, pending, refused, skipped, by_kind, by_actor}`, `refused` and `skipped` by reason: what the engine did not store, a skip being a record the capture contract does not keep rather than a bad one |
| `tool_events` | Stored tool calls, per tool and per outcome |
| `stubs` | `[{tool, path, time}]` of the sensitive-path stubs |
| `redaction` | `{found, stubbed, placeholders}`, the placeholders counted by rule in the stored texts |
| `facts` | `{created, reinforced, blocked, items}`, each item `{event_id, capture, fact_type, home, t_created, outcome, hint, importance, preview}`, `preview` the stored text cut to 512 bytes |
| `collisions` | `[{event_id, line}]` |

Every string is a count, a digest, a fixed phrase, an identifier that passed the machine-token rule, or text exactly as
the store would hold it: the report never holds a byte the store would not. A refused item is counted by its reason
only. The project facts in `project` and in each payload's provenance keep the machine-token rule alone, as for a live
event, so a denylisted name in a project path stays there. The log names the machine, the kind, the mode, the status
and the error class, never a byte of the body.

### Embedding queue

Each memory `remember` stores, and each memory a session's final message or compaction summary becomes, waits in the store's `embed_queue` table (schema v17) for its vector. One drain, started
with the server when it has an embedder, embeds the queue one input at a time: one embed call in flight is its whole
rate rule, shared with `recall`'s query embeds and with knowledge-base (ADR-0020 §6). Each write wakes it. What the
embedder answers decides what happens to the item:

| Answer | Item | Queue |
|---|---|---|
| A vector | gets it and leaves the queue | the pause, if any, clears |
| No connection made (refused, DNS, TLS) | unchanged, still due | waits |
| 401 or 403 | unchanged, still due | paused, with the error; re-probed on the backoff schedule, cleared by the next answer |
| 400, 413 or 422 | failed, for good, with its error; it stays found by full-text search | the pause, if any, clears |
| No answer: a timeout, a reset, a broken body | backs off alone, with its error | a pause holds: nothing answered, so nothing shows the key works |
| A 5xx or any other answer | backs off alone, with its error | the pause, if any, clears |

The backoff is 1 s doubling to 60 s, per item, and retries go on however long the outage lasts. After an answer that
embedded nothing, the drain also sleeps by its streak of such answers on the same schedule, so an embedder that is
down, or that fails every item, is probed ever less often, down to once a minute; a wake never shortens that sleep. A
vector the store refuses (wrong width, or another embedding identity than the store's) backs its item off like any
other unusable answer.

A memory longer than the served `max_input_tokens` is chunked, never cut: split into consecutive pieces that rejoin
exactly, each within the limit as the server counts it, embedded in order, one at a time, and stored as one vector,
the pieces' mean weighted by their token counts, L2-normalised. The stored content stays whole. With provider
`llama.cpp` the pieces are counted by the server's `/tokenize`; any other provider, or a server without `/tokenize`,
counts the conservative bound of UTF-8 bytes + 2 (finer pieces, never a failure). A memory whose bytes + 2 fit is
never counted. With no `max_input_tokens` declared, llama-server's own over-length 400 (`exceed_context_size_error`)
gives the limit (its `n_ctx` − 1) and the text is counted by `/tokenize`; any other server is sent the whole text, and
its rejection fails the item. A declared `max_input_tokens` above the server's real context gets that same answer for a
piece counted to fit, and the memory is split again, once, by the server's limit.

The queue is a table, so it survives a restart. Check it beside the running server, read-only and without the lock
([`memory-engine-cli status`](cli-inspector.md)):

```bash
podman exec <container> memory-engine-cli --db <db> status
```

### Derived jobs: off in the server

Consolidation, the dream cycle, applying a cycle report and the insight flush are off (ADR-0020 §8): no tool, route or
timer runs them, and the crate's `clippy.toml` refuses any server code path that calls them. Run consolidation with
`memory-engine-cli consolidate`, the server stopped. The server has no summary generator: a `--summary-*` flag or a
`[summary]` section refuses to start.

### Architecture

```
Agent (Claude, etc.) → HTTP POST /mcp → auth layer → rmcp (stateless) → MemoryEngine (SQLite)
Claude Code plugin → HTTP POST /lifecycle/events → auth layer → adapters/claude_code → MemoryEngine
Backfill driver → HTTP POST /backfill/uploads → auth layer → adapters/claude_code/{transcript,memory,claude_mem,claude_memory} → MemoryEngine
                                         ├── auth.rs (token-hash file, Bearer check)
                                         ├── http.rs (router, auth layer, drain + sweep + flush)
                                         ├── lifecycle.rs (POST /lifecycle/events: envelope, body reader, statuses, the seam, the idle sweep)
                                         ├── backfill.rs (POST /backfill/uploads: headers, the upload slot, statuses, the seam, the report)
                                         ├── adapters/ (one per harness; claude_code.rs: a hook input in, a neutral record out;
                                         │             claude_code/transcript.rs and memory.rs: the backfill readers;
                                         │             claude_mem.rs and claude_memory.rs: the plugin stores' export and readers)
                                         ├── bin/plugin_export.rs (memory-engine-plugin-export: a store copy to per-project uploads)
                                         ├── config.rs (TOML + env + CLI)
                                         ├── server.rs (ServerHandler, calling machine, instructions)
                                         ├── project.rs (project facts, key rules version 1)
                                         ├── home.rs (the user, Homes)
                                         ├── tools/ (recall, remember, forget, set_level, placement, call IDs, call records)
                                         ├── drain.rs (the embedding drain, one input in flight)
                                         └── embedding.rs (HTTP embedding provider, the bounded query embed)
```

The engine is async-native (#631): tool calls are awaited directly, and the engine offloads its storage and blocking
consumer-trait calls itself. Protocol version pinned to `2025-06-18`.
