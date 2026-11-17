# MCP Server (`memory-engine-mcp`)

## What

An MCP server binary that serves memory-engine over streamable HTTP (stateless `POST /mcp`, JSON responses), behind
per-machine Bearer tokens, and gives the model four tools: `recall`, `remember`, `forget` and `set_level` (ADR-0020 §8). Part of
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
| `--user` (required) | `MEMORY_MCP_USER` | The owner: the user level is `user:<user>` (ADR-0020 §7). A lowercase DNS label |
| `--allowed-host` (repeatable) | `MEMORY_MCP_ALLOWED_HOSTS` (comma list, no spaces) | A `Host` value served besides loopback |
| `--backup-dir` | `MEMORY_MCP_BACKUP_DIR` | Overrides `engine.backup_dir` |
| `--embed-dim` | `MEMORY_MCP_EMBED_DIM` | Overrides `engine.embed_dim` (lets a blank store start without a TOML file) |
| `--embed-url`, `--embed-model`, `--embed-api-key`, `--embed-provider` | `MEMORY_MCP_EMBED_URL`, `_MODEL`, `_API_KEY`, `_PROVIDER` | Override the `[embedding]` keys of the same meaning |
| `--embed-query-instruction` | `MEMORY_MCP_EMBED_QUERY_INSTRUCTION` | Overrides `query_instruction`; `''` for a symmetric model |
| `--embed-artifact-sha256`, `--embed-pooling`, `--embed-normalization`, `--embed-max-input-tokens` | `MEMORY_MCP_EMBED_ARTIFACT_SHA256`, `_POOLING`, `_NORMALIZATION`, `_MAX_INPUT_TOKENS` | Override the served-artifact keys (below) |

### Configuration

```toml
[engine]
db_path = "/path/to/memory.db"
embed_dim = 384  # Optional — probed from existing DB if omitted
backup_dir = "/path/to/backups"  # Optional — an existing directory; a migration first writes <db>.v<N>.bak there

[embedding]
endpoint = "http://localhost:11434/v1/embeddings"
model = "nomic-embed-text"
api_key = "sk-..."   # Optional — for authenticated endpoints
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

The four served-artifact keys (`artifact_sha256`, `pooling`, `normalization`, `max_input_tokens`) are all set or all absent; `artifact_sha256` is 64 lowercase hex digits. They are declared, never probed: the deployment checks the served file's SHA-256 at install.

#### Asymmetric models (TEI / Qwen)

Models like `Qwen/Qwen3-Embedding-0.6B` take a query-only instruction prefix and support Matryoshka (MRL) truncation. `recall` embeds its query with `embed_query` (prefix applied); `remember` embeds documents prefix-free.

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

An absent served artifact matches only another absent one, so a store recorded before schema v15 (or without the served keys) refuses a configuration that declares them, and the reverse. The recoveries are to drop the declaration and keep serving the recorded identity, or to start from a blank store. Reconstruction under the new identity is the normative path (ADR-0015 §4) but has no CLI or MCP surface yet (#689). The refused start has already migrated the store to schema v15, which an older binary refuses, so recover with the new binary; rolling back is not one of the recoveries.

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

SIGTERM or SIGINT stops accepting connections, gives open connections 5 s to finish, waits for every tool call
already started, flushes the `<db>.snapshot` sidecar and exits 0. A stop timeout shorter than the slowest call
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
it are keyless.

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
{"search":"hybrid","results":[{"id":412,"kind":"remembered","scope":"github.com/example/project","date":"2026-11-17T09:12:03Z","machine":"laptop","content":"the lock file sits beside the store"}]}
```

- `search`: `hybrid`; `full-text only` when no embedder is configured (the answer says so); `vector only` when the
  query holds no word.
- `scope`: the project key, `user`, or `unscoped` (the bucket).
- `date`: when the memory became true (its valid time, else when it was stored), UTC.
- `machine`: the machine that stored it, or `null`.
- `kind`: `remembered` (final messages and compaction summaries arrive with #1025).

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

The hint is a deterministic phrase match, outside the contract. `remember` needs an embedder; without one it refuses
(`error: no embedder is configured, so nothing can be remembered`).

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
another bucket or of the root, a forgotten one and a missing id all answer `error: not found`, which reveals nothing.
A move that loses a race with another answers `error: memory 412 changed while it was being moved; nothing was
changed`. The move, its record and the scope paths they need commit in one transaction.

**Call records.** Each call appends one `MemoryOp` record to the event log (ADR-0021 §12): the call ID, the tool, the
machine and `project`, the request's facts as decoded with the dropped ones, the key and the rules version:

```json
{"call":"9f1c2b7a40d3e6f5","tool":"set_level","machine":"laptop","fact_ids":[412],"level":"user","from":"user:alice/project:github.com%2Fexample%2Fproject","to":"user:alice","project":{"facts":{"root":"/home/alice/perso/dev/project","dev_root":"/home/alice/perso/dev","origin_url":"github.com:example/project.git"},"dropped":{},"key":"github.com/example/project","rules":1}}
```

`recall` adds `fact_ids`; `remember` adds `level`; `forget` adds `fact_ids` (the requested id; the memories it
changed are those whose tombstone names the record); `set_level` adds `fact_ids`, `level`, `from` and `to`; a failure adds
`error` instead: the message as answered, with the userinfo of every URL it quotes removed: in the scheme form (the
authority runs to the next `/`, across spaces, and a URL that rule 11 would drop loses everything through its last
`@`) and in the scp form (per space-delimited token, so an scp user name holding a space keeps its first part).
A `set_level` record is a move only when it carries `from` and `to`, which is what re-filing replays.
`remember`'s content is stored as the memory, as written: redacting it is #1024's.
Records sit under the calling machine's bucket, except a valid `remember`'s, which sits under the Home it wrote to.

### Derived jobs: off in the server

Consolidation, the dream cycle, applying a cycle report and the insight flush are off (ADR-0020 §8): no tool, route or
timer runs them, and the crate's `clippy.toml` refuses any server code path that calls them. Run consolidation with
`memory-engine-cli consolidate`, the server stopped. The server has no summary generator: a `--summary-*` flag or a
`[summary]` section refuses to start.

### Architecture

```
Agent (Claude, etc.) → HTTP POST /mcp → auth layer → rmcp (stateless) → MemoryEngine (SQLite)
                                         ├── auth.rs (token-hash file, Bearer check)
                                         ├── http.rs (router, auth layer, drain + flush)
                                         ├── config.rs (TOML + env + CLI)
                                         ├── server.rs (ServerHandler, calling machine, instructions)
                                         ├── project.rs (project facts, key rules version 1)
                                         ├── home.rs (the user, Homes)
                                         ├── tools/ (recall, remember, forget, set_level, placement, call IDs, call records)
                                         └── embedding.rs (HTTP embedding provider)
```

The engine is async-native (#631): tool calls are awaited directly, and the engine offloads its storage and blocking
consumer-trait calls itself. Protocol version pinned to `2025-06-18`.
