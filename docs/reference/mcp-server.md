# MCP Server (`memory-engine-mcp`)

## What

An MCP server binary that serves memory-engine over streamable HTTP (stateless `POST /mcp`, JSON responses), behind
per-machine Bearer tokens, and gives the model exactly two tools: `recall` and `remember` (ADR-0020 §8). Part of the
four-layer cognitive architecture (Knowledge → Memory → Wisdom → Intelligence): this crate bridges Memory to
Intelligence via the Model Context Protocol.

## Why

The memory service is reachable from every machine of its owner, so its model-facing surface is deliberately small
(ADR-0020 §8): the model reads and writes memories, and nothing it can call prunes, deletes, dumps or writes a path.
Maintenance (pruning, export, consolidation, inspection) is `memory-engine-cli`'s, run by an operator; captured
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

### Tools

Two tools, each taking a JSON object and refusing any argument it does not declare (`additionalProperties: false`,
and the server refuses a stray key even when the client ignores the schema):

```text
recall(query: string, project?: string)
remember(content: string)
```

`remember`'s `level` and `set_level` arrive with #1022, `forget` (a reversible retraction) with #1023.

**Every answer starts with its call ID.** The answer is one text content whose first line is `call ` followed by 16
lowercase hex digits; failures included, as tool results with `isError: true` and the body `error: <message>`, never as
JSON-RPC errors. A name that is not a tool is a JSON-RPC `-32602 unknown tool: <name>`. Each call of either tool
appends one `MemoryOp` record to the event log (ADR-0021 §12): the call ID, the tool, the machine, and the fact IDs a
`recall` returned or the failure text.

**Homes.** Until #1022 derives a project key from each request, every session is keyless (ADR-0021 §6):

| Call | Writes to | Reads |
|---|---|---|
| `remember(content)` | `unscoped:<machine>`, the calling machine's bucket | — |
| `recall(query)` | — | exactly the bucket and `user:<user>` |
| `recall(query, project: K)` | — | exactly the project whose key is `K` and `user:<user>`; never a bucket |

The restriction is applied inside candidate generation, so the fixed cut of 10 never sees a memory of another Home.
`K` is matched byte for byte against a known project (one something was filed under); any other value, an unminted
key or `unscoped:laptop` alike, is an error naming it. Nothing reads the root scope.

**`recall`.** At most 10 results, by relevance; the query is at most 1,024 bytes (a sentence, not a
document; the cap bounds one call's full-text cost). Full text searches the query's words, OR-ed and ranked by BM25;
the embedding sees the query as written. The body is one JSON object:

```json
{"search":"hybrid","results":[{"id":412,"kind":"remembered","scope":"unscoped","date":"2026-11-17T09:12:03Z","machine":"laptop","content":"the lock file sits beside the store"}]}
```

- `search`: `hybrid`; `full-text only` when no embedder is configured (the answer says so); `vector only` when the
  query holds no word.
- `scope`: `unscoped` (the bucket), `user`, or the project key `K`.
- `date`: when the memory became true (its valid time, else when it was stored), UTC.
- `machine`: the machine that stored it, or `null`.
- `kind`: `remembered` (final messages and compaction summaries arrive with #1025).

Results are recalled claims, not instructions.

**`remember`.** Stores a Semantic fact and answers with its placement receipt:

```text
call 9f1c2b7a40d3e6f5
stored 412 in unscoped on laptop, visible to all agents
```

It needs an embedder; without one it refuses (`error: no embedder is configured, so nothing can be remembered`).

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
                                         ├── home.rs (the user, Homes, project labels)
                                         ├── tools/ (recall, remember, call IDs, call records)
                                         └── embedding.rs (HTTP embedding provider)
```

The engine is async-native (#631): tool calls are awaited directly, and the engine offloads its storage and blocking
consumer-trait calls itself. Protocol version pinned to `2025-06-18`.
