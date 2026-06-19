# MCP Server (`memory-engine-mcp`)

## What

A stdio-based MCP server binary that exposes memory-engine as 26 tools (10 P0 + 5 P1 + 3 P2 + 2 Phase-5a outcome + 3 cognitive + 3 activity stream) for autonomous AI agents. Part of the four-layer cognitive architecture (Knowledge → Memory → Wisdom → Intelligence) — this crate bridges Memory to Intelligence via the Model Context Protocol.

## Why

Autonomous agents need a standard protocol to interact with their memory. Raw library calls aren't accessible from agent runtimes (Claude Code, etc.). The MCP server provides a thin, validated adapter layer that:

- Exposes the 5 memory primitives (ingest, query, consolidate, forget, resolve) as MCP tools
- Shapes responses via tiered depth (sparse/standard/full) to respect agent context window budgets
- Provides a pre-compaction flush endpoint for capturing insights before context compression
- Validates inputs at the transport boundary (the engine trusts its callers; the MCP server doesn't)

**Research basis:** NeuroStack's auto-escalating retrieval pattern (sparse → medium → full), ACC's bounded compressed cognitive state, MCP ecosystem analysis (5 memory servers evaluated).

## How

### Installation

```bash
cargo build -p memory-engine-mcp --release
```

### Usage

```bash
# With TOML config
memory-engine-mcp --config /path/to/mcp.toml

# With CLI args
memory-engine-mcp --db-path /path/to/memory.db --embed-url http://localhost:11434/v1/embeddings --embed-model nomic-embed-text

# With env vars
MEMORY_MCP_DB_PATH=/path/to/memory.db MEMORY_MCP_EMBED_URL=... memory-engine-mcp
```

### Configuration

```toml
[engine]
db_path = "/path/to/memory.db"
embed_dim = 384  # Optional — probed from existing DB if omitted

[embedding]
endpoint = "http://localhost:11434/v1/embeddings"
model = "nomic-embed-text"
api_key = "sk-..."   # Optional — for authenticated endpoints
provider = "ollama"  # Serving backend: ollama | tei | openai (default: ollama). Feeds the fingerprint.
dimensions = 384     # Native model dimension (validated against the raw response)
timeout_secs = 30
```

CLI flags override TOML values field-by-field (e.g., `--embed-api-key` overrides `api_key` in TOML while keeping `endpoint` and `model` from TOML; `--embed-provider` overrides `provider`).

#### Asymmetric models (TEI / Qwen)

Models like `Qwen/Qwen3-Embedding-0.6B` take a query-only instruction prefix and support Matryoshka (MRL) truncation. The MCP query path uses `embed_query` (prefix applied); `memory_add_fact` embeds documents prefix-free.

```toml
[engine]
db_path = "/path/to/memory.db"
embed_dim = 256       # The STORED dimension — must equal mrl_dim below

[embedding]
endpoint = "http://localhost:8080/v1/embeddings"
model = "Qwen/Qwen3-Embedding-0.6B"
provider = "tei"
dimensions = 1024     # NATIVE dim the model emits (validated against the raw response)
query_instruction = "Instruct: Given a search query, retrieve relevant memory facts.\nQuery: "
mrl_dim = 256         # Truncate + L2-renormalize to this; must equal engine embed_dim
```

`mrl_dim` is the stored (post-truncation) dimension and **must equal** the engine's `embed_dim` — the server rejects a mismatch at startup. `dimensions` stays the native pre-truncation length.

### Claude Code Integration

Add to `.claude/settings.json`:

```json
{
  "mcpServers": {
    "memory-engine": {
      "command": "/path/to/memory-engine-mcp",
      "args": ["--config", "/path/to/mcp.toml"]
    }
  }
}
```

### Tools

| Tool                         | Priority  | Purpose                                | Depth                |
| ---------------------------- | --------- | -------------------------------------- | -------------------- |
| `memory_ingest`              | P0        | Append event to log                    | —                    |
| `memory_add_fact`            | P0        | Add fact with embedding                | —                    |
| `memory_query`               | P0        | Hybrid FTS + vector search             | sparse/standard/full |
| `memory_resume_context`      | P0        | 5-tier cognitive boot                  | sparse/standard/full |
| `memory_list_due`            | P0        | Scheduled fact surfacing               | sparse/standard/full |
| `memory_next_due_time`       | P0        | Next scheduled time                    | —                    |
| `memory_explain_fact`        | P0        | Fact provenance                        | sparse/standard/full |
| `memory_get_fact`            | P0        | Single fact by ID                      | sparse/standard/full |
| `memory_statistics`          | P0        | Aggregate stats                        | —                    |
| `memory_flush_insights`      | P0        | Batch pre-compaction capture           | —                    |
| `memory_consolidate`         | P1        | Dedup + cluster facts into summaries   | —                    |
| `memory_forget`              | P1        | Ebbinghaus decay pruning               | —                    |
| `memory_dump_state`          | P1        | Export snapshot (JSON/SQLite)          | —                    |
| `memory_pin_fact`            | P1        | Make fact unforgettable                | —                    |
| `memory_unpin_fact`          | P1        | Allow forgetting a pinned fact         | —                    |
| `memory_replay_events`       | P2        | Replay the event log                   | —                    |
| `memory_fact_history`        | P2        | Bi-temporal history of a fact          | —                    |
| `memory_bootstrap_session`   | P2        | Seed a session from prior memory       | sparse/standard/full |
| `memory_record_outcome`      | Phase 5a  | Record an outcome for a fact           | —                    |
| `memory_outcome_counts`      | Phase 5a  | Aggregate outcome counts               | —                    |
| `memory_dream_cycle`         | Cognitive | Run the DreamCycle (`apply` flag)      | —                    |
| `memory_apply_cycle_report`  | Cognitive | Apply a previously-produced report     | —                    |
| `memory_get_recent_insights` | Cognitive | Recent flushed insights by scope       | sparse/standard/full |
| `memory_record_activity`     | Activity  | Record tool invocation with dedup      | —                    |
| `memory_checkpoint_session`  | Activity  | Checkpoint session state (LWW)         | —                    |
| `memory_load_context`        | Activity  | Load project context for session start | sparse/standard/full |

### Tiered Depth

Controls response verbosity to manage token budget:

| Depth      | ~Tokens/fact | Includes                                                    |
| ---------- | ------------ | ----------------------------------------------------------- |
| `sparse`   | ~15          | id, truncated content (200 chars), importance_score, scope  |
| `standard` | ~75          | All fields except embedding and content_hash                |
| `full`     | ~300+        | Everything: provenance, graph context, embedding dimensions |

Usage: pass `"depth": "sparse"` (or `"standard"`, `"full"`) as a tool parameter.

### Embedding

The server requires an OpenAI-compatible embedding endpoint for `memory_add_fact` and vector/hybrid queries. Compatible with:

- **ollama**: `http://localhost:11434/v1/embeddings`
- **OpenAI**: `https://api.openai.com/v1/embeddings`
- Any endpoint returning `{ "data": [{ "embedding": [...] }] }` or `{ "embeddings": [[...]] }`

For `memory_add_fact`, callers can bypass server-side embedding by passing a pre-computed `embedding` array.

### Pre-Compaction Flush

`memory_flush_insights` accepts a batch of insights for agents to capture before context window compaction. Each insight becomes a fact with `metadata.source = "pre_compaction_flush"`. Insights are validated individually (malformed entries reported as failures), then valid entries are batch-embedded and inserted atomically in a single transaction. If the batch insert fails (e.g., embedding API error), all valid entries are reported as failed.

### Consolidation (`memory_consolidate`)

Requires a summary generator (chat-completions endpoint) configured via `[summary]` in TOML or `--summary-url` / `--summary-model` CLI flags. If not configured, the tool returns a clear error.

The summary generator also requires an embedding provider (summaries must be embedded into the same vector space as facts). If `--summary-url` is set but no embedding provider is configured, the server logs a warning and disables consolidation.

Parameters: `dedup_threshold` (default 0.92), `min_cluster_size` (default 3).

### Forget (`memory_forget`)

All parameters are optional — defaults from `ForgetPolicy::default()`:

| Parameter                | Default | Description                                       |
| ------------------------ | ------- | ------------------------------------------------- |
| `half_life_days`         | 69.0    | Base Ebbinghaus half-life                         |
| `min_importance`         | 0.1     | Threshold below which facts are pruned            |
| `recency_weight`         | 0.3     | Weight for recency signal                         |
| `frequency_weight`       | 0.2     | Weight for access frequency                       |
| `graph_degree_weight`    | 0.3     | Weight for graph connectivity                     |
| `base_importance_weight` | 0.2     | Weight for base importance                        |
| `half_life_overrides`    | `{}`    | Per-FactType overrides, e.g. `{"Episodic": 30.0}` |

### Dump State (`memory_dump_state`)

Exports the full engine snapshot. Formats: `json`, `sqlite`. Client-supplied paths are restricted to the system temp directory for security. Default: `{temp_dir}/memory-dump-{timestamp}.json`.

### Pin / Unpin

`memory_pin_fact` and `memory_unpin_fact` toggle a fact's persistence. Pinned facts are immune to `memory_forget`.

### Cognitive Pipeline (`memory_dream_cycle`, `memory_apply_cycle_report`, `memory_get_recent_insights`)

These expose the Phase-5a [DreamCycle](../advanced/dream-cycle.md) over MCP. The engine separates _producing_ a cycle report (a pure analysis pass, no mutation) from _applying_ it (the mutating step). The MCP surface offers both the ergonomic one-call form and the explicit two-step gate:

| Tool                         | Behavior                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| ---------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `memory_dream_cycle`         | Runs `DefaultDreamCycle::with_defaults()` **through the #209 guard** (`run_dream_cycle_guarded`). If the caller wrote facts since the cursor, the run is **deferred**: `{ "did_run": false, "skipped": { "CallerWroteFacts": { "since_fact_id", "new_max_fact_id" } } }` (no report, nothing applied). Otherwise it runs: `{ "did_run": true, "report", "did_apply", "applied"? }` — `apply` (bool, default `true`) controls in-call application; `applied` present only when `did_apply` is true. |
| `memory_apply_cycle_report`  | Applies a report produced by an earlier `apply:false` run (serde round-trips across the boundary). Malformed JSON or a report referencing an unknown fact → `invalid_params`. Returns the `ApplyResult`.                                                                                                                                                                                                                                                                                           |
| `memory_get_recent_insights` | Returns facts flushed via `memory_flush_insights`, scoped to `project_path`'s subtree, newest-first, capped by `limit` (default 20). An unknown `project_path` returns an empty list, not an error. Honors tiered `depth`.                                                                                                                                                                                                                                                                         |

The dream cycle does **not** run consolidation — it is a lightweight, idempotent-by-report analysis pass. The `apply:false` → `memory_apply_cycle_report` split lets an agent review the proposed deltas before committing them. `memory_get_recent_insights` reads the same shared insight marker (`INSIGHT_MARKER_KEY`) that `memory_flush_insights` stamps, so the writer and reader cannot drift.

**Caller-write deferral (#209).** `memory_dream_cycle` stands down (`did_run: false`) when the caller wrote facts since the last run — the write/consolidate-race gate for a harness that fires writes and the cycle on the same trigger. It is a per-invocation deferral: the facts are processed by a later quiet run, not dropped. See [dream-cycle § Caller-write deferral](../advanced/dream-cycle.md#caller-write-deferral-209).

### Activity Stream (`memory_record_activity`)

Records tool invocations with server-side filtering. The pipeline runs: **ignore** (drop noise) → **dedup** (collapse repeats within a configurable window) → **promote** (auto-create facts from significant actions).

Parameters:

| Param           | Type   | Required | Description                                                         |
| --------------- | ------ | -------- | ------------------------------------------------------------------- |
| `tool`          | string | yes      | Tool name that was invoked                                          |
| `session_id`    | string | yes      | Current session ID                                                  |
| `args`          | object | no       | Tool arguments (arbitrary JSON)                                     |
| `result`        | string | no       | Tool result summary (truncated at 512 chars server-side)            |
| `timestamp`     | string | no       | ISO 8601 timestamp. Defaults to now                                 |
| `scope`         | string | no       | Scope path for the activity                                         |
| `outcome_class` | string | no       | `"success"`, `"error"`, `"test_failure"`, etc. Default: `"success"` |

Returns `{ activity_id, was_deduplicated, promoted_fact_id, status }`.

Dedup key: `(session_id, tool_name, args_hash, outcome_class, scope_id)` within the configured window (default 300s). Outcome-class-aware: a passing `cargo test` and a failing one within the same window are NOT collapsed.

Filtering policy is adapter-specific — the MCP adapter supplies Claude Code heuristics (ignore/promote patterns) via `activity_policy.rs`. The core engine is generic.

### Session Checkpoint (`memory_checkpoint_session`)

Saves session state (last-write-wins per `session_id`). Designed to be called from a Stop hook.

Parameters:

| Param        | Type   | Required | Description               |
| ------------ | ------ | -------- | ------------------------- |
| `session_id` | string | yes      | Session ID to checkpoint  |
| `scope`      | string | no       | Scope path                |
| `summary`    | string | no       | Free-form session summary |
| `metadata`   | object | no       | Arbitrary JSON metadata   |

Returns `{ session_id, checkpointed: true }`.

### Load Context (`memory_load_context`)

Loads project-scoped context for session bootstrap. Returns recent activities, the last checkpoint, and relevant scope-filtered facts in a single read snapshot (consistent cross-query results).

Parameters:

| Param            | Type    | Required | Description                                                 |
| ---------------- | ------- | -------- | ----------------------------------------------------------- |
| `scope`          | string  | yes      | Scope path                                                  |
| `activity_limit` | integer | no       | Max recent activities (default 20)                          |
| `fact_limit`     | integer | no       | Max relevant facts (default 10)                             |
| `depth`          | string  | no       | `"sparse"` / `"standard"` / `"full"` (default `"standard"`) |

Returns `{ scope_path, recent_activities, last_checkpoint, relevant_facts }` shaped by the requested depth level.

### Schema v9

The activity stream adds two tables in schema version 9:

- **`activities`** — append-only log of tool invocations with dedup counters, outcome classification, and optional promotion linkage (`promoted_fact_id` FK to `facts`).
- **`session_checkpoints`** — last-write-wins session state (scope, summary, metadata, timestamp).

Migration from v8 is automatic on first open.

### Architecture

```
Agent (Claude, etc.) → stdio JSON-RPC → memory-engine-mcp → MemoryEngine (SQLite)
                                         ├── config.rs (TOML + env + CLI)
                                         ├── server.rs (ServerHandler, spawn_blocking)
                                         ├── tools/   (18 handlers + dispatch)
                                         ├── depth.rs (response shaping)
                                         ├── activity_policy.rs (Claude Code filter heuristics)
                                         ├── embedding.rs (HTTP embedding provider)
                                         ├── summary.rs (HTTP summary generator)
                                         └── error.rs (MemoryError → MCP)
```

Tool calls run on tokio's blocking thread pool (`spawn_blocking`) since the engine is sync (SQLite). Protocol version pinned to `2025-06-18`.
