# memory-engine

Embedded memory engine for autonomous AI agents.

Provides 5 core primitives for agent long-term memory:
**Ingest**, **Query**, **Consolidate**, **Forget**, and **Resolve**.

Built on SQLite (WAL mode), with hybrid search (FTS5 + vector + RRF),
bi-temporal fact management, and trait-based extensibility.
Zero network or LLM dependencies in the core crate.

```{toctree}
:maxdepth: 2
:caption: Getting Started

getting-started/index
```

```{toctree}
:maxdepth: 2
:caption: Usage Guide

usage/index
```

```{toctree}
:maxdepth: 2
:caption: Advanced Topics

advanced/index
```

```{toctree}
:maxdepth: 2
:caption: Design & Research

design/index
```

```{toctree}
:maxdepth: 2
:caption: Reference

reference/index
```
