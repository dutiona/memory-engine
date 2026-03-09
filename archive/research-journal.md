# Autonomous Agent Research Journal

**Project**: Persistent autonomous long-running agent on Mac Mini M4
**Started**: 2026-03-07
**Hardware**:

- Mac Mini M4: 32GB unified RAM, 512GB SSD, M4 CPU/GPU
- Jetson Nano Orin: 8GB (satellite compute for training/fine-tuning)
- Main PC: 128GB RAM, Ryzen 9950X (32 cores), RTX 4090 24GB VRAM

**Backbone Model**: Qwen 3.5 A3B 35B MoE (reasoning + tool use)
**Primary Language**: Rust (with TS for web/UI, Python for glue/ML)
**Constraint**: No Opus/Claude API as autonomous backend (ToS). Interactive use via human-in-the-loop OK.

---

## Entry 1: Research Kickoff (2026-03-07)

### Scope

Build a persistent, autonomous, long-running AI agent that:

1. Runs 24/7 on Mac Mini M4 with Qwen 3.5 35B MoE as backbone
2. Accomplishes tasks, codes, trains/uses models, interacts with user
3. Can delegate hard problems to Opus/Codex/Gemini via human-in-the-loop
4. Maintains durable memory across reboots and context windows
5. Leverages Jetson Nano for satellite ML tasks (fine-tuning, evaluation)
6. Communicates via Telegram/Discord bridge

### Tool Ecosystem Requirements

1. Web browsing/understanding (firecrawl + alternatives)
2. Vision toolkit (vision-tools, omniparser, cvtk)
3. Documentation ingestion & retrieval (research-index)
4. Model management (ollama/vllm/lm-studio — pull, test, swap, fine-tune, evaluate)
5. Development IDE capabilities (coding, testing, learning)
6. Communication bridge (Telegram/Discord)
7. Skill & agent collections
8. Memory system (the hardest problem)

### Key Open Questions (initial)

- Q1: What is SOTA for long-term agent memory? (graphs vs vector DBs vs hybrid?)
- Q2: Can we fine-tune a "memory model" nightly from daily logs?
- Q3: What's the right abstraction for agent autonomy levels?
- Q4: How do existing frameworks (OpenClaw, etc.) handle persistence?
- Q5: What fits in 32GB unified RAM alongside Qwen 3.5 35B?
- Q6: Jetson Nano Orin 8GB — what can it realistically train/fine-tune?
- Q7: Rust agent frameworks — what exists? What needs building?
- Q8: MCP as the tool protocol — sufficient or needs extension?

---

## Entry 2: Academic Memory Research — Primary Findings (2026-03-07)

### Key Paper List (from Agent-Memory-Paper-List, 60+ papers)

The field has exploded in 2025-2026. Memory is categorized into 5 types:

1. **Factual Memory** (what happened): MAGMA, EverMemOS, Zep temporal KG, HippoRAG, GraphRAG
2. **Experiential Memory** (what worked): MemRL, Reflexion, Memento (case-based reasoning), ToolMem
3. **Working Memory** (current context): AgeMem, AgentFold, ACON context compression, Memory-as-Action
4. **Parametric Memory** (in-weights): MemLoRA, WISE, ELDER (mixture-of-LoRA), continual pre-training
5. **Latent Memory** (hidden states): Memory3, Titans (test-time memorization), KV cache compression

### Top 5 Most Relevant Papers

1. **Agentic Memory (AgeMem)** — Jan 2026, arXiv:2601.01885
   - Unified LTM+STM management as tool-based actions
   - Agent autonomously decides what/when to store/retrieve/summarize/discard
   - 3-stage progressive RL training: LTM → STM → unified
   - +4.82 to +8.57 pp over baselines on 5 benchmarks
   - **Relevance**: Directly applicable architecture. Memory ops as MCP tools.

2. **Memento** — Aug 2025, arXiv:2508.16153
   - "Fine-tuning agents WITHOUT fine-tuning LLMs"
   - Case-based reasoning with growing Case Bank (episodic memory)
   - Memory-augmented MDP with neural case-selection policy
   - 87.88% Pass@3 on GAIA validation (top-1)
   - **Relevance**: CRITICAL. Avoids fine-tuning entirely. Non-parametric. Works with frozen local models.

3. **MemGPT/Letta** — Oct 2023 → 2026 (evolved into Letta platform)
   - LLM-as-OS: virtual context management (RAM ↔ disk analogy)
   - Core memory (in-context) + Archival memory (out-of-context)
   - Self-editing memory through tool use
   - 2026 update: Context Repositories with git-based versioning
   - **Relevance**: Foundational architecture. Open-source. Python-based.

4. **CoALA** — Sep 2023, arXiv:2309.02427
   - Cognitive architecture framework: working memory + long-term memory + action space
   - Organizes agents along: information storage, action space, decision procedure
   - Retrospective survey of 300+ papers
   - **Relevance**: Theoretical framework for designing our memory system.

5. **Mem0** — 2024-2026, open-source + commercial
   - Hybrid: vector DB + knowledge graph + key-value store
   - Hierarchical: user/session/agent memory levels
   - 26% higher accuracy than OpenAI's memory, 91% lower latency
   - 90% token cost savings
   - Python SDK, self-hostable
   - **Relevance**: Production-ready memory layer. Could be adapted or referenced.

### The "Memory Model Fine-Tuning" Idea — Verdict

User's idea: fine-tune a secondary model nightly with daily logs as a "brain" callable via tools.

**Research says: Probably not the right approach.** Here's why:

- **LoRA learns less and forgets less** (arXiv:2405.09673): LoRA keeps base model frozen, BUT sequential fine-tuning on daily data causes performance drops on earlier data
- **Catastrophic forgetting**: Even with LoRA, nightly re-training risks overwriting knowledge from previous days
- **Elastic Weight Consolidation (EWC)**: Mitigates forgetting but adds complexity and doesn't fully solve it
- **Better alternatives exist**:
  - **Memento's case-based reasoning**: No fine-tuning needed, growing episodic memory bank
  - **MemLoRA**: Distills expert adapters (not daily re-training)
  - **Mem0's hybrid store**: Vector + graph + KV at retrieval time

**However**, there IS a viable variant:

- **Continual pre-training** (not fine-tuning) on accumulated experience (paper: "Scaling Agents via Continual Pre-training")
- Use the Jetson Nano for weekly/monthly LoRA training on curated, deduplicated experiences (not raw daily logs)
- The "brain model" concept works better as a **retrieval-specialized small model** that learns to INDEX and RETRIEVE from the memory store, not to CONTAIN the memories in its weights

### Memory Architecture Recommendation (preliminary)

Based on research, the strongest approach is **Memento-style case-based reasoning + Mem0-style hybrid storage**:

```
┌─────────────────────────────────────────────────────┐
│                    MEMORY SYSTEM                     │
├─────────────┬──────────────┬────────────────────────┤
│  WORKING    │  EPISODIC    │  SEMANTIC              │
│  (context)  │  (cases)     │  (knowledge)           │
├─────────────┼──────────────┼────────────────────────┤
│ Current     │ Case Bank    │ Knowledge Graph        │
│ context     │ (Memento)    │ (entities/relations)   │
│ window      │ + neural     │ + Vector embeddings    │
│ of Qwen     │ case         │ (semantic search)      │
│ 3.5 35B     │ selector     │ + KV store (facts)     │
├─────────────┼──────────────┼────────────────────────┤
│ AgeMem-     │ Grows over   │ Consolidation:         │
│ style       │ time.        │ daily → weekly →       │
│ self-       │ Importance   │ monthly summaries      │
│ managed     │ scoring +    │ with forgetting        │
│ context     │ retrieval    │ curves                 │
│ curation    │ policy       │                        │
└─────────────┴──────────────┴────────────────────────┘
```

### Partial Answer to Open Questions

- **Q1 ANSWERED**: Hybrid (graph + vector + KV) is SOTA. Not just one. Mem0 proves it.
- **Q2 PARTIALLY ANSWERED**: Nightly fine-tuning is risky (catastrophic forgetting). Better: case-based reasoning (Memento) + periodic LoRA on curated data.
- **Q3 OPEN**: Autonomy levels not yet researched.
- **Q4 OPEN**: Waiting for agent framework results.

---

## Entry 3: Hardware Analysis — Qwen 3.5 on M4 (2026-03-07)

### Qwen 3.5 35B-A3B Memory Requirements

| Quantization | File Size   | RAM Needed | Fits 32GB?             |
| ------------ | ----------- | ---------- | ---------------------- |
| IQ3_XS       | 14.5 GB     | ~16 GB     | Yes, 16GB headroom     |
| Q3_K_M       | 16.1 GB     | ~18 GB     | Yes, 14GB headroom     |
| **Q4_K_M**   | **21.2 GB** | **~22 GB** | **Yes, 10GB headroom** |
| Q5_K_M       | 24.8 GB     | ~26 GB     | Yes, 6GB headroom      |
| Q8_0         | 36.9 GB     | ~38 GB     | NO                     |

**Recommendation**: Q4_K_M — leaves ~10GB for OS, tools, memory DB, embeddings model.

### Performance on Apple Silicon

- **MLX**: 60-70+ tok/s on M4 Max at Q4. M4 (non-Max) likely 40-50 tok/s.
- **Ollama**: ~35 tok/s on M4 Max. ~20-25 tok/s on base M4.
- **MLX is 2x faster** than Ollama and uses **50% less memory**.
- **Prompt processing**: MLX 3-5x faster than Ollama.
- **Recommendation**: Use MLX backend (via LM Studio or direct mlx-lm).

### Can We Run a Second Model?

With Q4_K_M (~22GB), we have ~10GB left. Options:

- SmolLM2 1.7B or Qwen2.5-3B for embedding/retrieval tasks: ~2-3GB
- That leaves 7-8GB for OS + tools + memory DB
- **Verdict**: Tight but doable. Memory DB (SQLite + Qdrant) uses <1GB typically.
- **Alternative**: Use the embedding model on Jetson Nano instead.

### Jetson Nano Orin 8GB — Realistic Capabilities

- **Inference**: Models up to ~4B params (Qwen2.5-3B, Phi-3-mini, Gemma-3 4B)
- **LoRA fine-tuning**: Feasible for 1-3B models with QLoRA (4-bit)
- **67 TOPS** AI performance (Ampere GPU)
- **Good for**: Embedding generation, small model inference, weekly LoRA training
- **Bad for**: Running the backbone model, large-scale training
- **Power**: ~15W, suitable for 24/7 operation

### Key Insight: MLX is Critical

The M4 Mac Mini should use MLX exclusively (not Ollama) for the backbone model. This doubles throughput and halves memory usage, which is essential when we need RAM for the memory system too.

---

## Entry 4: Rust Agent Frameworks (2026-03-07)

### Landscape (from web search — pending detailed agent report)

| Framework        | Language   | Focus           | MCP Support | Memory                   | Notes                                                        |
| ---------------- | ---------- | --------------- | ----------- | ------------------------ | ------------------------------------------------------------ |
| **Rig**          | Rust       | LLM apps        | Unknown     | No built-in              | Most mature Rust LLM framework. 24.3% CPU (most efficient).  |
| **AutoAgents**   | Rust       | Multi-agent     | Unknown     | Unknown                  | <1.1GB peak memory vs >4.7GB for Python frameworks.          |
| **Anda**         | Rust       | ICP/TEE agents  | Unknown     | "Perpetually memorizing" | Blockchain-focused, niche.                                   |
| **Swarm (Rust)** | Rust       | Multi-agent     | MCP + A2A   | Unknown                  | Open standards (MCP, A2A). Configuration-based agent launch. |
| OpenClaw         | TypeScript | Autonomous      | Yes         | File-based               | The reference implementation. Many forks.                    |
| Letta            | Python     | Stateful agents | Partial     | Built-in (MemGPT)        | Best memory system. Not Rust.                                |
| CrewAI           | Python     | Multi-agent     | Unknown     | Limited                  | Orchestration-focused.                                       |

### Assessment (UPDATED after detailed reports)

Major discoveries from web scraping and framework research agents:

1. **ZeroClaw** (Rust, 1.3k stars): Pure Rust OpenClaw rewrite. 3.4MB binary, <10ms startup, <5MB RAM. Trait-driven, 22+ providers, built-in memory, encrypted secrets. **Best starting point for our Rust agent.**

2. **Panther** (Rust, 9 crates): Self-hosted AI agent daemon. Telegram/Discord/Slack/Matrix/CLI. Tokio async, subagent spawning, MCP support, Ollama native. 20-60MB idle. **Closest existing project to what we want.**

3. **Memori** (Rust+SQLite): Hybrid search (FTS5 + cosine vector, Reciprocal Rank Fusion). Auto-dedup (cosine >0.92), decay scoring (69-day half-life), 43us reads. **Drop-in memory layer candidate.**

4. **GraphThulhu** (Go, MCP server): 37 tools for Logseq/Obsidian knowledge graphs. Graph analysis (BFS, connected components, gap detection), decision tracking. **Best knowledge graph MCP server.**

5. **Total Recall**: Five-layer memory with "dream cycle" (nightly consolidation), preconscious buffer, emergency surfacing. Shell-based but architecturally excellent.

**Revised strategy**: Don't build from scratch. Compose from:

- **Panther** as the agent runtime reference (Rust daemon, multi-channel, subagents, MCP)
- **Rig** for LLM interface layer (Ollama/MLX)
- **Memori** for hybrid search memory
- **GraphThulhu** pattern for knowledge graph layer
- **rmcp** SDK for MCP tool orchestration
- **Total Recall's** dream cycle + consolidation patterns

---

## Entry 5: Key Discoveries from Web Scraping (2026-03-07)

### Critical Cross-Cutting Themes (from 10 scraped sources)

1. **Memory convergence**: All sources independently arrive at 4-5 layers (working, core, episodic, semantic, retrieval). This is now validated.

2. **Hybrid search > pure vector**: Knowledge graph + semantic + keyword (GraphRAG pattern). Community consensus: "you need both."

3. **Agent self-maintenance is key**: Best systems have the agent curate its own memory. Daily scratch -> curated knowledge promotion. Active curation > passive logging.

4. **Temporal contradictions UNSOLVED**: Every system struggles with changing facts. Decay scoring helps (~69 day half-life) but doesn't fully solve it.

5. **LLM-constructed keyword queries work**: claude-memory insight — when the retriever IS an LLM, FTS5+BM25 outperforms vector search because the agent constructs targeted queries. 74% on LoCoMo benchmark.

6. **Token economy matters**: Subagent delegation saves ~75% tokens (Deep Thought pattern). Strip tool call junk from sessions.

7. **Cron is the backbone**: Real-time interaction by day, batch consolidation at night ("dream cycle").

8. **Rust is the emerging standard**: Panther, ZeroClaw, Memori, llmfit — all Rust. Low memory, fast startup, safety guarantees.

9. **Obsidian/markdown as memory substrate**: Human-readable, inspectable, git-versionable. Survives model changes. Most durable format.

10. **Security through isolation**: Container/WASM sandboxing becoming table stakes. Credential brokering > plaintext API keys.

---

## Entry 6: Memory Systems Deep-Dive (2026-03-07)

### Critical New Discoveries (from agent report 03)

1. **SurrealDB 3.0** (Feb 2026): Single Rust-based multi-model DB — vector + graph + KV + time-series. $44M raised. "Replace your 5-database RAG stack with one." Eliminates need for separate Memori + graph + KV.

2. **LanceDB**: Embedded Rust vector DB (in-process, no server). Columnar with versioning. Native Rust SDK. Best lightweight option.

3. **Engram**: Cognitive memory MCP server. Ebbinghaus decay, BGE-M3 embeddings, FTS5/BM25 via RRF. Zero API cost. Already an MCP server.

4. **A-Mem (NeurIPS 2025)**: Zettelkasten-style self-organizing memory. Each memory = note with content + embeddings + links. New memories trigger updates to existing notes.

5. **Doc-to-LoRA** (Sakana AI, Feb 2026): Hypernetwork generates LoRA adapters from documents in one forward pass. No training loop. No catastrophic forgetting. <50MB per adapter.

6. **Sparse Memory Fine-tuning**: Only 11% knowledge drop (vs 89% full FT, 71% LoRA). Best parametric approach but requires specialized model architecture.

### Memory Architecture Recommendation (UPDATED)

5-layer hierarchy (CoALA-inspired, validated across all sources):

- L0: Context window (working memory, 32K tokens)
- L1: Core files (markdown, always loaded — CLAUDE.md pattern)
- L2: Episodic memory (session summaries, stored in SurrealDB)
- L3: Semantic memory (facts, knowledge graph in SurrealDB)
- L4: Procedural memory (skills, workflows — markdown files)

**Storage**: Event-sourced log (append-only, source of truth) → materialized views in SurrealDB (vector + graph + KV). Markdown files for human-readable layer.

**Consolidation**: Nightly dream cycle — summarize episodes → promote to semantic → decay stale facts.

**Forgetting**: Ebbinghaus decay (0.95^days_since_access). Archive below threshold. Delete after 90 days.

---

## Entry 7: MCP Ecosystem (2026-03-07)

### Key Findings

- **1,864+ MCP servers** exist (FastMCP registry). 97M+ monthly SDK downloads.
- **Rust SDKs**: rmcp (official, v0.16.0) and rust-mcp-sdk (v0.8.0). Both Tokio async.
- **Qwen 3.5 + MCP**: Works via OpenAI-compatible API format. Qwen-Agent has first-party MCP support.
- **Security**: 30 CVEs in 60 days. 43% vulnerable to command injection. **Run over stdio, not HTTP.**
- **Key servers for us**: filesystem, git, GitHub, code-sandbox, Playwright, Firecrawl, SQLite, Telegram, Discord.
- **A2A protocol**: For agent-to-agent (horizontal), complementary to MCP (vertical agent-to-tool). Relevant later for multi-agent.

### MCP + Non-Claude Models

Flow: MCP server → JSON schema tool definitions → translate to OpenAI function-calling format → Qwen generates tool calls → execute via MCP → return results.

Existing bridges: Dolphin MCP, ollama-mcp-bridge, langchain-mcp-adapters. Or build custom with rmcp crate.

---

## Entry 8: Multi-AI Debate Synthesis (2026-03-07)

3 rounds, 3 advisors (Claude, Codex/GPT-5.4, Gemini). Adversarial style.

### CONSENSUS (all 3 agree)

1. **Memory**: Event-sourced log as source of truth. Multi-model DB (SurrealDB) as materialized view. Graph is DERIVED, never source of truth.
2. **Build**: New Rust project, modular monolith. Cargo workspace with ~5 crates (core, memory, inference, tools, channels). Not a fork.
3. **Inference**: MLX for backbone on Mac Mini. Ollama for Jetson. Unified provider interface (Rig-based).
4. **Fine-tuning**: Don't fine-tune for factual knowledge. RAG handles facts. LoRA only for style/behavior, gated by eval.
5. **Escalation**: Async non-blocking. Intent-locked speculative execution. Risk-scored matrix. Dry-run for medium-risk, explicit approval for high-risk.
6. **Security**: MCP over stdio, sandboxed, no public exposure.

### OPEN DISAGREEMENTS

1. **SurrealDB vs FalkorDB vs SQLite+Petgraph**: Which multi-model DB? SurrealDB is Rust-native but young. SQLite+Petgraph is simpler.
2. **mlx-lm (Python) vs Candle/Ratchet (Rust)**: Immediate performance vs native Rust purity. mlx-lm is practical now; Candle is the long-term bet.
3. **LoRA frequency**: Continuous small-batch (Gemini) vs monthly gated (Claude) vs eval-triggered-only (Codex)?
4. **Jetson role**: When does it become worth the complexity?
5. **Event-sourcing**: Essential audit trail (Codex) or nice-to-have (Claude/Gemini)?

---

## Entry 9: Final Landscape & Open Questions (2026-03-07)

### The Architecture That Emerges

```
┌────────────────────────────────────────────────────────────────────┐
│                        Mac Mini M4 (32GB)                          │
│                                                                    │
│  ┌──────────────┐   ┌──────────────┐   ┌──────────────────────┐   │
│  │   Inference   │   │    Memory    │   │       Tools          │   │
│  │   (Rig)       │   │ (SurrealDB)  │   │    (rmcp MCP)       │   │
│  │              │   │              │   │                      │   │
│  │ MLX backend  │   │ Event log    │   │ filesystem, git,     │   │
│  │ Qwen 3.5 35B│   │ Vector+Graph │   │ shell, browser,      │   │
│  │ Q4_K_M      │   │ KV store     │   │ code-sandbox         │   │
│  │ ~22GB       │   │ Markdown     │   │                      │   │
│  └──────┬───────┘   └──────┬───────┘   └──────────┬───────────┘   │
│         │                  │                       │               │
│  ┌──────▼──────────────────▼───────────────────────▼───────────┐   │
│  │                    Agent Core (Rust)                         │   │
│  │  - Agent loop (ReAct / Plan-Execute)                        │   │
│  │  - Task scheduler (Tokio)                                   │   │
│  │  - Dream cycle (nightly consolidation)                      │   │
│  │  - Escalation queue (async, risk-scored)                    │   │
│  │  - State machine + checkpointing                            │   │
│  └──────────────────────────┬──────────────────────────────────┘   │
│                             │                                      │
│  ┌──────────────────────────▼──────────────────────────────────┐   │
│  │                    Channels                                  │   │
│  │  - CLI (primary during development)                          │   │
│  │  - Telegram (user communication)                             │   │
│  │  - Escalation bus (prompts for Opus/Codex/Gemini)           │   │
│  └──────────────────────────────────────────────────────────────┘   │
│                                                                    │
│  launchd: KeepAlive, RunAtLoad, graceful SIGTERM                  │
└────────────────────────────────────────────────────────────────────┘
         │ HTTP (Gigabit Ethernet)
         │
┌────────▼──────────────────────────┐
│     Jetson Nano Orin (8GB)         │
│  - Ollama: nomic-embed-text       │
│  - SmolLM2 1.7B (auxiliary)       │
│  - Monthly LoRA fine-tuning        │
│  - ~15W, 24/7                      │
└────────────────────────────────────┘
```

### Existing Components to Leverage

| Component             | Source        | Role                                    |
| --------------------- | ------------- | --------------------------------------- |
| Rig                   | crates.io     | LLM abstraction, Ollama provider        |
| rmcp                  | crates.io     | MCP client                              |
| SurrealDB             | surrealdb.com | Multi-model memory store                |
| LanceDB (backup)      | lancedb.com   | Lightweight vector alternative          |
| Panther patterns      | github        | Channel adapters, subagent spawning     |
| ZeroClaw patterns     | github        | Trait-driven extensibility              |
| Memori patterns       | github        | Hybrid search, decay scoring            |
| Total Recall patterns | github        | Dream cycle, 5-layer memory             |
| Engram                | github        | Cognitive memory MCP server (reference) |

### Open Questions Requiring Further Research

**Memory (highest priority)**:

- OQ1: SurrealDB 3.0 maturity — can it handle the concurrent read/write patterns of an agent? Benchmark needed.
- OQ2: Event-sourcing in Rust — use an existing crate (event-sourcing-rs?) or roll own on SQLite WAL?
- OQ3: Memory consolidation quality — how good is Qwen 3.5 at summarizing its own interactions?
- OQ4: Temporal contradiction resolution — none of the systems solve this well. Need a temporal truth-maintenance system.

**Inference**:

- OQ5: MLX quantization quality for Qwen 3.5 — does Q4_K_M via MLX match GGUF Q4_K_M quality?
- OQ6: mlx-lm stability under sustained 24/7 load — any known issues?
- OQ7: Candle/Ratchet Apple Silicon support timeline — when do they match MLX performance?

**Agent Architecture**:

- OQ8: Agent loop design — ReAct vs Plan-Execute vs hybrid? For autonomous long-running, Plan-Execute may be better.
- OQ9: Task scheduler design — priority queue? deadline-aware? preemptive?
- OQ10: How does the agent bootstrap? What's the "startup ritual"? (Vox pattern: load core memory, check pending tasks, review yesterday's summary)

**Autonomy & Safety**:

- OQ11: Risk scoring model — what dimensions? How to calibrate without extensive data?
- OQ12: Escalation UX — how does the user interact with pending escalation tickets via Telegram?
- OQ13: How to handle the agent running when user is asleep/unreachable for 8+ hours?

**Ecosystem**:

- OQ14: Qwen 3.5 tool-calling reliability with MCP — the GitHub issue about failures needs investigation.
- OQ15: MCP server sandboxing on macOS — what's the simplest container approach? Docker? Lima? nsjail?
- OQ16: Telegram bot latency and reliability for always-on agent communication.

### Academic Papers to Ingest into research-index

Priority papers for deeper review:

1. AgeMem (arXiv:2601.01885) — unified LTM+STM
2. Memento (arXiv:2508.16153) — case-based reasoning without fine-tuning
3. CoALA (arXiv:2309.02427) — cognitive architecture framework
4. A-Mem (arXiv:2502.12110) — Zettelkasten memory
5. Mem0 (arXiv:2504.19413) — hybrid memory architecture
6. Doc-to-LoRA (Sakana AI) — parametric memory without catastrophic forgetting
7. "Memory in the Age of AI Agents" survey (arXiv:2512.13564)
8. Sparse Memory Finetuning (arXiv:2510.15103)
9. Graphiti/Zep temporal KG (arXiv:2501.13956)

### What's NOT in This Research (known gaps)

- No hands-on benchmarks (everything is from published numbers)
- No SurrealDB 3.0 load testing for our specific access patterns
- No Qwen 3.5 MCP tool-calling reliability testing
- No security threat model for the agent
- No cost analysis (power, API costs for escalation, etc.)
- No comparison with commercial alternatives (Replit Agent, Devin, etc.)
- No investigation of the Julia microGPT training framework applicability

---

## Entry 10: Community Research Synthesis (2026-03-08)

### Sources Investigated

1. **OpenClaw ecosystem** — 247k stars, TypeScript → ZeroClaw (Rust), 4-layer markdown-canonical memory
2. **r/openclaw 3-layer memory** — breadcrumbs, compaction-triggered checkpoints, trigger-word protocols
3. **r/ClaudeCode second brain** — Mengram (3-type memory + procedure versioning), Ori Mnemos (graph-aware forgetting)
4. **r/ClaudeCode self-improvement** — `/insights` feedback loop, instruction-level self-modification

### Key Findings

5 architectural innovations worth adopting (not in academic papers):

1. **Pre-compaction memory flush** (OpenClaw) — silent turn before context eviction to persist state
2. **Breadcrumb indexing** (Reddit 3-layer) — sparse one-liner indexes in L2 pointing to deep L3 content
3. **Procedure versioning** (Mengram) — procedural memory branches on failure, merges on success
4. **Graph-aware forgetting** (Ori Mnemos) — combine time-decay with graph connectivity for pruning
5. **Instruction-level self-modification** (`/insights`) — agent reviews its own telemetry and updates system prompt

### Architecture Update

5-layer hierarchy VALIDATED by 4 independent community implementations. Added:

- Pre-compaction flush to L0
- Breadcrumb indexing to L1
- Procedure versioning to L4
- Graph-aware + time-decay hybrid forgetting to L3
- Self-review cron to dream cycle

### Community Skepticism Worth Heeding

- kubrador: "The capture layer is what matters; everything else is cope" — write-time quality gates are more important than sophisticated retrieval (aligns with ACE curation paper)
- TailorImaginary3629: "Unless you invest your own time to research, it's still a grave" — applies to autonomous agent too: memory without active use is dead weight
- Community enterprise scaling post dismissed as "generic BS" — skepticism toward overhyped frameworks is healthy

### For Daily Claude Code Workflow

Immediately actionable: `/insights` feedback loop, CLAUDE.md token budgeting (≤1000 tokens/section), periodic MEMORY.md pruning. See [06-community-research-synthesis.md](docs/research/06-community-research-synthesis.md) for full details.

### New Repos to Track

- **Mengram** (github.com/alibaizhanov/mengram) — 3-type memory + knowledge graph
- **Ori Mnemos** (github.com/aayoawoyemi/Ori-Mnemos) — graph-aware forgetting
- **Bosun** (github.com/virtengine/bosun) — "actually self-improving" system (needs investigation)

### New Open Questions

- NQ1: Graph-aware forgetting + temporal decay interaction — redundant or complementary?
- NQ2: Procedure version granularity — per failure? per session? per outcome?
- NQ3: Instruction self-modification gating — how to prevent drift?
- NQ4: Mengram 3-hook pattern overhead — worth the latency?

---

## Entry 11: Open Questions Research Round (2026-03-08)

Used search_index queries across 9 ingested papers (1284 chunks) to address OQ1-OQ4.

### OQ1: Storage Engine → REVISED

**Previous:** SurrealDB 3.0 as primary.
**Updated:** Start with **SQLite (FTS5) + LanceDB (vector) + in-memory Petgraph (graph)**. Migrate to SurrealDB when it proves stable. Event-sourced log guarantees replay into any backend.

Rationale: Papers show Zep's graph takes "several hours" of async processing vs Mem0's "under a minute." Real-time access matters. SQLite+LanceDB+Petgraph is the safest embedded combo that covers all access patterns.

### OQ2: Event-Sourcing → ANSWERED

Roll own on SQLite WAL. `events(id, timestamp, event_type, payload_json)` table. This is exactly what Graphiti does conceptually ("non-lossy dynamic updates") and what OpenClaw does practically (markdown as source, SQLite as derived).

### OQ3: Consolidation Quality → ANSWERED

Survey §5.2.1 describes 3-level consolidation: local (merge near-duplicates), cluster (group into thematic summaries), global (update core understanding). Nightly dream cycle implements all three. Raw event log = source of truth; summaries are derived views.

Key tradeoff: "Semantic summarization is lossy compression — prioritizes global coherence over local precision." Mitigation: never delete raw events.

### OQ4: Temporal Contradictions → PARTIALLY ANSWERED

Clear evolution in literature: destructive replacement → soft deletion with timestamps → bi-temporal modeling (Graphiti) → learned update policies (frontier).

**Adopt Graphiti's bi-temporal model** (4 timestamps per fact) + **Mem0's LLM-arbitrated CRUD** (ADD/UPDATE/DELETE/NOOP). Never hard-delete. Open sub-question: can Qwen 3.5 reliably arbitrate conflicts?

### Bonus: Dual-System Architecture

Survey §7: "System 1 (fast/instinctive) + System 2 (slow/deliberative)." For us: Qwen 3.5 = System 1, Opus/Codex via escalation = System 2. Memory bridges both.

Full details: [08-open-questions-research.md](docs/research/08-open-questions-research.md)

---

## Entry 12: Cross-Paper Relationship Graph (2026-03-08)

22 relationships mapped in research-index. CoALA (2023) is the hub — 5 papers extend it.

```
CoALA(3) ←extends── AgeMem(1), Memento(2), A-Mem(4), Graphiti(8), SparseMemFT(7)
Graphiti(8) ←extends── Mem0(5)
Survey(6) ──cites──→ all 7 other papers

Comparison pairs:
  AgeMem(1) ↔ Memento(2)     (both: experience-based reasoning)
  A-Mem(4) ↔ Mem0(5)         (both: automated memory management)
  A-Mem(4) ↔ AgeMem(1)       (both: agentic memory frameworks)
  Mem0(5) ↔ Graphiti(8)      (direct competitor, Mem0 paper §4 benchmarks against Graphiti)
  Doc-to-LoRA(9) ↔ SparseMemFT(7)  (both: parameter-efficient continual learning)
```

### Extraction Status

| Paper            | ID  | Chunks | Extracted | Methods | Datasets | Metrics |
| ---------------- | --- | ------ | --------- | ------- | -------- | ------- |
| AgeMem           | 1   | 122    | Yes       | 134     | 26       | 237     |
| Memento          | 2   | 107    | Yes       | 222     | 79       | 680     |
| CoALA            | 3   | 153    | Yes       | 264     | 55       | 69      |
| A-Mem            | 4   | 110    | Yes       | 85      | 37       | 1039    |
| Mem0             | 5   | 87     | Yes       | 90      | 29       | 219     |
| Survey           | 6   | 576    | Yes       | 957     | 328      | 149     |
| Sparse Memory FT | 7   | 56     | Yes       | 62      | 25       | 156     |
| Graphiti         | 8   | 52     | Yes       | 71      | 29       | 83      |
| Doc-to-LoRA      | 9   | 21     | Yes       | 16      | 9        | 28      |

**Totals (9/9):** 1901 methods, 617 datasets, 2660 metrics across all papers.
