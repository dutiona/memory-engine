# 05 - MCP Ecosystem & Tool Orchestration for Autonomous Agents

**Research date**: 2026-03-07
**Context**: Persistent autonomous agent on Mac Mini M4 with Qwen 3.5 35B MoE, needing a rich tool ecosystem.

---

## 1. MCP Ecosystem Overview (2026 State)

### What is MCP?

Model Context Protocol (MCP) is an open protocol created by Anthropic (November 2024) that standardizes how LLM applications connect to external tools, data sources, and services. It uses JSON-RPC over two transport mechanisms:

- **stdio** -- for local process communication (server runs as child process)
- **Streamable HTTP** -- for remote/networked servers (replaced SSE in spec 2025-06-18)

**Current spec version**: `2025-11-25` (latest as of March 2026). Key additions over the lifecycle:

- Streamable HTTP transport replacing SSE (2025-06-18)
- Session management via `MCP-Session-Id` headers
- Backward compatibility with 2024-11-05 SSE transport

**Governance**: Anthropic donated MCP to the Linux Foundation's Agentic AI Foundation (AAIF) in December 2025. Python and TypeScript SDKs have surpassed 97 million monthly downloads.

### MCP Server Registry

| Registry                                                           | Server Count | Notes                                                 |
| ------------------------------------------------------------------ | ------------ | ----------------------------------------------------- |
| [Official MCP Registry](https://registry.modelcontextprotocol.io/) | ~518         | Curated, vendor-neutral, API freeze v0.1 (2025-10-24) |
| [FastMCP](https://fastmcp.me/)                                     | ~1,864       | Broader community tracker                             |
| [mcpservers.org](https://mcpservers.org/)                          | 1,000+       | Aggregator                                            |
| [mcp-get.com](https://mcp-get.com/)                                | Unknown      | Package manager style                                 |

The ecosystem has grown explosively. In April 2025, OpenAI officially adopted MCP. Microsoft, Google, and most major AI tooling vendors now support it.

### Key MCP Servers for Our Use Case

| Category           | Server                                                                                                      | Maturity                | Notes                                                                      |
| ------------------ | ----------------------------------------------------------------------------------------------------------- | ----------------------- | -------------------------------------------------------------------------- |
| **Filesystem**     | [modelcontextprotocol/filesystem](https://github.com/modelcontextprotocol/servers/tree/main/src/filesystem) | Mature (reference impl) | Sandboxed directory access, read/write, search. Available as Docker image. |
| **Git**            | [modelcontextprotocol/git](https://github.com/modelcontextprotocol/servers)                                 | Mature                  | Clone, commit, branch, diff, log, status, push/pull.                       |
| **Git (GitHub)**   | [github/github-mcp-server](https://github.com/github/github-mcp-server)                                     | Mature (official)       | Issues, PRs, code review, actions, releases.                               |
| **Git (advanced)** | [cyanheads/git-mcp-server](https://github.com/cyanheads/git-mcp-server)                                     | Active                  | Worktree, rebase, cherry-pick, GPG signing, STDIO & HTTP.                  |
| **Git (review)**   | [GitKraken MCP](https://www.gitkraken.com/mcp)                                                              | Commercial              | Regression debugging, diff analysis, commit history.                       |
| **Code execution** | [code-sandbox-mcp](https://github.com/Automata-Labs-team/code-sandbox-mcp)                                  | Active                  | Docker-based sandboxed execution. Memory/CPU/time limits.                  |
| **Code execution** | [agent-infra/sandbox](https://github.com/agent-infra/sandbox)                                               | Active                  | AIO: Browser + Shell + File + MCP + VSCode in one container.               |
| **Code execution** | [container-mcp](https://github.com/54rt1n/container-mcp)                                                    | Active                  | Podman-based. File, code exec, bash, knowledgebase.                        |
| **Web browsing**   | Playwright MCP                                                                                              | Mature                  | Navigate, screenshot, click, fill forms, evaluate JS.                      |
| **Web browsing**   | [Firecrawl MCP](https://github.com/mendableai/firecrawl)                                                    | Mature                  | Scrape, crawl, search, extract, agent mode.                                |
| **Web browsing**   | Puppeteer MCP                                                                                               | Mature                  | Chrome automation via MCP.                                                 |
| **Database**       | SQLite MCP (reference)                                                                                      | Mature                  | Query, schema inspection, data manipulation.                               |
| **Database**       | PostgreSQL MCP                                                                                              | Mature                  | Multiple implementations available.                                        |
| **Shell**          | Built into multiple servers                                                                                 | Mature                  | Often bundled with filesystem or container servers.                        |
| **Docker**         | [Dagger container-use](https://www.pulsemcp.com/servers/dagger-container-use)                               | Active                  | Official container management MCP.                                         |
| **Slack**          | Slack MCP                                                                                                   | Mature                  | Read/send messages, channel management.                                    |
| **Discord**        | [discord-mcp](https://github.com/SaseQ/discord-mcp)                                                         | Active                  | Full Discord API: messages, threads, events, slash commands.               |
| **Discord**        | [Discord-AI-Agent](https://github.com/OoriData/Discord-AI-Agent)                                            | Active                  | MCP-powered Discord bot framework.                                         |
| **Telegram**       | [telegram-mcp](https://github.com/sparfenyuk/mcp-telegram)                                                  | Active                  | MTProto-based, full Telegram API.                                          |
| **Telegram**       | [telegram-mcp (Bot API)](https://github.com/guangxiangdebizi/telegram-mcp)                                  | Active                  | Bot API-based, modular architecture.                                       |

### MCP Client Libraries by Language

| Language        | SDK                                                                    | Status           | Notes                                                    |
| --------------- | ---------------------------------------------------------------------- | ---------------- | -------------------------------------------------------- |
| **TypeScript**  | [Official SDK](https://github.com/modelcontextprotocol/typescript-sdk) | Most mature      | Node.js, Bun, Deno. Server + client libs.                |
| **Python**      | [Official SDK](https://github.com/modelcontextprotocol/python-sdk)     | Mature           | Type-safe, HTTP scaffolding, tool helpers.               |
| **Rust**        | [rmcp (official)](https://github.com/modelcontextprotocol/rust-sdk)    | Active (v0.16.0) | Tokio async, `#[tool]` macro, client + server.           |
| **Rust**        | [rust-mcp-sdk](https://github.com/rust-mcp-stack/rust-mcp-sdk)         | Active (v0.8.0)  | Full 2025-11-25 spec, backward compat.                   |
| **Java/Kotlin** | Official SDK                                                           | Available        | Listed on official site.                                 |
| **C#**          | Official SDK                                                           | Available        | Listed on official site.                                 |
| **Go**          | Community only                                                         | Immature         | No official SDK yet. Multiple community implementations. |
| **C++**         | [gopher-mcp](https://github.com/GopherSecurity/gopher-mcp)             | Early            | Enterprise-grade security focus.                         |

**For our Rust-based agent**: Both `rmcp` (official) and `rust-mcp-sdk` are viable. `rmcp` is the official one and likely the better long-term bet. Both support async Tokio, client and server modes.

### MCP Security Concerns

This is a significant area of risk:

- **30 CVEs filed** in 60 days (early 2026)
- **38% of 500+ scanned servers** lack authentication entirely
- **8,000+ MCP servers exposed** on public internet (Trend Micro scan)
- **43% vulnerable** to command injection (Equixly assessment)
- **30% vulnerable** to SSRF
- **22% allow** arbitrary file access
- **36.7% of web-facing servers** have latent SSRF exposure

**Mitigation for our agent**: Run MCP servers locally over stdio (not HTTP), in sandboxed containers, with no public exposure. Use the official reference servers where possible.

---

## 2. MCP for Non-Claude Models

### Critical Question: Can Qwen 3.5 35B Use MCP?

**Yes, with caveats.** MCP is model-agnostic at the protocol level -- it standardizes tool/resource/prompt exchange between client and server. The model-specific part is how the client translates MCP tool definitions into the model's function-calling format.

### Qwen 3.5 35B MoE Tool Calling

Qwen 3.5-35B-A3B is a MoE model with 35B total params / 3B active. It has native tool calling support:

- Built-in function calling with parallel tool calls
- [Qwen-Agent](https://github.com/QwenLM/Qwen-Agent) framework with explicit MCP support
- Qwen-Agent encapsulates tool-calling templates and parsers internally
- MCP configuration files can define available tools directly

**Known issue**: There's an [open GitHub issue](https://github.com/QwenLM/Qwen3.5/issues/12) about Qwen 3.5 Plus MCP tool calling failures, suggesting the integration is still being ironed out.

### Open-Source MCP Clients for Local Models

| Client                                                                           | Description                                               | Model Support                                       |
| -------------------------------------------------------------------------------- | --------------------------------------------------------- | --------------------------------------------------- |
| [Dolphin MCP](https://github.com/QuixiAI/dolphin-mcp)                            | Python library + CLI. Multi-server, multi-model.          | OpenAI, Anthropic, Ollama, LMStudio, DeepSeek       |
| [mcp-client-for-ollama](https://github.com/jonigl/mcp-client-for-ollama)         | TUI client. Agent mode, human-in-the-loop, thinking mode. | Ollama (any model)                                  |
| [ollama-mcp-bridge](https://github.com/patruff/ollama-mcp-bridge)                | Bridge between Ollama and MCP servers.                    | Ollama                                              |
| [ollama-mcp-client](https://github.com/mihirrd/ollama-mcp-client)                | Simple MCP client for local Ollama models.                | Ollama                                              |
| [langchain-mcp-adapters](https://github.com/langchain-ai/langchain-mcp-adapters) | LangChain <-> MCP bridge.                                 | Any LangChain-supported model                       |
| [OpenCode](https://opencode.ai/)                                                 | Terminal coding agent. 75+ providers.                     | Ollama, vLLM, OpenAI, Anthropic, Copilot            |
| [Continue.dev](https://www.continue.dev/)                                        | IDE extension. MCP-native agents via config.yaml.         | Ollama, LM Studio, llama.cpp, any OpenAI-compatible |

### How MCP + Non-Claude Models Works

The flow:

1. MCP client connects to MCP server(s), discovers available tools (JSON schema)
2. Client converts MCP tool definitions to the model's native function-calling format (OpenAI-style `tools` array is the most common)
3. Model generates a response, optionally requesting tool calls
4. Client executes tool calls via MCP, returns results to model
5. Loop until model produces final answer

**Key insight**: The bridge layer (step 2) is where model-specific adaptation happens. Ollama exposes an OpenAI-compatible API, so any client that can convert MCP tools to OpenAI function-calling format works. Dolphin MCP and langchain-mcp-adapters both handle this.

### Recommended Stack for Our Agent

```
Qwen 3.5 35B (vLLM or Ollama)
    |
    v
Custom Rust MCP Client (using rmcp crate)
    |
    v
MCP Servers (filesystem, git, shell, browser, etc.)
```

Alternatively, use Qwen-Agent (Python) which has first-party MCP support and is maintained by the Qwen team.

---

## 3. Tool Orchestration Alternatives

### If MCP Doesn't Work Well

| Alternative                 | Description                                                  | Pros                                                   | Cons                                                                     |
| --------------------------- | ------------------------------------------------------------ | ------------------------------------------------------ | ------------------------------------------------------------------------ |
| **OpenAI Function Calling** | JSON schema tool definitions, model returns structured calls | Widely supported (Qwen, Mistral, LLaMA all support it) | No discovery protocol, no server lifecycle management                    |
| **LangChain Tools**         | Python framework with tool abstraction                       | Huge ecosystem, MCP adapters available                 | Python-only, heavy dependency, framework lock-in                         |
| **LangGraph**               | Graph-based agent flows on top of LangChain                  | Cycles, conditional logic, persistent state            | Same Python/framework lock-in issues                                     |
| **A2A (Agent-to-Agent)**    | Google's protocol for inter-agent communication              | 100+ enterprise supporters, stateful tasks             | Complementary to MCP (not a replacement), for agent-agent not agent-tool |
| **UTCP**                    | Universal Tool Calling Protocol                              | Lighter than MCP, supports REST/gRPC/WebSocket/CLI/MQ  | Newer, less ecosystem                                                    |
| **Custom Tool Registry**    | Roll your own JSON schema + executor                         | Full control, no dependencies                          | Maintenance burden, no ecosystem                                         |

### MCP vs A2A -- They're Complementary

- **MCP** = vertical: agent <-> tools (how an agent uses tools)
- **A2A** = horizontal: agent <-> agent (how agents collaborate)

For our single-agent system, MCP is the right choice. A2A becomes relevant if we later have multiple specialized agents.

### Protocol Landscape (2026)

Five open protocols forming the agentic stack:

1. **MCP** -- Agent-to-tool communication
2. **A2A** -- Agent-to-agent coordination (Google, 100+ enterprises)
3. **ACP** -- Agent Communication Protocol (IBM, merged into A2A late 2025)
4. **ANP** -- Agent Network Protocol (discovery and networking)
5. **AG-UI** -- Agent-User Interaction Protocol (UI rendering)

### Verdict

MCP is the clear winner for tool orchestration. It has:

- Largest ecosystem (500+ official servers, 1800+ community)
- Official Rust SDK
- Qwen-Agent first-party support
- OpenAI, Google, Microsoft adoption
- Linux Foundation governance

The main risk is security (see section 1), which we mitigate by running locally over stdio.

---

## 4. Development/IDE Tools for Autonomous Agents

### OpenCode

[OpenCode](https://opencode.ai/) is an open-source terminal-based AI coding agent. Think "Claude Code but model-agnostic."

- **75+ LLM providers** including Ollama, vLLM, OpenAI, Anthropic
- **GitHub Copilot backend** support (since Jan 2026) -- use existing $10-19/mo subscription
- **Mid-session model switching** -- change models without restarting
- **MCP server support** -- configure tools via config
- **Local model performance**: Tested with 16GB GPU, works but quality depends on model. Qwen3 on Ollama performs well for simpler tasks.

**Relevance to our agent**: OpenCode's architecture shows how to build a terminal coding agent with local models. We could use it directly, or study its approach for our custom agent.

### Aider

[Aider](https://aider.chat/) is an open-source AI pair programmer for terminal use.

- Works with **Ollama** for fully local, private operation
- Git-native: auto-commits, works within existing repos
- Best with Claude 3.7 Sonnet, DeepSeek R1/V3, GPT-4o
- Local models work but quality varies significantly
- **No MCP support** -- uses its own edit format (diff, whole-file, etc.)
- Active development with frequent model support updates

**Relevance**: Good reference for edit formats and git integration patterns. Less relevant for tool orchestration since it doesn't use MCP.

### Continue.dev

[Continue.dev](https://www.continue.dev/) is an open-source AI code assistant as IDE extension.

- **MCP-native**: Agents defined via `config.yaml` with MCP server configurations
- Supports Ollama, LM Studio, llama.cpp, and cloud providers
- VS Code and JetBrains plugins
- Agent mode with tool calling

**Relevance**: Shows how to compose MCP servers into a coding agent. Good architecture reference.

### SWE-agent

[SWE-agent](https://github.com/SWE-agent/SWE-agent) (Princeton/Stanford) takes GitHub issues and auto-fixes them.

- State of the art on SWE-bench (with Claude 3.7, as of Feb 2025)
- Uses custom tool interface (bash commands, file editing), not MCP
- Research-grade, focused on benchmarks
- Supports any LM as backend

### OpenHands (formerly OpenDevin)

[OpenHands](https://github.com/All-Hands-AI/OpenHands) is an open-source autonomous coding agent.

- End-to-end: editor + terminal + browser in a sandbox
- CodeAct framework: unified code action space
- Active community with benchmarking
- Docker-based isolation

**Relevance**: OpenHands is the closest existing project to what we're building. Study its architecture for sandbox design and tool composition.

### Summary Table

| Tool         | MCP Support  | Local Models | Autonomous | Language   |
| ------------ | ------------ | ------------ | ---------- | ---------- |
| OpenCode     | Yes          | Yes (Ollama) | Semi       | Go         |
| Aider        | No           | Yes (Ollama) | No (pair)  | Python     |
| Continue.dev | Yes (native) | Yes (Ollama) | Semi       | TypeScript |
| SWE-agent    | No           | Yes          | Yes        | Python     |
| OpenHands    | No           | Yes          | Yes        | Python     |

---

## 5. Communication Bridges

### Telegram (Recommended for Our Use Case)

Multiple MCP servers exist:

| Server                                                                            | API Type           | Features                                           |
| --------------------------------------------------------------------------------- | ------------------ | -------------------------------------------------- |
| [sparfenyuk/mcp-telegram](https://github.com/sparfenyuk/mcp-telegram)             | MTProto (Telethon) | Full API: chats, groups, media, contacts, settings |
| [guangxiangdebizi/telegram-mcp](https://github.com/guangxiangdebizi/telegram-mcp) | Bot API            | Modular, send/receive, media support               |
| [lane83-telegram](https://playbooks.com/mcp/lane83-telegram)                      | Bot API            | Whitelisted chat IDs, security-focused             |
| [Composio Telegram](https://mcp.composio.dev/telegram)                            | Bot API            | Managed integration                                |
| [Zapier Telegram MCP](https://zapier.com/mcp/telegram)                            | Bot API            | No-code integration                                |

**Recommended approach**: Use Bot API-based server (simpler, no user account needed). Create a dedicated bot, whitelist our chat IDs.

**Async communication pattern**:

1. Agent processes tasks autonomously
2. Sends status updates to Telegram on task completion/failure
3. User can send commands/queries to bot at any time
4. Bot queues messages and processes when agent is available

### Discord

| Server                                                                    | Notes                                             |
| ------------------------------------------------------------------------- | ------------------------------------------------- |
| [SaseQ/discord-mcp](https://github.com/SaseQ/discord-mcp)                 | Full Discord API, slash commands, buttons, modals |
| [OoriData/Discord-AI-Agent](https://github.com/OoriData/Discord-AI-Agent) | MCP-powered, built for AI chat                    |
| [discord-mcp (tolgasumer)](https://github.com/tolgasumer/discord-mcp)     | Real-time event streams, full tool suite          |

Discord has richer interaction capabilities (threads, reactions, embeds, buttons) but more complexity.

### Matrix/Element (Open Alternative)

- No dedicated MCP server found in searches
- [matrix-appservice-discord](https://github.com/matrix-org/matrix-appservice-discord) bridge exists for Discord interop
- Matrix is fully self-hostable and end-to-end encrypted
- Would require building a custom MCP server wrapping the Matrix SDK
- **Gap**: Matrix MCP server is a missing piece in the ecosystem

### Priority/Urgency System Design

For the agent to intelligently interrupt the user:

| Priority        | Behavior               | Example                                      |
| --------------- | ---------------------- | -------------------------------------------- |
| **P0 Critical** | Immediate notification | Security vulnerability found, system failure |
| **P1 High**     | Notify within 5 min    | Task completed, needs approval               |
| **P2 Medium**   | Batch, send hourly     | Progress updates, non-blocking questions     |
| **P3 Low**      | Daily digest           | Stats, logs, routine completions             |

Implementation: Agent maintains a priority queue. A background task checks the queue and sends via Telegram based on priority thresholds and user-configured quiet hours.

---

## 6. Missing Critical Tools -- Gap Analysis

### What Exists

| Category               | MCP Server Availability                                                                                                                                               | Maturity            |
| ---------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------- |
| **Calendar**           | [Google Calendar MCP](https://github.com/nspady/google-calendar-mcp), Apple Calendar MCP, [Microsoft 365 MCP](https://learn.microsoft.com/en-us/microsoft-agent-365/) | Mature              |
| **Task Management**    | Google Tasks MCP (Composio), Todoist MCP, Linear MCP                                                                                                                  | Active              |
| **Email**              | Microsoft Outlook MCP (Agent 365), Gmail MCP                                                                                                                          | Mature (enterprise) |
| **Screen Interaction** | Playwright (web), Puppeteer (web)                                                                                                                                     | Mature for web only |
| **API Testing**        | Postman MCP, various HTTP clients                                                                                                                                     | Active              |

### What's Missing or Immature

| Category                    | Status   | Gap Description                                                                                                                                       |
| --------------------------- | -------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Matrix/Element MCP**      | Missing  | No MCP server for Matrix protocol. Would need custom build.                                                                                           |
| **Native GUI interaction**  | Immature | MCP servers exist for web (Playwright) but not for native macOS apps. AppleScript or Accessibility API wrappers would be needed.                      |
| **Audio/Speech**            | Missing  | No MCP server for TTS/STT. Would need to wrap Whisper (STT) and a TTS engine as MCP tools.                                                            |
| **Payment/Commerce**        | Missing  | No MCP server for Stripe, PayPal, etc. Security implications make this intentionally avoided.                                                         |
| **Self-monitoring**         | Emerging | Agent observability exists as external platforms (Arize Phoenix, Langfuse, Galileo) but not as MCP self-monitoring tools.                             |
| **MCP Gateway**             | Emerging | Centralizes auth, rate limiting, observability. [ContextForge](https://github.com/IBM/contextforge) (IBM, open-source) and Bifrost are early options. |
| **Hardware/System Metrics** | Missing  | CPU temp, memory, disk, GPU utilization. Easy to build as custom MCP server.                                                                          |
| **Cron/Scheduling**         | Missing  | No MCP server for scheduling future tasks. Agent needs its own scheduler.                                                                             |
| **Knowledge Base / RAG**    | Partial  | Some MCP servers for vector DBs exist, but no unified knowledge management MCP.                                                                       |
| **PDF/Document Processing** | Partial  | Firecrawl handles web docs. Local PDF/DOCX processing MCP servers are sparse.                                                                         |

### Observability Stack for the Agent Itself

Existing options for monitoring the agent:

| Platform                                          | Type        | Key Feature                                        |
| ------------------------------------------------- | ----------- | -------------------------------------------------- |
| [Arize Phoenix](https://github.com/Arise-Phoenix) | Open-source | Drift detection, clustering, production monitoring |
| [Langfuse](https://langfuse.com/)                 | Self-hosted | Trace viewing, prompt versioning, cost tracking    |
| [Galileo AI](https://galileo.ai/)                 | Commercial  | Luna-2 evaluators, fast scoring                    |

**MCP Gateways** centralize agent-tool communication:

- Security (credential management)
- Observability (OpenTelemetry integration)
- Cost control (rate limiting)
- Governance (RBAC)

### Tools We Should Build (Custom MCP Servers)

For our Mac Mini M4 agent, these custom MCP servers would fill the gaps:

1. **System Monitor MCP** -- CPU, memory, GPU, disk, thermals (via `sysctl`, `powermetrics`)
2. **Task Scheduler MCP** -- Cron-like scheduling with priority, depends-on chains
3. **Knowledge Store MCP** -- SQLite + vector embeddings for agent memory/learning
4. **Self-Assessment MCP** -- Track task success/failure rates, token usage, time-per-task
5. **Matrix MCP** (if Matrix is chosen over Telegram)

---

## 7. Architecture Recommendation

Based on this research, the recommended tool orchestration stack:

```
                    +------------------+
                    |  Qwen 3.5 35B    |
                    |  (vLLM on M4)    |
                    +--------+---------+
                             |
                    +--------+---------+
                    |  Custom Rust     |
                    |  MCP Client      |
                    |  (using rmcp)    |
                    +--------+---------+
                             |
              +--------------+--------------+
              |              |              |
     +--------+--+  +-------+---+  +-------+---+
     | Core Tools |  | Dev Tools |  | Comms     |
     +------------+  +-----------+  +-----------+
     | filesystem |  | git       |  | telegram  |
     | shell/exec |  | github    |  | (discord) |
     | sqlite     |  | code-sand |  |           |
     | sys-monitor|  | browser   |  |           |
     +------------+  +-----------+  +-----------+
```

### Why MCP Over Alternatives

1. **Ecosystem**: 500+ official servers, growing rapidly
2. **Rust SDK**: Official `rmcp` crate, async, well-maintained
3. **Qwen support**: First-party via Qwen-Agent, OpenAI-compatible fallback
4. **Standardization**: Linux Foundation governance, adopted by all major vendors
5. **Transport flexibility**: stdio for local (our case), Streamable HTTP for future remote servers
6. **Security posture**: Local stdio avoids the HTTP attack surface entirely

### Key Risks

1. **Qwen tool calling reliability**: Known issues with Qwen 3.5 MCP integration. Test thoroughly.
2. **MCP security**: Never expose MCP servers over HTTP without gateway. Use stdio + containers.
3. **Ecosystem churn**: MCP is young. Spec may change. Pin SDK versions.
4. **Local model quality**: Tool calling with 3B active params (Qwen 3.5 MoE) may produce unreliable tool invocations. Have fallback/retry logic.

---

## Sources

### MCP Protocol & Registry

- [MCP Specification 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25)
- [Official MCP Registry](https://registry.modelcontextprotocol.io/)
- [MCP Registry GitHub](https://github.com/modelcontextprotocol/registry)
- [MCP Registry Blog Post](http://blog.modelcontextprotocol.io/posts/2025-09-08-mcp-registry-preview/)
- [Top 10 Most Popular MCP Servers 2026](https://fastmcp.me/blog/top-10-most-popular-mcp-servers)
- [MCP GitHub Organization](https://github.com/modelcontextprotocol)

### MCP SDKs

- [Official Rust SDK (rmcp)](https://github.com/modelcontextprotocol/rust-sdk)
- [rust-mcp-sdk](https://github.com/rust-mcp-stack/rust-mcp-sdk)
- [TypeScript SDK](https://github.com/modelcontextprotocol/typescript-sdk)
- [MCP SDK Comparison](https://www.stainless.com/mcp/mcp-sdk-comparison-python-vs-typescript-vs-go-implementations)

### Non-Claude MCP Usage

- [Dolphin MCP](https://github.com/QuixiAI/dolphin-mcp)
- [MCP Client for Ollama](https://github.com/jonigl/mcp-client-for-ollama)
- [Ollama MCP Bridge](https://github.com/patruff/ollama-mcp-bridge)
- [LangChain MCP Adapters](https://github.com/langchain-ai/langchain-mcp-adapters)
- [How to Use MCP with Ollama](https://apidog.com/blog/mcp-ollama/)
- [MCP Tool-Use with Ollama](https://medium.com/renaissance-learning-r-d/mcp-tool-use-with-ollama-to-empower-your-local-ai-agents-1f12df974982)

### Qwen Tool Calling

- [Qwen3 GitHub](https://github.com/QwenLM/Qwen3)
- [Qwen-Agent (MCP support)](https://github.com/QwenLM/Qwen-Agent)
- [Qwen Function Calling Docs](https://qwen.readthedocs.io/en/latest/framework/function_call.html)
- [Qwen 3.5 MCP Issue](https://github.com/QwenLM/Qwen3.5/issues/12)

### A2A & Alternative Protocols

- [A2A Announcement](https://developers.googleblog.com/en/a2a-a-new-era-of-agent-interoperability/)
- [MCP vs A2A Explained](https://www.clarifai.com/blog/mcp-vs-a2a-clearly-explained)
- [2026 AI Agent Protocol Wars](https://www.hungyichen.com/en/insights/ai-agent-protocol-wars)
- [6 MCP Alternatives 2026](https://www.merge.dev/blog/model-context-protocol-alternatives)
- [UTCP vs MCP](https://nordicapis.com/model-context-protocol-mcp-vs-universal-tool-calling-protocol-utcp/)
- [Top 5 Open Protocols for Multi-Agent AI 2026](https://onereach.ai/blog/power-of-multi-agent-ai-open-protocols/)

### Dev Tools

- [OpenCode](https://opencode.ai/docs/)
- [OpenCode CLI Guide 2026](https://yuv.ai/learn/opencode-cli)
- [Aider](https://aider.chat/)
- [Continue.dev](https://www.continue.dev/)
- [SWE-agent](https://github.com/SWE-agent/SWE-agent)
- [OpenHands](https://github.com/All-Hands-AI/OpenHands)

### Communication

- [Telegram MCP (MTProto)](https://github.com/sparfenyuk/mcp-telegram)
- [Telegram MCP (Bot API)](https://github.com/guangxiangdebizi/telegram-mcp)
- [Discord MCP](https://github.com/SaseQ/discord-mcp)
- [Discord AI Agent](https://github.com/OoriData/Discord-AI-Agent)

### Security

- [MCP Security Vulnerabilities 2026](https://www.practical-devsecops.com/mcp-security-vulnerabilities/)
- [State of MCP Server Security 2026](https://dev.to/ecap0/the-state-of-mcp-server-security-in-2026-118-findings-across-68-packages-4fkd)
- [8,000+ MCP Servers Exposed](https://cikce.medium.com/8-000-mcp-servers-exposed-the-agentic-ai-security-crisis-of-2026-e8cb45f09115)
- [Red Hat MCP Security Analysis](https://www.redhat.com/en/blog/model-context-protocol-mcp-understanding-security-risks-and-controls)

### Observability

- [AI Observability Tools 2026](https://www.braintrust.dev/articles/best-ai-observability-tools-2026)
- [MCP Observability Guide](https://mcpmanager.ai/blog/mcp-observability/)
- [MCP Gateways 2026](https://bytebridge.medium.com/mcp-gateways-in-2026-top-10-tools-for-ai-agents-and-workflows-d98f54c3577a)

### Sandbox / Code Execution

- [Code Sandbox MCP](https://github.com/Automata-Labs-team/code-sandbox-mcp)
- [AIO Sandbox](https://github.com/agent-infra/sandbox)
- [Container MCP](https://github.com/54rt1n/container-mcp)
