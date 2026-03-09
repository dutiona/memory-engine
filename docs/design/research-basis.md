# Research Basis

The memory-engine design is grounded in 9 academic papers, community implementations (OpenClaw ecosystem, Reddit), and a 3-round multi-AI adversarial debate. This document maps each paper to its concrete contribution and identifies the cross-paper convergence points.

## Paper Contributions

| Paper             | arXiv      | Contribution to Design                                                                                                                                                                                                                                                                                                                                                        |
| ----------------- | ---------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **CoALA**         | 2309.02427 | Cognitive architecture framework classifying agent memory into 4 types. Mapped to the `FactType` enum: `Episodic`, `Semantic`, `Procedural`. Working memory is explicitly out of scope -- it lives in the consumer's context window.                                                                                                                                          |
| **Graphiti**      | 2501.13956 | Bi-temporal model with 4 timestamps per fact: `t_created`/`t_expired` (system time, when the engine learned/forgot it) and `t_valid`/`t_invalid` (real-world time, when the fact was/becomes true). Conflict detection delegated to LLM via trait.                                                                                                                            |
| **Mem0**          | 2504.19413 | CRUD conflict resolution pattern: the `ConflictArbiter` trait returns `CrudDecision::Add`, `Update`, `Delete`, or `Noop`. Graph-based memory with entity-centric retrieval informed the petgraph integration. Hierarchical user/session/agent levels informed the scope tree.                                                                                                 |
| **A-Mem**         | 2502.12110 | Self-organizing memory without predefined schemas. Zettelkasten-inspired linking between facts via the graph. The "one store, multiple projections" principle: no partitioned tables, just a `fact_type` tag.                                                                                                                                                                 |
| **Memory Survey** | 2512.13564 | Three-pass consolidation taxonomy: (1) local dedup -- merge near-duplicates, (2) cluster fusion -- group into thematic summaries, (3) global integration -- update core understanding. Forgetting strategies: time expiration (Ebbinghaus decay), access frequency, informational value (graph connectivity). All three are signals in the `ForgetPolicy` importance scoring. |
| **AgeMem**        | 2601.01885 | Agentic memory framework where LTM and STM are jointly managed via explicit tool-based operations. The agent decides what/when to store, retrieve, summarize, discard. Validated the trait-based design: the engine provides primitives, the consumer provides intelligence.                                                                                                  |
| **SparseMemFT**   | 2510.15103 | Sparse memory fine-tuning achieves only 11% knowledge drop (vs 89% full fine-tuning, 71% LoRA). Informed the boundary decision: the engine is retrieval-only. Parameter updates (fine-tuning, LoRA adaptation) are explicitly out of scope.                                                                                                                                   |
| **Memento**       | 2508.16153 | Hierarchical memory with summarization. Case-based reasoning without fine-tuning. Informed the consolidation level hierarchy: `Local`, `Cluster`, `Global` in the `ConsolidationLevel` enum. The "dream cycle" concept (nightly batch consolidation) is the intended usage pattern for `consolidate()`.                                                                       |
| **Doc-to-LoRA**   | --         | Document-to-adapter pipeline using hypernetworks. Confirmed the engine boundary: store and retrieve facts; the consumer decides what to do with retrieved results (including whether to fine-tune, generate adapters, or just use them in context).                                                                                                                           |

## Cross-Paper Convergence

Several design decisions emerged from independent convergence across multiple papers:

**Bi-temporal is necessary, not optional.** CoALA identifies the need for temporal reasoning. Graphiti provides the concrete 4-timestamp model. Mem0's CRUD pattern operates on temporal facts. The survey catalogs temporal contradiction resolution strategies. All roads lead to bi-temporal.

**One store, multiple projections.** A-Mem explicitly rejects predefined schema layers. CoALA defines 4 memory types but not 4 storage backends. AgeMem treats memory operations as uniform tools. The implementation consequence: one `facts` table with a `fact_type` tag, not separate tables per type.

**Traits over built-in intelligence.** AgeMem and Mem0 both use LLM calls for memory operations (embedding, summarization, conflict resolution). But they disagree on which LLM and how to call it. The trait abstraction resolves this: the engine defines _what_ intelligence is needed; the consumer provides _how_.

**Event-sourcing as migration safety net.** The multi-AI debate reached consensus: event-sourced log as source of truth. The practical consequence is that the engine can replay its entire history into any future storage backend (SurrealDB, LanceDB, or anything else that matures). No lock-in.

**Consolidation is lossy compression with a safety net.** The survey explicitly states: "Semantic summarization is lossy compression -- prioritizes global coherence over local precision." The mitigation: never delete raw events. Summaries are derived views; the event log is immutable.

**Forgetting is multi-signal, not single-signal.** The survey catalogs three forgetting strategies. Graphiti uses time-based expiration. Mem0 uses LLM-arbitrated deletion. Community implementations (Ori Mnemos) add graph-aware decay. The engine combines all four signals: Ebbinghaus time decay, access frequency, graph connectivity, and base importance.

## Relationship Graph

CoALA (2023) is the conceptual hub. Five papers extend or refine its framework:

```
CoALA <--extends-- AgeMem, Memento, A-Mem, Graphiti, SparseMemFT
Graphiti <--extends-- Mem0
Survey --cites--> all other papers

Comparison pairs:
  AgeMem   <-> Memento     (both: experience-based reasoning)
  A-Mem    <-> Mem0        (both: automated memory management)
  A-Mem    <-> AgeMem      (both: agentic memory frameworks)
  Mem0     <-> Graphiti    (direct competitor; Mem0 paper benchmarks against Graphiti)
  Doc-LoRA <-> SparseMemFT (both: parameter-efficient continual learning)
```

## Conceptual Boundaries

A key distinction that emerged from the research:

| Concept       | Definition                                                                                                           | System                                              |
| ------------- | -------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------- |
| **Knowledge** | Raw content: documents, code, papers, web pages. Objective. Exists independently of the agent.                       | Knowledge Base (e.g., research-index, RAG pipeline) |
| **Memory**    | What the agent has internalized: facts derived from experience, observations, interactions. Subjective to the agent. | Memory Engine (this project)                        |
| **Wisdom**    | The model's learned representations, reasoning capabilities, behavioral patterns. Encoded in weights.                | The model itself (out of scope)                     |

The engine operates in the Memory layer. It does not store raw documents (that is the knowledge base's job), and it does not modify model weights (that is fine-tuning's job). It stores _what the agent has learned from its experiences_ as explicit, queryable, temporal facts.

The planned Phase 5 `KnowledgeBaseConnector` trait bridges Memory and Knowledge: facts can carry a `KnowledgeRef` URI pointing to the source document in a knowledge base, but the engine never fetches or stores the raw content itself.
