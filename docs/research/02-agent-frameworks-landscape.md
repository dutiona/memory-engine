# Agent Frameworks Landscape Research

**Date**: 2026-03-07
**Context**: Building a persistent autonomous long-running AI agent on Mac Mini M4 (32GB). Rust primary language. Backbone LLM: Qwen 3.5 35B MoE via Ollama.

---

## Table of Contents

1. [OpenClaw Ecosystem](#1-openclaw-ecosystem)
2. [Rust-Based Agent Frameworks](#2-rust-based-agent-frameworks)
3. [Other Autonomous Agent Frameworks](#3-other-autonomous-agent-frameworks)
4. [Agent Runtime Patterns](#4-agent-runtime-patterns)
5. [MCP Ecosystem](#5-mcp-model-context-protocol-ecosystem)
6. [Memory Systems](#6-memory-systems-for-agents)
7. [Comparative Analysis](#7-comparative-analysis)

---

## 1. OpenClaw Ecosystem

### OpenClaw

- **URL**: https://github.com/openclaw/openclaw
- **Language**: TypeScript
- **Stars**: ~247,000 (as of March 2026)
- **License**: Open source
- **Creator**: Peter Steinberger (joining OpenAI; project moving to open-source foundation)

**History**: Originally published November 2025 as "Clawdbot", renamed "Moltbot" (Jan 27 2026) after Anthropic trademark complaints, then "OpenClaw" three days later. One of the fastest-growing open-source projects in GitHub history.

**Architecture**:

- Orchestration-first design: prompts, tools, protocols, integrations
- TypeScript chosen for hackability over performance
- Plugin SDK for extensibility
- Memory via two agent-facing tools: `memory_search` (semantic recall with hybrid BM25 + embedding search) and `memory_get` (targeted file/line reads)
- Task queues, cron jobs, dashboard, multi-channel messaging (WhatsApp, Telegram, Slack, Discord, Gmail)

**Strengths**:

- Massive community and ecosystem (awesome-openclaw, skill repos)
- Comprehensive messaging integration
- Well-documented, auditable architecture
- Active development with foundation governance

**Weaknesses**:

- TypeScript: no native performance, GC pauses, not ideal for long-running memory-constrained agents
- Designed as a personal assistant, not a coding/training workhorse
- Security concerns led to multiple forks
- Creator departing to OpenAI raises governance questions

**Relevance to our use case**: LOW. TypeScript, personal-assistant oriented. Architecture patterns (memory, plugin SDK, task queues) worth studying.

---

### NanoClaw

- **URL**: https://github.com/qwibitai/nanoclaw
- **Language**: TypeScript (on Anthropic Agents SDK)
- **Stars**: ~7,000+
- **License**: MIT

**Architecture**: ~500 lines of TypeScript. Containerized for security isolation. Runs on Anthropic's Agent SDK. Auditable by human or AI in ~8 minutes.

**Strengths**: Security-first (containers), minimal attack surface, easy to audit.
**Weaknesses**: Tied to Anthropic SDK, TypeScript, minimal features by design.

---

### ZeroClaw

- **URL**: https://github.com/zeroclaw-labs/zeroclaw
- **Language**: Rust
- **Stars**: ~1,326
- **License**: Open source

**Architecture**:

- Trait-driven, fully modular: providers, channels, tools, memory, tunnels are all swappable traits
- 3.4 MB binary, cold startup <10ms
- 22+ provider compatibility (including OpenAI-compatible endpoints like Ollama)
- Built-in memory, observability, tool orchestration
- Sandbox controls, filesystem scoping, allowlists, encrypted secrets

**Strengths**:

- Rust: memory-safe, no GC, excellent for long-running processes
- Tiny footprint: runs on Raspberry Pi ($10 hardware)
- Trait-based extensibility makes it highly customizable
- Production security features built in

**Weaknesses**:

- Small community (1.3k stars vs OpenClaw's 247k)
- Less ecosystem tooling and integrations
- Young project, stability unproven at scale

**Relevance to our use case**: HIGH. Rust, trait-driven, small footprint, supports Ollama-compatible endpoints. Closest existing project to what we need.

---

### PicoClaw

- **URL**: GitHub (PicoClaw)
- **Language**: Go
- **Stars**: ~3,178

Focused on speed, simplicity, portability. Runs on $10 RISC-V boards. Not Rust, so less relevant.

### IronClaw

- **Language**: Rust
- **Stars**: Smaller community

Another Rust fork, less documented than ZeroClaw. Demands more Rust expertise.

### Nanobot

- **Language**: Python
- **Stars**: ~1,824

~4,000 lines of Python delivering core OpenClaw functionality. Python makes it easy to prototype but wrong language for our use case.

---

## 2. Rust-Based Agent Frameworks

### Rig (Rust Inference Gateway)

- **URL**: https://www.rig.rs / https://github.com/0xPlaygrounds/rig
- **Language**: Rust
- **Docs**: https://docs.rs/rig-core
- **Activity**: Actively maintained, used by Dria, Probe, NINE, Linera Protocol

**Architecture**:

- Composable traits wrapping LLM providers, embedding models, vector stores
- Agent abstraction from simple chatbots to full RAG systems
- 20+ model providers, 10+ vector stores
- Native Ollama integration (`http://localhost:11434`)
- No vendor lock-in, no unsafe code
- ~10 MB Docker images for simple bots

**Benchmarks**:

- 24.3% CPU usage (most efficient among tested frameworks)
- <1.1 GB peak memory (Python frameworks: >4.7 GB)
- Small binary sizes

**Strengths**:

- Most mature Rust LLM framework
- Excellent Ollama support (native provider)
- RAG built-in
- Type-safe, no GC pauses, predictable latency
- Active community and real production users

**Weaknesses**:

- Not an agent _runtime_ -- it's an LLM abstraction layer
- No built-in process supervision, task queues, or memory persistence
- No multi-agent orchestration out of the box
- You build the agent loop yourself

**Relevance to our use case**: HIGH as a building block. Use Rig as the LLM interface layer, build agent runtime on top.

---

### AutoAgents (LiquidOS)

- **URL**: https://github.com/liquidos-ai/AutoAgents
- **Language**: Rust
- **Docs**: https://liquidos-ai.github.io/AutoAgents/

**Architecture**:

- Multi-crate workspace: `autoagents-core`, `autoagents-llm`, `autoagents-telemetry`, `autoagents-toolkit`, `autoagents-derive`
- Environment manages Agents; each Agent has Tools, Memory, Executor
- ReAct (Reason-Act-Observe) and Plan-and-Execute patterns
- Structured tool calling with JSON schema validation
- Configurable memory backends
- OpenTelemetry integration
- Typed pub/sub communication between agents

**Benchmarks**:

- 25% lower latency vs average Python framework
- 36% more throughput under same concurrency
- 84% more throughput than LangGraph

**Strengths**:

- Full multi-agent framework, not just an LLM wrapper
- Memory and tool systems built in
- Derive macros for ergonomic agent definition
- Strong performance characteristics

**Weaknesses**:

- Younger project, smaller community than Rig
- Documentation still maturing
- Less production validation than Python alternatives

**Relevance to our use case**: HIGH. Closest to a complete Rust agent framework. Memory, tools, multi-agent, telemetry all included.

---

### Kowalski

- **URL**: https://github.com/yarenty/kowalski
- **Language**: Rust

**Architecture**:

- Modular multi-agent framework with specialized agent crates: academic, code, data, web
- Federation crate for inter-agent communication and pipeline automation
- Pluggable tools: CSV, code (Java/Python/Rust), web search, scraping, PDF
- Unified CLI across all agents
- Local-first design

**Strengths**:

- Specialized agents for different domains (code review, data analysis, web)
- Federation enables multi-agent pipelines
- Local-first philosophy matches our requirements

**Weaknesses**:

- Smaller community
- Less documented integration with Ollama/local models
- Niche use cases (academic, code review)

**Relevance to our use case**: MEDIUM. Federation concept interesting. Specialized agents could be adapted.

---

### Other Rust Options

| Name             | URL                                         | Notes                                                 |
| ---------------- | ------------------------------------------- | ----------------------------------------------------- |
| **Anda**         | https://github.com/ldclabs/anda             | ICP blockchain + TEE powered. Crypto-focused.         |
| **AgentAI**      | https://github.com/AdamStrojek/rust-agentai | Simple Rust library for AI agents.                    |
| **rs-graph-llm** | https://github.com/a-agmon/rs-graph-llm     | Graph-based multi-agent workflows in Rust.            |
| **GraphBit**     | N/A                                         | Enterprise-focused, deterministic, concurrency-first. |

---

## 3. Other Autonomous Agent Frameworks

### AutoGPT

- **URL**: https://github.com/Significant-Gravitas/AutoGPT
- **Language**: Python
- **Stars**: 170k+

**Current State (2026)**: Evolved from viral experiment to low-code platform. Still the benchmark name but surpassed in polish and enterprise readiness by newer frameworks. Strong for long-running independent tasks and prototyping automation. Fewer guardrails = more freedom but riskier for production.

**Relevance**: LOW. Python, cloud-oriented, not designed for local-first persistent agents.

---

### CrewAI

- **URL**: https://github.com/crewAIInc/crewAI / https://crewai.com
- **Language**: Python
- **Stars**: High (widely adopted)

**Architecture**:

- Role-based multi-agent framework
- Two modes: Crews (autonomous teams) and Flows (event-driven workflows)
- Independent of LangChain (built from scratch)
- Enterprise layer for deployment/monitoring

**Strengths**: Clean role-based model, production-ready, enterprise adoption (PwC, IBM, NVIDIA). 1.4B+ agentic automations.
**Weaknesses**: Python only, cloud-focused enterprise model.

**Relevance**: LOW for implementation. HIGH for architectural patterns (role-based agents, crew/flow separation).

---

### LangGraph

- **URL**: https://github.com/langchain-ai/langgraph
- **Language**: Python/TypeScript
- **Stars**: Part of LangChain ecosystem (38M+ monthly PyPI downloads)

**Architecture**:

- Stateful multi-actor applications as directed graphs (nodes, edges, shared state)
- Built-in persistence: save/resume at any point
- Human-in-the-loop first-class support
- Thousands of pre-built integrations

**Strengths**: Most mature agent orchestration framework. Durable state. Massive integration ecosystem.
**Weaknesses**: Python/TS only. Complex abstraction layers. Heavy dependency tree.

**Relevance**: LOW for implementation. HIGH for patterns (graph-based agent flows, durable state, checkpointing).

---

### OpenHands (formerly OpenDevin)

- **URL**: https://github.com/OpenHands/OpenHands / https://openhands.dev
- **Language**: Python
- **Stars**: 64k+

**Architecture**:

- Modular SDK (V1 refactored from monolithic design)
- Event log for state/memory (commands, edits, results)
- Sandboxed execution environments
- Model-agnostic multi-LLM routing
- Native GitHub/GitLab/CI/CD/Slack integrations

**Strengths**: Best open-source coding agent. Composable SDK. Cloud-scalable.
**Weaknesses**: Python. Cloud-first design.

**Relevance**: MEDIUM. Coding agent patterns worth studying. Event-log memory model is elegant.

---

### SWE-agent

- **Language**: Python
- **Focus**: Research-oriented SWE-bench evaluation
- **Status**: More research than production. OpenHands has surpassed it on benchmarks.

**Relevance**: LOW.

---

### Aider

- **URL**: https://aider.chat
- **Language**: Python
- **Stars**: Popular

**Architecture**:

- Terminal-first, Git-first coding agent
- Repository map for whole-codebase awareness
- Multi-file coordinated changes with auto-commits
- Supports Ollama and OpenAI-compatible endpoints for local models
- Clean architecture: map repo -> add context -> LLM generates diffs -> apply + commit

**Strengths**: Best terminal-based coding UX. Git-native. Local model support via Ollama.
**Weaknesses**: Python. Single-purpose (coding only). Not a general agent runtime.

**Relevance**: LOW for framework choice. HIGH for coding agent interaction patterns.

---

### PydanticAI

- **URL**: https://github.com/pydantic/pydantic-ai
- **Language**: Python
- **Stars**: Growing fast

**Architecture**: Type-safe agent framework with validated tool calls, typed dependencies/outputs. Model-agnostic. OpenTelemetry integration via Pydantic Logfire.

**Relevance**: LOW (Python). Design philosophy of type-safe agent definitions is well-matched to Rust's type system.

---

### OpenAI Agents SDK

- **URL**: https://github.com/openai/openai-agents-python
- **Language**: Python
- **Stars**: 19k+

Five primitives: Agents, Handoffs, Guardrails, Sessions, Tracing. Successor to Swarm. Provider-agnostic despite the name.

**Relevance**: LOW (Python, cloud-oriented). Handoff pattern worth studying.

---

## 4. Agent Runtime Patterns

### Process Supervision

| Platform       | Tool            | Key Features                                                                                |
| -------------- | --------------- | ------------------------------------------------------------------------------------------- |
| macOS          | **launchd**     | Native Mac daemon manager. KeepAlive, StartInterval, WatchPaths. Sends SIGTERM on shutdown. |
| Linux          | **systemd**     | After=/Before= for ordering. Restart=always. TimeoutStopSec for graceful shutdown.          |
| Cross-platform | **supervisord** | Python-based, simple config. Less integrated than native options.                           |

**Recommendation for Mac Mini M4**: Use `launchd` with a plist that sets `KeepAlive=true`, `RunAtLoad=true`, and handles SIGTERM for graceful state persistence.

### Graceful Shutdown and Restart

Best practices from production agent deployments:

1. **SIGTERM handler**: Save current task state, flush memory to disk, close LLM connections
2. **Checkpoint-based state**: Persist agent state at regular intervals (not just on shutdown)
3. **WAL (Write-Ahead Log)**: Event log approach (like OpenHands) ensures no work is lost
4. **Idempotent task design**: Tasks can be safely retried after restart

### Task Queue Management

Patterns observed across frameworks:

- **OpenClaw**: Built-in cron jobs + task queues
- **CrewAI Flows**: Event-driven task routing
- **Hydra**: Shared task queue across multiple AI models with intelligent routing
- **Circuit breaker**: Per-model failure tracking with automatic recovery after cool-down

For Rust implementation, consider:

- `tokio` channels for in-process task queues
- SQLite/sled for durable task persistence
- Priority queues with deadline-aware scheduling

### Error Recovery and Self-Healing

Key patterns from research:

1. **Automatic retries with exponential backoff**: Standard for transient LLM failures
2. **Circuit breaker**: Track failure rates per-provider, fail fast when threshold exceeded
3. **Graceful degradation**: Fall back to simpler models or cached responses
4. **Context-aware retry**: Include error context in retry prompt
5. **Self-evolving agents**: LLM-as-judge evals + iterative prompt refinement (OpenAI cookbook pattern)
6. **Escalation procedures**: Automatic notification when agent is stuck

Production results: 70% reduction in incident frequency, MTTR from 18min to <2min.

---

## 5. MCP (Model Context Protocol) Ecosystem

### Protocol Overview

MCP is the standardized integration layer for AI agents accessing tools. Analogous to USB-C for hardware -- universal connector between AI systems and external capabilities.

- **Governance**: Donated to Linux Foundation's Agentic AI Foundation (Dec 2025) by Anthropic, co-founded with Block and OpenAI, backed by Google, Microsoft, AWS, Cloudflare
- **Adoption**: 97M+ monthly SDK downloads (Python + TypeScript). 1,000+ community-built servers.
- **Impact**: 40-60% faster agent deployment times reported

### Rust MCP SDKs

| SDK                 | URL                                              | Notes                                                               |
| ------------------- | ------------------------------------------------ | ------------------------------------------------------------------- |
| **Official (rmcp)** | https://github.com/modelcontextprotocol/rust-sdk | Tokio async, client + server, multiple transport layers             |
| **rust-mcp-sdk**    | https://crates.io/crates/rust-mcp-sdk            | Full protocol implementation (2025-11-25 spec), backward compatible |
| **mcp_rust_sdk**    | https://docs.rs/mcp_rust_sdk                     | Alternative community implementation                                |
| **mcp_client_rs**   | https://docs.rs/mcp_client_rs                    | Client-only implementation                                          |

### MCP in Agent Frameworks

- **ZeroClaw**: Tool orchestration via MCP-compatible interface
- **Rig**: Can integrate MCP tools through provider abstraction
- **AutoAgents**: Toolkit crate could wrap MCP servers
- **OpenClaw**: Plugin SDK can expose MCP servers
- **PydanticAI, OpenAI SDK, Google ADK**: Native MCP support

### Relevance to Our Use Case

MCP is critical. Building our agent with MCP support means:

1. Access to 1,000+ existing tool servers (filesystem, git, databases, web)
2. Our agent's tools are reusable by other AI systems (Claude, Codex, Gemini)
3. The official Rust SDK (`rmcp`) is production-ready with tokio async
4. Standardized protocol means we don't reinvent tool interfaces

---

## 6. Memory Systems for Agents

### Mem0

- **URL**: https://github.com/mem0ai/mem0 / https://mem0.ai
- **Language**: Python (SDK), but architecture is language-agnostic (API-based)

**Architecture**:

- Extracts, consolidates, retrieves salient information from conversations
- Base Mem0: dedicated modules for memory extraction and update
- Mem0g: graph-based memory for complex relational structures
- Every memory timestamped, versioned, exportable

**Performance**: 26% accuracy boost, 91% lower p95 latency, 90% token savings vs naive context stuffing.

### Memory Architecture Patterns

From the research literature ("Memory in the Age of AI Agents", arXiv:2512.13564):

| Type                  | Purpose                                 | Implementation                       |
| --------------------- | --------------------------------------- | ------------------------------------ |
| **Working memory**    | Current task context                    | LLM context window                   |
| **Episodic memory**   | Specific past experiences and outcomes  | Event log + vector search            |
| **Semantic memory**   | General principles and patterns learned | Structured knowledge base            |
| **Procedural memory** | How to perform tasks                    | Tool definitions + learned workflows |

### Storage Infrastructure Options for Rust

| Storage                   | Use Case                                     | Rust Support          |
| ------------------------- | -------------------------------------------- | --------------------- |
| **SQLite** (via rusqlite) | Structured memory, task state, metadata      | Excellent             |
| **sled**                  | Embedded key-value store, fast writes        | Native Rust           |
| **Qdrant**                | Vector similarity search for semantic memory | Rust client available |
| **Tantivy**               | Full-text search (BM25) for keyword recall   | Native Rust           |
| **redb**                  | Simple embedded database                     | Native Rust           |

### Hybrid Search (OpenClaw's Approach)

OpenClaw's memory uses hybrid BM25 + embedding search:

- BM25 ranks converted to scores with smooth decay
- Top results dominate but lower ranks contribute
- This is directly implementable in Rust with Tantivy (BM25) + Qdrant (embeddings)

---

## 7. Comparative Analysis

### Framework Scoring for Our Use Case

Criteria: Rust compatibility, local-first (Ollama), long-running stability, memory handling, tool integration (MCP), community health, coding/training capability.

| Framework      | Language   | Local LLM           | Memory        | MCP         | Long-Running    | Score    |
| -------------- | ---------- | ------------------- | ------------- | ----------- | --------------- | -------- |
| **ZeroClaw**   | Rust       | Yes (22+ providers) | Built-in      | Partial     | Designed for it | **8/10** |
| **AutoAgents** | Rust       | Yes (pluggable)     | Configurable  | Via toolkit | Good (async)    | **8/10** |
| **Rig**        | Rust       | Yes (native Ollama) | No built-in   | Manual      | Building block  | **7/10** |
| **Kowalski**   | Rust       | Yes                 | Basic         | No          | Good            | **6/10** |
| **OpenClaw**   | TypeScript | Yes                 | Hybrid search | Via plugins | Yes             | **5/10** |
| **LangGraph**  | Python     | Yes                 | Durable state | Native      | Yes             | **4/10** |
| **CrewAI**     | Python     | Yes                 | Basic         | Partial     | Yes             | **4/10** |
| **OpenHands**  | Python     | Yes                 | Event log     | Partial     | Cloud-first     | **3/10** |

### What Exists vs. What We Need

**What we need**:

1. Rust agent runtime with long-running process management
2. Ollama integration for Qwen 3.5 35B MoE
3. Persistent multi-type memory (episodic, semantic, procedural)
4. MCP tool orchestration (use existing MCP servers)
5. Task queue with error recovery
6. Coding capability (edit files, run tests, git)
7. ML training capability (launch training runs, monitor)
8. Self-healing and graceful restart

**What exists**:

- **Rig** covers #2 (Ollama integration) excellently
- **AutoAgents** covers #1 partially, #5 partially
- **ZeroClaw** covers #1, #2, #5 partially
- **Official MCP Rust SDK** covers #4
- **No single framework** covers all 8 requirements

### Gap Analysis

| Requirement             | Gap                                                        | Closest Solution                       |
| ----------------------- | ---------------------------------------------------------- | -------------------------------------- |
| Rust agent runtime      | Partial -- ZeroClaw/AutoAgents exist but not battle-tested | ZeroClaw traits + custom runtime       |
| Ollama/Qwen integration | Solved                                                     | Rig's native Ollama provider           |
| Persistent memory       | No Rust-native solution                                    | Build with Tantivy + Qdrant + SQLite   |
| MCP tools               | SDK exists, no agent-level integration                     | rmcp SDK + custom tool registry        |
| Task queue              | No Rust agent framework has durable task queues            | tokio channels + SQLite WAL            |
| Coding capability       | No Rust coding agent exists                                | Build, study Aider/OpenHands patterns  |
| ML training             | Nothing exists                                             | Custom: launch processes, parse logs   |
| Self-healing            | Patterns exist, no Rust implementation                     | Circuit breaker + checkpoint + launchd |

### Recommended Architecture

```
                    ┌─────────────────────────────────┐
                    │         launchd (macOS)          │
                    │   Process supervision + restart  │
                    └──────────────┬──────────────────┘
                                   │
                    ┌──────────────▼──────────────────┐
                    │      Agent Runtime (custom)      │
                    │  - Task scheduler (tokio)        │
                    │  - Error recovery / circuit breaker│
                    │  - Checkpoint persistence        │
                    └──────────────┬──────────────────┘
                                   │
              ┌────────────────────┼────────────────────┐
              │                    │                     │
   ┌──────────▼─────────┐  ┌──────▼───────┐  ┌─────────▼────────┐
   │    LLM Layer (Rig)  │  │ Memory Layer │  │  Tool Layer (MCP)│
   │  - Ollama provider  │  │ - SQLite     │  │  - rmcp SDK      │
   │  - Qwen 3.5 35B     │  │ - Tantivy    │  │  - 1000+ servers │
   │  - Streaming        │  │ - Qdrant     │  │  - Custom tools  │
   └─────────────────────┘  └──────────────┘  └──────────────────┘
```

**Build strategy**: Don't adopt a single framework wholesale. Compose from:

1. **Rig** for LLM interface (Ollama/Qwen)
2. **rmcp** for MCP tool orchestration
3. **ZeroClaw's traits** as architectural reference for extensibility
4. **AutoAgents' patterns** for multi-agent and ReAct loops
5. **Custom runtime** for task queue, memory, process lifecycle
6. Study **OpenClaw** (memory), **Aider** (coding), **OpenHands** (event log) for patterns

---

## Sources

### OpenClaw Ecosystem

- [OpenClaw - Wikipedia](https://en.wikipedia.org/wiki/OpenClaw)
- [NanoClaw GitHub](https://github.com/qwibitai/nanoclaw)
- [ZeroClaw GitHub](https://github.com/zeroclaw-labs/zeroclaw)
- [ZeroClaw vs OpenClaw vs NanoClaw 2026 Comparison](https://www.lushbinary.com/blog/zeroclaw-openclaw-personal-ai-agents-compared-2026/)
- [OpenClaw Memory Architecture](https://www.mmntm.net/articles/openclaw-memory-architecture)
- [OpenClaw Architecture Explained](https://ppaolo.substack.com/p/openclaw-system-architecture-overview)
- [5 OpenClaw Alternatives - KDnuggets](https://www.kdnuggets.com/5-lightweight-and-secure-openclaw-alternatives-to-try-right-now)
- [NanoClaw - The Register](https://www.theregister.com/2026/03/01/nanoclaw_container_openclaw/)
- [NanoClaw - VentureBeat](https://venturebeat.com/orchestration/nanoclaw-solves-one-of-openclaws-biggest-security-issues-and-its-already)
- [Nanobot vs NanoClaw vs IronClaw comparison](https://medium.com/@gemQueenx/nanobot-vs-nanoclaw-vs-ironclaw-vs-zeroclaw-vs-picoclaw-vs-tinyclaw-which-openclaw-mini-wins-for-9a0537220f3b)

### Rust Frameworks

- [Rig - Build LLM Applications in Rust](https://www.rig.rs/)
- [Rig GitHub](https://github.com/0xPlaygrounds/rig)
- [Rig Ollama Provider Docs](https://docs.rs/rig-core/latest/rig/providers/ollama/index.html)
- [AutoAgents GitHub](https://github.com/liquidos-ai/AutoAgents)
- [AutoAgents Documentation](https://liquidos-ai.github.io/AutoAgents/)
- [AutoAgents Case Study](https://dev.to/harshal_rembhotkar/case-study-liquidoss-autoagents-building-smarter-ai-agents-in-rust-20nl)
- [Kowalski GitHub](https://github.com/yarenty/kowalski)
- [Kowalski DEV.to](https://dev.to/yarenty/kowalski-the-rust-native-agentic-ai-framework-53k4)
- [Rust Libraries for LLM Orchestration 2026](https://dasroot.net/posts/2026/02/rust-libraries-llm-orchestration-2026/)
- [Benchmarking AI Agent Frameworks 2026](https://dev.to/saivishwak/benchmarking-ai-agent-frameworks-in-2026-autoagents-rust-vs-langchain-langgraph-llamaindex-338f)
- [ZeroClaw DEV.to](https://dev.to/brooks_wilson_36fbefbbae4/zeroclaw-a-lightweight-secure-rust-agent-runtime-redefining-openclaw-infrastructure-2cl0)

### Python/TS Agent Frameworks

- [AutoGPT GitHub](https://github.com/Significant-Gravitas/AutoGPT)
- [CrewAI](https://crewai.com/)
- [CrewAI GitHub](https://github.com/crewAIInc/crewAI)
- [LangGraph](https://www.langchain.com/langgraph)
- [LangGraph GitHub](https://github.com/langchain-ai/langgraph)
- [OpenHands](https://openhands.dev/)
- [OpenHands GitHub](https://github.com/OpenHands/OpenHands)
- [OpenHands SDK Paper](https://arxiv.org/html/2511.03690v1)
- [Aider](https://aider.chat/)
- [PydanticAI GitHub](https://github.com/pydantic/pydantic-ai)
- [OpenAI Agents SDK GitHub](https://github.com/openai/openai-agents-python)
- [Top Agentic AI Frameworks 2026](https://www.alphamatch.ai/blog/top-agentic-ai-frameworks-2026)
- [LangGraph vs CrewAI vs Agents SDK 2026](https://particula.tech/blog/langgraph-vs-crewai-vs-openai-agents-sdk-2026)

### MCP

- [MCP Official Site](https://modelcontextprotocol.io/)
- [MCP Rust SDK GitHub](https://github.com/modelcontextprotocol/rust-sdk)
- [rust-mcp-sdk on crates.io](https://crates.io/crates/rust-mcp-sdk)
- [A Year of MCP Review](https://www.pento.ai/blog/a-year-of-mcp-2025-review)
- [MCP Guide 2026](https://www.buildmvpfast.com/blog/model-context-protocol-mcp-guide-2026)
- [MCP & Multi-Agent AI 2026](https://onereach.ai/blog/mcp-multi-agent-ai-collaborative-intelligence/)
- [Building MCP Server in Rust](https://oneuptime.com/blog/post/2026-01-07-rust-mcp-server/view)
- [Building stdio MCP Server in Rust - Shuttle](https://www.shuttle.dev/blog/2025/07/18/how-to-build-a-stdio-mcp-server-in-rust)

### Memory Systems

- [Mem0 GitHub](https://github.com/mem0ai/mem0)
- [Mem0 Research Paper](https://arxiv.org/abs/2504.19413)
- [Memory in the Age of AI Agents](https://arxiv.org/abs/2512.13564)
- [AI Agent Memory - Redis](https://redis.io/blog/ai-agent-memory-stateful-systems/)
- [Memory for AI Agents - The New Stack](https://thenewstack.io/memory-for-ai-agents-a-new-paradigm-of-context-engineering/)
- [3 Types of Long-term Memory for AI Agents](https://machinelearningmastery.com/beyond-short-term-memory-the-3-types-of-long-term-memory-ai-agents-need/)

### Runtime Patterns

- [Apple launchd Documentation](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html)
- [Autonomous Workflow Agent Architecture](https://agentic-patterns.com/patterns/autonomous-workflow-agent-architecture/)
- [Self-Evolving Agents - OpenAI Cookbook](https://cookbook.openai.com/examples/partners/self_evolving_agents/autonomous_agent_retraining)
- [Hydra Multi-Agent Orchestrator](https://github.com/PrimeLocus/Hydra)
- [Why Agentic AI Devs Move to Rust - Red Hat](https://developers.redhat.com/articles/2025/09/15/why-some-agentic-ai-developers-are-moving-code-python-rust)
