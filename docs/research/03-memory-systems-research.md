# Memory Systems for Persistent Autonomous Agents

**Research Date:** 2026-03-07
**Target Setup:** Mac Mini M4 (32GB), Qwen 3.5 35B-A3B MoE backbone, persistent across reboots/months

---

## Table of Contents

1. [Academic SOTA on Agent Memory](#1-academic-sota-on-agent-memory)
2. [Practical Memory Implementations](#2-practical-memory-implementations)
3. [Memory Architecture Patterns](#3-memory-architecture-patterns)
4. [The "Memory Model" Idea](#4-the-memory-model-idea)
5. [Memory Compaction & Retrieval](#5-memory-compaction--retrieval)
6. [Recommendations](#6-recommendations)

---

## 1. Academic SOTA on Agent Memory

### 1.1 Foundational Papers

#### MemGPT: Towards LLMs as Operating Systems (Packer et al., 2023)

- **Source:** [arXiv:2310.08560](https://arxiv.org/abs/2310.08560)
- **Key Insight:** Treats the LLM context window like OS virtual memory. Data moves between "main context" (RAM -- the token window) and "external context" (disk -- archival/recall storage). The LLM itself issues memory management syscalls (read/write/search) via function calls.
- **Architecture:** Two-tier: main context (in-window) + external context (vector DB). Interrupt-driven control flow between agent and user.
- **Results:** Enables document analysis far exceeding context window limits; multi-session chat with evolving personality.
- **Strengths:** Elegant OS analogy. Works with any LLM. No fine-tuning needed.
- **Weaknesses:** Every memory operation burns tokens (function calls). Latency scales with memory operations per turn. The LLM must "decide" to manage memory -- it can forget to do so.
- **Applicability:** High. Runs on any local model including Qwen 3.5 35B. The pattern is model-agnostic.

#### Letta (MemGPT's Successor, 2024-2026)

- **Source:** [letta.com](https://www.letta.com/), [GitHub](https://github.com/letta-ai/letta)
- **Key Insight:** Productionizes MemGPT into a platform. Adds "Context Repositories" with git-based versioning of memory, Conversations API for shared memory across parallel sessions, and idle-time memory refinement (agent thinks while not serving requests).
- **V1 Architecture (2025):** Redesigned agent loop optimized for modern reasoning models (GPT-5, Claude 4.5 Sonnet). Still supports original MemGPT pattern.
- **Strengths:** Production-ready. Model-agnostic. Multi-session. Active development.
- **Weaknesses:** Python-heavy. Server architecture adds complexity for embedded/local deployment.
- **Applicability:** Medium-high. Good reference architecture but may be heavier than needed for a single-agent local setup.

#### CoALA: Cognitive Architectures for Language Agents (Sumers et al., 2024)

- **Source:** [arXiv:2309.02427](https://arxiv.org/abs/2309.02427), TMLR 2024
- **Key Insight:** Proposes a unified framework organizing agents along three dimensions: (1) information storage (working + long-term memory), (2) action space (internal + external), (3) decision-making (plan + execute loop). The taxonomy is the contribution -- it maps the entire agent design space.
- **Memory Taxonomy:**
  - **Working memory:** Current context, goals, active plans
  - **Long-term episodic:** Specific past experiences with context
  - **Long-term semantic:** Abstracted facts and knowledge
  - **Long-term procedural:** Learned skills and routines
- **Strengths:** Best conceptual framework for reasoning about agent memory design. 300+ citations in bibliography.
- **Weaknesses:** Framework paper, not an implementation.
- **Applicability:** High as design guide. Use this taxonomy when architecting your memory system.

#### Reflexion: Language Agents with Verbal Reinforcement Learning (Shinn et al., NeurIPS 2023)

- **Source:** [arXiv:2303.11366](https://arxiv.org/abs/2303.11366)
- **Key Insight:** Instead of updating weights, agents store self-reflections as text in an episodic memory buffer. These reflections guide future decision-making. Verbal reinforcement learning -- the agent writes down what it learned from failure.
- **Results:** 91% pass@1 on HumanEval (vs 80% for GPT-4 at the time).
- **Strengths:** No fine-tuning. Dead simple. Works with any model.
- **Weaknesses:** Reflection quality depends on model capability. Buffer grows without bound unless pruned.
- **Applicability:** Very high. This is the simplest form of persistent learning. Store reflections as markdown files, inject relevant ones into context.

### 1.2 Survey Papers (2025-2026)

#### "Memory in the Age of AI Agents: A Survey" (Dec 2025)

- **Source:** [arXiv:2512.13564](https://arxiv.org/abs/2512.13564), [Paper list](https://github.com/Shichun-Liu/Agent-Memory-Paper-List)
- **Unified Taxonomy:**
  - **Forms:** Token-level (in-context), parametric (weights), latent (hidden states)
  - **Functions:** Factual, experiential, working
  - **Dynamics:** Formation, evolution (consolidation + forgetting), retrieval
- **Key Finding:** Traditional long/short-term dichotomy is insufficient. The field is fragmented -- loose terminology obscures real differences between systems.
- **Open Challenges:** Catastrophic forgetting, retrieval efficiency, structured vs unstructured vs neural memory.
- **Emerging Frontiers:** Memory automation, RL integration, multimodal memory, multi-agent memory, trustworthiness.

#### "A Survey on the Memory Mechanism of LLM-based Agents" (ACM TOIS 2025)

- **Source:** [ACM TOIS](https://dl.acm.org/doi/10.1145/3748302), [arXiv:2404.13501](https://arxiv.org/abs/2404.13501)
- **Key Insight:** Finer-grained analysis of memory read/write/management operations. Covers episodic, semantic, procedural separation in detail.

#### "Rethinking Memory Mechanisms of Foundation Agents" (Feb 2026)

- **Source:** [arXiv:2602.06052](https://arxiv.org/html/2602.06052v3)
- **Key Insight:** Proposes analyzing memory through operational lifecycle -- formation, evolution, retrieval -- rather than just type-based categories.

#### ICLR 2026 Workshop: "MemAgents: Memory for LLM-Based Agentic Systems"

- **Source:** [OpenReview](https://openreview.net/pdf?id=U51WxL382H)
- **Significance:** Dedicated ICLR workshop on agent memory signals this is now a first-class research area.

### 1.3 Notable Recent Systems

#### A-Mem: Agentic Memory for LLM Agents (NeurIPS 2025)

- **Source:** [arXiv:2502.12110](https://arxiv.org/abs/2502.12110), [GitHub](https://github.com/WujiangXu/A-mem)
- **Key Insight:** Self-organizing memory using Zettelkasten principles. Each interaction produces a "note" with content, timestamp, keywords, tags, context, embeddings, and links to related notes. New memories trigger updates to existing notes.
- **Strengths:** Dynamic linking creates emergent knowledge structure. NeurIPS-accepted quality.
- **Weaknesses:** Linking step requires LLM calls per memory operation.
- **Applicability:** High. The Zettelkasten pattern maps naturally to file-based or graph-based storage.

#### MemRL: Self-Evolving Agents via Runtime RL on Episodic Memory (Jan 2026)

- **Source:** Referenced in survey literature
- **Key Insight:** Uses reinforcement learning to optimize memory management policies at runtime.

#### MIRIX: Multi-Module Memory Architecture

- **Source:** Referenced in surveys
- **Architecture:** Core, Episodic, Semantic, Procedural, Resource, Knowledge Vault -- six specialized modules with type-specific access policies.
- **Applicability:** Good reference for designing multi-module memory, but likely over-engineered for single-agent setup.

---

## 2. Practical Memory Implementations

### 2.1 Claude Code's Memory System

- **Source:** [Claude Code Docs](https://code.claude.com/docs/en/memory)
- **Architecture:**
  - **CLAUDE.md files:** User-written markdown instructions. Read recursively from cwd up to root. Loaded at session start.
  - **Auto-memory (MEMORY.md):** Agent-written notes stored at `~/.claude/projects/<project>/memory/MEMORY.md`. Claude decides what's worth remembering. Includes build commands, debugging insights, architecture notes, style preferences.
  - **Topic files:** Agent can create sub-files like `debugging.md`, `api-conventions.md`.
- **Constraints:** Files over 200 lines consume excessive context and reduce adherence.
- **Key Insight:** Filesystem-based memory with markdown files is surprisingly effective. Letta benchmarked this pattern at 74% on LoCoMo, beating specialized memory libraries.
- **Strengths:** Zero infrastructure. Human-readable and editable. Version-controllable.
- **Weaknesses:** No semantic search. Linear scaling with file size. No forgetting/consolidation.

### 2.2 Windsurf (Cascade) Memory

- **Source:** [Windsurf comparison articles](https://windsurf.com/compare/windsurf-vs-cursor)
- **Architecture:**
  - **Indexing Engine:** Pre-scans entire repository, creates semantic index.
  - **Memories:** Persist across sessions. Both user-defined and automatic.
  - **Fast Context:** 8 parallel tool calls per turn, 10x faster than traditional agentic search.
- **Key Insight:** Automatic codebase indexing + cross-session memory + real-time awareness creates strongest IDE context management.

### 2.3 Cursor Memory

- **Source:** Various comparison articles
- **Architecture:** Single-session context window. Project rules for persistent instructions. MCP plugins for extension.
- **Key Insight:** Relies more on manual context management and project rules than automatic memory.

### 2.4 Mem0 (formerly EmbedChain)

- **Source:** [arXiv:2504.19413](https://arxiv.org/abs/2504.19413), [mem0.ai](https://mem0.ai/), [GitHub](https://github.com/mem0ai/mem0)
- **Architecture:** Hybrid datastore combining:
  - **Key-value store:** Quick access to structured facts/preferences
  - **Graph store:** Relationships between people, places, objects
  - **Vector store:** Semantic meaning and conversational context
- **Performance:** 26% relative improvement over OpenAI baseline on LLM-as-a-Judge. 91% lower p95 latency. 90%+ token cost savings.
- **Scale:** 186M API calls/month by Q3 2025. $24M raised from YC, Peak XV, Basis Set.
- **Strengths:** Production-proven at scale. Hybrid approach covers multiple retrieval patterns. Open source with hosted option.
- **Weaknesses:** Cloud-first design. Self-hosting adds infrastructure burden. Python-based.
- **Applicability:** Medium. The hybrid pattern (KV + graph + vector) is the right idea, but self-hosting on Mac Mini adds complexity.

### 2.5 Zep / Graphiti

- **Source:** [arXiv:2501.13956](https://arxiv.org/abs/2501.13956), [GitHub (Graphiti)](https://github.com/getzep/graphiti), [getzep.com](https://www.getzep.com/)
- **Architecture:** Temporal Knowledge Graph with three subgraphs:
  - **Episodic subgraph:** Specific interaction records
  - **Semantic subgraph:** Extracted entities and relationships
  - **Community subgraph:** Clustered topic groups
  - **Dual-timestamp model:** Event time + ingestion time for point-in-time queries
- **Performance:** 18.5% accuracy improvement over baselines. 90% latency reduction. Outperforms MemGPT on Deep Memory Retrieval benchmark.
- **Key Innovation:** Temporal awareness -- knows when facts were true, tracks how relationships change over time. Real-time incremental updates without batch recomputation.
- **Dependencies:** Requires Neo4j for graph storage.
- **Strengths:** Best-in-class temporal reasoning. Open source (Graphiti). State-of-the-art benchmark results.
- **Weaknesses:** Neo4j dependency is heavy for local deployment. LLM calls needed for entity extraction.
- **Applicability:** Medium. Excellent architecture but Neo4j on Mac Mini alongside Qwen is tight on resources.

### 2.6 Engram

- **Source:** [GitHub](https://github.com/foramoment/engram-ai-memory), [engram.to](https://engram.to/)
- **Architecture:** Cognitive memory system with:
  - **Memory types:** Semantic, episodic, procedural, working
  - **Search:** BGE-M3 embeddings + FTS5/BM25 via Reciprocal Rank Fusion
  - **Forgetting:** Ebbinghaus-inspired decay: `strength *= 0.95 ^ daysSinceLastAccess`
  - **Lifecycle:** Active -> Stale -> Archived
  - **Zero API cost:** Runs fully local with local embeddings
- **Integration:** MCP server for Claude Code, Cursor, other agents.
- **Strengths:** Local-first. Cognitive science-inspired. Zero API cost. MCP integration.
- **Weaknesses:** Newer project, less battle-tested. SQLite-based may not scale for very large memory stores.
- **Applicability:** High. Designed exactly for this use case -- local persistent memory for coding agents.

### 2.7 Motorhead

- **Source:** [YC Launch](https://www.ycombinator.com/launches/IUW-mot-rhead-llm-memory-server-built-in-rust)
- **Architecture:** LLM memory server built in Rust. Provides session management, memory storage, and retrieval.
- **Status:** Deprecated from LangChain as of v1.0 (Oct 2025). Project appears unmaintained.
- **Applicability:** Low. Deprecated. Historical interest only.

---

## 3. Memory Architecture Patterns

### 3.1 Vector DB + Embeddings

| Database          | Language                         | Embedded?          | Key Feature                                    | Mac Mini Fit                    |
| ----------------- | -------------------------------- | ------------------ | ---------------------------------------------- | ------------------------------- |
| **LanceDB**       | Rust core, Python/TS/Rust SDKs   | Yes (in-process)   | Columnar (Lance format), versioned, multimodal | Excellent -- no server needed   |
| **ChromaDB**      | Rust (rewritten 2025, 4x faster) | Yes                | Simple API, fast prototyping                   | Excellent                       |
| **Qdrant**        | Rust                             | Server or embedded | Filtering, discovery, recommendations          | Good (server mode)              |
| **Milvus/Zilliz** | Go/C++                           | No (distributed)   | Scale, GPU acceleration                        | Too heavy                       |
| **FAISS**         | C++                              | Library            | Raw speed, GPU support                         | Good for search, no persistence |

**Sources:** [LanceDB](https://lancedb.com/), [ChromaDB Rust rewrite](https://www.firecrawl.dev/blog/best-vector-databases), [Qdrant](https://qdrant.tech/)

**Verdict for our setup:** LanceDB is the clear winner. Rust core, embedded (no server), columnar format with versioning, native Rust SDK. ChromaDB is a close second for simplicity.

### 3.2 Knowledge Graphs

| Database      | Model                             | Key Feature                                        | Mac Mini Fit      |
| ------------- | --------------------------------- | -------------------------------------------------- | ----------------- |
| **Neo4j**     | Property graph                    | Mature, Cypher query language, vector search addon | Heavy (JVM-based) |
| **SurrealDB** | Multi-model (graph+doc+KV+vector) | Single DB for everything, Rust-based               | Good fit          |
| **TypeDB**    | Enhanced ER model                 | Strong type system, inference engine               | Medium            |

#### SurrealDB 3.0 (Feb 2026)

- **Source:** [surrealdb.com/3.0](https://surrealdb.com/3.0), [VentureBeat](https://venturebeat.com/data/surrealdb-3-0-wants-to-replace-your-five-database-rag-stack-with-one)
- **Key Insight:** "Replace your five-database RAG stack with one." Combines vectors, graphs, documents, KV, and time-series in a single Rust-based database. New WASM extension system (Surrealism) for custom logic inside the DB.
- **Raised:** $44M total ($23M in Feb 2026).
- **Strengths:** Single database for all memory needs. Rust-based, efficient. Active funding and development.
- **Weaknesses:** Younger than Neo4j. Vector search capabilities still maturing vs dedicated vector DBs.
- **Applicability:** High. The multi-model approach eliminates the need to run separate vector DB + graph DB + KV store.

### 3.3 Hybrid Approaches (Vector + Graph)

The consensus from 2025-2026 research: **hybrid beats pure**.

- **Qdrant + Neo4j:** 20-25% accuracy improvement over pure vector in enterprise RAG (Lettria case study). [Source](https://qdrant.tech/blog/case-study-lettria-v2/)
- **Mem0 hybrid:** KV + graph + vector stores combined. 26% improvement over OpenAI baseline.
- **Zep/Graphiti:** Temporal KG with vector similarity. SOTA on DMR benchmark.
- **SurrealDB 3.0:** Single database unifying all three.

**Verdict:** Use SurrealDB 3.0 if you want simplicity (one DB), or LanceDB + lightweight graph (e.g., in-memory Petgraph in Rust) if you want minimal footprint.

### 3.4 Fine-Tuning-Based Memory

Can you encode memory into model weights?

**Short answer: Not reliably, and not recommended as primary memory.**

- LoRA learns less and forgets less -- but the learning is too limited for continuous knowledge accumulation. [Source](https://arxiv.org/html/2405.09673v2)
- Full fine-tuning causes 89% performance drop on prior knowledge when learning new facts. [Source](https://arxiv.org/abs/2510.15103)
- LoRA reduces this to 71% drop, but that's still catastrophic.
- **Sparse memory finetuning** achieves only 11% drop -- the best result, but requires specialized memory layer architectures. [Source](https://arxiv.org/abs/2510.15103)

**Fine-tuning is better suited for:**

- Behavioral adaptation (tone, style, response format)
- Domain specialization (medical, legal vocabulary)
- NOT for factual memory that changes daily

### 3.5 Hierarchical Memory Architecture

The CoALA-inspired pattern most implementations converge on:

```
Layer 0: Context Window (working memory)
  |-- Current conversation, active goals, retrieved memories
  |-- Size: model's context window (e.g., 32K tokens)
  |-- Lifetime: single session
  |
Layer 1: Session Memory (short-term)
  |-- Current session state, tool results, intermediate reasoning
  |-- Size: bounded by session length
  |-- Lifetime: single session, persisted as episode
  |
Layer 2: Episodic Memory (medium-term)
  |-- Specific interactions, decisions, outcomes
  |-- Size: days to weeks of interactions
  |-- Lifetime: subject to consolidation/decay
  |
Layer 3: Semantic Memory (long-term)
  |-- Extracted facts, user preferences, project knowledge
  |-- Size: unbounded (indexed)
  |-- Lifetime: persistent, updated when contradicted
  |
Layer 4: Procedural Memory (long-term)
  |-- Learned skills, workflows, tool usage patterns
  |-- Size: grows slowly
  |-- Lifetime: persistent, refined through use
```

**Consolidation flow:** Episodes (L2) are periodically summarized into semantic facts (L3) and procedural rules (L4). Stale episodes decay or archive.

---

## 4. The "Memory Model" Idea

**Question:** Can we fine-tune a secondary model nightly with daily logs, creating a "brain" model callable as a tool?

### 4.1 Viability Assessment

**Technically possible but fraught with trade-offs.**

#### Arguments For:

- Compressed representation: A fine-tuned model encodes patterns, not raw data. More token-efficient at inference than RAG.
- Always-available: No retrieval latency. The knowledge is "in the weights."
- Small models can be effective for narrow domains.

#### Arguments Against:

- **Catastrophic forgetting is the killer:** Nightly fine-tuning would progressively overwrite earlier knowledge. After a week of nightly updates, day 1 knowledge is largely gone.
- **LoRA learns less than you'd think:** Parameter-efficient methods add limited capacity. They're good for style/behavior, bad for factual accumulation.
- **Verification is hard:** How do you know the model actually learned yesterday's facts? There's no reliable way to query "do you know X?" -- the model might hallucinate an answer.
- **Cost/time:** Even with QLoRA, fine-tuning a 3B model takes 30-60 minutes on Jetson Orin Nano 8GB. Daily is feasible but tight.

### 4.2 LoRA Fine-Tuning on Daily Logs

- **Data needed:** Minimum ~100 examples for LoRA to show effect. A day's worth of agent interactions could produce this, depending on activity level.
- **Format:** Instruction-tuning format works best: `(context, question, answer)` triples extracted from interaction logs.
- **Sequence length constraints:** 2048 tokens is the safe ceiling for single-GPU LoRA training on 35B MoE models.

### 4.3 Catastrophic Forgetting Mitigations

| Technique                              | Mechanism                                               | Forgetting Reduction       | Practical?                           |
| -------------------------------------- | ------------------------------------------------------- | -------------------------- | ------------------------------------ |
| **EWC** (Elastic Weight Consolidation) | Fisher information penalizes changing important weights | Moderate                   | Yes, but adds compute                |
| **Sparse Memory Finetuning**           | Only updates highly-activated memory slots              | 89% -> 11% drop            | Best results, requires memory layers |
| **CURLoRA**                            | CUR decomposition for stable LoRA updates               | Better than vanilla LoRA   | Yes                                  |
| **LoRA Merging**                       | Merge multiple LoRA adapters                            | Reduces forgetting         | Simple, effective                    |
| **FOREVER**                            | Forgetting-curve-inspired replay scheduling             | SOTA in continual learning | Good, but complex setup              |
| **I-LoRA**                             | Interpolation-based dual-memory replay                  | Good                       | Moderate complexity                  |

**Sources:** [EWC on Gemma2](https://arxiv.org/html/2505.05946v1), [Sparse Memory](https://arxiv.org/abs/2510.15103), [CURLoRA](https://arxiv.org/html/2408.14572v1), [FOREVER](https://arxiv.org/abs/2601.03938)

### 4.4 Doc-to-LoRA: A Better Approach?

- **Source:** [Sakana AI, Feb 2026](https://pub.sakana.ai/doc-to-lora/), [GitHub](https://github.com/SakanaAI/doc-to-lora)
- **Key Insight:** A hypernetwork generates LoRA adapters from documents in a single forward pass. No training loop needed. The document is "compressed" into a <50MB LoRA adapter regardless of document length.
- **Advantage over nightly fine-tuning:** No catastrophic forgetting. Each document gets its own adapter. Adapters can be composed.
- **Catch:** Requires the Doc-to-LoRA hypernetwork itself, which is a separate model that must be trained for your base model.
- **Applicability:** Watch this space. If a Doc-to-LoRA hypernetwork becomes available for Qwen-class models, this could be the best approach for encoding daily logs into parametric memory.

### 4.5 What Model Size for Jetson Orin Nano 8GB?

- **Inference:** 1B-3B quantized models comfortably fit. Llama 3.2 1B (1.3GB at Q8), Llama 3.2 3B, Phi-3 mini.
- **Performance:** 28-55 tok/s on quantized 1B-3B models.
- **Fine-tuning:** QLoRA on 1B-3B models is feasible. 3B is the practical ceiling for LoRA training in 8GB.
- **Recommendation:** Use a 1B-3B model as the "memory brain" if pursuing this approach. Qwen2.5-3B or Phi-3-mini are good candidates.

### 4.6 Verdict on the "Memory Model"

**Not recommended as the primary memory system.** The catastrophic forgetting problem is not solved for continuous daily fine-tuning. Instead:

1. **Primary memory:** RAG-based (vector/graph retrieval from structured memory store)
2. **Secondary enhancement (optional):** Doc-to-LoRA for compressing important knowledge into retrievable adapters
3. **If you must fine-tune:** Use sparse memory finetuning with EWC, retrain weekly not daily, and always maintain the raw data for re-training from scratch

---

## 5. Memory Compaction & Retrieval

### 5.1 Hierarchical Summarization

The pattern from MemGPT and successors:

```
Event (raw)      -> Episode (session summary)
Episode          -> Theme (cross-session patterns)
Theme            -> Principle (abstracted rules)
```

- **MemGPT approach:** Chunked episode-level summaries with approximate nearest-neighbor retrieval. Scales to millions of turns.
- **Mem0 approach:** LLM-based chat history summarization. Extracts key facts, preferences, and decisions.
- **Recursive summarization:** Iteratively compress dialogue histories. Each level loses detail but captures higher-order patterns.

**Source:** [Mem0 summarization guide](https://mem0.ai/blog/llm-chat-history-summarization-guide-2025)

### 5.2 Importance Scoring and Retrieval

Three standard metrics (from Generative Agents, Park et al.):

1. **Recency:** When was the memory last accessed? Exponential decay.
2. **Relevance:** Semantic similarity to current query (embedding cosine distance).
3. **Importance:** How significant is this memory? (LLM-scored or heuristic-scored)

**Advanced approaches:**

- **Cross-attention networks:** Train a small model to dynamically weight memories based on agent state. [Source](https://www.frontiersin.org/journals/psychology/articles/10.3389/fpsyg.2025.1591618/full)
- **MoE adaptive retrieval:** Learnable gate functions that dynamically adjust recency/relevance/importance weights per query. Adaptive stopping criteria for top-k retrieval.
- **A-Mem linking:** Zettelkasten-style connections between notes create retrieval paths beyond pure similarity search.

### 5.3 Forgetting Curves (Intentional Memory Decay)

| System         | Forgetting Mechanism            | Decay Formula                                |
| -------------- | ------------------------------- | -------------------------------------------- |
| **MemoryBank** | Ebbinghaus exponential decay    | R = e^(-t/S) where S = stability             |
| **Engram**     | Daily decay with access refresh | strength \*= 0.95^days_since_access          |
| **Mnemosyne**  | Hybrid scoring                  | connectivity + frequency + recency + entropy |
| **MemGAS**     | Gaussian Mixture clustering     | Prunes weakly-linked sessions                |

**Key insight from research:** Forgetting is a feature, not a bug. Strategic pruning improves retrieval quality by reducing noise. Systems that prune aggressively outperform those that hoard everything.

**Source:** [The Agent's Memory Dilemma](https://medium.com/@tao-hpu/the-agents-memory-dilemma-is-forgetting-a-bug-or-a-feature-a7e8421793d4)

### 5.4 Memory Indexing Strategies

1. **Flat vector search:** Simple, scales to ~100K memories well. Beyond that, use ANN indices.
2. **Hierarchical indexing:** Group memories by topic/project/time, search within relevant groups first.
3. **Graph-based indexing:** Link related memories, traverse graph for multi-hop retrieval.
4. **Temporal indexing:** Dual timestamps (event time + storage time) for point-in-time queries.
5. **Zettelkasten/A-Mem:** Atomic notes with emergent linking structure. Self-organizing.

**Recommendation for our setup:** Start with flat vector search (LanceDB). Add temporal metadata. Implement Zettelkasten-style linking when memory exceeds ~10K entries.

---

## 6. Recommendations

Ranked by suitability for: Mac Mini M4 32GB, Qwen 3.5 35B-A3B, persistent autonomous agent, months of operation.

### Tier 1: Do These First

#### 1. File-Based Core Memory (Reflexion + CLAUDE.md pattern)

- **What:** Markdown files for persistent instructions, learned rules, project knowledge. Agent writes self-reflections after task completion.
- **Why:** 74% on LoCoMo with zero infrastructure. Human-readable. Debuggable. Git-versionable.
- **Implementation:** `~/.agent/memory/core.md` (always loaded), `~/.agent/memory/reflections/` (selected by relevance), `~/.agent/memory/projects/<name>/` (project-specific).
- **Effort:** Low.

#### 2. LanceDB for Semantic Memory

- **What:** Embedded vector DB (Rust core) for searchable episodic and semantic memories.
- **Why:** No server process. In-process Rust library. Columnar format with versioning. Native Rust SDK.
- **Implementation:** Store memory entries as Lance records with embeddings (use local BGE-M3 or similar), metadata (timestamps, importance scores, tags), and raw text.
- **Effort:** Low-medium.

#### 3. Ebbinghaus-Inspired Forgetting

- **What:** Decay function on memory importance scores. Memories refresh on access, decay when unused.
- **Why:** Prevents unbounded memory growth. Improves retrieval quality by pruning noise.
- **Implementation:** `importance *= 0.95^days_since_access`. Archive below threshold. Delete archived memories after 90 days.
- **Effort:** Low.

### Tier 2: Add These for Production Quality

#### 4. Hierarchical Summarization Pipeline

- **What:** Nightly job: raw interactions -> episode summaries -> cross-episode themes -> persistent principles.
- **Why:** Compresses days of interactions into retrievable knowledge. Prevents the "1GB of logs" problem.
- **Implementation:** Qwen 3.5 summarizes its own interactions during idle time (Letta's "thinking while idle" pattern).
- **Effort:** Medium.

#### 5. SurrealDB for Relational Memory

- **What:** Multi-model DB providing graph queries over entities and relationships alongside vector search.
- **Why:** Single database for facts, relationships, and semantic search. Rust-based, efficient. Replaces need for Neo4j + separate vector DB.
- **Implementation:** Store entities (people, projects, tools), relationships (works-on, depends-on, caused-by), and facts with temporal validity ranges.
- **Effort:** Medium.

#### 6. A-Mem / Zettelkasten Linking

- **What:** When storing a new memory, find related existing memories and create bidirectional links.
- **Why:** Creates emergent knowledge structure. Enables multi-hop retrieval (A relates to B relates to C).
- **Implementation:** On memory insert, embed + search for top-5 similar existing memories. If similarity > threshold, create link.
- **Effort:** Medium.

### Tier 3: Experimental / Future

#### 7. Doc-to-LoRA for Knowledge Compression

- **What:** Use hypernetwork to compress important documents/logs into LoRA adapters.
- **Why:** Constant memory footprint (<50MB per adapter) regardless of document size. No retrieval latency.
- **Status:** Watch for Qwen-compatible hypernetworks from Sakana AI or community.
- **Effort:** High. Depends on ecosystem maturity.

#### 8. Sparse Memory Finetuning

- **What:** Periodically (weekly) update memory layer parameters with accumulated knowledge.
- **Why:** Only 11% forgetting vs 89% for full fine-tuning. Best parametric memory approach available.
- **Status:** Requires memory-layer-equipped model architecture. Not available for Qwen 3.5 out of the box.
- **Effort:** High. Requires model architecture changes.

#### 9. Small "Brain" Model on Jetson

- **What:** Qwen2.5-3B fine-tuned weekly with accumulated knowledge, callable as memory tool.
- **Why:** Compressed knowledge in weights. Always-available without retrieval.
- **Caveat:** Only pursue if you have excess engineering bandwidth. The catastrophic forgetting problem is real. Use CURLoRA + EWC + weekly (not daily) training. Always maintain raw data for scratch retraining.
- **Effort:** Very high. Ongoing maintenance.

### Architecture Summary

```
+------------------------------------------------------------------+
|                    Mac Mini M4 (32GB)                              |
|                                                                    |
|  +--------------------+     +----------------------------------+  |
|  |  Qwen 3.5 35B-A3B  |     |    Memory System (Rust)          |  |
|  |  (Main Agent)       |     |                                  |  |
|  |  ~20GB VRAM         |<--->|  Layer 0: Context (32K tokens)   |  |
|  +--------------------+     |  Layer 1: Core files (markdown)   |  |
|                              |  Layer 2: LanceDB (episodes)     |  |
|                              |  Layer 3: SurrealDB (facts+graph)|  |
|                              |  Layer 4: Procedural (markdown)  |  |
|                              |                                  |  |
|                              |  Nightly: Summarize + Decay      |  |
|                              +----------------------------------+  |
|                                                                    |
|  Resource Budget:                                                  |
|  - Qwen 3.5 35B-A3B Q4: ~18-20GB                                 |
|  - LanceDB: in-process, <1GB                                      |
|  - SurrealDB: ~500MB-1GB                                          |
|  - Embeddings model (BGE-small): ~500MB                            |
|  - OS + headroom: ~8-10GB                                          |
+------------------------------------------------------------------+
```

### Key Principles

1. **Start simple, add complexity only when needed.** Markdown files + vector search covers 80% of use cases.
2. **Forgetting is essential.** Without decay, memory degrades retrieval quality within weeks.
3. **Consolidation is the key differentiator.** The nightly summarization pipeline is what separates a competent agent from a toy.
4. **Avoid fine-tuning as primary memory.** The math on catastrophic forgetting doesn't work for daily updates. Use retrieval.
5. **Rust-native storage.** LanceDB + SurrealDB are both Rust-based, matching the likely implementation language for an autonomous agent on constrained hardware.

---

## Sources

### Foundational Papers

- [MemGPT: Towards LLMs as Operating Systems](https://arxiv.org/abs/2310.08560) - Packer et al., 2023
- [Cognitive Architectures for Language Agents (CoALA)](https://arxiv.org/abs/2309.02427) - Sumers et al., TMLR 2024
- [Reflexion: Language Agents with Verbal Reinforcement Learning](https://arxiv.org/abs/2303.11366) - Shinn et al., NeurIPS 2023
- [A-Mem: Agentic Memory for LLM Agents](https://arxiv.org/abs/2502.12110) - Xu et al., NeurIPS 2025
- [Zep: A Temporal Knowledge Graph Architecture for Agent Memory](https://arxiv.org/abs/2501.13956) - Rasmussen et al., 2025

### Surveys

- [Memory in the Age of AI Agents](https://arxiv.org/abs/2512.13564) - Dec 2025
- [A Survey on the Memory Mechanism of LLM-based Agents](https://arxiv.org/abs/2404.13501) - ACM TOIS 2025
- [Rethinking Memory Mechanisms of Foundation Agents](https://arxiv.org/html/2602.06052v3) - Feb 2026
- [ICLR 2026 MemAgents Workshop](https://openreview.net/pdf?id=U51WxL382H)

### Continual Learning & Fine-Tuning

- [LoRA Learns Less and Forgets Less](https://arxiv.org/html/2405.09673v2) - 2024
- [Continual Learning via Sparse Memory Finetuning](https://arxiv.org/abs/2510.15103) - Lin et al., Oct 2025
- [CURLoRA: Stable LLM Continual Fine-Tuning](https://arxiv.org/html/2408.14572v1) - 2024
- [FOREVER: Forgetting Curve-Inspired Memory Replay](https://arxiv.org/abs/2601.03938) - Jan 2026
- [EWC for Continual Pre-Training of Gemma2](https://arxiv.org/html/2505.05946v1) - 2025
- [Doc-to-LoRA and Text-to-LoRA](https://pub.sakana.ai/doc-to-lora/) - Sakana AI, Feb 2026

### Practical Systems

- [Letta (MemGPT successor)](https://www.letta.com/) - [GitHub](https://github.com/letta-ai/letta)
- [Mem0](https://mem0.ai/) - [arXiv:2504.19413](https://arxiv.org/abs/2504.19413) - [GitHub](https://github.com/mem0ai/mem0)
- [Graphiti (Zep open source)](https://github.com/getzep/graphiti)
- [Engram](https://github.com/foramoment/engram-ai-memory)
- [Claude Code Memory Docs](https://code.claude.com/docs/en/memory)
- [Letta Benchmarking AI Agent Memory](https://www.letta.com/blog/benchmarking-ai-agent-memory)
- [MemoryBank: Enhancing LLMs with Long-Term Memory](https://ojs.aaai.org/index.php/AAAI/article/view/29946)

### Databases

- [LanceDB](https://lancedb.com/) - [Rust SDK](https://docs.rs/lancedb/latest/lancedb/)
- [SurrealDB 3.0](https://surrealdb.com/3.0)
- [Qdrant + Neo4j GraphRAG](https://qdrant.tech/documentation/examples/graphrag-qdrant-neo4j/)
