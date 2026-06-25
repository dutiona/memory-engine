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

Shows why a fact is in its current state: active/expired/pinned/due, scope path, importance breakdown (base vs composite), access count, source event ID, and graph context (degree, component size, neighbor IDs).

#### `query <text>` — Full-Text + (optional) Vector Search

```bash
# FTS-only (no embedder configured)
memory-engine-cli query "database migration"
memory-engine-cli query "auth" --scope "project/backend" --limit 5

# Hybrid FTS + vector: pass an embedder and the query text is embedded via embed_query
memory-engine-cli query "how does auth work" \
  --embed-url http://localhost:8080/v1/embeddings \
  --embed-model Qwen/Qwen3-Embedding-0.6B --embed-provider tei \
  --query-instruction "Instruct: Given a search query, retrieve relevant memory facts.\nQuery: "
```

FTS5 text search by default. **When `--embed-url` + `--embed-model` are supplied (#619), the query text is embedded via `embed_query`** (applying an asymmetric model's query-instruction prefix) for hybrid FTS + vector retrieval; with no embedder it stays FTS-only. The embedder flags are shared with `batch-ingest` / `bootstrap` / `consolidate` (`--embed-provider`, `--embed-api-key`, `--query-instruction`, `--mrl-dim`, `--native-dim`, `--embed-timeout`) and the declared `--embed-provider` must match the store's recorded identity (#614). Filters: `--scope`, `--limit`, `--fact-type` (episodic/semantic/procedural), `--min-importance`, `--pinned-only`.

#### `export -o <path>` — Export State

```bash
memory-engine-cli export -o backup.json
memory-engine-cli export -o backup.db --export-format sqlite
memory-engine-cli export -o backup.json.gz --export-format json-gz
memory-engine-cli export -o backup.json.zst --export-format json-zst
```

Exports the full engine state. JSON is human-readable; SQLite is a WAL-safe binary copy; compressed formats save space.

#### `import <snapshot>` — Restore from Snapshot

```bash
memory-engine-cli --db new-agent.db import backup.json
memory-engine-cli --db new-agent.db import backup.json.gz --embed-dim 384
```

Restores a JSON snapshot into a **new** database (refuses to overwrite existing). Auto-detects `embed_dim` from plain JSON; use `--embed-dim` for compressed snapshots.

**Note:** Import currently has a known issue with schema version mismatch (v6 vs v5 in restore path, tracked in #80). The test is `#[ignore]` pending library fix.

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

1. Opens the engine via `db::open_engine()` (probes `embed_dim` from config table)
2. Calls the library's inspection API
3. Formats output according to `--format`

The `open_engine()` function sets `backup_dir` adjacent to the database as a safety measure — building a file-backed `MemoryEngine` may run schema migrations. A true read-only open path is tracked in #103.

### Known Limitations

- **`query` is FTS-only without an embedder**: with no `--embed-url`/`--embed-model` the `query` subcommand does FTS5 text matching only. Supplying the embedder flags (#619) enables hybrid FTS + vector search via `embed_query`.
- **`dump --limit` pushes SQL LIMIT**: `list_active_facts(Some(n))` adds a `LIMIT` clause to avoid materializing the full corpus.
- **Import schema version**: Export/import roundtrip fails due to schema v6 vs restore v5 mismatch. Tracked in #80.
