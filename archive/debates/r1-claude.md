# Round 1: Claude (Host) — Opening Position

## 1. Memory Architecture

**Position: Memori as hot path + knowledge graph as warm path. Don't merge them.**

Memori's 43us FTS5+vector hybrid is the operational memory — what the agent queries during active reasoning. The knowledge graph (GraphThulhu pattern, but rewritten in Rust) is the structural memory — relationships, entity types, decision history. They serve different access patterns:

- Hot path (Memori): "What did I learn about X?" → FTS5+vector, <1ms
- Warm path (Graph): "How does X relate to Y? What decisions led to Z?" → graph traversal, <50ms
- Cold path (Archival): Full markdown files on disk, git-versioned

The GraphRAG entry-point pattern works for discovery, but for an autonomous agent, most queries are targeted recall, not discovery. Memori handles 80% of cases. The graph handles the 20% that require relational reasoning.

**Challenge to the question itself**: The real problem isn't search speed — it's memory WRITING. Who decides what gets stored, how it's categorized, and when it's consolidated? AgeMem's approach (memory ops as tool actions) is the right abstraction. The agent should have `memory_store`, `memory_search`, `memory_relate`, `memory_consolidate` as MCP tools.

## 2. Build vs Compose

**Position: New project, extracting patterns. Don't fork.**

Panther is an excellent reference but it was designed as a personal assistant, not a general-purpose autonomous agent. Its memory is conversation-history-based, not the multi-layer architecture we need. Forking means:

- Inheriting 9 crates of design decisions that may not align
- Being constrained by Panther's message bus abstraction
- Dependency on Panther's release cycle and breaking changes

Instead: New Cargo workspace that takes specific patterns:

- From Panther: message bus (Tokio MPSC), channel adapters (Telegram/Discord), MCP integration, subagent spawning
- From ZeroClaw: trait-driven extensibility, encrypted secrets, security model
- From Rig: LLM provider abstraction, Ollama integration
- From Memori: hybrid search, decay scoring
- Own: memory architecture, task scheduler, dream cycle, escalation system

This is more work upfront but avoids the fork tax.

## 3. MLX vs Ollama

**Position: MLX for inference via mlx-lm server, Ollama for Jetson and model management only.**

The 2x speed and 50% RAM savings from MLX are not optional on a 32GB machine. Those 5-10GB saved are the difference between running tools or not. But Ollama provides:

- Model management (pull, list, update)
- OpenAI-compatible API for Jetson remote inference
- Easy model switching

Solution: `mlx-lm` or `vllm-mlx` serves the backbone model with an OpenAI-compatible API on localhost. Ollama runs on Jetson for embeddings and auxiliary models. The agent's LLM client (via Rig) talks to both through the same OpenAI-compatible interface. No special handling needed.

## 4. Nightly Memory Model

**Position: Don't fine-tune anything. Case-based reasoning (Memento) + learned retrieval ranking.**

The "brain model" idea is romantic but impractical because:

- Catastrophic forgetting is a real, unsolved problem for sequential fine-tuning
- LoRA on daily logs would overwrite older knowledge
- A 1-3B model's capacity is tiny — it can't "contain" months of experience

What CAN work:

- **Memento-style case bank**: Store successful task trajectories as episodes. Neural case selector retrieves relevant episodes at decision time. No fine-tuning required.
- **Learned retrieval ranker**: Train a small model (on Jetson, monthly) to re-rank search results from Memori. Input: query + candidate memories. Output: relevance score. This is a RANKING task, not a knowledge task — much easier to train without catastrophic forgetting.
- **Dream cycle consolidation**: Nightly batch job that summarizes daily episodes into semantic entries, promotes patterns to the knowledge graph, and decays stale facts. This is the right use of the Jetson — batch processing, not inference.

## 5. Autonomy & Human-in-the-Loop

**Position: Three-tier escalation with async queue and timeout-based degradation.**

Tier 1 — **Autonomous**: Agent handles with local Qwen 3.5. No human needed. (Most tasks.)
Tier 2 — **Escalation queue**: Agent drafts a prompt, places it in an escalation queue, continues other work. Sends Telegram notification to user. User feeds to Claude/Codex/Gemini when available. Response injected back into agent context.
Tier 3 — **Blocking escalation**: Agent determines it CANNOT proceed without external help. Pauses current task, works on other tasks. After configurable timeout (e.g., 4 hours), either retries with local model or marks task as blocked.

The key abstraction: an `EscalationRequest` struct with:

- `prompt`: The crafted prompt for the external model
- `priority`: low/medium/high/critical
- `timeout`: Duration before degraded fallback
- `fallback_strategy`: retry_local | skip | block
- `context`: Relevant memory snippets for the human to include

The agent should NEVER block entirely. It always has a backlog of tasks to work on.
