# Community Research Synthesis: Memory Systems & Self-Improvement

**Date:** 2026-03-08
**Sources:** OpenClaw ecosystem, r/openclaw (3-layer memory), r/ClaudeCode (second brain + self-improvement), academic papers (AgeMem, Memento, CoALA, A-Mem, Mem0, Graphiti)

---

## 1. Source Summary

### 1.1 OpenClaw Ecosystem (247k stars, TypeScript → ZeroClaw Rust)

**Architecture:**

- **Gateway + Agent Runtime** separation — gateway handles routing/auth/config, runtime handles LLM interaction
- **Lane Queue** — serialized per-session execution, eliminates race conditions
- **Workspace-as-code** — SOUL.md, TOOLS.md, IDENTITY.md, HEARTBEAT.md define agent behavior
- **Pre-compaction memory flush** — silent turn nudges LLM to persist state before context eviction
- **Hierarchical tool policy** — global → agent → group → sandbox levels

**Memory (4-layer):**

| Layer           | Scope                             | Storage             |
| --------------- | --------------------------------- | ------------------- |
| Session context | Current conversation              | In-memory           |
| Daily logs      | `memory/YYYY-MM-DD/`              | Markdown files      |
| Long-term       | `MEMORY.md` with curated insights | Markdown file       |
| Semantic search | Chunked embeddings                | SQLite + sqlite-vec |

Markdown is canonical, SQLite is derived index. Human-readable, git-versionable, survives everything.

**ZeroClaw (Rust rewrite):** 3.4MB binary, <5MB RAM, <10ms startup, 22+ providers, trait-driven modular architecture. Config-compatible with OpenClaw.

### 1.2 Reddit 3-Layer Memory Architecture (r/openclaw)

**Architecture:**

| Layer         | Name                                                                                           | Access                              | Token Budget     |
| ------------- | ---------------------------------------------------------------------------------------------- | ----------------------------------- | ---------------- |
| L1: Brain     | 7 markdown files (SOUL.md, AGENTS.md, MEMORY.md, USER.md, TOOLS.md, IDENTITY.md, HEARTBEAT.md) | Always loaded                       | ~7K tokens total |
| L2: Memory    | Daily notes + breadcrumb files                                                                 | Semantic search via `memory_search` | 4KB/file max     |
| L3: Reference | SOPs, playbooks, research                                                                      | On-demand via explicit read         | Unlimited        |

**Key innovations:**

- **Breadcrumb pattern** — one-line facts in L2 with pointers to L3 depth (sparse index over deep material)
- **Compaction-triggered checkpoint** — hooks into OpenClaw's auto-compaction to save state before context eviction
- **5 trigger-word protocols**: `recover` (context rebuild), `checkpoint` (state save), `trim` (maintenance), `recalibrate` (drift correction), `checkboard` (status dump)
- **Token budget discipline** — 500-1000 tokens per L1 file. "Agents skim bloated files" — confirmed by attention research

**Limitations:** No database, no decay, no graph, no automation beyond compaction hook. Manual trigger words. No forgetting mechanism. No code repo.

### 1.3 Second Brain for Claude Code (r/ClaudeCode)

**Mengram** (most architecturally significant):

- **3-type memory:** semantic (facts via embeddings), episodic (events with outcomes), procedural (self-versioning workflows)
- Knowledge graph on top of embeddings (not raw vector search)
- **3 Claude Code hooks:** profile on session start, memory search on every prompt, save after responses
- Obsidian import to knowledge graph
- **Procedural memory versions on failure** — workflows evolve without human intervention
- Repo: github.com/alibaizhanov/mengram

**Ori Mnemos** (novel forgetting mechanism):

- Markdown files with YAML metadata + wiki-links creating traversable graph
- 384-dim embeddings (MiniLM) + MCP server interface
- **Graph-aware forgetting** — prunes low-value notes based on graph connectivity (isolated nodes pruned)
- Repo: github.com/aayoawoyemi/Ori-Mnemos

**Wolly_Bolly's pipeline** (practical cron approach):

- QuickCapture → cron agent → structured vault → 4 Kanban boards as triage
- Global `/cerebro` skill for vault interaction
- Hot capture, cold organization (separation of concerns)

### 1.4 Self-Improving Claude Code (r/ClaudeCode)

**The `/insights` feedback loop** (Vishal Sachdev, Substack):

- Work → `/insights` analyzes sessions → report identifies friction → add fixes to CLAUDE.md → repeat
- Concrete findings: Claude searching filesystem for general knowledge (9 times), observer agents wasteful for short sessions (20% overhead), pre-deployment constraint checks needed
- **Meta-loop:** Feed insights report back to Claude to rewrite CLAUDE.md (automated rule generation)
- "CLAUDE.md is the onboarding doc; `/insights` is the performance review"
- Gist: vishalsachdev/2f2a0e339616548bc42a131b95a0eb85

**Community skepticism:**

- "A lot of generic BS" (top comment on enterprise scaling post)
- Bosun (virtengine/bosun) cited as "actually self-improving"
- kubrador's challenge: "The capture layer is what matters; everything else is cope" — if you can't distinguish signal from noise at write time, downstream processing doesn't fix it

---

## 2. Cross-Reference: Community vs Academic

### 2.1 Where Community Validates Academic Findings

| Academic Finding                                                           | Community Validation                                                                                                          |
| -------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| CoALA 4-type memory (working/episodic/semantic/procedural)                 | OpenClaw's 4-layer, Reddit's 3-layer, Mengram's 3-type all independently converge on similar hierarchies                      |
| ACE curation: active memory curation > passive logging (10.6% improvement) | kubrador's "capture layer is what matters" and OpenClaw's pre-compaction flush both emphasize write-time quality              |
| Memento: case-based reasoning without fine-tuning                          | Mengram's procedural memory that versions on failure is the same concept — learn from past experiences without weight updates |
| AgeMem: memory ops as tool calls                                           | OpenClaw treats memory as tools (memory_search, memory_write), Reddit 3-layer uses trigger-word protocols                     |
| Reflexion: verbal reinforcement learning                                   | The `/insights` loop is Reflexion applied to the system prompt instead of in-context reflections                              |

### 2.2 Where Community Goes Beyond Academic Papers

1. **Graph-aware forgetting** (Ori Mnemos) — No academic paper we've found uses graph connectivity as a forgetting heuristic. CoALA doesn't address forgetting at all. Ebbinghaus decay (Engram) is time-based only. Graph topology adds a structural dimension: isolated memories with no connections are less valuable regardless of recency.

2. **Self-versioning procedures** (Mengram) — Academic procedural memory (CoALA L4) is static. Mengram treats procedures as living documents that branch on failure. This is closer to evolutionary programming than to standard agent memory.

3. **Instruction-level self-modification** (`/insights` loop) — None of the academic papers describe an agent reviewing its own session telemetry and modifying its own system prompt. This is self-modification at the config level (low-risk, reversible, high-impact) vs. the weight level (LoRA, high-risk, irreversible without checkpoints).

4. **Pre-compaction memory flush** (OpenClaw) — No academic paper addresses the practical problem of context window compaction. OpenClaw's solution (silent turn before eviction) is a pragmatic engineering innovation.

5. **Breadcrumb indexing** (Reddit 3-layer) — A human-readable sparse index over deep reference material. Store the lookup key at the searchable layer, keep the payload at the deep layer. Cheaper than full-text indexing, more flexible than embeddings.

### 2.3 Where Community Falls Short

1. **No temporal contradiction handling** — Every community system ignores the problem of changing facts. Academic papers (Graphiti temporal KG, Zep) at least attempt timestamps and validity intervals.

2. **No benchmarks** — Community systems have zero quantitative evaluation. No comparison against baselines, no reproducible results. Mengram and Ori Mnemos are promising architectures with no evidence they actually work better than naive approaches.

3. **No multi-agent coordination** — All community systems are single-agent. Academic papers (AgeMem, Memento) at least consider multi-agent scenarios.

4. **No formal forgetting policy** — Ori Mnemos has graph-aware forgetting but no threshold tuning, no evaluation of what gets lost. Memori has Ebbinghaus decay with 69-day half-life, but this was chosen arbitrarily.

---

## 3. Revised Architecture Recommendations

### 3.1 For the Persistent Autonomous Agent (Mac Mini M4 + Qwen 3.5)

The community research validates our 5-layer hierarchy and adds 5 concrete improvements:

```
L0: Context Window (working memory, managed by inference engine)
    ├── Token budget: ≤7K for always-injected context (OpenClaw's number)
    └── Pre-compaction flush before any context eviction [NEW from OpenClaw]

L1: Core Files (markdown, always loaded)
    ├── SOUL.md (purpose/behavior), TOOLS.md, IDENTITY.md
    ├── MEMORY.md (curated active state, strict 1000-token budget)
    └── Breadcrumb index files pointing to L3 depth [NEW from Reddit 3-layer]

L2: Episodic Memory (SurrealDB)
    ├── Session summaries with outcomes (success/failure tagged)
    ├── Case bank for similar-situation retrieval (Memento pattern)
    └── Self-versioning procedures that branch on failure [NEW from Mengram]

L3: Semantic Memory (SurrealDB graph + vector)
    ├── Facts, entities, relationships (temporal KG à la Graphiti)
    ├── Graph-aware importance scoring [NEW from Ori Mnemos]
    └── Ebbinghaus decay (0.95^days) + graph connectivity hybrid

L4: Procedural Memory (markdown files)
    ├── Skills, workflows, SOPs (human-readable, version-controlled)
    └── Procedure evolution: branch on failure, merge on success [NEW from Mengram]
```

**New architectural elements from community research:**

1. **Pre-compaction flush** (from OpenClaw) → Before any context summarization/truncation, trigger a memory persistence pass. This is a safety net against context window amnesia.

2. **Breadcrumb indexing** (from Reddit 3-layer) → L1/L2 contain sparse one-liner indexes pointing to full content in L3/L4. Keeps searchable layers lean.

3. **Procedure versioning** (from Mengram) → Procedural memory is no longer static. When a workflow fails, create a branched version incorporating the failure. When it succeeds, merge learnings back.

4. **Graph-aware forgetting** (from Ori Mnemos) → Combine Ebbinghaus time-decay with graph connectivity score. A memory connected to many other memories decays slower. Isolated memories decay faster. This prevents unbounded growth while preserving structurally important information.

5. **Self-review cron** (from `/insights` loop) → Nightly dream cycle includes: (a) summarize episodes → promote to semantic, (b) analyze recent sessions for friction patterns → update procedural memory, (c) run graph-aware forgetting → archive/delete low-value memories.

### 3.2 For Daily Claude Code Workflow

Immediately actionable improvements:

| Improvement                 | Source           | How to Implement                                                              | Priority                  |
| --------------------------- | ---------------- | ----------------------------------------------------------------------------- | ------------------------- |
| `/insights` feedback loop   | Substack article | Run `/insights` weekly, merge findings into CLAUDE.md                         | **High**                  |
| CLAUDE.md token budget      | Reddit 3-layer   | Keep CLAUDE.md under 1000 tokens per section. Move details to linked files.   | **High**                  |
| Memory pruning              | Ori Mnemos       | Periodically review MEMORY.md, remove entries not referenced in 30+ days      | **Medium**                |
| Session profile hook        | Mengram          | Claude Code hook on session start that loads relevant project context         | **Medium**                |
| Pre-session memory search   | Mengram          | Hook on prompt submit that searches past memories for relevant context        | **Low** (may add latency) |
| Meta-loop CLAUDE.md rewrite | `/insights`      | Feed insights report to Claude, ask it to rewrite CLAUDE.md merging new rules | **Medium**                |

### 3.3 Novel Ideas Worth Investigating

1. **Bosun** (virtengine/bosun) — Described as "actually self-improving" system. No detailed architecture available yet. Worth tracking.

2. **Kanban-as-index** (Wolly_Bolly) — Using structured boards (not flat files) as the triage layer. The agent maintains project boards that serve as navigable indexes into the knowledge base.

3. **Hot capture / cold organization** — Separate the capture path (fast, async, fire-and-forget) from the organization path (cron, batched, LLM-intensive). For the Mac Mini agent: capture everything during active work, organize during idle time.

4. **Instruction-level self-modification** — The agent modifies its own system prompt based on performance analysis. This is a lightweight, reversible form of self-improvement that doesn't require fine-tuning. For the autonomous agent, the SOUL.md could evolve over time through a gated review process.

---

## 4. Open Questions Updated

### Resolved by Community Research

- **OQ4 (temporal contradiction resolution):** Not resolved, but Graphiti's dual-timestamp approach (created_at + valid_at) with invalidation edges is the best available solution. Community has nothing better. Remains hard.
- **OQ10 (bootstrap ritual):** OpenClaw's workspace-first approach answers this: load SOUL.md + TOOLS.md + MEMORY.md + check HEARTBEAT.md for pending tasks + review yesterday's daily log.

### New Questions Raised

- **NQ1:** How does graph-aware forgetting interact with temporal decay? Do we need both, or does one subsume the other?
- **NQ2:** What is the right granularity for procedure versioning? Version per failure? Per session? Per significant outcome?
- **NQ3:** How to gate instruction-level self-modification? The agent editing its own SOUL.md is powerful but risky. What approval mechanism prevents drift?
- **NQ4:** Mengram's 3-hook pattern (profile/search/save) adds latency to every prompt. What's the measured overhead? Is it worth it?

---

## 5. Tools & Repos Discovered

| Tool           | URL                               | What It Does                                                      | Relevance                                         |
| -------------- | --------------------------------- | ----------------------------------------------------------------- | ------------------------------------------------- |
| **Mengram**    | github.com/alibaizhanov/mengram   | 3-type memory (semantic/episodic/procedural) with knowledge graph | High — reference for procedural memory versioning |
| **Ori Mnemos** | github.com/aayoawoyemi/Ori-Mnemos | Markdown + wiki-links + graph-aware forgetting                    | High — forgetting mechanism to adopt              |
| **Bosun**      | github.com/virtengine/bosun       | Self-building/self-improving system                               | Unknown — needs investigation                     |
| **ZeroClaw**   | github.com/zeroclaw-labs/zeroclaw | Rust OpenClaw rewrite, trait-driven                               | High — architecture reference                     |
| **PicoClaw**   | (Sipeed)                          | Go, IoT/embedded, runs on $10 boards                              | Low — wrong hardware target                       |
| **NanoClaw**   | (community)                       | Security-hardened OpenClaw fork                                   | Medium — security patterns to adopt               |

---

## 6. Key Takeaways

### For the autonomous agent

1. **Our 5-layer hierarchy is validated** by 4 independent community implementations arriving at similar structures.
2. **Add pre-compaction flush** — this is the single most impactful engineering insight from OpenClaw.
3. **Add graph-aware forgetting** — combining time-decay with structural importance prevents unbounded memory growth while preserving valuable interconnected knowledge.
4. **Procedure versioning** is the bridge between static skills and learned behavior without fine-tuning.
5. **Event-sourced log remains essential** — no community system has this, and it's a differentiator. Markdown-canonical (OpenClaw) is great for simple agents but insufficient for auditability and temporal queries.

### For daily Claude Code

1. **Run `/insights` weekly** and merge findings into CLAUDE.md — the simplest high-impact change.
2. **MEMORY.md needs periodic pruning** — it only grows, and attention research shows LLMs skim bloated context.
3. **The meta-loop** (Claude rewrites CLAUDE.md from insights) is worth trying as an experiment.

### What we're NOT doing (with reasons)

- Not adopting pure-markdown memory (OpenClaw pattern) — insufficient for multi-hop queries and temporal reasoning
- Not using Mengram's API — we're building local-first, and Mengram is API-dependent
- Not implementing trigger-word protocols (Reddit 3-layer) — we want autonomous operation, not human-triggered commands
- Not building on OpenClaw/ZeroClaw directly — we need tighter memory integration than their plugin system allows, and our backbone (Qwen 3.5 via MLX) requires custom inference integration
