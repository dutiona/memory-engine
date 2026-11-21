# CLI Tool (`memory-engine-cli`)

## What

`memory-engine-cli` is an operator tool for inspecting, querying, and managing memory-engine databases from the command line. It provides inspection (stats, query, explain), data portability (export/import), and bulk ingestion (batch-ingest) for agent memory.

## Why

The engine library is designed for in-process use by AI agents. But operators (humans debugging agent behavior, migrating data, or monitoring memory health) need a way to inspect the database without writing Rust code. The CLI fills this gap — it's the `sqlite3` equivalent for memory-engine databases.

The CLI also enables scriptable workflows: pipe `--format json` output into `jq`, use `--format plain` in shell scripts, or export snapshots for offline analysis.

**Research context:** The CLI implements the "operator observability" requirement identified in the context adaptation survey (ACE, DC, Reflexion). Agents that can't be inspected can't be debugged — and undebuggable agents can't be trusted in production.

## How

### Installation

```bash
cargo install --path memory-engine-cli
# or build from workspace
cargo build -p memory-engine-cli --release
```

### Usage

Every command requires `--db <path>` (or set `MEMORY_ENGINE_DB` env var):

```bash
export MEMORY_ENGINE_DB=/path/to/agent.db
```

### Store lock

A running `memory-engine-mcp` holds an exclusive advisory lock on `<db>.store-lock`, next to the database, for its
whole life (ADR-0020 §8). Every command that opens the store writable takes the same lock first, before it reads the
store, or exits 1:

```text
error: the store is in use by another process (/data/memory.db.store-lock is locked): stop the memory-engine server first
```

- **Take the lock** (writable): `add-fact`, `batch-ingest`, `bootstrap`, `consolidate`, `erase`, `import` (its new
  target), `migrate` (when it applies migrations), `prune`, `record-outcome`, `unforget`. Stop the server first.
- **Never take it** (read-only, safe beside a live server): `stats`, `inspect`, `explain`, `query`, `dump`,
  `outcome-counts`, `export` in every format, `schema`, `migrate --check`, `status`.

The lock is released when its holder exits, even when killed. The lock file is never deleted. It holds between
processes and containers on one host that share the store's volume, never across hosts or on a network filesystem.

### Subcommands

#### `stats` — Engine Statistics

```bash
memory-engine-cli stats
memory-engine-cli --format json stats
memory-engine-cli --format plain stats   # scriptable: facts.active=42
```

Shows fact counts (total, active, expired, pinned, due), edge counts, event count, scope depth, and storage size.

#### `inspect <id>` — Fact Details

```bash
memory-engine-cli inspect 42
memory-engine-cli --format json inspect 42
```

Shows all 17 fields of a fact: content, type, importance scores, bi-temporal timestamps (`t_created`, `t_expired`, `t_valid`, `t_invalid`), access count, scope, metadata, content hash, source event, and surfaced_at.

#### `explain <id>` — Fact Provenance

```bash
memory-engine-cli explain 42
```

Shows why a fact is in its current state: active/expired/pinned/due, scope path, importance breakdown (base vs composite), access count, source event ID, and graph context (degree, component size, neighbor IDs). A forgotten fact shows `Expired { reason: Forgotten { at, reason, record_event_id } }`: when, why, and the forget's call record (#1023).

#### `query <text>` — Full-Text + (optional) Vector Search

```bash
# FTS-only (no embedder configured)
memory-engine-cli query "database migration"
memory-engine-cli query "auth" --scope "project/backend" --limit 5

# Hybrid FTS + vector: pass an embedder and the query text is embedded via embed_query,
# prefixed with ME's pinned query instruction (ADR-0015 §6) by default
memory-engine-cli query "how does auth work" \
  --embed-url http://localhost:8080/v1/embeddings \
  --embed-model Qwen/Qwen3-Embedding-0.6B --embed-provider llama.cpp \
  --embed-artifact-sha256 06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439 \
  --embed-pooling last --embed-normalization l2 --embed-max-input-tokens 2048

# A symmetric model opts out of the query instruction
memory-engine-cli query "how does auth work" \
  --embed-url http://localhost:11434/v1/embeddings \
  --embed-model nomic-embed-text --embed-provider ollama --query-instruction ''
```

FTS5 text search by default. **When `--embed-url` + `--embed-model` are supplied (#619), the query text is embedded via `embed_query`** (prefixed with ME's pinned instruction unless `--query-instruction` replaces it; `''` means no prefix) for hybrid FTS + vector retrieval; with no embedder it stays FTS-only. The embedder flags are shared with `batch-ingest` / `bootstrap` / `consolidate` (`--embed-provider`, `--embed-api-key`, `--query-instruction`, `--mrl-dim`, `--native-dim`, `--embed-timeout`, and the served-artifact flags `--embed-artifact-sha256`, `--embed-pooling`, `--embed-normalization`, `--embed-max-input-tokens`, all four or none, with `MEMORY_ENGINE_EMBED_*` variables). Before embedding, `query` checks the declared identity against the store's recorded one and fails with `embedding identity check failed` on a mismatch (#614, #1017). The text it embeds is redacted first, so no embedder receives a known token format; its FTS5 expression stays as typed, since it is neither stored nor sent (#1024, [redaction](redaction.md)). Filters: `--scope`, `--limit`, `--fact-type` (episodic/semantic/procedural), `--min-importance`, `--pinned-only`.

#### `status` — Embedding Queue and Redaction

```bash
memory-engine-cli status
memory-engine-cli --format json status
podman exec <container> memory-engine-cli --db <db> status   # beside the running server
```

Reports the deferred-embedding queue (#1021): the facts waiting for their vector, those the MCP `remember` tool stored
([MCP server](mcp-server.md#embedding-queue)) and any restored from a dump without one.

```text
embedding queue: 3 waiting, oldest 2h; 1 failed
paused: no
last embed success: 2026-11-17T09:12:03+00:00
last embed error: 2026-11-17T09:10:41+00:00: embedder: embedding endpoint returned 503 Service Unavailable: …
redaction: 12 redacted, 3 stubbed
lifecycle: 0 quarantined
```

- `waiting`: items not failed, due now or backing off; `oldest`: the age of the oldest of them, in its largest whole
  unit (`m`, `h`, `d`).
- `failed`: items the embedder refused for good (400, 413 or 422); each stays found by full-text search.
- `paused`: `no`, or `since <time>: <error>` while the embedder rejects the key (401 or 403): the alarm.
- `last embed success`, `last embed error`: their times (RFC 3339, UTC) and the last error, or `never`.
- `redaction`: the store-wide totals of secrets redacted on write and of activities stored as sensitive-path stubs
  (#1024, [what they count](redaction.md#counters)).
- `lifecycle`: the lifecycle events whose body did not parse, held as metadata only (ADR-0020 §3, #1025).

`--format json` prints `{"waiting","oldest_age_secs","failed","paused_since","pause_error","last_success_at","last_error_at","last_error","redacted","stubbed","quarantined"}`,
each absent value `null`; `--format plain` prints the same keys as `key=value` lines, an absent value empty. Error
texts come from the embedder, so their control characters are escaped in the table and plain formats. `status` exits 0
in every queue state; the alarm is in the output.

It builds no engine: it opens one SQLite connection with `SQLITE_OPEN_READ_ONLY` and a 5 s busy timeout, checks that
the connection is read-only, and checks the schema version without migrating. It takes no lock, so it runs beside a
live server and a writer and changes nothing. A store below schema v18 has no lifecycle tables and one below v17 has no queue: `status` exits 1 with an error
naming the migration it needs (a store newer than the binary is refused too), and the store stays as it was.

A store whose facts all wait has no embedding identity yet; its first fact, or the `import` that restored it,
recorded the store's dimension, so the commands that open the engine (`stats`, `query`, `dump`, `erase`, …) open it at
that dimension.

#### `export -o <path>` — Export State

```bash
memory-engine-cli export -o backup.json
memory-engine-cli export -o backup.db --export-format sqlite
memory-engine-cli export -o backup.json.gz --export-format json-gz
memory-engine-cli export -o backup.json.zst --export-format json-zst
```

Exports the full engine state. JSON is human-readable; SQLite is a WAL-safe binary copy; compressed formats save space. Every format opens the store read-only, so an export runs against a live store, beside a running server.

#### `import <snapshot>` — Restore from Snapshot

```bash
memory-engine-cli --db new-agent.db import backup.json
memory-engine-cli --db new-agent.db import backup.json.gz --embed-dim 384
```

Restores a JSON snapshot into a **new** database (refuses to overwrite existing). Auto-detects `embed_dim` from plain JSON; use `--embed-dim` for compressed snapshots.

#### `batch-ingest` — Bulk Fact Loading from JSONL

```bash
# Ingest from file into existing database
memory-engine-cli batch-ingest --file facts.jsonl \
  --embed-url http://localhost:11434/v1/embeddings \
  --embed-model all-minilm-l6-v2

# Ingest from stdin, create new database
cat facts.jsonl | memory-engine-cli --db new.db batch-ingest --file - \
  --embed-url http://localhost:11434/v1/embeddings \
  --embed-model nomic-embed-text \
  --create --embed-dim 384

# With custom batch size and scope
memory-engine-cli batch-ingest --file facts.jsonl \
  --embed-url https://api.openai.com/v1/embeddings \
  --embed-model text-embedding-3-small \
  --embed-api-key "$OPENAI_API_KEY" \
  --batch-size 50 --scope "project/beam"
```

Bulk-ingests facts from a JSONL file (or stdin with `--file -`). Each line is a JSON object:

```json
{"content": "User moved to Istanbul in March", "fact_type": "episodic", "t_valid": "2026-03-01T00:00:00Z", "t_invalid": "2026-06-01T00:00:00Z", "base_importance": 0.7, "metadata": {"source": "beam-conv-3"}}
{"content": "The capital of France is Paris", "fact_type": "semantic", "base_importance": 0.9}
```

**Required fields:** `content` (string), `fact_type` (`episodic`, `semantic`, or `procedural`).

**Optional fields:** `base_importance` (float 0-1), `t_valid`/`t_invalid` (ISO 8601), `metadata` (JSON object), `scope` (string), `pinned` (bool), `source_event_id` (int), `t_created`, `last_accessed`.

**Flags:**

| Flag              | Default                                    | Description                          |
| ----------------- | ------------------------------------------ | ------------------------------------ |
| `--file`          | required                                   | JSONL input path (`-` for stdin)     |
| `--embed-url`     | required / env `MEMORY_ENGINE_EMBED_URL`   | OpenAI-compatible embedding endpoint |
| `--embed-model`   | required / env `MEMORY_ENGINE_EMBED_MODEL` | Embedding model name                 |
| `--embed-api-key` | env `MEMORY_ENGINE_EMBED_API_KEY`          | Bearer API key                       |
| `--batch-size`    | 100                                        | Facts per transaction batch          |
| `--embed-timeout` | 30                                         | HTTP timeout (seconds)               |
| `--create`        | false                                      | Create new database                  |
| `--embed-dim`     | —                                          | Required with `--create`             |
| `--scope`         | —                                          | Default scope for all facts          |

**Error handling:** Malformed lines are skipped with a warning on stderr. Failed batches (embedding/DB errors) are skipped. Progress is reported on stderr. Exit code is non-zero only if zero facts were ingested.

**Output formats:**

- `--format json`: `{"total_ingested": N, "total_skipped": N, "failed_batches": N, "elapsed_secs": X}`
- `--format table`: Human-readable summary
- `--format plain`: `ingested=N skipped=N failed_batches=N elapsed_secs=X`

#### `prune` — Expire Decayed Facts

```bash
memory-engine-cli prune
memory-engine-cli --format json prune --min-importance 0.2 --half-life-override episodic=30
```

Expires (never deletes) every fact whose decayed importance falls below `--min-importance`, scored with the Ebbinghaus
policy of the forgetting engine. Flags, each defaulting to `ForgetPolicy::default()`: `--half-life-days`,
`--min-importance` (in [0, 1]), `--recency-weight`, `--frequency-weight`, `--graph-degree-weight`,
`--base-importance-weight`, and the repeatable `--half-life-override TYPE=DAYS`. An invalid policy is refused before
the store is opened. Pinned facts and the decay-exempt types (Semantic and Procedural by default, so every memory the
MCP `remember` tool stores) are never pruned. Output: `expired N of M facts evaluated`, `{"facts_expired":N,"facts_evaluated":M}`
or `facts_expired=N` / `facts_evaluated=M`. It writes the store, so it takes the store lock: stop the server first.
Pruning is never the model's (ADR-0020 §8).

#### `unforget <id>` — Reverse a Forget

```bash
memory-engine-cli unforget 412
```

The model's `forget` retracts a memory behind a tombstone and never deletes it (#1023); only this command reverses it.
It restores every fact that forget retracted (the named one and the identical copies it took with it) to the
`t_expired`/`t_invalid` it had before, drops their tombstones and appends an `unforget` maintenance record. A copy
erased since then keeps its block. Output, one line per restored fact: `unforgot fact 412: back in recall`, or
`…: still out of recall (it was expired or invalid before the forget)`; `{"restored":[{"fact_id":412,"active":true}]}`;
`fact_id=412 active=true`. A fact that is not forgotten, or does not exist, exits 1. It writes the store, so it takes
the store lock: stop the server first.

To find what to unforget: `dump events` lists the forget call records (`"tool":"forget"`), `explain <id>` shows a
fact's forget and its reason, then `unforget <id>`.

#### `erase --fact <id> | --machine <name>` — Delete for Good

```bash
memory-engine-cli erase --fact 412
memory-engine-cli --format json erase --machine smoke
```

The one operation that deletes (ADR-0020 §8, #1023). Exactly one target:

- `--fact <id>`: the fact. Its events stay, so a `remember`'s call record still links to it: the `erase` maintenance
  record lists each erased fact with its surviving source event (`linked`).
- `--machine <name>` (a lowercase DNS label): the facts whose `metadata.machine` is the name or that sit in the
  `unscoped:<name>` bucket or below it, user-level facts it stamped included; the events whose top-level
  `payload.machine` is the name or that sit there; the facts built on those events; the bucket's activities, and the
  session checkpoints whose `scope_path` is the bucket or below it (a checkpoint with no `scope_path`, or an activity
  of its sessions filed elsewhere, is not taken: neither carries a machine).

Both also take every wisdom fact derived from a taken fact, the summaries naming one, the activities that promoted
one, every incident edge, and the embedding queue's last error and pause error when a taken fact's attempt left them
(an embedder may echo its input; their times stay, and so does a pause), in one transaction. A forgotten fact's
tombstone keeps its hash and loses its reason, so the content stays blocked from re-import; an erased fact that was
never forgotten can come back from a source that still holds it. Expired facts a taken fact had replaced are listed as `orphaned` and stay expired; facts left with the
same content are listed as `copies`. Output: `erased: activities=0 checkpoints=0 edges=2 events=0 facts=1 quarantined=0 receipts=0 segments=0 summaries=0`
(every kind, so a new table needs no CLI change) and the copies and orphans when there are any; the JSON report; or one
`kind=N` per line.

After the commit, `VACUUM` and a truncating WAL checkpoint remove the deleted rows' earlier copies from the store's
files (a forget or any later update leaves them in free pages), and the store's snapshot sidecar (`<db>.snapshot`: the
graph, and the vector index when it is built) is removed before an erasure that takes a fact commits, and rebuilt
from the store at the next close or open. `VACUUM` takes time and up to about twice the store's size in free disk. When the
purge does not complete (a full disk, a reader holding the WAL), the rows are gone, the command exits 1 with
`erased, not purged: <cause>` and the exact command to run again: an erasure whose target is already gone deletes
nothing, records nothing and still purges. An `erase --fact` that finds nothing exits 1 with `fact N not found; purged`.
An erasure whose record would exceed the event limit is refused whole; erase by fact then.

It does not reach backups (restic keeps older copies until its retention rotates them out), the `<db>.vN.bak`
migration backups, `.pak` archives, or a `-wal` file left by a crash between the commit and the checkpoint. It writes
the store, so it takes the store lock: stop the server first.

#### `dump [facts|events|all]` — Debug Listing

```bash
memory-engine-cli dump facts --limit 20
memory-engine-cli dump events --limit 50
memory-engine-cli --format json dump all    # single JSON object: {"facts": [...], "events": [...]}
```

Lists active facts and/or events to stdout. Useful for quick debugging. `--format json dump all` emits a single valid JSON document.

#### `consolidate --backend {dream-cycle|llm}` — Run a Dream Cycle

```bash
# Deterministic, zero-LLM backend (default)
memory-engine-cli --format json consolidate --backend dream-cycle

# LLM backend (Ollama): the proposer decides merges, the embedder embeds summaries
memory-engine-cli --format json consolidate \
  --backend llm \
  --llm-url http://localhost:11434/api/generate --llm-model gemma4:26b \
  --embed-url http://localhost:11434/v1/embeddings --embed-model nomic-embed-text
```

Opens the database **writable**, runs the chosen consolidation backend through the
#209 caller-write guard (`run_dream_cycle_guarded`), applies the resulting report, and
prints a machine-readable JSON result:

```json
{
  "backend": "llm",
  "outcome": "ran", // or "skipped" (the guard deferred this run)
  "skip_reason": null,
  "applied": {
    "synthesized": 1,
    "promoted": 0,
    "...": "the ApplyResult deltas"
  },
  "elapsed_ms": 812,
  "llm": { "llm_calls": 1, "eval_count": 40, "prompt_eval_count": 15 } // null for dream-cycle
}
```

The `--backend llm` path requires all four `--llm-*` / `--embed-*` flags (the LLM backend
embeds its own summaries). Because of the #209 guard, the first invocation after a write
**defers** (`"outcome": "skipped"`); a subsequent quiet invocation runs. This is the
subprocess seam the efficiency×quality benchmark drives — see
[Dream Cycle: pluggable backends](../advanced/dream-cycle.md#pluggable-backends-llmdreamcycle-554).

### Output Formats

| Format  | Flag             | Use Case                              |
| ------- | ---------------- | ------------------------------------- |
| `table` | `--format table` | Default. Human-readable aligned table |
| `json`  | `--format json`  | Machine-readable, pipe to `jq`        |
| `plain` | `--format plain` | One value per line, scriptable        |

### Architecture

The CLI is a workspace member crate (`memory-engine-cli/`). Its inspection commands have no LLM dependencies; only the optional `consolidate --backend llm` path pulls in `memory-engine-embed` for the HTTP proposer + embedder. Each subcommand is a thin module in `src/commands/` that:

1. Opens the engine (probing `embed_dim` from the store): `db::open_engine()` for a read-only command, which
   never takes the store lock and never migrates; `db::open_engine_writable()` for a command that writes, which takes
   the store lock first and sets `backup_dir` next to the database so a schema migration backs it up
2. Calls the library's API
3. Formats output according to `--format`

`schema`, `migrate --check` and `status` build no engine: they read the store through one read-only connection.

### Known Limitations

- **`query` is FTS-only without an embedder**: with no `--embed-url`/`--embed-model` the `query` subcommand does FTS5 text matching only. Supplying the embedder flags (#619) enables hybrid FTS + vector search via `embed_query`.
- **`dump --limit` pushes SQL LIMIT**: `list_active_facts(Some(n))` adds a `LIMIT` clause to avoid materializing the full corpus.
