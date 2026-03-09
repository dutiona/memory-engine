# Research Notes: Future Phase Topics

Date: 2026-03-09

---

## 1. Agent Memory Protocols

### MCP as the De Facto Standard

Anthropic's Model Context Protocol (MCP), released November 2024, has become the dominant standard for connecting AI agents to external data sources and tools. It defines a universal interface through which agents interact with memory, filesystems, APIs, and knowledge bases. Major adopters include Google, Block, Bloomberg, and Amazon.

### MCP Memory Servers

Anthropic ships a reference [Knowledge Graph Memory Server](https://github.com/modelcontextprotocol/servers/tree/main/src/memory) that persists memory as a local knowledge graph with three primitives: **Entities** (nodes), **Relations** (directed edges in active voice), and **Observations** (discrete facts about entities). This is intentionally simple --- a building block, not a production system.

More capable open-source MCP memory servers have emerged:

- **[Hindsight](https://hindsight.vectorize.io/blog/2026/03/04/mcp-agent-memory)** --- extracts structured facts, resolves entities, generates embeddings, indexes for retrieval. Designed as a drop-in for any MCP-compatible agent.
- **[Redis Agent Memory Server](https://github.com/redis/agent-memory-server)** --- two-tier memory (working + long-term), semantic search, configurable strategies. Dual REST/MCP interface.
- **[Cognee](https://www.cognee.ai/blog/deep-dives/model-context-protocol-cognee-llm-memory-made-simple)** --- exposes memory as a first-class MCP tool with graph + vector dual representation.
- **[mcp-mem0](https://github.com/coleam00/mcp-mem0)** --- Mem0-backed MCP server for long-term agent memory.

### Framework-Level Memory Patterns

Each major framework handles memory differently, with no cross-framework standard beyond MCP:

- **LangChain/LangGraph**: Memory modules (buffer, summary, vector store) integrate into chains. LangGraph adds stateful persistence with checkpointing.
- **CrewAI**: Built-in memory types (short-term, long-term, entity, contextual) configured per-agent. 40% of Fortune 500 reportedly using it.
- **AutoGen** (now Microsoft Agent Framework): Conversational history as primary memory; external vector stores for long-term. Merging with Semantic Kernel, 1.0 GA targeted Q1 2026.
- **LlamaIndex**: Strongest at RAG-centric document retrieval; memory is essentially the index.

### Takeaway

MCP is the closest thing to a standard protocol for agent-to-KB communication. The memory server ecosystem is young but converging on entity-graph + vector-embedding as the dual representation. No protocol yet standardizes _memory semantics_ (forgetting, consolidation, conflict resolution) --- only the transport layer.

---

## 2. Encrypted Sync for Local-First Apps

### The Local-First Movement

[Ink & Switch](https://www.inkandswitch.com/) established the principles: data lives on-device, the cloud is a relay, users retain ownership. CRDTs are the foundational technology enabling multi-writer conflict-free replication.

### CRDT Libraries

- **[Automerge](https://automerge.org/)**: JSON document model, implemented in Rust with JS bindings via WASM. Mature but historically had performance issues with large documents. Good for structured data.
- **[Yjs](https://github.com/yjs/yjs)**: Modular framework, excels at text collaboration (ProseMirror, CodeMirror, Monaco integrations). Fastest CRDT library for text editing. JS-native.
- **[cr-sqlite](https://github.com/vlcn-io/cr-sqlite)**: Adds CRDT-based multi-master replication to SQLite via a runtime extension. Enables the familiar SQL model with convergent replication. The project was significant but the original maintainer (vlcn.io) has had intermittent activity.
- **[SQLiteSync](https://github.com/sqliteai/sqlite-sync)**: Newer SQLite extension using CRDTs for conflict-free sync. Aims to be a more maintained alternative to cr-sqlite.

### End-to-End Encryption + CRDTs

The hard problem: CRDTs require causal metadata to merge, but E2EE wants to encrypt everything. Solutions:

- **[SecSync](https://www.secsync.com/)** (by Nik Graf): Library that bridges Yjs/Automerge with E2EE. Encrypts CRDT operations while preserving enough metadata for merging. The most practical open-source solution currently.
- **Serenity Notes**: Production app demonstrating E2EE + CRDT collaborative notes.
- **Signal Protocol adaptation**: Signal's Double Ratchet works for messages but doesn't directly solve CRDT sync. The key management patterns (prekey bundles, session management) are relevant for establishing encrypted channels between devices.

### SQLite Sync State of the Art

For syncing SQLite across devices with encryption: no single turnkey solution exists. The practical approach is cr-sqlite or SQLiteSync for CRDT-based replication, combined with an encryption layer (libsodium secretbox for at-rest, Noise Protocol or TLS for in-transit). LiteFS (by Fly.io) provides FUSE-based SQLite replication but is single-writer only --- not suitable for multi-device local-first.

### Takeaway

Automerge (Rust) + SecSync-style encryption is the most promising stack for encrypted local-first sync. For SQLite specifically, cr-sqlite/SQLiteSync provide the CRDT layer but encryption must be composed separately. The gap is a unified, audited solution combining all three (CRDT + E2EE + SQLite).

---

## 3. Agent Wake-Up / Context Reconstruction

### The Core Problem

When a long-running agent resumes after interruption, it must reconstruct working context from persistent state. Naive approaches (replay full transcript) cause unbounded context growth. Research shows agent performance degrades after ~35 minutes of continuous operation, making efficient resumption critical.

### Recent Research (2025-2026)

- **[Agent Cognitive Compressor (ACC)](https://arxiv.org/abs/2601.11653)**: Bio-inspired memory controller that maintains a bounded **Compressed Cognitive State (CCS)** --- the sole persistent internal state across turns. Replaces transcript replay with online compression that preserves invariants while resisting drift. This is the closest analog to Cathedral's `wake()` in the research literature.

- **[ACON (Agent Context Optimization)](https://arxiv.org/abs/2510.00615)**: Unified framework compressing both environment observations and interaction histories. Reduces memory usage 26-54% while preserving >95% accuracy. Can be distilled into smaller compressor models.

- **[E-mem (Episodic Context Reconstruction)](https://arxiv.org/abs/2601.21714)**: Shifts from memory preprocessing to episodic reconstruction. Multiple assistant agents maintain uncompressed memory contexts; a master agent orchestrates global planning. Hierarchical, not monolithic.

- **[Active Context Compression](https://arxiv.org/abs/2601.07190)**: Autonomous memory management where the agent itself decides what to compress and when, rather than following a fixed schedule.

### Cognitive Science Parallels

The ACC work explicitly draws from cognitive science: the Compressed Cognitive State mirrors how human working memory maintains a limited-capacity representation that primes retrieval from long-term memory. Key concepts:

- **Memory priming**: Activating related concepts to reduce retrieval latency. Analogous to pre-loading relevant context into the agent's prompt.
- **Spreading activation**: In human semantic networks, activating one node partially activates connected nodes. Graph-based agent memory (like the MCP knowledge graph) naturally supports this.
- **Consolidation**: Sleep-dependent memory consolidation in humans has a rough analog in offline batch processing of agent memories (deduplication, summarization, forgetting).

### Other Systems with Resume Primitives

Beyond Cathedral's `wake()`:

- **OpenAI Agents SDK**: Session-based memory management with explicit session persistence and restoration.
- **Mem0**: Provides persistent memory that survives across conversations, letting agents recall past decisions without full reconstruction.
- **Zylos Research (2026)**: Documents patterns for long-running agents including checkpoint-and-resume with task decomposition.

### Takeaway

ACC's Compressed Cognitive State is the most principled approach: a bounded, updatable representation that replaces transcript replay. The cognitive science analogy of working memory as a "context reconstruction primer" rather than a complete record is the right mental model. Cathedral's `wake()` should aim for CCS-like properties: bounded size, online updateable, sufficient to prime retrieval of detailed memories from the graph.

---

## 4. Graph Visualization in WASM/Rust

### Rust WASM Frameworks

- **[egui](https://github.com/emilk/egui)**: Immediate-mode GUI, mature WASM support via eframe. Best for tool UIs and dashboards. Not optimized for large-scale graph rendering but works well for moderate graphs.
- **[Leptos](https://leptos.dev/)**: Reactive web framework, fine-grained reactivity, SSR support. Closest to a "Rust React." SVG manipulation is natural. Growing ecosystem including [lodviz-rs](https://autognosi.medium.com/scalable-crates-with-lod-in-wasm-interactive-svg-charts-in-pure-rust-for-dashboards-f6f5103dba38) for data visualization.
- **[Dioxus](https://dioxuslabs.com/)**: Cross-platform (web, desktop, mobile) with React-like API. Uses Tauri for desktop. Good DX but younger ecosystem than Leptos for web-specific work.

### Graph-Specific Libraries (Rust)

- **[petgraph](https://github.com/petgraph/petgraph)**: The standard graph data structure library in Rust. Not a visualization library, but provides the data model. Has a [WASM wrapper](https://github.com/urbdyn/petgraph-wasm) (work-in-progress) for NPM consumption.
- **[fdg-sim](https://github.com/grantshandy/fdg)**: Force-directed graph simulation library in Rust. Provides the physics simulation (Fruchterman-Reingold, etc.) without coupling to a specific renderer. Can target WASM. [Live examples](https://grantshandy.github.io/fdg/).
- **[egui_graphs](https://github.com/blitzarx1/egui_graphs)**: Interactive graph visualization widget built on egui + petgraph. The most complete Rust-native graph visualization solution, but inherits egui's rendering limitations for very large graphs.
- **[RustGrapher](https://github.com/iceHtwoO/RustGrapher)**: Force-directed graph simulation and visualization in Rust.

### JS-Side Options (for hybrid approaches)

- **[Sigma.js](https://www.sigmajs.org/)**: WebGL-based graph renderer. Handles large graphs (10k+ nodes) efficiently. Best option if you want Rust for compute (layout via WASM) and JS for rendering.
- **d3-force**: Canvas/SVG-based. Good for small-to-medium graphs (<1k nodes) with heavy customization needs. Not suitable for large graphs.
- **Hybrid pattern**: petgraph + fdg-sim compiled to WASM for layout computation, sigma.js or raw WebGL for rendering. This separates the expensive physics simulation (Rust/WASM) from the rendering (GPU via WebGL).

### Practical Assessment for Large Graphs

For rendering graphs with thousands of nodes in a browser:

1. **Best pure-Rust path**: petgraph + fdg-sim + egui_graphs. Limited by egui's rendering performance at scale.
2. **Best hybrid path**: petgraph + fdg-sim (WASM) + sigma.js (WebGL). Best performance, but requires JS interop.
3. **Best DX path**: Leptos + d3.js via wasm-bindgen. Familiar patterns, good for moderate-sized graphs.

### Takeaway

No single Rust-native solution handles large graph visualization end-to-end in the browser at production quality. The hybrid approach (Rust/WASM for graph algorithms and layout, WebGL/sigma.js for rendering) is the pragmatic choice for large graphs. For moderate graphs in a tool UI, egui_graphs is sufficient and keeps the stack pure Rust.
