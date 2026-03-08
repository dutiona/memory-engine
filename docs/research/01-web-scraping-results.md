# Web Scraping Results: Persistent Autonomous AI Agent Research

Scraped on 2026-03-07.

---

## 1. Reddit: Persistent AI Assistant with Claude Code + Obsidian + QMD

- **Source**: https://www.reddit.com/r/ClaudeCode/comments/1rn38wh/i_built_a_persistent_ai_assistant_with_claude/
- **Title/Topic**: Building "Vox" -- a persistent AI assistant using Claude Code + Obsidian + QMD

### Key Insights

- Obsidian vault serves as the AI's external brain with four zones: brain (stable memory), journal (daily digests), library (projects/references), dashboard (current priorities)
- Memory modeled on human cognition: working memory (context window + crash buffer), episodic (daily session digests), semantic (stable fact files), procedural (operating instructions), identity (persona/core file), retrieval (QMD)
- Session digests are structured: Context, Decisions, Facts Learned, Related Projects, Keywords
- The assistant bootstraps itself -- the user created the vault folder, Claude Code built most of the structure, operational files, and workflows over time
- Real-world integration: Google Calendar monitoring, Govee light control, schedule-aware proactive surfacing
- A startup ritual ensures the agent re-orients each session

### Notable comment (LifeBandit666 -- "Deep Thought" system)

- Two-agent split: Deep Thought (brain, lives in Obsidian) and Marvin (body, controls Home Assistant)
- Subagent system routes tasks to Haiku bots instead of doing everything in Opus -- saves ~75% tokens
- Overnight cron pipeline: 3am subagents process inbox, extract to vault, sync todos; 4am creates daily briefing with tasks, calendar, weather
- "Mistakes and lessons" file for stale memory: when corrected, writes mistake, fixes memory, moves to "corrected mistakes" with summary

### Tools/Libraries Mentioned

- **Claude Code**: acting agent
- **Obsidian**: long-term memory substrate (markdown vault)
- **QMD**: semantic/hybrid search retrieval layer
- **Home Assistant**: home automation (via Marvin agent)
- **Basic Memory** (docs.basicmemory.com): alternative mentioned by commenter
- **Govee API**: smart light control

### Architecture Decisions

- Local-first, human-readable, inspectable, editable memory
- Durable across model changes (plain markdown)
- No black-box memory -- user can open vault and read the assistant's brain
- Crash buffer / working memory file for resilience
- Async instruction drop folder for deferred processing

### Lessons Learned / Warnings

- Contradiction tracking is a major unsolved problem -- old/wrong facts fossilize
- Need memory confidence + sources (explicitly told vs. inferred)
- Stale/deprecated memory handling needed -- changing preferences persist forever
- Promise tracking ("we'll come back to that") is difficult
- Initiative rules needed to prevent annoying proactivity
- Token economy is real -- subagent delegation saves ~75% of tokens

### Relevance to Our Project: **HIGH**

- Directly addresses the persistent autonomous agent architecture we're building. The Obsidian-as-brain pattern, session digests, startup rituals, and multi-agent token optimization are all directly applicable. The open problems list is our roadmap.

---

## 2. GitHub: Total Recall -- Autonomous Agent Memory

- **Source**: https://github.com/gavdalf/total-recall
- **Title/Topic**: Five-layer observational memory for OpenClaw agents (~$0.10/month)

### Key Insights

- Two-generation architecture: v1.x (Observer -> Reflector -> Dream Cycle -> session recovery) + v2.0 Ambient Intelligence Engine (AIE)
- AIE pipeline: sensor-sweep -> event bus (JSONL) -> rumination-engine -> preconscious-select -> ambient-actions -> emergency-surface -> buffer-inject
- "Dream Cycle" = nightly memory consolidation (inspired by human sleep consolidation)
- Preconscious buffer: scored insights ready for injection into the live session
- Emergency surfacing: pushes urgent alerts through Telegram/Discord/webhook with quiet hours support
- Connector-based sensor system: calendar, Gmail, IONOS email, Fitbit, file watching -- all toggleable
- v2.1.0 adds two-gate LLM email scoring: Gate 1 = learned sender cache, Gate 2 = LLM content triage
- Importance scoring with decay (WP2) and pattern promotion (WP4) -- the "Wisdom Builder"

### Tools/Libraries Mentioned

- **Shell scripts** (100% bash): entire system is shell scripts
- **jq**: JSON processing
- **Python/PyYAML**: configuration parsing
- **OpenRouter/Ollama/vLLM/llama.cpp**: configurable LLM backends for rumination
- **Telegram/Discord/webhooks**: notification channels

### Architecture Decisions

- Entirely shell-based -- no compiled dependencies beyond jq and Python
- Event bus is a JSONL file (simple, appendable, greppable)
- Cron-driven pipeline: sensor-sweep every 15 min, preconscious-select every 30 min, emergency-surface every 30 min
- All paths configurable via single `config/aie.yaml`
- Ambient actions are read-only lookups (weather, calendar, Gmail search, places, web search)
- Additive design: v2.0 AIE doesn't remove or replace v1.x scripts

### Lessons Learned / Warnings

- Cross-platform portability required careful work (stat, date, md5 differences between Linux/macOS)
- Data-loss prevention: hash written AFTER successful append
- Locking with UID isolation and trap cleanup needed for concurrent cron jobs
- Atomic writes via temp file + mv for safety
- Quiet hours for notifications -- important for a system that runs 24/7

### Relevance to Our Project: **HIGH**

- The most complete open-source autonomous agent memory system found. The five-layer architecture (observer, reflector, dream cycle, ambient intelligence, emergency surface) is a mature design. The dream cycle and preconscious buffer concepts are particularly novel. Shell-based implementation means easy prototyping but limited scalability.

---

## 3. GitHub: Panther -- Self-hosted AI Agent Daemon (Rust)

- **Source**: https://github.com/PantherApex/Panther
- **Title/Topic**: Self-hosted AI agent daemon in Rust that runs on your machine and talks through messaging apps

### Key Insights

- Rust daemon (9 focused crates) that acts as a persistent AI assistant reachable from Telegram, Discord, Slack, Email, Matrix, or CLI
- 12 LLM provider support (Ollama through Cohere) via single `ProviderInterface` trait
- Full local mode: Ollama + CLI channel = zero data leaves the machine
- Subagent architecture: `spawn` tool creates independent agent instances in Tokio tasks, running in parallel
- Memory: persistent conversation history, user profile injection into every system prompt, session consolidation for large histories
- Cron scheduling with three types: exact timestamp, repeating interval, cron expression with timezone
- Media capture fallback chains for every platform (screenshot, webcam, audio, screen recording)
- Custom skills: any executable in `~/.panther/skills/` becomes a callable tool

### Tools/Libraries Mentioned

- **Rust + Tokio**: async runtime, one task per session
- **Serenity**: Discord WebSocket gateway
- **Brave Search API**: web search
- **Groq Whisper**: voice message transcription
- **MCP (JSON-RPC 2.0 over stdio)**: tool server integration
- **Ollama**: local inference

### Architecture Decisions

- Message bus (Tokio MPSC channels) fully decouples bot adapters from agent logic
- Per-session semaphore serializes messages from same chat; different sessions run concurrently
- Agent loop: up to 40 tool iterations per turn, with circuit breaker
- Tool results truncated to configurable char limit (default 500) to manage context
- No listening ports -- outbound-only connections (no inbound attack surface)
- Static blocklist for destructive shell commands (`rm -rf`, `mkfs`, fork bombs)
- 20-60 MB idle memory footprint, <1s startup

### Lessons Learned / Warnings

- "Giving an LLM shell access carries real risk if adversarial content enters through fetched web pages or tool results"
- Blocklist is defense-in-depth, not a sandbox
- Run under least-privilege account
- Smaller local models may produce malformed tool calls; `max_iterations` acts as circuit breaker
- Session consolidation needed when history grows large -- summarize older exchanges

### Relevance to Our Project: **HIGH**

- Best-in-class example of a self-hosted AI agent daemon. The Rust architecture (9-crate workspace), multi-channel support, subagent spawning, and provider abstraction are production-grade patterns. The security model (allow_from whitelists, command blocklist, no inbound ports) is thoughtful. Directly applicable as an execution layer.

---

## 4. GitHub: GraphThulhu -- MCP Server for Knowledge Graphs

- **Source**: https://github.com/skridlevsky/graphthulhu
- **Title/Topic**: MCP server giving AI full access to Logseq or Obsidian knowledge graphs (37 tools)

### Key Insights

- 37 MCP tools across 9 categories: Navigate, Search, Analyze, Write, Decision, Journal, Flashcard, Whiteboard, Health
- Supports both Logseq (via HTTP API) and Obsidian (direct file access) backends
- Backend interface pattern: all tools program against `backend.Backend` interface, not concrete clients
- Full block trees returned (not flat text) -- every page has nested hierarchy with parsed metadata
- Search results include parent chain + siblings for contextual understanding
- Decision tracking protocol: create, resolve, defer (warns after 3+ deferrals), health audit
- Knowledge gap detection: orphan pages, dead ends, weakly-linked areas
- Topic cluster discovery via connected component analysis
- Obsidian backend: heading-based block tree, block UUIDs persisted via HTML comments, fsnotify file watching, atomic writes

### Tools/Libraries Mentioned

- **Go + official MCP Go SDK**: server implementation
- **Logseq HTTP API**: backend for Logseq
- **fsnotify**: file watching for Obsidian backend
- **DataScript/Datalog**: escape hatch for arbitrary Logseq queries

### Architecture Decisions

- Backend interface with optional capability interfaces (PropertySearcher, TagSearcher checked at runtime)
- In-memory graph for analysis (BFS, connected components, gap detection) -- keeps per-query latency low
- Content parsing on every block: extracts [[links]], ((refs)), #tags, key::value properties
- Heading-based blocks for Obsidian with deterministic UUID fallback
- Version control warning on startup if graph isn't git-controlled

### Lessons Learned / Warnings

- Write operations cannot be undone without version control
- Compact mode had exponential block blowup bug (1455 blocks -> 47610) -- careful with recursive enrichment
- Read-only mode available for safety

### Relevance to Our Project: **HIGH**

- The knowledge graph approach (vs. flat memory files) is directly relevant. The 37-tool MCP interface is the most comprehensive knowledge graph integration found. Decision tracking, knowledge gap analysis, and topic clustering are valuable for autonomous agents that need to manage their own knowledge.

---

## 5. Reddit: Knowledge Graph vs. Vector Memory for AI Agents

- **Source**: https://www.reddit.com/r/openclaw/comments/1rkyky2/i_gave_my_ai_agent_a_knowledge_graph_instead_of/
- **Title/Topic**: Using GraphThulhu for knowledge graph-based agent memory instead of vector embeddings

### Key Insights

- After 1 month: 404 pages, 1,451 cross-references -- a web of connected knowledge
- Three problems with vector memory:
  1. Single-angle retrieval: search query must match storage angle
  2. No structure: core preferences and one-off events look the same
  3. No relationships: can't see that A caused B
- Knowledge graph advantages:
  1. Multi-hook retrieval is free -- every [[link]] is a retrieval path
  2. Types are native -- pages have type, status, timestamps
  3. Agent maintains it itself during periodic heartbeats (daily notes = scratch, graph = curated)
  4. Survives everything -- plain markdown, no database, git for versioning
- Planned: RAG on top of graph -- embed page contents for fuzzy semantic entry, then graph traversal for context expansion (Microsoft GraphRAG pattern)
- Tradeoff: more upfront structure; agent needs discipline to always link related pages and follow property standards

### Tools/Libraries Mentioned

- **GraphThulhu**: MCP server for Logseq/Obsidian knowledge graphs
- **Microsoft GraphRAG**: research paper validating semantic search + graph traversal pattern

### Architecture Decisions

- Daily notes as scratch paper, knowledge graph as curated long-term memory
- Agent self-maintains the graph during heartbeats -- promotes important content from dailies
- Every page has: type (project/decision/research/lesson/intel), status, created/updated timestamps
- Future hybrid: semantic search for discovery + graph links for context expansion

### Lessons Learned / Warnings

- "More upfront structure than 'just embed everything'" -- requires agent discipline
- Token usage concern raised in comments -- graph traversal can be expensive
- Community consensus: "You need both" (graph + vector)

### Relevance to Our Project: **HIGH**

- The strongest argument found for knowledge graph memory over vector-only approaches. The daily-notes-to-curated-graph promotion pattern is a key insight. The planned GraphRAG hybrid (semantic entry point + graph expansion) is likely the optimal architecture.

---

## 6. Reddit: Building a Memory System for Coding Agents (SQLite + FTS5)

- **Source**: https://www.reddit.com/r/ClaudeCode/comments/1r1w397/what_i_learned_building_a_memory_system_for_my/
- **Title/Topic**: claude-memory plugin -- SQLite + FTS5 keyword search for session recall

### Key Insights

- Five practical memory layers: Working (context window), Core (always-in-context CLAUDE.md), Procedural (skills/tools), Archival (curated notes), Recall (raw conversation history)
- Claude Code already covers core (CLAUDE.md), procedural (skills), and partial archival (auto memory). Missing: recall (searchable conversation history)
- Key insight: **keyword search works because the agent constructs the queries, not the user**. The LLM extracts substantive keywords ("schema" OR "ALTER TABLE" OR "migration") rather than forwarding vague questions
- FTS5 + BM25 is fast, debuggable, zero-dependency. When results are weak, the agent retries with different terms -- system self-corrects
- Letta's benchmarking: plain filesystem approach scored 74% on LoCoMo, outperforming several sophisticated embedding/retrieval systems
- Two retrieval mechanisms: (1) automatic context injection on session start (loads previous session), (2) on-demand search via past-conversations skill
- Session sync: hook fires on session stop, reads JSONL, parses to structured messages, writes to SQLite
- Average session context: 2-3k tokens after stripping tool call junk; <5% cases hit 5k tokens

### Tools/Libraries Mentioned

- **SQLite + FTS5**: full-text search with BM25 ranking
- **Claude Code hooks**: automatic sync on session stop, context injection on start
- **claude-memory / Claudest**: the open-source plugin (github.com/gupsammy/Claudest)
- **Letta/MemGPT**: referenced for virtual memory management framing
- **CoALA paper**: agent memory cognitive science framework

### Architecture Decisions

- Deliberately simple: few hundred lines of Python, no external dependencies
- Keyword-based search over vector/semantic -- LLM compensates for storage simplicity
- Automatic context injection vs. on-demand search as two complementary retrieval paths
- Branch detection in conversation history
- Background async sync so it never blocks shutdown

### Lessons Learned / Warnings

- "Vector databases add storage overhead for embeddings. Knowledge graphs require extraction pipelines, entity resolution, and graph query layers. These aren't free."
- Staleness detection is an unsolved gap -- "we're using Redis" vs "we moved off Redis last month"
- Temporal weighting helps but doesn't fully solve staleness
- For coding agents specifically, keyword search + LLM query construction is sufficient
- "The simplest approach that works is the right one to start with"

### Relevance to Our Project: **HIGH**

- The five-layer memory taxonomy is the clearest framework found. The insight about LLM-constructed keyword queries outperforming vector search for recall is counterintuitive and well-argued. The practical approach (SQLite + FTS5, no dependencies) is a strong starting point before adding complexity.

---

## 7. Reddit: r/AIMemory Subreddit -- Notable Posts

- **Source**: https://www.reddit.com/r/AIMemory/
- **Title/Topic**: Browse of hot/best posts on the AIMemory subreddit

### Key Insights

**Post: "Agents can be right and still feel unreliable"**

- Correctness is not enough; organizations need _reconstructability_ -- why was this correct at the time? What assumptions were active?
- "Autonomy scales capability. Legibility scales trust."
- Design question: capability-first or legibility-of-decisions-first?

**Post: "Progressive disclosure, applied recursively -- key to infinite context?"**

- Recursive progressive disclosure as a technique for managing infinite context
- Hierarchical summarization where each level reveals more detail on demand

**Post: "I need AI memory to handle contradictions & timestamped data"**

- Real use case: daily emails with budget changes, channel pivots, strategy reversals
- Temporal memory problem: "Budget was $120k" on day 1, "Budget reduced to $85k" on day 2
- Cognee and Graphiti tested but not giving accurate answers for temporal queries
- Need: memory that handles contradictions with timestamps, not just latest-wins

**Post: "Rust+SQLite persistent memory for AI coding agents (43us reads)" -- Memori**

- Hybrid search: FTS5 + cosine vector search fused with Reciprocal Rank Fusion
- Auto-dedup: cosine similarity >0.92 triggers update instead of insert
- Decay scoring: logarithmic access boost + exponential time decay (~69 day half-life)
- Built-in embeddings: fastembed AllMiniLM-L6-V2 ships with binary
- Performance: UUID get 43us, FTS5 search 65us (1K memories), hybrid search 1.1ms (1K)
- 195 tests, all real SQLite, no mocking

### Tools/Libraries Mentioned

- **Memori** (github.com/archit15singh/memori): Rust core + Python CLI, SQLite, hybrid search
- **Cognee**: knowledge graph memory (had accuracy issues with temporal data)
- **Graphiti**: temporal knowledge graph (also tested)
- **fastembed / AllMiniLM-L6-V2**: local embedding model

### Architecture Decisions

- Reciprocal Rank Fusion for combining keyword + vector search results
- ~69 day half-life for memory decay -- frequently used memories surface, stale ones fade
- Brute-force vector search adequate to ~100K memories, isolated for drop-in HNSW replacement

### Lessons Learned / Warnings

- Temporal contradictions remain the hardest unsolved problem in agent memory
- Auto-dedup prevents memory bloat but needs careful similarity thresholds
- Reconstructability/legibility of agent decisions matters as much as correctness

### Relevance to Our Project: **HIGH**

- The temporal contradiction problem and the Memori hybrid search approach are both directly relevant. The decay scoring model (69-day half-life) is a concrete solution for staleness. Reciprocal Rank Fusion for combining keyword + vector is worth adopting.

---

## 8. Reddit: Julia microGPT Port

- **Source**: https://www.reddit.com/r/Julia/comments/1rkfm8b/i_ported_karpathys_microgpt_to_julia_in_99_lines/
- **Title/Topic**: Port of Karpathy's microgpt to Julia with manual backprop, ~1600x faster than CPython

### Key Insights

- 99-line Julia GPT with analytical gradients instead of autograd tape -- ~20 BLAS calls vs ~57,000 autograd nodes
- ~1600x faster than CPython, ~4x faster than Rust (disputed -- Rust commenter matched with SIMD)
- Manual matrix-level backprop: RMSNorm backward, softmax Jacobian, dK/dQ asymmetry in attention
- Limitation: n_layer=4 loop reuses same weight matrices (not independent per-layer weights)
- Vocab size only 27 (lowercase letters + BOS) -- not a general-purpose implementation

### Tools/Libraries Mentioned

- **Julia**: language, BLAS for matrix ops
- **microgpt** (Karpathy): original Python implementation
- **microjpt**: the Julia port (github.com/ssrhaso/microjpt)

### Architecture Decisions

- Analytical gradients at matrix level vs. scalar autograd
- Trade: generality for speed (not a fair comparison to autograd-based systems per commenters)

### Lessons Learned / Warnings

- Community pushback: changing autograd to manual backprop makes comparison irrelevant to Karpathy's educational purpose
- Julia's BLAS performance is genuinely impressive for numerical computing
- Rust with SIMD can match or beat Julia for this workload

### Relevance to Our Project: **LOW**

- Interesting for ML fundamentals knowledge but not directly applicable to building a persistent autonomous agent. The Julia performance characteristics are notable but tangential.

---

## 9. Reddit: OpenClaw Alternatives (2026)

- **Source**: https://www.reddit.com/r/clawdbot/comments/1r6xqaq/top_openclaw_alternatives_worth_actually_trying/
- **Title/Topic**: Comparison of OpenClaw alternatives focused on security and lightweight design

### Key Insights

- **TrustClaw**: managed/cloud option, OAuth for app connections, agent never sees raw API keys, 1000+ integrations
- **NanoClaw**: fits in 8-minute code read, Apple Containers isolation, bash runs inside container not host, supports Agent Swarms
- **ZeroClaw**: pure Rust rewrite, <5MB RAM, <10ms startup, 3.4MB binary (vs OpenClaw's ~390MB Node runtime), migration tool with dry-run preview, 1017 tests
- **Nanobot**: ~4000 lines Python vs OpenClaw's 430,000+, WhatsApp/Telegram/Slack/Discord/Email support, runs on Raspberry Pi (191MB), background sub-agents, MCP support
- **memU**: knowledge graph of habits/context across sessions for long-term memory
- **IronClaw**: NEAR AI project, WASM container per tool with capability-based permissions, API keys never touch tool code
- **Moltworker**: runs agent inside Cloudflare Sandbox container, R2 for persistent storage, AI Gateway for secret management, ~$5/month
- **PicoClaw** (from comments): Go, <10MB RAM, ~1s boot time

### Tools/Libraries Mentioned

- All listed above plus:
- **Infisical**: self-hosted secret management (mentioned in security-focused comment)
- **agent-zero**: another alternative (agent-zero.ai)
- **pynchy**: Python alternative with LiteLLM integration and container isolation

### Architecture Decisions

- Security is the primary differentiator: container isolation (NanoClaw, IronClaw, Moltworker), credential brokering (TrustClaw), encrypted secrets (ZeroClaw)
- Lightweight footprint matters: ZeroClaw 3.4MB binary, Nanobot 191MB, PicoClaw <10MB RAM
- Notable comment (Silverjerk): "Pick a tool, focus on the work. Be safe, think about risks, read documentation. No one is giving agents a free pass."

### Lessons Learned / Warnings

- OpenClaw's security model (shell access + plaintext API keys + unrestricted local exec) drove migration
- Container isolation is becoming table stakes for agent security
- Secret management should be architectural, not bolt-on
- 430,000+ lines of code vs ~4,000 lines raises questions about complexity vs. capability

### Relevance to Our Project: **MEDIUM**

- Security architecture patterns (container isolation, credential brokering, capability-based permissions) are directly applicable. The lightweight alternatives show that agent functionality doesn't require massive codebases. ZeroClaw's Rust approach and IronClaw's WASM sandboxing are worth studying.

---

## 10. GitHub: llmfit -- Hardware-Aware LLM Model Selection

- **Source**: https://github.com/AlexsJones/llmfit
- **Title/Topic**: Terminal tool that right-sizes LLM models to system hardware (RAM, CPU, GPU)

### Key Insights

- 12.4k stars, actively maintained Rust tool (Cargo workspace: llmfit-core, llmfit-tui, llmfit-desktop)
- Detects hardware (NVIDIA multi-GPU, AMD, Intel Arc, Apple Silicon, Ascend NPU), scores models across quality/speed/fit/context
- Dynamic quantization: walks hierarchy from Q8_0 down to Q2_K, picks highest quality that fits
- MoE support: Mixtral 8x7B has 46.7B params but only activates ~12.9B per token
- Speed estimation: memory-bandwidth-bound formula with ~80 GPU bandwidth lookup table
- Four scoring dimensions: Quality (params, family, quant penalty), Speed (estimated tok/s), Fit (memory utilization sweet spot 50-80%), Context (window capability)
- Plan mode: inverts analysis to "what hardware do I need for this model?"
- REST API (`llmfit serve`) for cluster schedulers, with filtering and top-model selection per node
- 303 commits, Homebrew/Scoop/Docker/Nix packaging

### Tools/Libraries Mentioned

- **Rust + ratatui**: TUI framework
- **Ollama, llama.cpp, MLX**: supported runtime providers
- **HuggingFace API**: model database source
- **fastembed**: local embeddings
- **sympozium**: sister project for managing agents in Kubernetes

### Architecture Decisions

- Compile-time embedded model database (scraped from HF, stored in JSON)
- Use-case-specific scoring weights (Chat weights Speed 0.35, Reasoning weights Quality 0.55)
- GPU memory override for broken autodetection
- Context-length cap for estimation separate from model's advertised max

### Lessons Learned / Warnings

- GPU VRAM autodetection fails on some systems (VMs, passthrough)
- MoE models need special handling -- total params != active params
- Speed estimation needs efficiency factor (0.55) for kernel overhead, KV-cache

### Relevance to Our Project: **MEDIUM**

- Not directly about agent memory/persistence, but highly relevant for the infrastructure layer. When selecting which local model to run for agent inference, llmfit provides the hardware-aware scoring. The REST API for cluster scheduling could be useful for multi-agent deployments. The MoE-aware fitting is important for running large models locally.

---

## Cross-Cutting Themes

### 1. Memory Architecture Convergence

Multiple sources independently converge on a layered memory model:

- **Working memory**: context window (all sources)
- **Core/procedural**: always-loaded instructions (CLAUDE.md pattern, Panther's profile injection, Vox's procedural memory file)
- **Episodic**: session digests / conversation recall (Total Recall's observer, claude-memory's JSONL sync, Vox's daily notes)
- **Semantic/archival**: curated long-term knowledge (knowledge graphs, Obsidian vaults, stable fact files)
- **Retrieval layer**: search mechanism over stored knowledge (FTS5, QMD, GraphThulhu, vector embeddings)

### 2. Knowledge Graph > Vector-Only (But Hybrid is Best)

- GraphThulhu author and multiple commenters argue knowledge graphs provide multi-hook retrieval, type-awareness, and relationship visibility that vectors lack
- However, consensus is emerging around **hybrid**: semantic search for discovery (entry point), graph traversal for context expansion (Microsoft GraphRAG pattern)
- The Memori tool demonstrates Reciprocal Rank Fusion as a practical way to combine keyword + vector results

### 3. Obsidian as the Dominant Memory Substrate

- Vox, Deep Thought, GraphThulhu, and multiple commenters use Obsidian vaults
- Reasons: human-readable, inspectable, editable, git-versionable, survives model changes
- Plain markdown files on disk is the most durable format found

### 4. The Staleness/Contradiction Problem is Unsolved

- Every memory system struggles with temporal facts ("budget was X" then "budget changed to Y")
- Solutions attempted: decay scoring (~69 day half-life), mistakes-and-lessons files, timestamps on everything
- No satisfactory automated solution found -- most rely on manual correction or LLM-driven review

### 5. Agent Self-Maintenance of Memory

- Best systems have the agent itself curate its memory (Vox building its own structure, Total Recall's dream cycle, GraphThulhu's agent-maintained graph)
- Daily notes as scratch paper -> curated knowledge promotion is a recurring pattern
- Active curation > passive logging (aligns with ACE paper findings)

### 6. Token Economy is Critical

- Deep Thought's subagent routing saves ~75% tokens by delegating to Haiku
- claude-memory strips tool call junk from sessions, reducing to 2-3k tokens average
- Multiple projects optimize for minimal context injection to preserve working memory

### 7. Security Through Isolation

- Container isolation (NanoClaw, IronClaw, Moltworker) becoming standard
- Credential brokering (TrustClaw, AI Gateway) separates secrets from agent code
- WASM sandboxing (IronClaw) offers finest-grained capability control
- "Giving an LLM shell access carries real risk" -- defense-in-depth is mandatory

### 8. Cron/Scheduled Processing is the Backbone

- Total Recall, Vox/Deep Thought, and Panther all use scheduled jobs for memory maintenance
- Overnight consolidation ("dream cycle"), daily briefing generation, sensor sweeps
- The pattern: real-time interaction during the day, batch processing at night

### 9. LLM-Constructed Queries Beat Semantic Search for Recall

- claude-memory's key insight: when the retriever IS an LLM, keyword search works because the agent constructs targeted queries, not vague natural language
- FTS5 + BM25 is debuggable, fast, zero-dependency vs. vector search black box
- Letta's benchmarks confirm: filesystem approach scored 74% on LoCoMo, matching sophisticated pipelines

### 10. Rust is the Emerging Language for Agent Infrastructure

- Panther (daemon), ZeroClaw (agent), Memori (memory core), llmfit (model selection) all Rust
- Reasons: low memory footprint, fast startup, safety guarantees, single binary distribution
- Go also represented (GraphThulhu) but Rust dominates new projects
