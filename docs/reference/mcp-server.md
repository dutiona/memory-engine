# MCP Server (`memory-engine-mcp`)

## What

A stdio-based MCP server binary that exposes memory-engine as 10 tools for autonomous AI agents. Part of the four-layer cognitive architecture (Knowledge → Memory → Wisdom → Intelligence) — this crate bridges Memory to Intelligence via the Model Context Protocol.

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

| Tool                    | Purpose                      | Depth                |
| ----------------------- | ---------------------------- | -------------------- |
| `memory_ingest`         | Append event to log          | —                    |
| `memory_add_fact`       | Add fact with embedding      | —                    |
| `memory_query`          | Hybrid FTS + vector search   | sparse/standard/full |
| `memory_resume_context` | 5-tier cognitive boot        | sparse/standard/full |
| `memory_list_due`       | Scheduled fact surfacing     | sparse/standard/full |
| `memory_next_due_time`  | Next scheduled time          | —                    |
| `memory_explain_fact`   | Fact provenance              | sparse/standard/full |
| `memory_get_fact`       | Single fact by ID            | sparse/standard/full |
| `memory_statistics`     | Aggregate stats              | —                    |
| `memory_flush_insights` | Batch pre-compaction capture | —                    |

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

### Architecture

```
Agent (Claude, etc.) → stdio JSON-RPC → memory-engine-mcp → MemoryEngine (SQLite)
                                         ├── config.rs (TOML + env + CLI)
                                         ├── server.rs (ServerHandler, spawn_blocking)
                                         ├── tools/   (10 handlers + dispatch)
                                         ├── depth.rs (response shaping)
                                         ├── embedding.rs (HTTP provider)
                                         └── error.rs (MemoryError → MCP)
```

Tool calls run on tokio's blocking thread pool (`spawn_blocking`) since the engine is sync (SQLite). Protocol version pinned to `2025-06-18`.
