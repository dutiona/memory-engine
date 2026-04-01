# MCP Server (`memory-engine-mcp`)

## What

A stdio-based MCP server binary that exposes memory-engine as 15 tools (10 P0 + 5 P1) for autonomous AI agents. Part of the four-layer cognitive architecture (Knowledge → Memory → Wisdom → Intelligence) — this crate bridges Memory to Intelligence via the Model Context Protocol.

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
api_key = "sk-..."  # Optional — for authenticated endpoints
dimensions = 384
timeout_secs = 30
```

CLI flags override TOML values field-by-field (e.g., `--embed-api-key` overrides `api_key` in TOML while keeping `endpoint` and `model` from TOML).

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

| Tool                    | Priority | Purpose                              | Depth                |
| ----------------------- | -------- | ------------------------------------ | -------------------- |
| `memory_ingest`         | P0       | Append event to log                  | —                    |
| `memory_add_fact`       | P0       | Add fact with embedding              | —                    |
| `memory_query`          | P0       | Hybrid FTS + vector search           | sparse/standard/full |
| `memory_resume_context` | P0       | 5-tier cognitive boot                | sparse/standard/full |
| `memory_list_due`       | P0       | Scheduled fact surfacing             | sparse/standard/full |
| `memory_next_due_time`  | P0       | Next scheduled time                  | —                    |
| `memory_explain_fact`   | P0       | Fact provenance                      | sparse/standard/full |
| `memory_get_fact`       | P0       | Single fact by ID                    | sparse/standard/full |
| `memory_statistics`     | P0       | Aggregate stats                      | —                    |
| `memory_flush_insights` | P0       | Batch pre-compaction capture         | —                    |
| `memory_consolidate`    | P1       | Dedup + cluster facts into summaries | —                    |
| `memory_forget`         | P1       | Ebbinghaus decay pruning             | —                    |
| `memory_dump_state`     | P1       | Export snapshot (JSON/SQLite)         | —                    |
| `memory_pin_fact`       | P1       | Make fact unforgettable              | —                    |
| `memory_unpin_fact`     | P1       | Allow forgetting a pinned fact       | —                    |

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

`memory_flush_insights` accepts a batch of insights for agents to capture before context window compaction. Each insight becomes a fact with `metadata.source = "pre_compaction_flush"`. Supports partial success — individual failures are reported without aborting the batch.

### Consolidation (`memory_consolidate`)

Requires a summary generator (chat-completions endpoint) configured via `[summary]` in TOML or `--summary-url` / `--summary-model` CLI flags. If not configured, the tool returns a clear error.

The summary generator also requires an embedding provider (summaries must be embedded into the same vector space as facts). If `--summary-url` is set but no embedding provider is configured, the server logs a warning and disables consolidation.

Parameters: `dedup_threshold` (default 0.92), `min_cluster_size` (default 3).

### Forget (`memory_forget`)

All parameters are optional — defaults from `ForgetPolicy::default()`:

| Parameter                | Default | Description                            |
| ------------------------ | ------- | -------------------------------------- |
| `half_life_days`         | 69.0    | Base Ebbinghaus half-life              |
| `min_importance`         | 0.1     | Threshold below which facts are pruned |
| `recency_weight`         | 0.3     | Weight for recency signal              |
| `frequency_weight`       | 0.2     | Weight for access frequency            |
| `graph_degree_weight`    | 0.3     | Weight for graph connectivity          |
| `base_importance_weight` | 0.2     | Weight for base importance             |
| `half_life_overrides`    | `{}`    | Per-FactType overrides, e.g. `{"Episodic": 30.0}` |

### Dump State (`memory_dump_state`)

Exports the full engine snapshot. Formats: `json`, `sqlite`. Client-supplied paths are restricted to the system temp directory for security. Default: `{temp_dir}/memory-dump-{timestamp}.json`.

### Pin / Unpin

`memory_pin_fact` and `memory_unpin_fact` toggle a fact's persistence. Pinned facts are immune to `memory_forget`.

### Architecture

```
Agent (Claude, etc.) → stdio JSON-RPC → memory-engine-mcp → MemoryEngine (SQLite)
                                         ├── config.rs (TOML + env + CLI)
                                         ├── server.rs (ServerHandler, spawn_blocking)
                                         ├── tools/   (15 handlers + dispatch)
                                         ├── depth.rs (response shaping)
                                         ├── embedding.rs (HTTP embedding provider)
                                         ├── summary.rs (HTTP summary generator)
                                         └── error.rs (MemoryError → MCP)
```

Tool calls run on tokio's blocking thread pool (`spawn_blocking`) since the engine is sync (SQLite). Protocol version pinned to `2025-06-18`.
